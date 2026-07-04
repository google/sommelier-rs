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

// Opcodes are taken directly from the XML protocol definition:
// third_party/protocols/keyboard-extension-unstable-v1.xml
//
// zcr_extended_keyboard_v1 requests:
//   request index 0 = destroy
//   request index 1 = ack_key
// zcr_keyboard_extension_v1 requests:
//   request index 0 = get_extended_keyboard
//
// These are validated against the generated protocol code in the
// `opcode_constants_match_generated_protocol` test below. If the XML
// is ever updated, update both the constants and the test.
const ZCR_EXTENDED_KEYBOARD_DESTROY: u16 = 0;
const ZCR_EXTENDED_KEYBOARD_ACK_KEY: u16 = 1;
const ZCR_KEYBOARD_EXTENSION_GET_EXTENDED_KEYBOARD: u16 = 0;

/// A read-only view of a shared-memory fd mapped into the process address space.
///
/// All `unsafe` for the mmap/munmap pair is confined here:
/// - `from_fd`: calls `mmap(MAP_SHARED, PROT_READ)` and stores the pointer + length.
/// - `as_bytes`: constructs a slice; valid because the mapping covers exactly `len` bytes.
/// - `Drop`: calls `munmap`; the pointer and length are never mutated after construction.
struct MmapView {
    ptr: std::ptr::NonNull<std::ffi::c_void>,
    len: usize,
}

impl MmapView {
    /// Map `len` bytes from `fd` at offset 0 as read-only shared memory.
    /// Returns `None` if `len` is zero or if `mmap` fails.
    fn from_fd(fd: std::os::unix::io::RawFd, len: usize) -> Option<Self> {
        use nix::sys::mman::{mmap, MapFlags, ProtFlags};
        use std::os::unix::io::BorrowedFd;

        let nonzero_len = std::num::NonZeroUsize::new(len)?;
        // Safety: fd is valid for the duration of this call; mmap does not
        // retain it. The returned pointer owns the mapping until munmap.
        let ptr = unsafe {
            let borrowed = BorrowedFd::borrow_raw(fd);
            mmap(
                None,
                nonzero_len,
                ProtFlags::PROT_READ,
                MapFlags::MAP_SHARED,
                borrowed,
                0,
            )
            .ok()?
        };
        Some(Self { ptr, len })
    }

    /// View the mapped region as a byte slice.
    fn as_bytes(&self) -> &[u8] {
        // Safety: ptr points to `self.len` readable bytes for the lifetime of
        // self (mapping is alive until Drop); no other writer exists (PROT_READ).
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr() as *const u8, self.len) }
    }
}

impl Drop for MmapView {
    fn drop(&mut self) {
        // Safety: ptr and len were set by mmap and never modified.
        // munmap can only fail with EINVAL (bad addr/len alignment), which
        // cannot happen here because ptr and len came directly from a
        // successful mmap call and are never mutated. The debug_assert
        // catches any future copy-paste of this code into a context where
        // that invariant might not hold.
        let res = unsafe { nix::sys::mman::munmap(self.ptr, self.len) };
        debug_assert!(
            res.is_ok(),
            "munmap on a valid mmap mapping must not fail: {:?}",
            res
        );
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
    /// Keys dropped on press (by evdev keycode, as sent in `wl_keyboard.key`);
    /// their corresponding release events are also dropped.
    ///
    /// Evdev keycodes — not keysyms — are used intentionally: a `wl_keyboard.key`
    /// release event always carries the same evdev code as its corresponding press,
    /// regardless of the active XKB shift level or any layout change that occurs
    /// between the press and the release. Tracking by keycode therefore gives a
    /// correct and unambiguous press↔release pairing.
    dropped_keys: std::collections::HashSet<u32>,
    /// Statically enforce `!Sync`: `KeyboardHandler` must never be shared
    /// across threads. `xkb::State` uses non-atomic interior mutation.
    _not_sync: std::marker::PhantomData<*mut ()>,
}

// # Safety
//
// `xkb::Context`, `Keymap`, and `State` are not `Send`. `KeyboardHandler`
// is only ever accessed from the single Tokio task that owns the `Client`;
// Tokio requires `Send` for spawned futures, so we satisfy it manually.
//
// INVARIANT: This `Send` impl is valid ONLY because:
//   1. `KeyboardHandler` is exclusively owned by one `Client` task.
//   2. `Client` tasks are never migrated across OS threads. This is guaranteed
//      by `#[tokio::main(flavor = "current_thread")]` in main.rs, which runs
//      the entire async runtime on a single OS thread. See the SAFETY INVARIANT
//      comment above that attribute in main.rs for the rationale.
//   3. `Sync` is statically inhibited via `PhantomData<*mut ()>`, so no
//      shared reference (`&KeyboardHandler`) can be sent across threads.
//
// *** AUDIT REQUIRED if any of the following changes: ***
//   - The Tokio executor type in main.rs (the flavor MUST remain
//     "current_thread"; switching to "multi_thread" makes this impl unsound)
//   - The handler lifecycle (e.g., storing it in an Arc)
//   - The field list of KeyboardHandler (e.g., adding a raw pointer)
unsafe impl Send for KeyboardHandler {}

impl KeyboardHandler {
    pub fn new() -> Self {
        Self {
            context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            keymap: None,
            state: None,
            modifiers: 0,
            dropped_keys: std::collections::HashSet::new(),
            _not_sync: std::marker::PhantomData,
        }
    }

    /// Check if the pressed key matches any configured host accelerators.
    fn is_host_accelerator(
        &self,
        accelerators: &[crate::accelerator::Accelerator],
        key: u32,
    ) -> bool {
        let Some(state) = &self.state else {
            return false;
        };

        let xkb_keycode = xkb::Keycode::new(key + 8);
        // Use key_get_one_sym so that the full XKB state (including active shift
        // level) is considered. key_get_syms_by_level at level 0 would always
        // return the unshifted symbol, causing <Shift>-modified accelerators to
        // fail to match when Shift is held.
        //
        // key_get_one_sym returns KEY_NoSymbol when more than one keysym is
        // mapped to this key at the current shift level (e.g. dead keys or
        // Unicode combining sequences). In that case we conservatively return
        // false (do not treat the key as a host accelerator), which is the
        // correct safe default: we'd rather forward an unknown key to the guest
        // than accidentally suppress it.
        let sym = state.key_get_one_sym(xkb_keycode);
        if sym.raw() == xkb::keysyms::KEY_NoSymbol {
            return false;
        }
        let lower_sym = crate::accelerator::keysym_to_lower(sym.raw());
        for acc in accelerators {
            if self.modifiers == acc.modifiers && lower_sym == acc.symbol {
                log::trace!(
                    "Accelerator match: key={}, modifiers={:#x}, sym={:#x}",
                    key,
                    self.modifiers,
                    lower_sym
                );
                return true;
            }
        }
        false
    }

    /// Ensure the `zcr_extended_keyboard_v1` object is bound for this keyboard.
    ///
    /// This is idempotent: if the extended keyboard is already bound for
    /// `host_keyboard_id`, this function is a no-op.
    ///
    /// This is called from `on_enter` (rather than `wl_seat.get_keyboard`) because
    /// the host keyboard ID (`ctx.last_sender_id` in a host→client event) is only
    /// known once we process a host event — `get_keyboard` is a client→host request
    /// and at that point we only have the guest keyboard ID. `on_enter` is always
    /// sent by the host before any `wl_keyboard.key` event, so this is safe:
    /// the extended keyboard will be bound before the first key event.
    ///
    /// # Ordering invariant
    /// `get_extended_keyboard` is sent via `client_to_host_queue` and will be
    /// flushed in the same proxy loop iteration as the forwarded `wl_keyboard.enter`
    /// event. Exo enables `SetNeedKeyboardKeyAcks(true)` upon processing
    /// `get_extended_keyboard`. Any key events queued by the compositor *before*
    /// this request is processed (e.g. auto-repeat already in-flight) will not have
    /// an active TTL in `pending_key_acks_`; Exo will apply its default policy for
    /// those keys. This mirrors the behavior of the C sommelier reference.
    pub(crate) fn ensure_extended_keyboard_bound(ctx: &mut Context, host_keyboard_id: HostId) {
        if let Some(extension_host_id) = ctx.host_keyboard_extension_id {
            if !ctx
                .keyboard_to_extended_keyboard
                .contains_key(&host_keyboard_id)
            {
                let host_extended_id = HostId::from_allocated(ctx.shadow_table.allocate_host_id());
                ctx.keyboard_to_extended_keyboard
                    .insert(host_keyboard_id, host_extended_id);
                ctx.shadow_table.track_host_interface(
                    host_extended_id.0,
                    "zcr_extended_keyboard_v1".to_string(),
                );

                // zcr_keyboard_extension_v1.get_extended_keyboard(new_id, keyboard)
                // payload = [new_id(4)][keyboard(4)] = 8 bytes.
                let mut builder = crate::wire::MessageBuilder::new();
                builder.write_u32(host_extended_id.0);
                builder.write_u32(host_keyboard_id.0);
                let msg = builder.build_message(
                    extension_host_id.0,
                    ZCR_KEYBOARD_EXTENSION_GET_EXTENDED_KEYBOARD,
                );
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
    ///
    /// If the protocol is available (`host_keyboard_extension_id` is set) but the
    /// extended keyboard hasn't been bound for this keyboard ID yet, that indicates
    /// a key event arrived before `on_enter` was processed. This should not happen
    /// in normal Wayland flow (Exo always sends `on_enter` before `key`), so we
    /// emit a warning to aid debugging if it ever occurs.
    fn send_ack_key(ctx: &mut Context, host_keyboard_id: HostId, serial: u32, handled: bool) {
        if ctx.host_keyboard_extension_id.is_none() {
            // Protocol not available on this compositor; silently skip.
            return;
        }
        let Some(&host_extended_id) = ctx.keyboard_to_extended_keyboard.get(&host_keyboard_id)
        else {
            log::warn!(
                "ack_key: no extended keyboard bound for host_keyboard_id={}; \
                 key event arrived before on_enter? serial={}, handled={}",
                host_keyboard_id.0,
                serial,
                handled
            );
            return;
        };
        // zcr_extended_keyboard_v1.ack_key — payload = [serial(4)][handled(4)].
        let mut builder = crate::wire::MessageBuilder::new();
        builder.write_u32(serial);
        builder.write_u32(if handled { 1u32 } else { 0u32 });
        let msg = builder.build_message(host_extended_id.0, ZCR_EXTENDED_KEYBOARD_ACK_KEY);
        ctx.client_to_host_queue.push((msg, Vec::new()));
    }
}

impl Default for KeyboardHandler {
    fn default() -> Self {
        Self::new()
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

        // A zero-size keymap is malformed; mmap(len=0) is UB per POSIX.
        if size == 0 {
            log::warn!("on_keymap: received zero-size keymap from host, ignoring");
            return Action::Forward;
        }

        // size is a u32 from the Wayland wire; the cast to usize is lossless on
        // 64-bit Linux (the only supported target for sommelier).
        let Some(mapping) = MmapView::from_fd(fd, size as usize) else {
            log::error!("on_keymap: mmap failed for fd={}, size={}", fd, size);
            return Action::Forward;
        };
        let slice = mapping.as_bytes();

        // Per the Wayland spec, wl_keyboard.keymap.size always includes exactly
        // one trailing NUL terminator. Strip it unconditionally so that XKB
        // receives clean text. If the host sends a malformed keymap without the
        // terminator this will still be safe (the XKB parser will reject it
        // rather than reading out-of-bounds — we clamped to `size` bytes above).
        //
        // We use a debug_assert (not a hard error) because a missing terminator
        // is a host protocol violation, not a local invariant failure; the proxy
        // should degrade gracefully rather than crash.
        debug_assert!(
            !slice.is_empty() && slice[slice.len() - 1] == 0,
            "on_keymap: keymap data is missing the trailing NUL required by the Wayland spec"
        );
        let len = (size as usize).saturating_sub(1);

        match std::str::from_utf8(&slice[..len]) {
            Err(e) => {
                log::error!("on_keymap: keymap bytes are not valid UTF-8: {}", e);
                // Clear keymap, state, and drop set together: all three must
                // remain mutually consistent. Leaving `keymap` set while `state`
                // is None creates a split where future code reading `keymap`
                // operates on stale data with no active XKB state to validate
                // against.
                self.keymap = None;
                self.state = None;
                self.dropped_keys.clear();
            }
            Ok(s) => match xkb::Keymap::new_from_string(
                &self.context,
                // `new_from_string` takes ownership; copy required because `s`
                // borrows from the mmap region which is dropped at the end of
                // this function.
                s.to_owned(),
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            ) {
                None => {
                    log::error!("on_keymap: xkbcommon failed to compile the keymap string");
                    // Clear keymap, state, and drop set together for the same
                    // consistency reason as the UTF-8 error case above: all
                    // three must remain in sync.
                    self.keymap = None;
                    self.state = None;
                    self.dropped_keys.clear();
                }
                Some(keymap) => {
                    self.state = Some(xkb::State::new(&keymap));
                    self.keymap = Some(keymap);
                    // Clear stale drop state: a key dropped under the old
                    // keymap may map to a different keysym under the new one,
                    // and a forgotten drop entry would cause a stuck key.
                    self.dropped_keys.clear();
                    log::info!("XKB keymap loaded successfully");
                }
            },
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
        let host_keyboard_id = HostId::from_event_sender(ctx);
        let guest_keyboard_id = ctx
            .shadow_table
            .guest_id_of(host_keyboard_id)
            .map(|g| g.0)
            .unwrap_or(0);
        let guest_surface_id = ctx.shadow_table.get_guest_id(surface).unwrap_or(0);

        // Lazily bind the extended keyboard object on first enter.
        // This sends zcr_keyboard_extension_v1.get_extended_keyboard to the
        // host, which enables ack mode (SetNeedKeyboardKeyAcks(true) in Exo).
        Self::ensure_extended_keyboard_bound(ctx, host_keyboard_id);

        // Defer activation updates until after the loop so we don't borrow ctx
        // while iterating over text_inputs.
        let mut text_inputs_to_update = Vec::new();
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
                        let msg = builder.build_message(*guest_text_input_id, 0);
                        ctx.host_to_client_queue.push((msg, Vec::new()));
                        text_inputs_to_update.push(*guest_text_input_id);
                    }
                }
            }
        }

        // Now safe to call update_host_activation (borrows ctx mutably).
        for id in text_inputs_to_update {
            crate::handler::text_input::update_host_activation(ctx, id);
        }

        Action::Forward
    }

    fn on_leave(&mut self, ctx: &mut Context, _serial: u32, surface: u32) -> Action {
        // on_leave is a host→client event: last_sender_id is the host keyboard ID.
        let host_keyboard_id = HostId::from_event_sender(ctx);
        let guest_keyboard_id = ctx
            .shadow_table
            .guest_id_of(host_keyboard_id)
            .map(|g| g.0)
            .unwrap_or(0);
        let guest_surface_id = ctx.shadow_table.get_guest_id(surface).unwrap_or(0);

        // Same deferred pattern as on_enter.
        let mut text_inputs_to_update = Vec::new();
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
                        let msg = builder.build_message(*guest_text_input_id, 1);
                        ctx.host_to_client_queue.push((msg, Vec::new()));
                        text_inputs_to_update.push(*guest_text_input_id);
                    }
                }
            }
        }

        // Same deferred pattern as on_enter.
        for id in text_inputs_to_update {
            crate::handler::text_input::update_host_activation(ctx, id);
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
        let host_keyboard_id = HostId::from_event_sender(ctx);
        let mut action = Action::Forward;
        let mut handled = true; // Default: guest handles the key.

        // WL_KEY_PRESSED = 1, WL_KEY_RELEASED = 0.
        // `other` catches any future unknown state values (e.g. if Wayland adds
        // a new key-repeat state) without silently falling through to a wrong arm.
        // In Rust, integer match arms are unordered — each arm matches its exact
        // pattern and `other` fires only for values not matched above.
        match state {
            WL_KEY_PRESSED => {
                // Key pressed: check if this is a host accelerator.
                if self.is_host_accelerator(&ctx.accelerators, key) {
                    action = Action::Drop;
                    handled = false;
                    self.dropped_keys.insert(key);
                }
                // Send ack_key only for press events, matching the C sommelier
                // reference implementation. Exo places only press events into
                // pending_key_acks_ and never expects an ack for a release;
                // a release ack would target a non-existent serial and be
                // silently ignored — but we avoid sending it for clarity and
                // to match the C reference exactly.
                Self::send_ack_key(ctx, host_keyboard_id, serial, handled);
            }
            WL_KEY_RELEASED => {
                // Key released: if we dropped the press, drop the release too
                // to avoid stuck-key state in the guest.
                if self.dropped_keys.remove(&key) {
                    action = Action::Drop;
                }
            }
            other => {
                log::warn!("on_key: received unknown key state {}, ignoring", other);
            }
        }

        action
    }

    /// Track modifier state so on_key can resolve the correct keysym.
    ///
    /// # Modifier policy: DEPRESSED | LATCHED only (no LOCKED)
    /// We intentionally exclude `STATE_MODS_LOCKED` (Caps Lock, etc.) from the
    /// accelerator modifier check. The C sommelier reference makes the same
    /// choice: host accelerators are defined in terms of actively-pressed keys,
    /// not persistent lock state. A `<Shift>` accelerator would not match when
    /// Caps Lock is on but Shift is not held — this is consistent with how
    /// ChromeOS accelerator keys are documented and tested.
    /// If locked-modifier accelerators are ever needed, change `components` to
    /// `xkb::STATE_MODS_EFFECTIVE` (which ORs depressed, latched, and locked).
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
            // Use the xkbcommon logical modifier name constants (e.g. MOD_NAME_ALT = "Mod1")
            // rather than raw X11 modifier group strings. These are the stable canonical names
            // that match across different keyboard layouts, matching C sommelier's use of
            // XKB_MOD_NAME_ALT, XKB_MOD_NAME_LOGO, etc.
            if state.mod_name_is_active(xkb::MOD_NAME_CTRL, components) {
                self.modifiers |= crate::accelerator::CONTROL_MASK;
            }
            if state.mod_name_is_active(xkb::MOD_NAME_ALT, components) {
                self.modifiers |= crate::accelerator::ALT_MASK;
            }
            if state.mod_name_is_active(xkb::MOD_NAME_SHIFT, components) {
                self.modifiers |= crate::accelerator::SHIFT_MASK;
            }
            if state.mod_name_is_active(xkb::MOD_NAME_LOGO, components) {
                self.modifiers |= crate::accelerator::SUPER_MASK;
            }
        } else {
            // XKB state is not yet initialised (keymap not yet received). The
            // Wayland spec permits modifiers to arrive before the keymap on
            // reconnect. The modifier bitmask stays at 0 (no modifiers assumed)
            // until the keymap arrives and on_modifiers is called again.
            log::debug!(
                "on_modifiers: XKB state not yet initialised (keymap not received); \
                 modifier event ignored (depressed={:#x}, latched={:#x}, locked={:#x})",
                mods_depressed,
                mods_latched,
                mods_locked
            );
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
        let Some(host_keyboard_id) = ctx
            .shadow_table
            .host_id_of(GuestId::from_request_sender(ctx))
        else {
            return Action::Forward;
        };
        // Clear per-keyboard state: dropped-key set and modifier bitmask.
        // Both must be reset so that a re-created keyboard starts from a clean
        // slate and doesn't inherit stale state from the previous session.
        //
        // dropped_keys: a key dropped under the old session would cause its
        //   release event to be silently swallowed on the new keyboard.
        // modifiers: if the new keyboard receives a key event before the first
        //   wl_keyboard.modifiers, the accelerator check would use stale modifier
        //   bits and could produce wrong NOT_HANDLED/HANDLED decisions.
        self.dropped_keys.clear();
        self.modifiers = 0;
        if let Some(host_extended_id) = ctx.keyboard_to_extended_keyboard.remove(&host_keyboard_id)
        {
            // zcr_extended_keyboard_v1.destroy — no payload (8-byte header only).
            let msg = crate::wire::MessageBuilder::new()
                .build_message(host_extended_id.0, ZCR_EXTENDED_KEYBOARD_DESTROY);
            ctx.client_to_host_queue.push((msg, Vec::new()));
            // Unregister from the host dispatch table so stale peek_key events
            // (version ≥ 2) sent after destroy cannot be dispatched to a dead object.
            ctx.shadow_table.remove_host_interface(host_extended_id.0);
            log::debug!(
                "Destroyed extended keyboard: host_extended_id={} for host_keyboard_id={}",
                host_extended_id.0,
                host_keyboard_id.0
            );
        }
        Action::Forward
    }
}

/// `zcr_keyboard_extension_v1` handler — sommelier only sends requests to this
/// interface (i.e. `get_extended_keyboard`); the host never sends events back to
/// the factory object, so all event callbacks are empty stubs.
impl crate::protocols::keyboard_extension_unstable_v1::zcr_keyboard_extension_v1::ZcrKeyboardExtensionV1Handler for KeyboardHandler {}

/// `zcr_extended_keyboard_v1` handler — sommelier only sends requests to this
/// interface (i.e. `ack_key`, `destroy`). The v1 protocol has no host events.
///
/// `on_peek_key` is a v2 addition; it is not requested here but is stubbed
/// with a `log::warn!` so that a v2 compositor is immediately visible in logs
/// rather than silently discarded, making future version upgrades easier to detect.
impl crate::protocols::keyboard_extension_unstable_v1::zcr_extended_keyboard_v1::ZcrExtendedKeyboardV1Handler for KeyboardHandler {
    fn on_peek_key(
        &mut self,
        _ctx: &mut crate::state::Context,
        _serial: u32,
        _time: u32,
        _key: u32,
        _state: u32,
    ) -> crate::wire::Action {
        log::warn!(
            "zcr_extended_keyboard_v1: received peek_key (v2 event); \
             sommelier is bound at v1 and does not handle peek_key. \
             Consider upgrading to v2 if the host compositor requires it."
        );
        crate::wire::Action::Forward
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::wayland::wl_keyboard::WlKeyboardHandler;
    use std::os::unix::io::AsRawFd;

    /// Helper: create an anonymous memfd, write the default keymap into it
    /// (with a trailing NUL byte as required by the Wayland spec), and call
    /// on_keymap so the handler builds its XKB state. Returns the keymap
    /// object so tests can look up keycodes.
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
        nix::unistd::write(&fd, keymap_str.as_bytes()).expect("write keymap failed");
        // Write the NUL terminator required by the Wayland spec
        // (wl_keyboard.keymap.size always includes it). on_keymap's debug_assert
        // checks for this byte, so omitting it would fire in debug builds.
        nix::unistd::write(&fd, &[0u8]).expect("write NUL failed");

        // size = string bytes + 1 NUL terminator, matching what a real host sends.
        let size = keymap_str.len() as u32 + 1;
        handler.on_keymap(ctx, 1, fd.as_raw_fd(), size);
        assert!(handler.keymap.is_some(), "keymap should be loaded");
        keymap
    }

    /// Find the evdev keycode for a given keysym in the keymap.
    ///
    /// Returns `None` if the keysym is not present. Call sites should use
    /// `.expect("<description>")` so that test-failure output names the keysym
    /// that was missing rather than showing a bare panic from inside this helper.
    ///
    /// # Note on shift-level lookup
    /// This helper uses `key_get_syms_by_level(..., level=0)` which returns the
    /// **unshifted** (base level) symbol for each key. This is intentional for
    /// the purposes of *finding* a keycode given a base keysym: tests using this
    /// helper should not involve shifted accelerators (e.g. `<Shift>A`), since
    /// such tests would need a different lookup strategy. The production code uses
    /// `key_get_one_sym` which correctly accounts for the active shift level at
    /// runtime.
    fn find_keycode(keymap: &xkb::Keymap, target: u32) -> Option<u32> {
        let min = keymap.min_keycode().raw();
        let max = keymap.max_keycode().raw();
        for k in min..=max {
            let syms = keymap.key_get_syms_by_level(k.into(), 0, 0);
            if syms.iter().any(|s| s.raw() == target) {
                // XKB keycodes = evdev keycode + 8 (XKB_KEYCODE_OFFSET in xkbcommon.h).
                // Wayland wl_keyboard.key uses evdev keycodes, so subtract 8.
                return Some(k - 8);
            }
        }
        None
    }

    #[test]
    fn accelerator_keys_are_dropped_and_acked_not_handled() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new_for_test(
            false,
            false,
            crate::accelerator::parse_accelerators("<Control>a").unwrap(),
        );

        let keymap = load_test_keymap(&mut handler, &mut ctx);
        let wl_key_a =
            find_keycode(&keymap, xkb::keysyms::KEY_a).expect("KEY_a not found in keymap");

        // Press Ctrl
        let ctrl_mask = 1 << keymap.mod_get_index("Control");
        handler.on_modifiers(&mut ctx, 0, ctrl_mask, 0, 0, 0);

        // Set up extended keyboard tracking so ack_key is sent.
        // host_keyboard_extension_id must be Some to enable the protocol path.
        // Use non-reserved IDs (not 0 or 1, which are null/wl_display).
        ctx.host_keyboard_extension_id = Some(HostId(99));
        ctx.keyboard_to_extended_keyboard
            .insert(HostId(5), HostId(50));
        ctx.last_sender_id = 5;

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
        let mut ctx = Context::new_for_test(
            false,
            false,
            crate::accelerator::parse_accelerators("<Control>a").unwrap(),
        );

        let keymap = load_test_keymap(&mut handler, &mut ctx);
        let wl_key_b =
            find_keycode(&keymap, xkb::keysyms::KEY_b).expect("KEY_b not found in keymap");

        // Press Ctrl
        let ctrl_mask = 1 << keymap.mod_get_index("Control");
        handler.on_modifiers(&mut ctx, 0, ctrl_mask, 0, 0, 0);

        ctx.host_keyboard_extension_id = Some(HostId(99));
        ctx.keyboard_to_extended_keyboard
            .insert(HostId(5), HostId(50));
        ctx.last_sender_id = 5;

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
        let mut ctx = Context::new_for_test(
            false,
            false,
            crate::accelerator::parse_accelerators("<Control>a").unwrap(),
        );

        let keymap = load_test_keymap(&mut handler, &mut ctx);
        let wl_key_a =
            find_keycode(&keymap, xkb::keysyms::KEY_a).expect("KEY_a not found in keymap");

        let ctrl_mask = 1 << keymap.mod_get_index("Control");
        handler.on_modifiers(&mut ctx, 0, ctrl_mask, 0, 0, 0);

        ctx.host_keyboard_extension_id = Some(HostId(99));
        ctx.keyboard_to_extended_keyboard
            .insert(HostId(5), HostId(50));
        ctx.last_sender_id = 5;

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

    /// Regression test: on_keymap must use mmap rather than read/seek because:
    /// 1. The host sends keymap data via shared memory (memfd); mmap reads from
    ///    offset 0 regardless of the fd cursor position.
    /// 2. read() would advance the fd offset, preventing the fd from being
    ///    forwarded with its original data intact.
    /// 3. On non-seekable fds the cursor cannot be reset to 0 at all.
    ///
    /// This test creates a memfd, writes the keymap, and does NOT seek back
    /// to the start — simulating a host that wrote and then sent the fd.
    /// The on_keymap implementation must still load the keymap via mmap.
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
        let fd = memfd_create(name.as_c_str(), MFdFlags::empty()).expect("memfd_create failed");
        nix::unistd::write(&fd, keymap_bytes).expect("write failed");
        // Write the NUL terminator that on_keymap expects (wl_keyboard.keymap.size
        // always includes it). Without this, the mmap covers one byte beyond what
        // was written; we'd rely on OS zero-fill of fresh anonymous pages, which is
        // guaranteed on Linux but is not required by POSIX.
        nix::unistd::write(&fd, &[0u8]).expect("write NUL failed");
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
            !ctx.keyboard_to_extended_keyboard
                .contains_key(&HostId(host_keyboard_id)),
            "extended keyboard map must be cleared after release"
        );

        // destroy message must have been queued to the host.
        assert_eq!(ctx.client_to_host_queue.len(), 1, "destroy must be queued");
        let (msg, _) = &ctx.client_to_host_queue[0];
        // Message: [sender_id(4)] [size_opcode(4)]  — opcode 0, len 8.
        let sender = u32::from_ne_bytes(msg[0..4].try_into().unwrap());
        let word2 = u32::from_ne_bytes(msg[4..8].try_into().unwrap());
        assert_eq!(
            sender, host_extended_id,
            "destroy must target host_extended_id"
        );
        assert_eq!(word2 >> 16, 8, "message length must be 8");
        assert_eq!(word2 & 0xFFFF, 0, "opcode must be 0 (destroy)");
    }

    #[test]
    fn bind_extended_keyboard_is_idempotent() {
        let mut ctx = Context::new(false, false);
        ctx.host_keyboard_extension_id = Some(HostId(99));

        // First call: should send get_extended_keyboard.
        KeyboardHandler::ensure_extended_keyboard_bound(&mut ctx, HostId(10));
        assert_eq!(ctx.client_to_host_queue.len(), 1);

        // Second call with the same host_keyboard_id: must not send again.
        ctx.client_to_host_queue.clear();
        KeyboardHandler::ensure_extended_keyboard_bound(&mut ctx, HostId(10));
        assert!(
            ctx.client_to_host_queue.is_empty(),
            "bind must be idempotent: no second get_extended_keyboard"
        );
    }

    #[test]
    fn is_host_accelerator_returns_false_without_xkb_state() {
        let handler = KeyboardHandler::new(); // no keymap loaded
        let accelerators = crate::accelerator::parse_accelerators("<Control>a").unwrap();
        // Must degrade gracefully, not panic.
        assert!(
            !handler.is_host_accelerator(&accelerators, 30),
            "should return false when XKB state is not initialised"
        );
    }

    // -----------------------------------------------------------------------
    // New tests added by code review fixes
    // -----------------------------------------------------------------------

    #[test]
    fn on_keymap_ignores_zero_size() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);
        // A zero-size keymap must be rejected gracefully (mmap(len=0) is UB).
        // fd=0 (stdin) won't be mmap'd because the size check fires first.
        let action = handler.on_keymap(&mut ctx, 1 /* XKB_V1 */, 0, 0 /* size=0 */);
        assert_eq!(
            action,
            Action::Forward,
            "zero-size keymap must forward, not panic"
        );
        assert!(
            handler.keymap.is_none(),
            "keymap must not be set after zero-size event"
        );
    }

    #[test]
    fn on_keymap_ignores_unknown_format() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);
        // Format 0 = WL_KEYMAP_FORMAT_NO_KEYMAP; forward without loading.
        let action = handler.on_keymap(&mut ctx, 0 /* format=no_keymap */, 0, 100);
        assert_eq!(action, Action::Forward);
        assert!(handler.keymap.is_none());
    }

    /// Regression: on_keymap must log an error and not crash when the
    /// keymap data is not valid UTF-8, or when xkbcommon rejects the
    /// string. In both cases the handler must degrade gracefully:
    /// keymap stays None, state stays None, Action::Forward is returned.
    #[test]
    fn on_keymap_logs_error_on_invalid_utf8() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);

        use nix::sys::memfd::{memfd_create, MFdFlags};
        use std::ffi::CString;
        let name = CString::new("test-bad-keymap").unwrap();
        let fd = memfd_create(name.as_c_str(), MFdFlags::empty()).expect("memfd_create failed");
        // Write a lone continuation byte — invalid UTF-8 — followed by a NUL
        // terminator, because wl_keyboard.keymap.size always includes the NUL.
        let bad_bytes: &[u8] = &[0xFF, 0xFE, 0xFD, 0x00];
        nix::unistd::write(&fd, bad_bytes).expect("write failed");

        use std::os::unix::io::AsRawFd;
        let action = handler.on_keymap(
            &mut ctx,
            1, /* XKB_V1 */
            fd.as_raw_fd(),
            bad_bytes.len() as u32,
        );
        assert_eq!(action, Action::Forward, "invalid UTF-8 must still forward");
        assert!(
            handler.keymap.is_none(),
            "keymap must remain None on parse error"
        );
        assert!(
            handler.state.is_none(),
            "state must remain None on parse error"
        );
    }

    #[test]
    fn on_keymap_logs_error_on_invalid_xkb_string() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);

        use nix::sys::memfd::{memfd_create, MFdFlags};
        use std::ffi::CString;
        let name = CString::new("test-bad-xkb").unwrap();
        let fd = memfd_create(name.as_c_str(), MFdFlags::empty()).expect("memfd_create failed");
        // Valid UTF-8 but not a valid XKB keymap. Include trailing NUL because
        // wl_keyboard.keymap.size always includes the NUL terminator.
        let garbage = b"this is not a valid xkb keymap\0";
        nix::unistd::write(&fd, garbage).expect("write failed");

        use std::os::unix::io::AsRawFd;
        let action = handler.on_keymap(
            &mut ctx,
            1, /* XKB_V1 */
            fd.as_raw_fd(),
            garbage.len() as u32,
        );
        assert_eq!(
            action,
            Action::Forward,
            "invalid XKB string must still forward"
        );
        assert!(
            handler.keymap.is_none(),
            "keymap must remain None on XKB compile error"
        );
        assert!(
            handler.state.is_none(),
            "state must remain None on XKB compile error"
        );
    }

    #[test]
    fn send_ack_key_warns_when_protocol_bound_but_keyboard_not_registered() {
        // Regression: if host_keyboard_extension_id is Some (protocol available)
        // but the keyboard hasn't been registered via on_enter yet, send_ack_key
        // must not panic or silently send a garbage ack. No queue entry expected.
        let mut ctx = Context::new(false, false);
        ctx.host_keyboard_extension_id = Some(HostId(99)); // protocol bound
                                                           // keyboard_to_extended_keyboard is empty (on_enter not yet received)

        KeyboardHandler::send_ack_key(&mut ctx, HostId(10), 1, true);

        assert!(
            ctx.client_to_host_queue.is_empty(),
            "no ack_key must be queued when keyboard is not yet registered"
        );
    }

    #[test]
    fn opcode_encoding_get_extended_keyboard_is_zero() {
        // Regression: get_extended_keyboard uses opcode 0. Verify the wire
        // message encodes it correctly (not accidentally a non-zero opcode).
        let mut ctx = Context::new(false, false);
        ctx.host_keyboard_extension_id = Some(HostId(99));

        KeyboardHandler::ensure_extended_keyboard_bound(&mut ctx, HostId(10));
        assert_eq!(ctx.client_to_host_queue.len(), 1);
        let (msg, _) = &ctx.client_to_host_queue[0];
        let word2 = u32::from_ne_bytes(msg[4..8].try_into().unwrap());
        let opcode = word2 & 0xFFFF;
        assert_eq!(
            opcode, 0,
            "get_extended_keyboard must use opcode 0, got {}",
            opcode
        );
    }

    /// Structural regression: dropped_keys must be cleared when the keyboard is released.
    ///
    /// Without the `dropped_keys.clear()` in `on_release`, a re-created keyboard (guest
    /// destroys and re-creates wl_keyboard, which is common on focus changes) inherits
    /// stale drop state and silently swallows release events for keys it never saw pressed,
    /// leaving the guest in a stuck-key state.
    #[test]
    fn dropped_keys_cleared_on_release() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new_for_test(
            false,
            false,
            crate::accelerator::parse_accelerators("<Control>a").unwrap(),
        );

        let keymap = load_test_keymap(&mut handler, &mut ctx);
        let wl_key_a =
            find_keycode(&keymap, xkb::keysyms::KEY_a).expect("KEY_a not found in keymap");

        ctx.host_keyboard_extension_id = Some(HostId(99));
        ctx.keyboard_to_extended_keyboard
            .insert(HostId(10), HostId(50));

        // Press Ctrl+A (host accelerator) with host keyboard ID 10.
        let ctrl_mask = 1 << keymap.mod_get_index("Control");
        handler.on_modifiers(&mut ctx, 0, ctrl_mask, 0, 0, 0);
        ctx.last_sender_id = 10; // host keyboard ID (on_key is host→client)
        let action = handler.on_key(&mut ctx, 1, 0, wl_key_a, WL_KEY_PRESSED);
        assert_eq!(action, Action::Drop);
        assert!(
            handler.dropped_keys.contains(&wl_key_a),
            "key must be in dropped_keys after drop"
        );

        // Guest destroys the keyboard (client→host: last_sender_id is the guest ID).
        ctx.shadow_table.map_id(5, 10); // guest 5 ↔ host 10
        ctx.last_sender_id = 5;
        handler.on_release(&mut ctx);

        // dropped_keys must be empty after release — the new session starts clean.
        assert!(
            handler.dropped_keys.is_empty(),
            "dropped_keys must be cleared by on_release to avoid stuck-key on re-bind"
        );

        // A release of the same key on the re-bound keyboard should now be forwarded,
        // not silently dropped.
        ctx.last_sender_id = 10;
        let release_action = handler.on_key(&mut ctx, 2, 0, wl_key_a, WL_KEY_RELEASED);
        assert_eq!(
            release_action,
            Action::Forward,
            "release of key not in dropped_keys must be forwarded after re-bind"
        );
    }

    /// Structural test: `MessageBuilder::build_message` must produce well-formed wire frames.
    ///
    /// This catches any future refactoring that breaks the `[sender][size<<16|opcode][payload]`
    /// encoding, which would silently corrupt all internal protocol messages.
    #[test]
    fn build_wayland_msg_encodes_frame_correctly() {
        let payload = [0x01u8, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00]; // two u32 words
        let mut builder = crate::wire::MessageBuilder::new();
        builder.write_u32(0x00000001);
        builder.write_u32(0x00000002);
        let msg = builder.build_message(42, 7);

        let sender = u32::from_ne_bytes(msg[0..4].try_into().unwrap());
        assert_eq!(sender, 42, "sender_id wrong");

        let expected_total_len = 8u32 + payload.len() as u32; // header + payload
        let word2 = u32::from_ne_bytes(msg[4..8].try_into().unwrap());
        assert_eq!(word2 >> 16, expected_total_len, "size field wrong");
        assert_eq!(word2 & 0xFFFF, 7, "opcode field wrong");

        assert_eq!(&msg[8..], &payload, "payload bytes wrong");
    }

    /// Validate that our hand-written opcode constants match the values the
    /// code-generator derives from the XML. If the XML is ever updated and
    /// the generated opcodes change, this test fails \u2014 preventing the silent
    /// serialization corruption that would otherwise occur.
    ///
    /// The generated `Request::opcode()` method is the authoritative source:
    ///   zcr_extended_keyboard_v1::Request::Destroy => 0
    ///   zcr_extended_keyboard_v1::Request::AckKey  => 1
    ///   zcr_keyboard_extension_v1::Request::GetExtendedKeyboard => 0
    #[test]
    fn opcode_constants_match_generated_protocol() {
        use crate::protocols::keyboard_extension_unstable_v1::{
            zcr_extended_keyboard_v1::Request as ExtKbReq,
            zcr_keyboard_extension_v1::Request as ExtFactReq,
        };

        assert_eq!(
            ExtKbReq::Destroy {}.opcode(),
            ZCR_EXTENDED_KEYBOARD_DESTROY,
            "ZCR_EXTENDED_KEYBOARD_DESTROY constant out of sync with generated protocol"
        );
        assert_eq!(
            ExtKbReq::AckKey {
                serial: 0,
                handled: 0
            }
            .opcode(),
            ZCR_EXTENDED_KEYBOARD_ACK_KEY,
            "ZCR_EXTENDED_KEYBOARD_ACK_KEY constant out of sync with generated protocol"
        );
        assert_eq!(
            ExtFactReq::GetExtendedKeyboard { id: 0, keyboard: 0 }.opcode(),
            ZCR_KEYBOARD_EXTENSION_GET_EXTENDED_KEYBOARD,
            "ZCR_KEYBOARD_EXTENSION_GET_EXTENDED_KEYBOARD constant out of sync with generated protocol"
        );
    }

    /// Regression: dropped_keys must be cleared when a new keymap arrives so
    /// that keysym re-mappings across keymap updates cannot leave stale drop
    /// entries that would cause stuck keys.
    #[test]
    fn dropped_keys_cleared_on_keymap_reload() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new_for_test(
            false,
            false,
            crate::accelerator::parse_accelerators("<Control>a").unwrap(),
        );

        let keymap = load_test_keymap(&mut handler, &mut ctx);
        let wl_key_a =
            find_keycode(&keymap, xkb::keysyms::KEY_a).expect("KEY_a not found in keymap");

        // Simulate dropping a key (press of Ctrl+A)
        let ctrl_mask = 1 << keymap.mod_get_index("Control");
        handler.on_modifiers(&mut ctx, 0, ctrl_mask, 0, 0, 0);
        ctx.host_keyboard_extension_id = Some(HostId(99));
        ctx.keyboard_to_extended_keyboard
            .insert(HostId(5), HostId(50));
        ctx.last_sender_id = 5;
        let action = handler.on_key(&mut ctx, 1, 0, wl_key_a, WL_KEY_PRESSED);
        assert_eq!(action, Action::Drop);
        assert!(handler.dropped_keys.contains(&wl_key_a));

        // Reload keymap: dropped_keys must be cleared.
        load_test_keymap(&mut handler, &mut ctx);
        assert!(
            handler.dropped_keys.is_empty(),
            "dropped_keys must be cleared on keymap reload to prevent stuck keys after layout change"
        );

        // After reload, Ctrl+A must still be recognized as a host accelerator
        // and dropped. This guards against a regression where clearing
        // dropped_keys also breaks the accelerator matching logic.
        let action = handler.on_key(&mut ctx, 2, 0, wl_key_a, WL_KEY_PRESSED);
        assert_eq!(
            action,
            Action::Drop,
            "accelerator must still be recognized and dropped after keymap reload"
        );
    }

    /// Regression: on_release must reset modifiers to 0 so a re-bound keyboard
    /// does not inherit stale modifier state from the previous session. Without
    /// this, a key event arriving before the next wl_keyboard.modifiers could
    /// be matched against the old (wrong) modifier mask.
    #[test]
    fn modifiers_reset_on_release() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);

        ctx.shadow_table.map_id(5, 10); // guest 5 <-> host 10

        // Inject a modifier state directly (no XKB state needed for this test).
        handler.modifiers = crate::accelerator::CONTROL_MASK;
        assert_ne!(handler.modifiers, 0, "precondition: modifiers are non-zero");

        // Release the keyboard (client->host request: last_sender_id = guest ID).
        ctx.last_sender_id = 5;
        handler.on_release(&mut ctx);

        assert_eq!(
            handler.modifiers, 0,
            "modifiers must be cleared by on_release to avoid stale state on re-bind"
        );
    }

    /// N8: on_release with an unknown guest keyboard ID (keyboard that never
    /// received on_enter) must return Forward without panicking or queuing anything.
    #[test]
    fn on_release_with_unknown_guest_id_forwards_gracefully() {
        let mut handler = KeyboardHandler::new();
        let mut ctx = Context::new(false, false);
        // last_sender_id 999 is not in the shadow table.
        ctx.last_sender_id = 999;
        assert_eq!(handler.on_release(&mut ctx), Action::Forward);
        assert!(
            ctx.client_to_host_queue.is_empty(),
            "no destroy message should be queued for an unknown keyboard"
        );
    }
}
