/*
Copyright 2026 Google LLC

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

     https://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
*/

//! Keyboard event handling with ChromeOS accelerator interception.
//!
//! When `zcr_keyboard_extension_v1` is available from the host, this handler
//! enables the ack-key protocol so that sommelier can decide per-key whether
//! the host compositor (ChromeOS/Exo) should process the key as an accelerator.
//!
//! Keys listed in `SOMMELIER_ACCELERATORS` are acked as `NOT_HANDLED` (host
//! runs the accelerator, key is not forwarded to guest). All other keys are
//! acked as `HANDLED` (guest receives the key, host skips the accelerator).
//!
//! See `docs/KEYBOARD_SHORTCUT_INHIBITION.md` for the full protocol flow.

use crate::protocols::wayland::wl_keyboard;
use crate::state::{Context, GuestId, HostId};
use crate::wire::{Action, MessageBuilder};
use xkbcommon::xkb;

/// `wl_keyboard.key` state values (Wayland spec §wl_keyboard.key).
const WL_KEY_PRESSED: u32 = 1;
const WL_KEY_RELEASED: u32 = 0;

/// `wl_keyboard.keymap` format value for XKB (Wayland spec §wl_keyboard.keymap_format).
const WL_KEYMAP_FORMAT_XKB_V1: u32 = 1;

/// A read-only view of a shared-memory fd mapped into the process address space.
///
/// All `unsafe` for the mmap/munmap pair is confined here:
/// - `new`: calls `mmap(MAP_SHARED, PROT_READ)` and stores the pointer + length.
/// - `as_bytes`: constructs a slice; valid because the mapping covers exactly `len` bytes.
/// - `Drop`: calls `munmap`; the pointer and length are never mutated after construction.
pub struct MmapView {
    ptr: std::ptr::NonNull<std::ffi::c_void>,
    len: usize,
}

impl MmapView {
    /// Map `len` bytes from `fd` at offset 0 as read-only shared memory.
    /// Returns `None` if `len` is zero or if `mmap` fails.
    pub fn from_fd(fd: std::os::unix::io::RawFd, len: usize) -> Option<Self> {
        use nix::sys::mman::{mmap, MapFlags, ProtFlags};
        use std::os::unix::io::BorrowedFd;

        let nonzero_len = std::num::NonZeroUsize::new(len)?;
        // Safety: fd is valid for the duration of this call; mmap does not
        // retain it. The returned pointer owns the mapping until munmap.
        let ptr = unsafe {
            let borrowed = BorrowedFd::borrow_raw(fd);
            mmap(None, nonzero_len, ProtFlags::PROT_READ, MapFlags::MAP_SHARED, borrowed, 0).ok()?
        };
        Some(Self { ptr, len })
    }

    /// View the mapped region as a byte slice.
    pub fn as_bytes(&self) -> &[u8] {
        // Safety: ptr points to `self.len` readable bytes for the lifetime of
        // self (mapping is alive until Drop); no other writer exists (PROT_READ).
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr() as *const u8, self.len) }
    }
}

impl Drop for MmapView {
    fn drop(&mut self) {
        // Safety: ptr and len were set by mmap and never modified.
        unsafe { let _ = nix::sys::mman::munmap(self.ptr, self.len); }
    }
}

/// Keyboard handler that tracks XKB state for keysym resolution and sends
/// `ack_key` responses to the host via `zcr_extended_keyboard_v1`.
pub struct KeyboardHandler {
    context: xkb::Context,
    keymap: Option<xkb::Keymap>,
    state: Option<xkb::State>,
    /// Current modifier bitmask (using accelerator.rs conventions).
    modifiers: u32,
    /// Keys dropped on press; their release events are also dropped.
    dropped_keys: std::collections::HashSet<u32>,
}

// xkb::Context, Keymap, and State are not Send/Sync, but we only access them
// from the single-threaded client task. The tokio runtime requires Send for
// task-spawned futures, so we provide the guarantee manually.
unsafe impl Send for KeyboardHandler {}
unsafe impl Sync for KeyboardHandler {}

impl KeyboardHandler {
    pub fn new() -> Self {
        Self {
            context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            keymap: None,
            state: None,
            modifiers: 0,
            dropped_keys: std::collections::HashSet::new(),
        }
    }

    /// Check if the pressed key matches any configured host accelerators.
    fn is_host_accelerator(&self, accelerators: &[crate::accelerator::Accelerator], key: u32) -> bool {
        let Some(state) = &self.state else { return false; };
        let Some(keymap) = &self.keymap else { return false; };

        let xkb_keycode = xkb::Keycode::new(key + 8);
        let syms = keymap.key_get_syms_by_level(xkb_keycode, state.key_get_layout(xkb_keycode), 0);

        // Only match keys that resolve to exactly one keysym. Keys producing
        // 0 or 2+ symbols are ambiguous and intentionally excluded from
        // accelerator matching to avoid spurious intercepts.
        if syms.len() == 1 {
            let lower_sym = crate::accelerator::keysym_to_lower(syms[0].raw());
            for acc in accelerators {
                if self.modifiers == acc.modifiers && lower_sym == acc.symbol {
                    log::debug!("Accelerator match: key={}, modifiers={:#x}, sym={:#x}", key, self.modifiers, lower_sym);
                    return true;
                }
            }
        }
        false
    }

    /// Lazily bind zcr_keyboard_extension_v1.get_extended_keyboard.
    fn bind_extended_keyboard(&self, ctx: &mut Context, host_keyboard_id: HostId) {
        if let Some(extension_host_id) = ctx.host_keyboard_extension_id {
            if !ctx.keyboard_to_extended_keyboard.contains_key(&host_keyboard_id) {
                let host_extended_id = HostId(ctx.shadow_table.allocate_host_id());
                ctx.keyboard_to_extended_keyboard
                    .insert(host_keyboard_id, host_extended_id);
                ctx.shadow_table
                    .track_host_interface(host_extended_id.0, "zcr_extended_keyboard_v1".to_string());

                // zcr_keyboard_extension_v1.get_extended_keyboard(new_id, keyboard)
                let mut builder = MessageBuilder::new();
                builder.write_u32(host_extended_id.0);
                builder.write_u32(host_keyboard_id.0);

                let mut msg = Vec::new();
                msg.extend_from_slice(&extension_host_id.0.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = len << 16; // opcode 0: get_extended_keyboard
                msg.extend_from_slice(&word2.to_ne_bytes());
                msg.extend_from_slice(&builder.payload);
                ctx.client_to_host_queue.push((msg, Vec::new()));
                log::debug!(
                    "Bound extended keyboard: host_extended_id={} for host_keyboard_id={}",
                    host_extended_id.0,
                    host_keyboard_id.0
                );
            }
        }
    }

    /// Send zcr_extended_keyboard_v1.ack_key to the host.
    fn send_ack_key(&self, ctx: &mut Context, host_keyboard_id: HostId, serial: u32, handled: bool) {
        if let Some(&host_extended_id) = ctx.keyboard_to_extended_keyboard.get(&host_keyboard_id) {
            let handled_val: u32 = if handled { 1 } else { 0 };
            let mut builder = MessageBuilder::new();
            builder.write_u32(serial);
            builder.write_u32(handled_val);

            let mut msg = Vec::new();
            msg.extend_from_slice(&host_extended_id.0.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 1u32; // opcode 1: ack_key
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);
            ctx.client_to_host_queue.push((msg, Vec::new()));
        }
    }
}

impl wl_keyboard::WlKeyboardHandler for KeyboardHandler {
    /// Parse the keymap to set up XKB state for keysym resolution.
    /// The host sends keymap data via a shared-memory fd (e.g. memfd).
    /// We mmap it to read without consuming data, so the fd can still
    /// be forwarded to the guest client.
    fn on_keymap(
        &mut self,
        _ctx: &mut Context,
        format: u32,
        fd: std::os::unix::io::RawFd,
        size: u32,
    ) -> Action {
        // Only handle XKB_V1 format keymaps.
        if format != WL_KEYMAP_FORMAT_XKB_V1 {
            return Action::Forward;
        }

        let Some(mapping) = MmapView::from_fd(fd, size as usize) else {
            log::error!("on_keymap: mmap failed for fd={}, size={}", fd, size);
            return Action::Forward;
        };
        let slice = mapping.as_bytes();

        // Strip the trailing null terminator if present.
        let len = if !slice.is_empty() && slice[slice.len() - 1] == 0 {
            slice.len() - 1
        } else {
            slice.len()
        };

        if let Ok(s) = std::str::from_utf8(&slice[..len]) {
            if let Some(keymap) = xkb::Keymap::new_from_string(
                &self.context,
                s.to_string(),
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            ) {
                self.state = Some(xkb::State::new(&keymap));
                self.keymap = Some(keymap);
                log::info!("XKB keymap loaded successfully");
            }
        }
        // `mapping` drops here, unmapping the region.

        Action::Forward
    }

    fn on_enter(
        &mut self,
        ctx: &mut Context,
        _serial: u32,
        surface: u32, // Host ID
        _keys: &[u8],
    ) -> Action {
        // on_enter is a host→client event: last_sender_id is the host keyboard ID.
        let host_keyboard_id = HostId(ctx.last_sender_id);
        let guest_keyboard_id = ctx.shadow_table.guest_id_of(host_keyboard_id).map(|g| g.0).unwrap_or(0);
        let guest_surface_id = ctx.shadow_table.get_guest_id(surface).unwrap_or(0);

        // Lazily bind the extended keyboard object on first enter.
        // This sends zcr_keyboard_extension_v1.get_extended_keyboard to the
        // host, which enables ack mode (SetNeedKeyboardKeyAcks(true) in Exo).
        self.bind_extended_keyboard(ctx, host_keyboard_id);

        if guest_surface_id != 0 {
            if let Some(&guest_seat_id) = ctx.keyboard_to_seat.get(&guest_keyboard_id) {
                ctx.active_surface_for_seat
                    .insert(guest_seat_id, guest_surface_id);

                // Find the v3 text input for this seat
                for (guest_text_input_id, state) in ctx.text_inputs.iter_mut() {
                    if state.guest_seat == guest_seat_id {
                        state.active_surface = Some(guest_surface_id);

                        // Send zwp_text_input_v3.enter (opcode 0)
                        let mut builder = MessageBuilder::new();
                        builder.write_u32(guest_surface_id);

                        let mut msg = Vec::new();
                        msg.extend_from_slice(&guest_text_input_id.to_ne_bytes());
                        let len = (builder.payload.len() + 8) as u32;
                        let word2 = len << 16;
                        msg.extend_from_slice(&word2.to_ne_bytes());
                        msg.extend_from_slice(&builder.payload);
                        ctx.host_to_client_queue.push((msg, Vec::new()));
                    }
                }
            }
        }

        Action::Forward
    }

    fn on_leave(&mut self, ctx: &mut Context, _serial: u32, surface: u32) -> Action {
        // on_leave is a host→client event: last_sender_id is the host keyboard ID.
        let host_keyboard_id = HostId(ctx.last_sender_id);
        let guest_keyboard_id = ctx.shadow_table.guest_id_of(host_keyboard_id).map(|g| g.0).unwrap_or(0);
        let guest_surface_id = ctx.shadow_table.get_guest_id(surface).unwrap_or(0);

        if guest_surface_id != 0 {
            if let Some(&guest_seat_id) = ctx.keyboard_to_seat.get(&guest_keyboard_id) {
                ctx.active_surface_for_seat.remove(&guest_seat_id);

                // Find the v3 text input for this seat
                for (guest_text_input_id, state) in ctx.text_inputs.iter_mut() {
                    if state.guest_seat == guest_seat_id {
                        state.active_surface = None;

                        // Send zwp_text_input_v3.leave (opcode 1)
                        let mut builder = MessageBuilder::new();
                        builder.write_u32(guest_surface_id);

                        let mut msg = Vec::new();
                        msg.extend_from_slice(&guest_text_input_id.to_ne_bytes());
                        let len = (builder.payload.len() + 8) as u32;
                        let word2 = (len << 16) | 1u32;
                        msg.extend_from_slice(&word2.to_ne_bytes());
                        msg.extend_from_slice(&builder.payload);
                        ctx.host_to_client_queue.push((msg, Vec::new()));
                    }
                }
            }
        }

        Action::Forward
    }

    /// Handle key events: resolve keysym, check against SOMMELIER_ACCELERATORS,
    /// and send ack_key to the host.
    fn on_key(
        &mut self,
        ctx: &mut Context,
        serial: u32,
        _time: u32,
        key: u32,
        state: u32,
    ) -> Action {
        // on_key is a host→client event: last_sender_id is the host keyboard ID.
        let host_keyboard_id = HostId(ctx.last_sender_id);
        let mut action = Action::Forward;
        let mut handled = true; // Default: guest handles the key.

        if state == WL_KEY_PRESSED {
            // Key pressed: check if this is a host accelerator.
            if self.is_host_accelerator(&ctx.accelerators, key) {
                action = Action::Drop;
                handled = false;
                self.dropped_keys.insert(key);
            }
        } else if state == WL_KEY_RELEASED {
            // Key released: if we dropped the press, drop the release too
            // to avoid stuck-key state in the guest.
            if self.dropped_keys.remove(&key) {
                action = Action::Drop;
                handled = false;
            }
        }

        // Send ack_key only for press events, matching the C sommelier
        // reference implementation. Exo only queues presses in
        // pending_key_acks_; sending acks for releases would be a no-op
        // against non-existent serials but is deliberately avoided for clarity.
        if state == WL_KEY_PRESSED {
            self.send_ack_key(ctx, host_keyboard_id, serial, handled);
        }

        action
    }

    /// Track modifier state so on_key can resolve the correct keysym.
    fn on_modifiers(
        &mut self,
        _ctx: &mut Context,
        _serial: u32,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    ) -> Action {
        if let Some(state) = &mut self.state {
            state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);

            self.modifiers = 0;
            let components = xkb::STATE_MODS_DEPRESSED | xkb::STATE_MODS_LATCHED;
            if state.mod_name_is_active("Control", components) {
                self.modifiers |= crate::accelerator::CONTROL_MASK;
            }
            if state.mod_name_is_active("Mod1", components) {
                self.modifiers |= crate::accelerator::ALT_MASK;
            }
            if state.mod_name_is_active("Shift", components) {
                self.modifiers |= crate::accelerator::SHIFT_MASK;
            }
            if state.mod_name_is_active("Mod4", components) {
                self.modifiers |= crate::accelerator::SUPER_MASK;
            }
        }
        Action::Forward
    }

    /// Clean up when the guest destroys the wl_keyboard object.
    ///
    /// `on_release` is a **client→host request**: `ctx.last_sender_id` is the
    /// **guest** keyboard ID. We must translate it to a host ID before looking
    /// up `keyboard_to_extended_keyboard` (which is keyed by host IDs).
    /// Using the typed [`GuestId`] / [`HostId`] wrappers makes a wrong-direction
    /// lookup a compile error.
    fn on_release(&mut self, ctx: &mut Context) -> Action {
        // Translate guest ID → host ID. Returns None for unknown keyboards
        // (e.g. keyboards that never received an on_enter event).
        let Some(host_keyboard_id) = ctx.shadow_table.host_id_of(GuestId(ctx.last_sender_id)) else {
            return Action::Forward;
        };
        if let Some(host_extended_id) = ctx.keyboard_to_extended_keyboard.remove(&host_keyboard_id) {
            // zcr_extended_keyboard_v1.destroy is opcode 0, no payload.
            let mut msg = Vec::new();
            msg.extend_from_slice(&host_extended_id.0.to_ne_bytes());
            let word2 = 8u32 << 16; // len=8, opcode=0 (destroy)
            msg.extend_from_slice(&word2.to_ne_bytes());
            ctx.client_to_host_queue.push((msg, Vec::new()));
            log::debug!(
                "Destroyed extended keyboard: host_extended_id={} for host_keyboard_id={}",
                host_extended_id.0,
                host_keyboard_id.0
            );
        }
        Action::Forward
    }
}

// Empty impls for keyboard extension protocol handlers.
// We don't receive requests/events on these; we only send ack_key.
impl crate::protocols::keyboard_extension_unstable_v1::zcr_keyboard_extension_v1::ZcrKeyboardExtensionV1Handler for KeyboardHandler {}
impl crate::protocols::keyboard_extension_unstable_v1::zcr_extended_keyboard_v1::ZcrExtendedKeyboardV1Handler for KeyboardHandler {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::wayland::wl_keyboard::WlKeyboardHandler;
    use std::os::unix::io::AsRawFd;

    /// Helper: create an anonymous memfd, write the default keymap into it,
    /// and call on_keymap so the handler builds its XKB state. Returns the
    /// keymap object so tests can look up keycodes.
    fn load_test_keymap(handler: &mut KeyboardHandler, ctx: &mut Context) -> xkb::Keymap {
        let dummy_ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &dummy_ctx,
            "",
            "",
            "",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .unwrap();
        let keymap_str = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);

        // Use an anonymous memfd so we don't need the `tempfile` crate.
        // nix is already a dependency of the main crate.
        use nix::sys::memfd::{memfd_create, MFdFlags};
        use std::ffi::CString;
        let name = CString::new("sommelier-test-keymap").unwrap();
        let fd = memfd_create(name.as_c_str(), MFdFlags::empty()).expect("memfd_create failed");
        nix::unistd::write(&fd, keymap_str.as_bytes()).expect("write failed");

        handler.on_keymap(ctx, 1, fd.as_raw_fd(), keymap_str.len() as u32 + 1);
        assert!(handler.keymap.is_some(), "keymap should be loaded");
        keymap
    }

    /// Find the evdev keycode for a given keysym in the keymap.
    fn find_keycode(keymap: &xkb::Keymap, target: u32) -> u32 {
        let min = keymap.min_keycode().raw();
        let max = keymap.max_keycode().raw();
        for k in min..=max {
            let syms = keymap.key_get_syms_by_level(k.into(), 0, 0);
            if syms.iter().any(|s| s.raw() == target) {
                return k - 8; // Convert XKB keycode to evdev (Wayland) keycode
            }
        }
        panic!("keysym {:#x} not found in keymap", target);
    }

    #[test]
    fn accelerator_keys_are_dropped_and_acked_not_handled() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);
        ctx.accelerators = crate::accelerator::parse_accelerators("<Control>a").unwrap();

        let keymap = load_test_keymap(&mut handler, &mut ctx);
        let wl_key_a = find_keycode(&keymap, xkb::keysyms::KEY_a);

        // Press Ctrl
        let ctrl_mask = 1 << keymap.mod_get_index("Control");
        handler.on_modifiers(&mut ctx, 0, ctrl_mask, 0, 0, 0);

        // Set up extended keyboard tracking so ack_key is sent
        ctx.keyboard_to_extended_keyboard.insert(HostId(0), HostId(50));
        ctx.last_sender_id = 0;

        // Ctrl+A should be dropped (host accelerator)
        ctx.client_to_host_queue.clear();
        let action = handler.on_key(&mut ctx, 42, 0, wl_key_a, 1);
        assert_eq!(action, Action::Drop, "accelerator key should be dropped");

        // Verify ack_key was sent with handled=NOT_HANDLED (0)
        assert!(
            !ctx.client_to_host_queue.is_empty(),
            "ack_key should be queued"
        );
        let (msg, _) = &ctx.client_to_host_queue[0];
        // Message: [sender_id(4)] [size_opcode(4)] [serial(4)] [handled(4)]
        let handled_val = u32::from_ne_bytes(msg[12..16].try_into().unwrap());
        assert_eq!(handled_val, 0, "accelerator should be acked as NOT_HANDLED");
    }

    #[test]
    fn non_accelerator_keys_are_forwarded_and_acked_handled() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);
        ctx.accelerators = crate::accelerator::parse_accelerators("<Control>a").unwrap();

        let keymap = load_test_keymap(&mut handler, &mut ctx);
        let wl_key_b = find_keycode(&keymap, xkb::keysyms::KEY_b);

        // Press Ctrl
        let ctrl_mask = 1 << keymap.mod_get_index("Control");
        handler.on_modifiers(&mut ctx, 0, ctrl_mask, 0, 0, 0);

        ctx.keyboard_to_extended_keyboard.insert(HostId(0), HostId(50));
        ctx.last_sender_id = 0;

        // Ctrl+B is NOT in accelerators → forward to guest
        ctx.client_to_host_queue.clear();
        let action = handler.on_key(&mut ctx, 43, 0, wl_key_b, 1);
        assert_eq!(
            action,
            Action::Forward,
            "non-accelerator key should be forwarded"
        );

        // Verify ack_key was sent with handled=HANDLED (1)
        let (msg, _) = &ctx.client_to_host_queue[0];
        let handled_val = u32::from_ne_bytes(msg[12..16].try_into().unwrap());
        assert_eq!(handled_val, 1, "non-accelerator should be acked as HANDLED");
    }

    #[test]
    fn dropped_key_release_is_also_dropped() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);
        ctx.accelerators = crate::accelerator::parse_accelerators("<Control>a").unwrap();

        let keymap = load_test_keymap(&mut handler, &mut ctx);
        let wl_key_a = find_keycode(&keymap, xkb::keysyms::KEY_a);

        let ctrl_mask = 1 << keymap.mod_get_index("Control");
        handler.on_modifiers(&mut ctx, 0, ctrl_mask, 0, 0, 0);

        ctx.keyboard_to_extended_keyboard.insert(HostId(0), HostId(50));
        ctx.last_sender_id = 0;

        // Press → Drop
        let action = handler.on_key(&mut ctx, 1, 0, wl_key_a, 1);
        assert_eq!(action, Action::Drop);

        // Release → also Drop (prevents stuck key in guest)
        let action = handler.on_key(&mut ctx, 2, 0, wl_key_a, 0);
        assert_eq!(action, Action::Drop);

        // Next press of same key after release should still be evaluated
        let action = handler.on_key(&mut ctx, 3, 0, wl_key_a, 1);
        assert_eq!(action, Action::Drop);
    }

    /// Regression test: on_keymap must use mmap (not read/pread) because:
    /// 1. The host sends keymap data via shared memory (memfd). mmap works.
    /// 2. read() would consume data, preventing the fd from being forwarded.
    /// 3. pread()/read_at() fails with ESPIPE on non-seekable fds.
    /// 4. Even on seekable fds, the cursor may be at EOF after the host wrote.
    ///
    /// This test creates a memfd, writes the keymap, and does NOT seek back
    /// to the start — simulating a host that wrote then sent the fd. The
    /// on_keymap implementation must handle this via mmap (offset 0).
    #[test]
    fn keymap_loads_from_non_rewound_memfd() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);

        let dummy_ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &dummy_ctx,
            "",
            "",
            "",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .unwrap();
        let keymap_str = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
        let keymap_bytes = keymap_str.as_bytes();

        use nix::sys::memfd::{memfd_create, MFdFlags};
        use std::ffi::CString;

        let name = CString::new("test-keymap-norw").unwrap();
        let fd = memfd_create(name.as_c_str(), MFdFlags::empty())
            .expect("memfd_create failed");
        nix::unistd::write(&fd, keymap_bytes).expect("write failed");
        // Deliberately do NOT seek back to 0.
        // read()/pread() from here would get 0 bytes or fail.
        // mmap with offset 0 must still work.

        handler.on_keymap(&mut ctx, 1, fd.as_raw_fd(), keymap_bytes.len() as u32 + 1);
        assert!(
            handler.keymap.is_some(),
            "keymap must load via mmap even when fd cursor is at EOF"
        );
        assert!(
            handler.state.is_some(),
            "XKB state must be initialized after keymap load"
        );
    }

    #[test]
    fn on_release_destroys_extended_keyboard_and_cleans_map() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);

        // Use DISTINCT guest (5) and host (10) IDs to verify that on_release
        // correctly translates the guest sender ID to the host key, rather than
        // using last_sender_id (a guest ID) directly as a host map key.
        let guest_keyboard_id: u32 = 5;
        let host_keyboard_id: u32 = 10;
        let host_extended_id: u32 = 50;

        ctx.shadow_table.map_id(guest_keyboard_id, host_keyboard_id);
        ctx.keyboard_to_extended_keyboard
            .insert(HostId(host_keyboard_id), HostId(host_extended_id));
        // Simulate a wl_keyboard.release from the guest (last_sender_id = guest ID).
        ctx.last_sender_id = guest_keyboard_id;

        let action = handler.on_release(&mut ctx);
        assert_eq!(action, Action::Forward);

        // Map entry must be removed so re-binding is possible.
        assert!(
            !ctx.keyboard_to_extended_keyboard.contains_key(&HostId(host_keyboard_id)),
            "extended keyboard map must be cleared after release"
        );

        // destroy message must have been queued to the host.
        assert_eq!(ctx.client_to_host_queue.len(), 1, "destroy must be queued");
        let (msg, _) = &ctx.client_to_host_queue[0];
        // Message: [sender_id(4)] [size_opcode(4)]  — opcode 0, len 8.
        let sender = u32::from_ne_bytes(msg[0..4].try_into().unwrap());
        let word2 = u32::from_ne_bytes(msg[4..8].try_into().unwrap());
        assert_eq!(sender, host_extended_id, "destroy must target host_extended_id");
        assert_eq!(word2 >> 16, 8, "message length must be 8");
        assert_eq!(word2 & 0xFFFF, 0, "opcode must be 0 (destroy)");
    }

    #[test]
    fn bind_extended_keyboard_is_idempotent() {
        let handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);
        ctx.host_keyboard_extension_id = Some(HostId(99));

        // First call: should send get_extended_keyboard.
        handler.bind_extended_keyboard(&mut ctx, HostId(10));
        assert_eq!(ctx.client_to_host_queue.len(), 1);

        // Second call with the same host_keyboard_id: must not send again.
        ctx.client_to_host_queue.clear();
        handler.bind_extended_keyboard(&mut ctx, HostId(10));
        assert!(
            ctx.client_to_host_queue.is_empty(),
            "bind must be idempotent: no second get_extended_keyboard"
        );
    }

    #[test]
    fn is_host_accelerator_returns_false_without_xkb_state() {
        let handler = KeyboardHandler::new(); // no keymap loaded
        let accelerators =
            crate::accelerator::parse_accelerators("<Control>a").unwrap();
        // Must degrade gracefully, not panic.
        assert!(
            !handler.is_host_accelerator(&accelerators, 30),
            "should return false when XKB state is not initialised"
        );
    }
}
