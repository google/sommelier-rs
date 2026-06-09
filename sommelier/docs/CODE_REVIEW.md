# Code Review: `zcr_keyboard_extension_v1` — virtwl upstream diff

**Reviewer:** Google Engineer  
**Branch:** `feature/keyboard-extension-v2` vs `origin/virtwl`  
**Files changed:** 13 (+2258 / −37)  
**Tests:** 24 passed, 0 failed ✅

---

## Summary

This CL adds ChromeOS accelerator passthrough to `sommelier-rs` via the
`zcr_keyboard_extension_v1` protocol. The scope is well-contained: a new
`accelerator.rs` module, extension of `KeyboardHandler`, shadow-table typed
ID wrappers (`GuestId`/`HostId`), removal of sentinel placeholder IDs, and
bidirectional queue flushing in the proxy loop.

Overall the implementation is solid: unsafe code is minimal and well-justified,
the type system catches direction bugs at compile time, and test coverage is
thorough. The comments below range from **blocking** issues to **nits**.

---

## Blocking / Must-Fix

### 1. Opcode constants are load-bearing but have no compile-time tie to the XML

**File:** `src/handler/keyboard.rs:41–53`

```rust
// If the XML is ever updated, these constants MUST be kept in sync.
const ZCR_EXTENDED_KEYBOARD_DESTROY: u16 = 0;
const ZCR_EXTENDED_KEYBOARD_ACK_KEY: u16 = 1;
const ZCR_KEYBOARD_EXTENSION_GET_EXTENDED_KEYBOARD: u16 = 0;
```

The comment says "MUST be kept in sync" but there is no mechanism that
enforces this. The build system already generates a Rust protocol file from
the XML (`build.rs`). The generated code almost certainly contains typed
request/event enumerations or associated constants. **The generated opcode
values should be referenced here rather than re-declared as magic numbers.**

If `build.rs`'s code-gen does not expose per-message opcode constants today,
add them (or at minimum add a `#[test]` that reads the XML at test time and
asserts the values match). A stale constant here silently corrupts every ack
and destroy message sent to Exo.

### 2. `build_wayland_msg` duplicates serialization already done by `MessageBuilder`

**File:** `src/handler/keyboard.rs:160–177` and `src/handler/registry.rs:96–105`

Two callers construct raw wire frames by hand (length word, opcode packing,
`extend_from_slice`). `MessageBuilder` exists precisely to encapsulate this.
The keyboard handler's private `build_wayland_msg` is an API-compatible
alternative — but having two such helpers makes every future reader wonder
which to use, and risks them diverging.

Either:
- Extend `MessageBuilder` to accept an explicit sender-id and opcode, making
  it suitable for both use-sites; or
- At minimum, add a `debug_assert` that the hand-rolled layout matches
  `MessageBuilder`'s output in a unit test.

This is not a correctness bug today, but it is a maintainability hazard.

---

## Should-Fix

### 3. `allocate_host_id` can infinite-loop if the ID space is exhausted

**File:** `src/state.rs`

```rust
// NOTE: If every ID in [2, u32::MAX] is simultaneously live this loop
// will never terminate.
loop {
    ...
}
```

The comment acknowledges this. Given that Exo caps object counts in the
thousands, infinite looping is not a realistic runtime concern — but the
comment's passive "not a realistic concern" framing is not good enough for a
production proxy. Replace the `loop` with a bounded attempt:

```rust
// Try at most u32::MAX times; panic rather than spin forever.
for _ in 0..u32::MAX {
    ...
    if id >= 2 && !self.host_to_guest.contains_key(&id) {
        return id;
    }
}
panic!("sommelier: host object ID space exhausted (this cannot happen in practice)");
```

A `panic!` with a clear message is far preferable to a process freeze that
requires `SIGKILL` to recover from.

### 4. `on_keymap` does not reset `state` when keymap reloads

**File:** `src/handler/keyboard.rs:334–339`

```rust
Some(keymap) => {
    self.state = Some(xkb::State::new(&keymap));
    self.keymap = Some(keymap);
```

This is correct for the common case (compositor sends one keymap per session).
However, the Wayland spec explicitly allows the compositor to send a new
`wl_keyboard.keymap` at any time (e.g. when the user switches input method or
keyboard layout). When that happens, `dropped_keys` should be cleared:
keys dropped under the old keymap (whose keysyms may differ under the new one)
could cause stuck-key state. A one-liner `self.dropped_keys.clear()` in the
`Some(keymap)` branch makes the invariant explicit.

### 5. `send_ack_key` is `fn` on `&self` but calls `push` on `ctx`

**File:** `src/handler/keyboard.rs:258`

```rust
fn send_ack_key(&self, ctx: &mut Context, ...)
```

This method mutates `ctx.client_to_host_queue` (push) but takes `&self`.
That is fine from a Rust perspective, but it makes the signature misleading:
a reader expects `&self` methods to be pure queries. Consider either:

- Making it `&mut self` (consistent with `on_key`), or
- Keeping it `&self` but adding a doc comment noting the side-effect on `ctx`.

Same applies to `bind_extended_keyboard`.

### 6. Missing `<Hyper>` / `<Super>` alias asymmetry

**File:** `src/accelerator.rs:104`

```rust
"<super>" | "<win>" => modifiers |= SUPER_MASK,
```

`<Win>` is accepted as a shorthand for `<Super>`. But C sommelier configs
also sometimes use `<Search>` (the ChromeOS Search/Launcher key maps to Super
on Chromebooks). If you want this to be a drop-in replacement for C sommelier
configs, add `"<search>"` as an alias here. Not blocking, but worth a TODO
comment at minimum.

### 7. `KeyboardHandler::new()` and `Default` are split across two `impl` blocks

**File:** `src/handler/keyboard.rs:134–151`

```rust
impl KeyboardHandler {
    pub fn new() -> Self { ... }
}

impl Default for KeyboardHandler {
    fn default() -> Self { Self::new() }
}
```

Rust convention (and Clippy's `clippy::new_without_default` lint) wants
`Default` implemented when `new()` takes no arguments. That's done — good.
But having two separate `impl KeyboardHandler` blocks immediately adjacent
(one for `new()`, one for message helpers) is noise. Merge them.

---

## Nits / Style

### 8. `WL_KEY_RELEASED = 0` should use a named constant in the match arm

**File:** `src/handler/keyboard.rs:455`

```rust
WL_KEY_RELEASED => { ... }
```

This is correct, but note that `WL_KEY_RELEASED` is `0` and an unnamed
`_` arm would match any other value including `0`. The current ordering
(`PRESSED` = 1 first, `RELEASED` = 0 second, `other` last) is correct.
Add an inline comment that the order matters because `0` is the last explicit
arm before the catch-all.

### 9. `MmapView` is `pub(crate)` but only used in one module

`MmapView` has `pub(crate)` visibility and is exported from `keyboard.rs` via
`pub(crate) struct MmapView`. Its only consumer is `on_keymap` in the same
file. Make it `pub(super)` or just private (`struct MmapView`) since it is an
implementation detail of the keymap-loading logic.

### 10. `registry.rs`: `zcr_keyboard_extension_v1` bind uses `MessageBuilder` partially

**File:** `src/handler/registry.rs:88–104`

```rust
let mut builder = MessageBuilder::new();
builder.write_u32(name);
builder.write_string(interface);
builder.write_u32(1);
builder.write_u32(host_id);

let mut full_msg = Vec::new();
full_msg.extend_from_slice(&registry_host_id.to_ne_bytes());
let len = (builder.payload.len() + 8) as u32;
let word2 = (len << 16) | (wl_registry::REQ_BIND as u32);
...
```

This is the same hand-rolled framing as point 2. All the other `on_global`
arms in `registry.rs` follow the same pattern, so this is consistent within
the file. But it further motivates fixing point 2: one `MessageBuilder::build`
call should cover the header too.

### 11. `allocate_host_id` wrap logic is clever but not obviously correct

**File:** `src/state.rs`

```rust
self.next_host_id = self.next_host_id.wrapping_add(1).max(2);
if id >= 2 && !self.host_to_guest.contains_key(&id) {
    return id;
}
```

The `id >= 2` guard on the *old* value of `next_host_id` combined with
`.max(2)` on the *new* value is correct but requires careful reading.
A short inline comment explaining the two-invariant dance would help:

```rust
// Advance the counter; .max(2) handles the 0→1→2 skip after u32::MAX wrap.
self.next_host_id = self.next_host_id.wrapping_add(1).max(2);
// Check the *pre-advance* id (which we tentatively return).
if id >= 2 && !self.host_to_guest.contains_key(&id) {
    return id;
}
```

### 12. Test helper `load_test_keymap` passes `keymap_str.len() + 1` as `size`

**File:** `src/handler/keyboard.rs:589`

```rust
handler.on_keymap(ctx, 1, fd.as_raw_fd(), keymap_str.len() as u32 + 1);
```

The `+ 1` accounts for the null terminator that `on_keymap` strips. The
`size` argument to `wl_keyboard.keymap` per the Wayland spec is "the number
of bytes in the keymap, **including the null terminator**". So `+ 1` is
technically correct — but it means the test is also testing the null-stripping
logic, which is intentional but should be noted in the doc comment. The
`keymap_loads_from_non_rewound_memfd` test does the same and this is fine;
just add a comment in `load_test_keymap` explaining why `+1`.

### 13. Unused `MessageBuilder` import in `keyboard.rs`

**File:** `src/handler/keyboard.rs:31`

```rust
use crate::wire::{Action, MessageBuilder};
```

`MessageBuilder` is used in `on_enter` and `on_leave` (for the text-input
synthetic events). This is fine, but given those methods were pre-existing,
it's worth checking if `MessageBuilder` is also used anywhere in the new
keyboard-extension code paths. It is not — the new paths use
`build_wayland_msg` instead. That is consistent but reinforces point 2.

---

## Positive Observations

- **`GuestId` / `HostId` newtypes** are an excellent choice. The
  `from_event_sender` / `from_request_sender` constructor names make the
  direction explicit at every call site, and the `PhantomData<*mut ()>` trick
  for `!Sync` without a raw pointer field is idiomatic.

- **`MmapView` RAII** correctly confines all `unsafe` to one type. The
  `from_fd` → `as_bytes` → `Drop` contract is clearly documented and sound.

- **Sentinel ID removal** in `registry.rs` is a clean simplification. The old
  `0xFE000000 | host_id` placeholder pattern was brittle (potential ID
  collision, confusing semantics). `track_host_interface` is the right
  abstraction.

- **Bidirectional queue flush** in `proxy.rs` is necessary and the ordering
  rationale (forwarded key arrives at guest before ack reaches host, within
  Exo's 1000 ms TTL) is well-documented.

- **Test coverage** is comprehensive: 24 tests covering normal flow, edge
  cases (zero-size keymap, invalid UTF-8, invalid XKB, unknown format),
  regression cases (non-rewound fd, dropped-keys on release), and structural
  invariants (wire frame encoding). The wire-frame byte-level assertions are
  particularly valuable for catching silent serialization regressions.

- **`SOMMELIER_ACCELERATORS` parse robustness**: degrading to empty list on
  parse error (rather than panicking or silently ignoring all keys) is the
  right policy for a proxy that must not crash every app in the container.

---

## Summary Table

| # | Severity | File | Issue |
|---|----------|------|-------|
| 1 | **Blocking** | `keyboard.rs` | Opcode constants not tied to generated XML constants |
| 2 | **Should-fix** | `keyboard.rs`, `registry.rs` | Duplicate wire-frame serialization logic |
| 3 | **Should-fix** | `state.rs` | `allocate_host_id` can loop forever on exhaustion |
| 4 | **Should-fix** | `keyboard.rs` | `on_keymap` should clear `dropped_keys` on keymap reload |
| 5 | Nit | `keyboard.rs` | `send_ack_key(&self)` signature misleads (mutates ctx) |
| 6 | Nit | `accelerator.rs` | Missing `<Search>` alias for ChromeOS Search key |
| 7 | Nit | `keyboard.rs` | Split adjacent `impl KeyboardHandler` blocks |
| 8 | Nit | `keyboard.rs` | Comment the match-arm ordering in `on_key` |
| 9 | Nit | `keyboard.rs` | `MmapView` visibility wider than needed |
| 10 | Nit | `registry.rs` | Hand-rolled framing (consistent with existing style but reinforces #2) |
| 11 | Nit | `state.rs` | `allocate_host_id` wrap logic needs inline comment |
| 12 | Nit | `keyboard.rs` | `+ 1` in test helper should be documented |
| 13 | Nit | `keyboard.rs` | `MessageBuilder` imported but not used by new code paths |

**LGTM with the two blocking and four should-fix items addressed.**
