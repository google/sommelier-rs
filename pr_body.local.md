# Pull Request: Wayland IME Translation Bridge and Compositor v4 Emulation

## Overview
This PR implements a translation layer in `sommelier-rs` that bridges the guest client's **`zwp_text_input_v3`** requests to the host compositor's **`zwp_text_input_v1`** (and **`zcr_text_input_extension_v1`**) protocols, and translates **`wl_compositor` version 4** (`wl_surface.damage_buffer`) requests into **`wl_compositor` version 3** (`wl_surface.damage`) requests.

These shims are fully isolated behind a toggleable flag (`--emulate-compat-shims`) to preserve existing non-IME deployments.

---

## Rationale & Key Architecture

### 1. The IME Gap (v3 ↔ v1)
Modern graphical toolkits (e.g., WINE's native `winewayland.drv`, GTK4, Qt6) strictly require the standard `zwp_text_input_v3` protocol for Input Method Editor (IME) support. However, ChromeOS/Exo hosts only advertise `zwp_text_input_v1` and the proprietary `zcr_text_input_extension_v1`.

By intercepting the registry advertisement:
- The guest is presented with `zwp_text_input_manager_v3`.
- The proxy binds `zwp_text_input_manager_v1` and `zcr_text_input_extension_v1` on the host.

### 2. Transactional Emulation (`done` events)
- **Problem**: `zwp_text_input_v3` is transactional (events are batched until a `done` event is received). `zwp_text_input_v1` applies text updates immediately.
- **Solution**: The translation layer caches changes and immediately appends a synthesized, monotonically increasing `done` serial event to the client when translating host `on_preedit_string`, `on_commit_string`, and `on_delete_surrounding_text` events.

### 3. Surface & Seat Focus Coordination
IME activation/deactivation requires coordinating both keyboard focus (`on_enter`/`on_leave`) and client activation commits. If a client attempts to commit activation before the seat/surface mappings are resolved on the host, the activation is deferred and retried when keyboard focus is established.

### 4. Compositor v4 Damage Emulation
- **Problem**: Hosts running version 3 of `wl_compositor` do not support `wl_surface.damage_buffer` (added in version 4).
- **Solution**: Intercepts `wl_surface.damage_buffer` (specified in buffer coordinates), scales the coordinates back to surface-local space (outward-rounded), and issues a standard `wl_surface.damage` request.

---

## Detailed Changes

### `sommelier/src/handler/`
- **`text_input.rs`**: Main translation bridge. Implements the `TextInputV3Handler`, `TextInputV1Handler`, and manager equivalents. Maps v3 content purposes to extended v1 input types, handles lossy conversion for non-adjacent deletions, and implements transaction boundaries.
- **`compositor.rs`**: Tracks surface buffer scale factors and intercepts `damage_buffer` to scale and forward `damage` events to the host.
- **`keyboard.rs`**: Updates seat/surface focus tracking to retry deferred activations on `on_enter` and clean up deactivations on `on_leave`.
- **`opcodes.rs`**: Centralizes protocol opcodes to avoid raw numeric magic numbers.
- **`registry.rs`**: Translates registry advertisement and caps the host binding version to 3 while advertising version 4 to the guest.

### `sommelier/src/state.rs`
- Introduces `emulate_compat_shims` configuration flag.
- Adds `GuestId` and `HostId` type safety wrappers to prevent ID direction/mapping errors.
- Integrates `keysym_to_keycode` mapping for IME composition.

---

## Verification & Testing
A total of **44 unit and integration tests** have been introduced or refactored to validate correctness across all components under `sommelier/src/handler/tests`.

All tests pass cleanly:
```bash
$ nix develop --command bash -c "cd _local/sommelier-rs/sommelier && cargo test"
running 67 tests
...
test result: ok. 67 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
```

### Covered Test Cases:
1. Monotonic `done` serial sequence matching.
2. Handling `i32::MIN` negation overflows inside `delete_surrounding_text`.
3. Outward rounding behavior for sub-pixel surface damage conversions.
4. Scale-factor clamping (scale must be >= 1) to prevent division-by-zero crashes.
5. Graceful parsing failures for malformed UTF-8/XKB keymaps.
6. Multi-seat/text input surface lifecycle teardowns.
