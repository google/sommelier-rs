# Keyboard Shortcut Inhibition in ChromeOS Exo

## Overview

When a Wayland application runs inside a ChromeOS Linux container (Crostini) via
`sommelier`, it receives keyboard events through the Exo compositor. However,
Exo also intercepts many key combinations as Chrome accelerators (e.g. Ctrl+T →
new tab, Ctrl+W → close tab). This document explains how the interception works,
how the official mechanism to suppress it works, and what sommelier-rs must
implement to make this work correctly.

---

## Key Event Flow in Exo

```
Physical key press
      │
      ▼
Exo compositor (components/exo/keyboard.cc)
      │
      ├─ 1. ProcessAcceleratorIfReserved()
      │       Checks a tiny hardcoded list (kReservedAccelerators):
      │         - F13
      │         - Shift+Alt+I
      │         - Ctrl+Alt+Z
      │       Ctrl+T is NOT here. Almost nothing is.
      │
      ├─ 2. delegate_->OnKeyboardKey() → sends wl_keyboard.key to client
      │       The client (sommelier-rs) receives the key via virtio-wl.
      │
      └─ 3. Accelerator handling — two modes depending on ack mode:
            ┌─ ack mode OFF (default, no zcr_keyboard_extension_v1):
            │    Exo immediately runs ProcessAccelerator() on the event.
            │    Ctrl+T opens a new Chrome tab right now.
            │    The client also receives the key — double-processing!
            │
            └─ ack mode ON (zcr_keyboard_extension_v1 bound):
                 Exo stores the event in pending_key_acks_ with a 1000ms TTL.
                 Exo calls event->SetHandled() — normal accelerator dispatch blocked.
                 Waits for client to send zcr_extended_keyboard_v1.ack_key(serial, state).
                   - state=HANDLED  → Exo skips the accelerator. Guest app handles it. ✓
                   - state=NOT_HANDLED → Exo runs ProcessAccelerator(). Host handles it.
                   - timeout (1000ms) → treated as NOT_HANDLED.
```

---

## The `zcr_keyboard_extension_v1` Protocol

This is a **ChromeOS-specific Wayland extension** defined in:
`third_party/wayland-protocols/unstable/keyboard/keyboard-extension-unstable-v1.xml`

### Interface summary

| Interface | Direction | Description |
|---|---|---|
| `zcr_keyboard_extension_v1` | global (host→client) | Factory for extended keyboard objects |
| `zcr_extended_keyboard_v1` | per-keyboard object | Carries `ack_key` request and `peek_key` event |

### Requests (client → host)

```
zcr_keyboard_extension_v1.get_extended_keyboard(id, wl_keyboard)
  → Creates a zcr_extended_keyboard_v1 for the given keyboard.
  → Side effect: enables ack mode in Exo for this keyboard.

zcr_extended_keyboard_v1.ack_key(serial, handled_state)
  → serial: matches the serial from wl_keyboard.key
  → handled_state: 0 = not_handled, 1 = handled
```

### Events (host → client)

```
zcr_extended_keyboard_v1.peek_key(serial, time, code, state)
  → Sent before wl_keyboard.key so client can preview the key.
  → Only sent for version >= PEEK_KEY_SINCE_VERSION.
```

---

## Exo Host-Side Implementation

From `components/exo/wayland/zcr_keyboard_extension.cc`:

When `get_extended_keyboard` is called:
```cpp
keyboard->SetNeedKeyboardKeyAcks(true);
```

When `ack_key` is received:
```cpp
void AckKeyboardKey(uint32_t serial, bool handled) {
    keyboard_->AckKeyboardKey(serial, handled);
}
```

From `components/exo/keyboard.cc`, `AckKeyboardKey`:
```cpp
void Keyboard::AckKeyboardKey(uint32_t serial, bool handled) {
    auto it = pending_key_acks_.find(serial);
    if (it == pending_key_acks_.end()) return;
    auto* key_event = &it->second.first;
    if (!handled && !key_event->handled() && focus_)
        ProcessAccelerator(focus_, key_event);  // ← runs Chrome accelerator
    pending_key_acks_.erase(serial);
}
```

The `ProcessAccelerator` call invokes the browser's `FocusManager`, which
handles all Chrome shortcuts including Ctrl+T (new tab), Ctrl+W (close tab), etc.

---

## The `zwp_keyboard_shortcuts_inhibit_manager_v1` Protocol

This is a **standard Wayland protocol** (`keyboard-shortcuts-inhibit-unstable-v1`).

From `components/exo/wayland/zwp_keyboard_shortcuts_inhibit_manager.cc`:

```cpp
class KeyboardShortcutsInhibitor : public SurfaceObserver {
 public:
  explicit KeyboardShortcutsInhibitor(Surface* surface) : surface_(surface) {
    surface->SetKeyboardShortcutsInhibited(true);
  }
  ~KeyboardShortcutsInhibitor() override {
    if (surface_) surface_->SetKeyboardShortcutsInhibited(false);
  }
};
```

The `SetKeyboardShortcutsInhibited(true)` call sets a flag on the surface.
**However, this flag is NOT checked in `keyboard.cc`'s accelerator path.**
Searching Exo source confirms it is only used for reporting/state tracking and
does NOT suppress `ProcessAccelerator`. This protocol is effectively a no-op for
preventing accelerator interception in ChromeOS.

> **The `zwp_keyboard_shortcuts_inhibitor_v1` protocol does not work in ChromeOS Exo
> for preventing Chrome accelerators. The `zcr_keyboard_extension_v1` ack mechanism
> is the only working approach.**

---

## The `SOMMELIER_ACCELERATORS` Environment Variable

The system default (`/etc/systemd/user/sommelier@0.service.d/cros-sommelier-override.conf`):
```ini
Environment="SOMMELIER_ACCELERATORS=Super_L,<Alt>bracketleft,<Alt>bracketright,<Alt>tab"
```

**Semantics**: Keys in this list are keys that the **host** should handle (passed to
the guest as `NOT_HANDLED`). All other keys are acked as `HANDLED` to the host,
preventing Chrome from running its accelerator.

`Ctrl+T` is **not** in the system accelerator list, so with `zcr_keyboard_extension_v1`
properly implemented, Ctrl+T should be acked as `HANDLED` → forwarded to the guest
Chromium → guest opens a new tab. ✓

---

## What Must Be Implemented

### 1. Bind `zcr_keyboard_extension_v1` from the host registry

When the host compositor advertises `zcr_keyboard_extension_v1` during the
registry enumeration phase, the proxy must bind to it and retain a reference
for later use.

### 2. After keyboard creation, request `get_extended_keyboard`

Once a `wl_seat.get_keyboard` creates a host-side keyboard object, the proxy
must immediately send:
```
zcr_keyboard_extension_v1.get_extended_keyboard(new_id, host_keyboard_id)
```
This allocates a `zcr_extended_keyboard_v1` object on the host and enables
ack mode for that keyboard (i.e. Exo begins holding accelerators pending
acknowledgement rather than processing them immediately).

### 3. Acknowledge each key event with `ack_key`

For every `wl_keyboard.key` event received from the host, the proxy must:
1. Capture the serial from the event.
2. Determine whether the key combo is in the configured accelerator list
   (keys the **host** should handle).
3. Send `zcr_extended_keyboard_v1.ack_key(serial, handled_state)` back to
   the host:
   - `NOT_HANDLED (0)` if the key is in the accelerator list → host runs
     its accelerator action.
   - `HANDLED (1)` otherwise → host skips the accelerator. The guest
     application processes the key event it already received (the
     `wl_keyboard.key` is forwarded to the guest *before* the `ack_key`
     reaches the host; Exo holds the accelerator in `pending_key_acks_`
     until the ack or its 1000 ms TTL fires).

### 4. Retain or remove the `zwp_keyboard_shortcuts_inhibitor_v1` path

The auto-inhibitor created on focus enter/leave is effectively a no-op in
Chrome OS Exo (see above). It can be removed once `zcr_keyboard_extension_v1`
acks are working, or kept as a belt-and-suspenders measure since it is
harmless.

---

## References

### Chromium Source Code (Exo compositor)

- **Exo keyboard dispatch & ack mechanism**
  `components/exo/keyboard.cc`
  https://chromium.googlesource.com/chromium/src/+/refs/heads/main/components/exo/keyboard.cc

- **`zcr_keyboard_extension_v1` server-side implementation** (ack protocol, `SetNeedKeyboardKeyAcks`)
  `components/exo/wayland/zcr_keyboard_extension.cc`
  https://chromium.googlesource.com/chromium/src/+/refs/heads/main/components/exo/wayland/zcr_keyboard_extension.cc

- **`zwp_keyboard_shortcuts_inhibit_manager_v1` server-side implementation** (sets surface flag; does NOT suppress accelerators)
  `components/exo/wayland/zwp_keyboard_shortcuts_inhibit_manager.cc`
  https://chromium.googlesource.com/chromium/src/+/refs/heads/main/components/exo/wayland/zwp_keyboard_shortcuts_inhibit_manager.cc

### Protocol Specifications

- **`zcr_keyboard_extension_v1` (ChromeOS-specific)**
  `third_party/wayland-protocols/unstable/keyboard/keyboard-extension-unstable-v1.xml`
  https://chromium.googlesource.com/chromium/src/+/refs/heads/main/third_party/wayland-protocols/unstable/keyboard/keyboard-extension-unstable-v1.xml

- **`zwp_keyboard_shortcuts_inhibit_manager_v1` (standard Wayland protocol)**
  https://wayland.app/protocols/keyboard-shortcuts-inhibit-unstable-v1

### ChromeOS System Configuration

- **System sommelier service override** (defines default `SOMMELIER_ACCELERATORS`):
  `/etc/systemd/user/sommelier@0.service.d/cros-sommelier-override.conf`
  (read directly from the ChromeOS device during investigation)

### Related Reading

- **Wayland protocol overview for `zwp_keyboard_shortcuts_inhibit_manager_v1`**
  https://wayland.app/protocols/keyboard-shortcuts-inhibit-unstable-v1

- **ChromeOS sommelier README** (background on accelerators env var semantics)
  https://chromium.googlesource.com/chromiumos/platform2/+/HEAD/vm_tools/sommelier/README.md
