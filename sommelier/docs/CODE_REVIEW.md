# Code Review: `feature/keyboard-extension-v2` vs `virtwl`

**Reviewer:** (Google Engineer)
**Date:** 2026-06-10
**Scope:** `git diff virtwl..HEAD` — 1,953 insertions, 80 deletions across 19 files

---

## Summary

This CL adds `zcr_keyboard_extension_v1` support to `sommelier-rs`, implementing
the accelerator-passthrough mechanism that lets the ChromeOS host compositor
(Exo) intercept keyboard shortcuts (e.g. Ctrl+Space for IME) before the Wayland
guest sees them. The core approach is sound and the implementation is
significantly more robust than a naïve port of the C sommelier reference.
The typed `GuestId`/`HostId` wrappers in particular prevent an entire class of
direction-confusion bugs that were endemic in the original C code.

Overall this is **close to LGTM**, with a handful of items that should be fixed
or at least discussed before submission.

---

## Critical / Must-Fix

### C1 — `unsafe impl Send for KeyboardHandler` is unsound if `xkb::State` is `!Send`

**File:** `sommelier/src/handler/keyboard.rs`

The comment says xkb objects are only ever accessed from the single Tokio task.
That is true *today*, but the `unsafe impl Send` is a global assertion visible
to the entire compiler. If someone later moves the handler into a `tokio::spawn`
or a `rayon` pool by accident, the compiler will not catch it.

The safer pattern is to wrap the non-Send fields in a `newtype` that is
explicitly `!Sync` and document the contract there, or — if the Tokio executor
is always single-threaded for this code path — pin the runtime to
`tokio::runtime::Builder::new_current_thread()` so that `Send` bounds are never
required in the first place.

As written, this is technically unsound. It will not cause UB in practice
*unless* the task migration invariant is violated, but it is a footgun. At
minimum, the comment should say **"this impl must be audited any time the
executor or handler lifecycle changes"** more prominently.

**Recommendation:** Either gate on a single-threaded executor (preferred for
this use case) or add a `PhantomData<*mut ()>` field to statically opt the
struct out of `Sync` and add a runtime assertion that the handler is only ever
used from one thread.

---

### C2 — `build_wayland_msg` encodes size incorrectly for multi-word payloads

**File:** `sommelier/src/handler/keyboard.rs`, line ~158

```rust
msg.extend_from_slice(&((total_len << 16) | opcode as u32).to_ne_bytes());
```

The Wayland wire format packs `size` in the **upper 16 bits** of the second
word and `opcode` in the **lower 16 bits**. `total_len << 16` is a `u32`
left-shift — this is correct only if `total_len` fits in 16 bits. The
`debug_assert!` checks this at test time, but it fires only in debug builds.

More critically: `opcode` is declared as `u16` but is widened to `u32` with
`| opcode as u32`. If `opcode` is wider than 16 bits this truncates silently —
but since `opcode` is already `u16`, casting it to `u32` is fine. No bug here
on the current call-sites, but the encoding comment says:

> `[size<<16|opcode(4)]`

The comment is wrong — the size field is **bits 31:16** of the word (it is
*already* the upper half), not `size << 16`. The `total_len` value must be the
*total message length in bytes* (header + payload), which is what is computed.
Verify this matches the actual wire format:

- Word 1: `object_id` (u32)
- Word 2: `(message_size_bytes << 16) | opcode` (u32)

That is: the **upper 16 bits** contain total byte-length, **lower 16** contain
opcode. The current code does this correctly (`(total_len << 16) | opcode`).
The comment is just slightly confusing. **Fix the comment.**

The test `build_wayland_msg_encodes_frame_correctly` validates this, but it
only runs in test builds. Consider also validating with a const-eval if possible
in the future.

---

### C3 — `smallvec = "*"` in `Cargo.toml` — **must not land upstream**

**File:** `sommelier/Cargo.toml`

```toml
smallvec = "*"
```

Wildcard versions are explicitly prohibited by Cargo policy and will cause `cargo
publish` to reject the crate. More practically, they break reproducibility —
a `cargo update` six months from now can silently pull in a semver-incompatible
`smallvec 2.x`.

**Fix:** Pin to a concrete version, e.g. `smallvec = "1.13"`.

Additionally, consider whether `SmallVec` is pulling its weight here. The
hot-path benefit (avoiding a `Vec::new()` allocation for 16-byte ack messages)
is real, but it adds a dependency and complicates the queue type signature
(`Vec<(SmallVec<[u8;32]>, Vec<RawFd>)>`). An alternative is to keep `Vec<u8>`
for the buffers and preallocate with `Vec::with_capacity(16)` — the savings are
identical on the happy path since allocators cache small sizes.

This is worth a quick team discussion: if the performance gain is deemed
necessary, pin the version and explicitly enable `smallvec`'s `union` feature
for the true inline storage.

---

## Non-Critical / Suggestions

### S1 — `bind_extended_keyboard` is called from `on_enter`; document the ordering invariant

**File:** `sommelier/src/handler/keyboard.rs`, `bind_extended_keyboard`

The comment correctly explains why `on_enter` is used instead of
`wl_seat.get_keyboard`. However, Exo's `SetNeedKeyboardKeyAcks(true)` is set
the moment it processes `get_extended_keyboard`. If a `wl_keyboard.key` event
was already queued by the compositor (e.g. auto-repeat keys queued before
`on_enter` lands), those events will arrive **before** `ack_key` mode is
enabled, meaning the TTL window will not be running for them.

This is not a bug in your code (C sommelier has the same race), but it should be
documented. Add a comment noting that keys pressed before `on_enter` will not
have an active TTL and will be handled by Exo's default policy.

---

### S2 — `dropped_keys` is a `HashSet<u32>` but will almost always contain 0 or 1 elements

**File:** `sommelier/src/handler/keyboard.rs`

A `HashSet` for tracking dropped keys has `O(1)` ops but non-trivial constant
overhead (heap allocation, hash state). In practice a user holds at most 2–3
keys simultaneously. A `SmallVec<[u32; 4]>` or even a plain `[Option<u32>; 4]`
with linear search would be allocation-free for the common case.

Not a correctness issue — just a micro-optimization worth considering if
allocator pressure matters in the event loop.

---

### S3 — `on_modifiers` only tracks `DEPRESSED | LATCHED`, not `LOCKED`

**File:** `sommelier/src/handler/keyboard.rs`, `on_modifiers`

```rust
let components = xkb::STATE_MODS_DEPRESSED | xkb::STATE_MODS_LATCHED;
```

`Caps Lock` is a `LOCKED` modifier. If an accelerator list includes
`<Shift>something` (where Shift is effectively active via Caps Lock), the check
will fail because `STATE_MODS_LOCKED` is excluded. The C sommelier uses
`XKB_STATE_MODS_EFFECTIVE` which ORs all three. Consider whether this is the
intended policy. If Caps Lock accelerators are out of scope, add a comment
explaining why locked modifiers are excluded.

---

### S4 — `allocate_host_id` wrap-around still has a subtle issue

**File:** `sommelier/src/state.rs`

```rust
self.next_host_id = self.next_host_id.wrapping_add(1).max(2);
```

The comment says "handles the u32::MAX → 0 → 2 wrap in one step", but the
logic is:

1. `next_host_id = u32::MAX`
2. `wrapping_add(1)` → `0`
3. `.max(2)` → `2`

Then the outer check `if id >= 2` tests the **old** value (`u32::MAX`), which
passes. So `u32::MAX` would be returned if it is not in the map. This is
actually correct — the only change is skipping 0 and 1 in the _next_ cycle.

The subtle issue: if the table is completely full (all IDs from 2 to
`u32::MAX` are live), this loops forever. The original code had the same bug.
For a reviewer this is a known limitation acceptable in practice; just add a
comment explicitly noting that the function assumes the table is never
exhausted.

---

### S5 — `remove_host_interface` comment should mention the recycle window

**File:** `sommelier/src/state.rs`

The doc comment says "prevent stale events for the recycled ID from being
dispatched". Add a note that `peek_key` events (protocol v2+) may arrive after
the `destroy` request is sent but before Exo processes it (pipelining). Exo
should not send events after seeing `destroy`, but documenting the potential
race helps future auditors.

---

### S6 — `is_host_accelerator` clones `keysym_to_lower` at match-time even though it's pre-normalised at parse-time

**File:** `sommelier/src/handler/keyboard.rs`, `is_host_accelerator`

```rust
let lower_sym = crate::accelerator::keysym_to_lower(sym.raw());
for acc in accelerators {
    if self.modifiers == acc.modifiers && lower_sym == acc.symbol {
```

The `lower_sym` call is a cross-FFI call on each key event. For most keysyms
this is a no-op (they are already lowercase), but the FFI overhead is real.
Consider caching the lowercased sym: it is already computed at parse time (in
`parse_accelerator`). The match site could be simplified to just compare
`sym.raw()` if the production `Accelerator` is always stored lowercased (which
it is — the parse sets `symbol: keysym_to_lower(...)`). However, the runtime
keysym from `key_get_one_sym` may still be uppercase (e.g. `KEY_A` when Shift
is held), so the lowercasing at the match site is correct. The FFI cost is
acceptable for key-event frequency (60 Hz worst case for auto-repeat).

This is not a bug — just noted for completeness.

---

### S7 — Wire message hand-serialization is fragile; consider generated protocol bindings

**File:** `sommelier/src/handler/keyboard.rs` and `registry.rs`

The CL hand-serializes Wayland messages with `extend_from_slice` and hardcoded
opcodes (`ZCR_EXTENDED_KEYBOARD_ACK_KEY = 1`). The codegen infrastructure from
`wayland_codegen` already generates typed request builders for other protocols.
Using generated code for `zcr_keyboard_extension_v1` requests would eliminate
the opcode magic constants and the risk of mismatched payload sizes.

The `build_wayland_msg` helper is a good abstraction layer, but it still
requires callers to know the opcode numerically. At minimum, the opcode
constants should live in the generated `keyboard_extension_unstable_v1` module,
not in `keyboard.rs`.

---

### S8 — Empty protocol handler impls should get a doc comment

**File:** `sommelier/src/handler/keyboard.rs`

```rust
impl crate::protocols::keyboard_extension_unstable_v1::zcr_keyboard_extension_v1::ZcrKeyboardExtensionV1Handler for KeyboardHandler {}
impl crate::protocols::keyboard_extension_unstable_v1::zcr_extended_keyboard_v1::ZcrExtendedKeyboardV1Handler for KeyboardHandler {}
```

These look suspicious at first glance (why implement a trait with no methods?).
Add a comment: `// No host→client events expected; impl satisfies the dispatch trait.`

---

## Positive Observations

These are things done *well* that I'd call out in a real review:

- **`GuestId`/`HostId` typed wrappers** — excellent. This is the single most
  important correctness improvement over the C reference. The direction bug in
  `on_release` (using guest ID as host map key) is exactly the kind of error
  that killed the original implementation, and the type system now makes it
  impossible.

- **`MmapView` RAII** — clean, minimal, well-documented unsafe surface. The
  `from_fd(len=0) → None` guard is important since `mmap(len=0)` is POSIX UB.

- **Graceful degradation on `SOMMELIER_ACCELERATORS` parse error** — logging
  and falling back to "no accelerators" is the right behavior for a proxy.
  Crashing would break every app in the container.

- **Bidirectional queue flush in `proxy.rs`** — this was a silent latency
  bug in any implementation that didn't flush back-channel messages. The
  `ack_key` arrives at Exo within the same epoll cycle that delivered the key
  event, well inside the 1 000 ms TTL.

- **`bind_extended_keyboard` idempotency** and the corresponding test — good
  defensive coding; calling `on_enter` multiple times for the same keyboard must
  not send duplicate `get_extended_keyboard` requests.

- **Test coverage** — 14 unit tests, including a regression for the
  `on_keymap`-via-`mmap` path and the `dropped_keys.clear()` on release.
  This is solid for a first pass. The tests directly assert wire byte layout,
  which is the right level of detail for a serialization layer.

- **Removal of placeholder sentinel IDs** (`0xFE000000 | host_id` etc.) from
  `registry.rs` — these magic numbers were a ticking time bomb; the new
  `track_host_interface`-only approach is much cleaner.

---

## Pre-Submit Checklist

- [ ] **C3:** Pin `smallvec` to a concrete version (or remove the dependency)
- [ ] **C2:** Fix the `build_wayland_msg` comment to match the actual wire encoding
- [ ] **C1:** Either document the `unsafe impl Send` invariant more strongly or switch to single-threaded executor
- [ ] **S3:** Decide and document the `LOCKED` modifier policy in `on_modifiers`
- [ ] **S4:** Add a comment in `allocate_host_id` noting the "table full" infinite loop
- [ ] **S7:** Move `ZCR_EXTENDED_KEYBOARD_ACK_KEY` etc. to the generated protocol module
- [ ] Run `cargo clippy --all-targets -- -D warnings` — verify clean

---

*This review was performed against `git diff virtwl..feature/keyboard-extension-v2`
(~1,953 insertions). No runtime testing was performed by the reviewer.*
