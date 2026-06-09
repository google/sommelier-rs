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
//! See `docs/keyboard-shortcut-inhibition.md` for the full protocol flow.

use crate::protocols::wayland::wl_keyboard;
use crate::state::Context;
use crate::wire::{Action, MessageBuilder};
use xkbcommon::xkb;

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
}

impl wl_keyboard::WlKeyboardHandler for KeyboardHandler {
    /// Parse the keymap to set up XKB state for keysym resolution.
    fn on_keymap(
        &mut self,
        _ctx: &mut Context,
        format: u32,
        fd: std::os::unix::io::RawFd,
        size: u32,
    ) -> Action {
        // Only handle XKB_V1 format keymaps.
        if format != 1 {
            return Action::Forward;
        }

        use std::fs::File;
        use std::io::{Read, Seek, SeekFrom};
        use std::mem::ManuallyDrop;
        use std::os::unix::io::FromRawFd;

        // Wrap the raw fd safely without taking ownership or closing it.
        let mut file = ManuallyDrop::new(unsafe { File::from_raw_fd(fd) });
        let _ = file.seek(SeekFrom::Start(0));

        let mut buf = vec![0u8; size as usize];
        if let Ok(bytes_read) = file.read(&mut buf) {
            if bytes_read > 0 {
                // Strip the trailing null terminator if present.
                let len = if buf[bytes_read - 1] == 0 {
                    bytes_read - 1
                } else {
                    bytes_read
                };

                if let Ok(s) = std::str::from_utf8(&buf[..len]) {
                    if let Some(keymap) = xkb::Keymap::new_from_string(
                        &self.context,
                        s.to_string(),
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    ) {
                        self.state = Some(xkb::State::new(&keymap));
                        self.keymap = Some(keymap);
                        log::debug!("XKB keymap loaded successfully");
                    }
                }
            }
        }

        Action::Forward
    }

    fn on_enter(
        &mut self,
        ctx: &mut Context,
        _serial: u32,
        surface: u32, // Host ID
        _keys: &[u8],
    ) -> Action {
        let host_keyboard_id = ctx.last_sender_id;
        let guest_keyboard_id = ctx.shadow_table.get_guest_id(host_keyboard_id).unwrap_or(0);
        let guest_surface_id = ctx.shadow_table.get_guest_id(surface).unwrap_or(0);

        // Lazily bind the extended keyboard object on first enter.
        // This sends zcr_keyboard_extension_v1.get_extended_keyboard to the
        // host, which enables ack mode (SetNeedKeyboardKeyAcks(true) in Exo).
        if let Some(extension_host_id) = ctx.host_keyboard_extension_id {
            if !ctx
                .keyboard_to_extended_keyboard
                .contains_key(&host_keyboard_id)
            {
                let host_extended_id = ctx.shadow_table.allocate_host_id();
                ctx.keyboard_to_extended_keyboard
                    .insert(host_keyboard_id, host_extended_id);
                ctx.shadow_table
                    .track_host_interface(host_extended_id, "zcr_extended_keyboard_v1".to_string());

                // zcr_keyboard_extension_v1.get_extended_keyboard(new_id, keyboard)
                let mut builder = MessageBuilder::new();
                builder.write_u32(host_extended_id);
                builder.write_u32(host_keyboard_id);

                let mut msg = Vec::new();
                msg.extend_from_slice(&extension_host_id.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = len << 16; // opcode 0: get_extended_keyboard
                msg.extend_from_slice(&word2.to_ne_bytes());
                msg.extend_from_slice(&builder.payload);
                ctx.client_to_host_queue.push((msg, Vec::new()));
                log::debug!(
                    "Bound extended keyboard: host_extended_id={} for host_keyboard_id={}",
                    host_extended_id,
                    host_keyboard_id
                );
            }
        }

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
        let host_keyboard_id = ctx.last_sender_id;
        let guest_keyboard_id = ctx.shadow_table.get_guest_id(host_keyboard_id).unwrap_or(0);
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
        let host_keyboard_id = ctx.last_sender_id;
        let mut action = Action::Forward;
        let mut handled = true; // Default: guest handles the key.

        if state == 1 {
            // Key pressed: check if this is a host accelerator.
            if let Some(xkb_state) = &self.state {
                // Wayland key codes are evdev codes; XKB adds an offset of 8.
                let code = (key + 8).into();
                let syms = xkb_state.key_get_syms(code);
                if syms.len() == 1 {
                    let lower_sym =
                        unsafe { crate::accelerator::xkb_keysym_to_lower(syms[0].raw()) };
                    for acc in &ctx.accelerators {
                        if self.modifiers == acc.modifiers && lower_sym == acc.symbol {
                            // This key is a host accelerator. Don't forward
                            // to the guest; let the host handle it.
                            action = Action::Drop;
                            handled = false;
                            self.dropped_keys.insert(key);
                            break;
                        }
                    }
                }
            }
        } else if state == 0 {
            // Key released: if we dropped the press, drop the release too
            // to avoid stuck-key state in the guest.
            if self.dropped_keys.remove(&key) {
                action = Action::Drop;
                handled = false;
            }
        }

        // Send ack_key if we have an extended keyboard for this keyboard.
        // This is what actually controls whether the host runs the accelerator.
        if let Some(&host_extended_id) = ctx.keyboard_to_extended_keyboard.get(&host_keyboard_id) {
            let handled_val: u32 = if handled { 1 } else { 0 };
            let mut builder = MessageBuilder::new();
            builder.write_u32(serial);
            builder.write_u32(handled_val);

            let mut msg = Vec::new();
            msg.extend_from_slice(&host_extended_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 1u32; // opcode 1: ack_key
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);
            ctx.client_to_host_queue.push((msg, Vec::new()));
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
        ctx.keyboard_to_extended_keyboard.insert(0, 50);
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

        ctx.keyboard_to_extended_keyboard.insert(0, 50);
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

        ctx.keyboard_to_extended_keyboard.insert(0, 50);
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
}
