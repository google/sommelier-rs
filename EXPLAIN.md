# Keyboard Shortcut Inhibition — Problem, Root Cause & Fix

## The Problem

When a Wayland app (e.g. Chromium) runs inside a ChromeOS Linux container via
`sommelier`, pressing keys like **Ctrl+Space** or **Alt+[** was being intercepted
by ChromeOS/Exo and handled as host accelerators — even though the intent was for
the guest application to receive them.

Conversely, in a broken earlier state, *all* shortcuts were being swallowed by the
guest and never reaching the host — meaning host-level keys like the ChromeOS
launcher (Super_L) stopped working entirely.

---

## Root Cause

ChromeOS Exo handles keyboard events in two modes:

### Mode 1 — Default (no ack protocol)
```
Physical key → Exo sends wl_keyboard.key to client
                    AND immediately processes Chrome accelerator
```
The key is double-processed: both the guest app and ChromeOS act on it.
There is no way for the client to say "I'll handle this one."

### Mode 2 — Ack mode (`zcr_keyboard_extension_v1` bound)
```
Physical key → Exo stores key in pending_key_acks_ (1s timeout)
             → Exo sends wl_keyboard.key to client
             → Client sends ack_key(serial, HANDLED or NOT_HANDLED)
               - HANDLED    → Exo skips the accelerator. Guest handles it.
               - NOT_HANDLED → Exo runs ProcessAccelerator(). Host handles it.
               - timeout    → treated as NOT_HANDLED
```

**This is the only real mechanism for controlling accelerator behavior in Exo.**

The `zwp_keyboard_shortcuts_inhibit_manager_v1` standard Wayland protocol exists
but is effectively a **no-op** in Exo — it sets a flag on the surface that is
never consulted in the accelerator dispatch path.

### What was wrong in the upstream `virtwl` branch

The upstream `virtwl` branch (the starting point) had **no implementation of
`zcr_keyboard_extension_v1` at all**. Without it, Exo operates in Mode 1
(the default): it sends `wl_keyboard.key` to the client *and simultaneously*
processes Chrome accelerators on its own, with no input from the client.

This means:

1. **Host accelerators always fire.** Keys like Ctrl+Space (IME toggle), Alt+[
   (ChromeOS window snap), Super_L (launcher) are all processed by ChromeOS
   regardless of what the guest app wants to do.

2. **No per-key control is possible.** Without ack mode enabled, there is no
   wire to send any signal to Exo. The client is just a passive receiver.

The symptom was: Ctrl+Space could not be used for IME input inside the guest
because ChromeOS was always intercepting it as a system shortcut.


---

## The Fix

### 1. Bind `zcr_keyboard_extension_v1` internally (`registry.rs`)

When the host advertises `zcr_keyboard_extension_v1` in the global registry,
sommelier binds it immediately (not exposing it to the guest) and stores the
host object ID in `ctx.host_keyboard_extension_id`.

```rust
} else if interface == "zcr_keyboard_extension_v1" {
    let host_id = ctx.shadow_table.allocate_host_id();
    ctx.host_keyboard_extension_id = Some(host_id);
    // ... send wl_registry.bind to host
}
```

### 2. Enable ack mode per keyboard on first enter (`keyboard.rs`)

On the first `wl_keyboard.enter` for a given keyboard object, sommelier sends
`zcr_keyboard_extension_v1.get_extended_keyboard` to the host. This is the
trigger that calls `SetNeedKeyboardKeyAcks(true)` in Exo — enabling ack mode.

```rust
// zcr_keyboard_extension_v1.get_extended_keyboard(new_id, keyboard)
builder.write_u32(host_extended_id);  // new zcr_extended_keyboard_v1
builder.write_u32(host_keyboard_id);  // the wl_keyboard to extend
ctx.client_to_host_queue.push((msg, Vec::new()));
```

### 3. Track XKB state for keysym resolution (`keyboard.rs`)

`on_keymap` parses the keymap via `mmap` + `xkbcommon` to set up an XKB state
machine. `on_modifiers` keeps the modifier state up to date. This allows `on_key`
to resolve the exact keysym for any key press.

### 4. Decide and ack every key (`keyboard.rs`)

`on_key` now:
1. Resolves the keysym using XKB state.
2. Checks it against the parsed `SOMMELIER_ACCELERATORS` list.
3. Sends `zcr_extended_keyboard_v1.ack_key(serial, handled)` back to the host:
   - **Keys in `SOMMELIER_ACCELERATORS`** → `NOT_HANDLED (0)` → host handles, key dropped from guest
   - **All other keys** → `HANDLED (1)` → host skips accelerator, key forwarded to guest

```rust
if state == 1 /* pressed */ {
    for acc in &ctx.accelerators {
        if self.modifiers == acc.modifiers && lower_sym == acc.symbol {
            action = Action::Drop;   // don't forward to guest
            handled = false;          // tell host: NOT_HANDLED
            break;
        }
    }
}
// Send ack_key regardless
let handled_val: u32 = if handled { 1 } else { 0 };
// ... builder writes serial + handled_val to client_to_host_queue
```

### 5. Flush the ack back to the host in the same pass (`proxy.rs`)

The Wayland message loop processes one direction at a time. When handling a
host→client `wl_keyboard.key` event, the ack needs to go the *other* way
(client→host). A new **bidirectional queue flush** was added:

```rust
// After forwarding host→client events, flush any queued client→host messages
// (e.g. ack_key responses generated during this pass).
let reverse_queue = match direction {
    Direction::ClientToHost => &mut self.ctx.host_to_client_queue,
    Direction::HostToClient => &mut self.ctx.client_to_host_queue,
};
// drain and send back through conn (the source connection)
```

Without this, ack_key messages would sit in the queue and never be sent until
the *next* client→host message arrived — causing Exo's 1-second timeout to
fire and treating everything as NOT_HANDLED.

### 6. Parse `SOMMELIER_ACCELERATORS` correctly (`accelerator.rs`)

A new module parses the comma-separated env var:

```
SOMMELIER_ACCELERATORS="<Control>space,<Alt>bracketleft"
                                   ↑
                              comma required
```

Each token is split on `,`, modifiers (`<Control>`, `<Alt>`, `<Shift>`) are
stripped, and the remainder is looked up as an XKB keysym name. Invalid names
are silently skipped.

---

## Summary of Changes

| File | Change |
|---|---|
| `accelerator.rs` | New: parse `SOMMELIER_ACCELERATORS` into `Vec<Accelerator>` |
| `handler/keyboard.rs` | XKB state tracking; `on_key` ack logic; lazy extended keyboard binding on enter |
| `handler/registry.rs` | Internally bind `zcr_keyboard_extension_v1`; expose `zwp_keyboard_shortcuts_inhibit_manager_v1` to guests |
| `proxy.rs` | Bidirectional queue flush so ack_key is sent back to host in same event-processing pass |
| `state.rs` | New context fields: `accelerators`, `keyboard_to_extended_keyboard`, `host_keyboard_extension_id` |
| `build.rs` + `main.rs` | Register and include generated code for the two new protocol XMLs |
| `third_party/protocols/` | Add `keyboard-extension-unstable-v1.xml` and `keyboard-shortcuts-inhibit-unstable-v1.xml` |

## Usage

```bash
export SOMMELIER_ACCELERATORS="<Control>space,<Alt>bracketleft"
sommelier wayland-proxy-0
```

Keys in the list → ChromeOS handles them.  
All other keys → guest app handles them, ChromeOS does not intercept.
