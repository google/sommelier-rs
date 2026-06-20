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

use crate::protocols::text_input_extension_unstable_v1::zcr_extended_text_input_v1;
use crate::protocols::text_input_extension_unstable_v1::zcr_text_input_extension_v1;
use crate::protocols::text_input_unstable_v1::zwp_text_input_manager_v1;
use crate::protocols::text_input_unstable_v1::zwp_text_input_v1;
use crate::protocols::text_input_unstable_v3::zwp_text_input_manager_v3;
use crate::protocols::text_input_unstable_v3::zwp_text_input_v3;
use crate::state::Context;
use crate::wire::{Action, MessageBuilder};

// Helpers that eliminate repetitive wire-format construction.
// host→client messages go to the guest's virtual input device (e.g. synthetic keyboard events).
// client→host messages go to the host compositor (e.g. activate, commit_state).
fn queue_host_msg(ctx: &mut Context, sender_id: u32, opcode: u16, builder: MessageBuilder) {
    ctx.host_to_client_queue
        .push((builder.build_message(sender_id, opcode), Vec::new()));
}

fn queue_client_msg(ctx: &mut Context, sender_id: u32, opcode: u16, builder: MessageBuilder) {
    ctx.client_to_host_queue
        .push((builder.build_message(sender_id, opcode), Vec::new()));
}

pub struct TextInputManagerV1Handler;
impl zwp_text_input_manager_v1::ZwpTextInputManagerV1Handler for TextInputManagerV1Handler {}

pub struct TextInputV1Handler;
impl zwp_text_input_v1::ZwpTextInputV1Handler for TextInputV1Handler {
    fn on_preedit_string(
        &mut self,
        ctx: &mut Context,
        serial: u32,
        text: &String,
        _commit: &String,
    ) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            // Track the host serial so commit_state can forward it back.
            // Needed by ChromeOS/Exo to validate the commit sequence.
            let commit_serial = if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
                state.host_serial = serial;
                state.current_preedit = text.clone();
                state.commit_serial
            } else {
                0
            };

            // v3 preedit_string (opcode 2): cursor_end = text.len() selects entire preedit.
            let mut b = MessageBuilder::new();
            b.write_string(text);
            b.write_i32(0);
            b.write_i32(text.len() as i32);
            queue_host_msg(ctx, guest_id, 2, b);

            // v3 done (opcode 5): signals the guest that the preedit update is complete.
            let mut b = MessageBuilder::new();
            b.write_u32(commit_serial);
            queue_host_msg(ctx, guest_id, 5, b);
        }
        Action::Drop
    }

    fn on_commit_string(&mut self, ctx: &mut Context, serial: u32, text: &String) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            let commit_serial = if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
                state.host_serial = serial;
                state.current_preedit.clear();
                state.commit_serial
            } else {
                0
            };

            // Explicitly clear preedit before commit (v3 preedit_string "").
            // Without this, the guest may keep stale underline after commit.
            let mut b = MessageBuilder::new();
            b.write_string("");
            b.write_i32(0);
            b.write_i32(0);
            queue_host_msg(ctx, guest_id, 2, b);

            // v3 commit_string (opcode 3).
            let mut b = MessageBuilder::new();
            b.write_string(text);
            queue_host_msg(ctx, guest_id, 3, b);

            // v3 done (opcode 5) with serial.
            let mut b = MessageBuilder::new();
            b.write_u32(commit_serial);
            queue_host_msg(ctx, guest_id, 5, b);
        }
        Action::Drop
    }

    fn on_keysym(
        &mut self,
        ctx: &mut Context,
        serial: u32,
        time: u32,
        sym: u32,
        state: u32,
        _modifiers: u32,
    ) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            if let Some(s) = ctx.text_inputs.get_mut(&guest_id) {
                s.host_serial = serial;
            }
        }

        // Cache XKB context/keymap per thread to avoid recompiling on each key repeat.
        thread_local! {
            static XKB_CACHE: std::cell::RefCell<Option<(xkbcommon::xkb::Context, xkbcommon::xkb::Keymap)>> =
                const { std::cell::RefCell::new(None) };
        }

        let found_keycode = XKB_CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.is_none() {
                let xkb_ctx = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
                if let Some(km) = xkbcommon::xkb::Keymap::new_from_names(
                    &xkb_ctx, "", "", "", "", None, xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
                ) {
                    *cache = Some((xkb_ctx, km));
                }
            }
            // Entire lookup stays inside `with()` so the RefCell borrow is dropped
            // before returning — no raw pointer escape across the thread_local boundary.
            let mut found = None;
            if let Some((_, keymap)) = cache.as_ref() {
                for keycode_raw in keymap.min_keycode().raw()..=keymap.max_keycode().raw() {
                    let keycode = keycode_raw.into();
                    let syms = keymap.key_get_syms_by_level(keycode, 0, 0);
                    if syms.iter().any(|s| s.raw() == sym) {
                        found = Some(keycode_raw - 8);
                        break;
                    }
                }
            }
            found
        });

        // Forward the key event to the guest wl_keyboard with the real serial/time.
        // Previously hardcoded 0 for both, which caused ChromeOS/Exo to reject events.
        if let Some(keycode) = found_keycode {
            let keyboards = ctx.shadow_table.find_by_interface("wl_keyboard");
            if let Some(&keyboard_id) = keyboards.first() {
                let mut b = MessageBuilder::new();
                b.write_u32(serial);
                b.write_u32(time);
                b.write_u32(keycode);
                b.write_u32(state);
                queue_host_msg(ctx, keyboard_id, 3, b);
            }
        }
        Action::Drop
    }

    fn on_enter(&mut self, _ctx: &mut Context, _surface: u32) -> Action {
        Action::Drop
    }

    fn on_leave(&mut self, _ctx: &mut Context) -> Action {
        Action::Drop
    }

    fn on_modifiers_map(&mut self, _ctx: &mut Context, _map: &[u8]) -> Action {
        Action::Drop
    }

    fn on_input_panel_state(&mut self, _ctx: &mut Context, _state: u32) -> Action {
        Action::Drop
    }

    fn on_preedit_styling(
        &mut self,
        _ctx: &mut Context,
        _index: u32,
        _length: u32,
        _style: u32,
    ) -> Action {
        Action::Drop
    }

    fn on_preedit_cursor(&mut self, _ctx: &mut Context, _index: i32) -> Action {
        Action::Drop
    }

    fn on_cursor_position(&mut self, _ctx: &mut Context, _index: i32, _anchor: i32) -> Action {
        Action::Drop
    }

    fn on_delete_surrounding_text(
        &mut self,
        ctx: &mut Context,
        index: i32,
        length: u32,
    ) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            let commit_serial = if let Some(state) = ctx.text_inputs.get(&guest_id) {
                state.commit_serial
            } else {
                0
            };

            // Clamp u32→i32 to avoid panic on impossible lengths.
            let length_i32 = i32::try_from(length).unwrap_or_else(|_| {
                log::warn!(
                    "on_delete_surrounding_text: length {} exceeds i32::MAX, clamping",
                    length
                );
                i32::MAX
            });

            // Convert v3's cursor-relative index to v1's before/after lengths.
            // index < 0 means delete before cursor; use i64 to avoid i32::MIN overflow
            // when negating (-i32::MIN == i32::MIN due to two's complement).
            let before_length = if index < 0 {
                (-(index as i64)) as u32
            } else {
                0
            };
            let after_length = if index.saturating_add(length_i32) > 0 {
                index.saturating_add(length_i32) as u32
            } else {
                0
            };

            // v1 delete_surrounding_text (opcode 4): before_length, after_length.
            let mut b = MessageBuilder::new();
            b.write_u32(before_length);
            b.write_u32(after_length);
            queue_host_msg(ctx, guest_id, 4, b);

            // v3 done (opcode 5) to commit the delete.
            let mut b = MessageBuilder::new();
            b.write_u32(commit_serial);
            queue_host_msg(ctx, guest_id, 5, b);
        }
        Action::Drop
    }

    fn on_language(&mut self, ctx: &mut Context, serial: u32, _language: &String) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            // Track serial so the next commit_state sends it to the host.
            if let Some(s) = ctx.text_inputs.get_mut(&guest_id) {
                s.host_serial = serial;
            }
        }
        Action::Drop
    }

    fn on_text_direction(&mut self, ctx: &mut Context, serial: u32, _direction: u32) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            // Track serial so the next commit_state sends it to the host.
            if let Some(s) = ctx.text_inputs.get_mut(&guest_id) {
                s.host_serial = serial;
            }
        }
        Action::Drop
    }
}

pub struct TextInputExtensionV1Handler;
impl zcr_text_input_extension_v1::ZcrTextInputExtensionV1Handler for TextInputExtensionV1Handler {}

pub struct ExtendedTextInputV1Handler;
impl zcr_extended_text_input_v1::ZcrExtendedTextInputV1Handler for ExtendedTextInputV1Handler {
    // Translates zcr_extended_text_input_v1::set_preedit_region (cursor-relative index/length)
    // into zwp_text_input_v1::delete_surrounding_text + preedit_string + done.
    fn on_set_preedit_region(&mut self, ctx: &mut Context, index: i32, length: u32) -> Action {
        let host_ext_id = ctx.last_sender_id;

        struct PreeditAction {
            guest_id: u32,
            commit_serial: u32,
            before_length: u32,
            after_length: u32,
            preedit_text: String,
        }

        // Lookup by host_ext_id because this event arrives on the extended text input protocol.
        let action = ctx
            .text_inputs
            .iter_mut()
            .find(|(_, s)| s.host_ext_id == host_ext_id)
            .and_then(|(&guest_id, state)| {
                let commit_serial = state.commit_serial;
                let (text, cursor, _anchor) = state.surrounding_text.as_ref()?;
                let cursor_i64 = *cursor as i64;
                let index_i64 = index as i64;
                let start_idx = cursor_i64 + index_i64;
                let length_i64 = length as i64;

                // Validate bounds and char boundaries for CJK safety.
                if start_idx >= 0
                    && start_idx + length_i64 <= text.len() as i64
                    && text.is_char_boundary(start_idx as usize)
                    && text.is_char_boundary((start_idx + length_i64) as usize)
                {
                    let preedit_text = text[start_idx as usize..(start_idx + length_i64) as usize].to_string();
                    let before_length = if start_idx < cursor_i64 {
                        (cursor_i64 - start_idx) as u32
                    } else {
                        0
                    };
                    let after_length = if start_idx + length_i64 > cursor_i64 {
                        (start_idx + length_i64 - cursor_i64) as u32
                    } else {
                        0
                    };
                    state.current_preedit = preedit_text.clone();
                    Some(PreeditAction { guest_id, commit_serial, before_length, after_length, preedit_text })
                } else {
                    log::warn!(
                        "on_set_preedit_region: calculated range [{}, {}] is out of bounds or invalid for text of length {}",
                        start_idx, start_idx + length_i64, text.len()
                    );
                    None
                }
            });

        // Send three v1 messages: delete_surrounding_text to replace the region,
        // preedit_string to display highlighted text, and done to commit.
        if let Some(a) = action {
            let mut b = MessageBuilder::new();
            b.write_u32(a.before_length);
            b.write_u32(a.after_length);
            queue_host_msg(ctx, a.guest_id, 4, b);

            let mut b = MessageBuilder::new();
            b.write_string(&a.preedit_text);
            b.write_i32(0);
            b.write_i32(a.preedit_text.len() as i32);
            queue_host_msg(ctx, a.guest_id, 2, b);

            let mut b = MessageBuilder::new();
            b.write_u32(a.commit_serial);
            queue_host_msg(ctx, a.guest_id, 5, b);
        }

        Action::Drop
    }
    fn on_clear_grammar_fragments(&mut self, _ctx: &mut Context, _start: u32, _end: u32) -> Action {
        Action::Drop
    }
    fn on_add_grammar_fragment(
        &mut self,
        _ctx: &mut Context,
        _start: u32,
        _end: u32,
        _suggestion: &String,
    ) -> Action {
        Action::Drop
    }
    fn on_set_autocorrect_range(&mut self, _ctx: &mut Context, _start: u32, _end: u32) -> Action {
        Action::Drop
    }
    fn on_set_virtual_keyboard_occluded_bounds(
        &mut self,
        _ctx: &mut Context,
        _x: i32,
        _y: i32,
        _width: i32,
        _height: i32,
    ) -> Action {
        Action::Drop
    }
    // Commits the cached preedit text to the guest as a v3 commit_string + done sequence.
    // The preedit was cached by on_set_preedit_region; no surrounding_text needed here.
    fn on_confirm_preedit(&mut self, ctx: &mut Context, _selection_behavior: u32) -> Action {
        let host_ext_id = ctx.last_sender_id;
        let confirm = ctx
            .text_inputs
            .iter_mut()
            .find(|(_, s)| s.host_ext_id == host_ext_id)
            .map(|(&guest_id, state)| {
                let preedit_text = state.current_preedit.clone();
                let commit_serial = state.commit_serial;
                state.current_preedit.clear();
                (guest_id, preedit_text, commit_serial)
            });

        if let Some((guest_id, preedit_text, commit_serial)) = confirm {
            // Skip empty commits to avoid confusing the guest.
            if !preedit_text.is_empty() {
                let mut b = MessageBuilder::new();
                b.write_string(&preedit_text);
                queue_host_msg(ctx, guest_id, 3, b);

                let mut b = MessageBuilder::new();
                b.write_u32(commit_serial);
                queue_host_msg(ctx, guest_id, 5, b);
            }
        }
        Action::Drop
    }
}

pub struct TextInputManagerV3Handler;
impl zwp_text_input_manager_v3::ZwpTextInputManagerV3Handler for TextInputManagerV3Handler {
    fn on_get_text_input(&mut self, ctx: &mut Context, id: u32, seat: u32) -> Action {
        let host_v1_id = ctx.shadow_table.allocate_host_id();
        let host_ext_id = ctx.shadow_table.allocate_host_id();

        if let Some(host_manager_id) = ctx.host_text_input_manager_v1_id {
            let mut b = MessageBuilder::new();
            b.write_u32(host_v1_id);
            queue_client_msg(ctx, host_manager_id, 0, b);
        }

        if let Some(host_ext_manager_id) = ctx.host_text_input_extension_v1_id {
            let mut b = MessageBuilder::new();
            b.write_u32(host_ext_id);
            b.write_u32(host_v1_id);
            queue_client_msg(ctx, host_ext_manager_id, 0, b);
        }

        ctx.shadow_table.map_id(id, host_v1_id);
        ctx.shadow_table
            .track_interface(id, "zwp_text_input_v3".to_string());
        ctx.shadow_table
            .track_host_interface(host_v1_id, "zwp_text_input_v1".to_string());
        ctx.shadow_table
            .track_host_interface(host_ext_id, "zcr_extended_text_input_v1".to_string());

        let active_surface = ctx.active_surface_for_seat.get(&seat).copied();

        ctx.text_inputs.insert(
            id,
            crate::state::TextInputState {
                host_v1_id,
                host_ext_id,
                guest_seat: seat,
                active_surface,
                enabled: false,
                enabled_changed: false,
                surrounding_text: None,
                surrounding_text_dirty: false,
                content_hint: 0,
                content_purpose: 0,
                cursor_rect: None,
                text_change_cause: 0,
                current_preedit: String::new(),
                commit_serial: 0,
                host_serial: 0,
                host_activated: false,
            },
        );

        Action::Drop
    }
}

// Decouples host-side text-input activation from the v3 on_commit lifecycle.
// Called from keyboard enter/leave after setting active_surface, so we never
// send activate with a null surface even if the client commits before focus.
pub(crate) fn update_host_activation(ctx: &mut Context, guest_id: u32) {
    struct ActivationAction {
        host_v1_id: u32,
        host_seat: u32,
        host_surface: u32,
        target_activated: bool,
    }

    // Immutable get(): the borrow is dropped before the later get_mut().
    let action = ctx.text_inputs.get(&guest_id).map(|state| {
        let host_seat = ctx.shadow_table.get_host_id(state.guest_seat).unwrap_or(0);
        let host_surface = state
            .active_surface
            .and_then(|s| ctx.shadow_table.get_host_id(s))
            .unwrap_or(0);
        let target_activated = state.enabled && host_surface != 0;
        ActivationAction {
            host_v1_id: state.host_v1_id,
            host_seat,
            host_surface,
            target_activated,
        }
    });

    if let Some(a) = action {
        let prev_activated = ctx
            .text_inputs
            .get(&guest_id)
            .map(|s| s.host_activated)
            .unwrap_or(false);

        if a.target_activated != prev_activated {
            if a.target_activated {
                log::info!(
                    "update_host_activation: activating text input v1 (guest_id={}, host_v1_id={}, host_surface={})",
                    guest_id,
                    a.host_v1_id,
                    a.host_surface,
                );
                let mut b = MessageBuilder::new();
                b.write_u32(a.host_seat);
                b.write_u32(a.host_surface);
                queue_client_msg(ctx, a.host_v1_id, 0, b);
            } else {
                log::info!(
                    "update_host_activation: deactivating text input v1 (guest_id={}, host_v1_id={})",
                    guest_id,
                    a.host_v1_id
                );
                let mut b = MessageBuilder::new();
                b.write_u32(a.host_seat);
                queue_client_msg(ctx, a.host_v1_id, 1, b);
            }

            // Separate get_mut() call — immutable borrow from action is already dropped.
            if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
                state.host_activated = a.target_activated;
            }
        }
    }
}

pub struct TextInputV3Handler;
impl zwp_text_input_v3::ZwpTextInputV3Handler for TextInputV3Handler {
    fn on_enable(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.enabled = true;
            state.enabled_changed = true;
        }
        Action::Drop
    }

    fn on_disable(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.enabled = false;
            state.enabled_changed = true;
        }
        Action::Drop
    }

    fn on_set_surrounding_text(
        &mut self,
        ctx: &mut Context,
        text: &String,
        cursor: i32,
        anchor: i32,
    ) -> Action {
        let guest_id = ctx.last_sender_id;
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.surrounding_text = Some((text.clone(), cursor, anchor));
            state.surrounding_text_dirty = true;
        }
        Action::Drop
    }

    fn on_set_text_change_cause(&mut self, ctx: &mut Context, cause: u32) -> Action {
        let guest_id = ctx.last_sender_id;
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.text_change_cause = cause;
        }
        Action::Drop
    }

    fn on_set_content_type(&mut self, ctx: &mut Context, hint: u32, purpose: u32) -> Action {
        let guest_id = ctx.last_sender_id;
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.content_hint = hint;
            state.content_purpose = purpose;
        }
        Action::Drop
    }

    fn on_set_cursor_rectangle(
        &mut self,
        ctx: &mut Context,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> Action {
        let guest_id = ctx.last_sender_id;
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.cursor_rect = Some((x, y, width, height));
        }
        Action::Drop
    }

    // Translates zwp_text_input_v3::commit into a batch of zwp_text_input_v1 messages.
    // The serial is incremented before update_host_activation so activation uses it.
    fn on_commit(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            // wrapping_add prevents panic on overflow; .max(1) avoids serial 0 (unset state).
            state.commit_serial = state.commit_serial.wrapping_add(1).max(1);
        }

        // May send activate/deactivate to the host if enabled/surface changed.
        update_host_activation(ctx, guest_id);

        // Extract all state values while holding the mutable borrow, then drop it
        // so queue_client_msg can borrow ctx again.
        struct CommitActions {
            host_v1_id: u32,
            host_ext_id: u32,
            text_info: Option<(String, i32, i32)>,
            content_type: Option<(u32, u32)>,
            cursor_rect: Option<(i32, i32, i32, i32)>,
            host_serial: u32,
        }

        let actions = ctx.text_inputs.get_mut(&guest_id).map(|state| {
            state.enabled_changed = false;
            CommitActions {
                host_v1_id: state.host_v1_id,
                host_ext_id: state.host_ext_id,
                // Only send surrounding text if it changed (dirty flag).
                text_info: if state.surrounding_text_dirty {
                    state.surrounding_text_dirty = false;
                    state.surrounding_text.clone()
                } else {
                    None
                },
                // content_hint/purpose are consumed once and reset after sending.
                content_type: if state.content_hint != 0 || state.content_purpose != 0 {
                    let hint = state.content_hint;
                    let purpose = state.content_purpose;
                    state.content_hint = 0;
                    state.content_purpose = 0;
                    Some((hint, purpose))
                } else {
                    None
                },
                cursor_rect: state.cursor_rect.take(),
                host_serial: state.host_serial,
            }
        });

        if let Some(a) = actions {
            // set_surrounding_text (opcode 5) on v1.
            if let Some((text, cursor, anchor)) = a.text_info {
                let mut b = MessageBuilder::new();
                b.write_string(&text);
                b.write_u32(cursor as u32);
                b.write_u32(anchor as u32);
                queue_client_msg(ctx, a.host_v1_id, 5, b);
            }

            // set_content_type (opcode 6) on v1 + set_input_type (opcode 6) on extended.
            if let Some((hint, purpose)) = a.content_type {
                let mut b = MessageBuilder::new();
                b.write_u32(hint);
                b.write_u32(purpose);
                queue_client_msg(ctx, a.host_v1_id, 6, b);

                // Map zwp_text_input_v3 content purpose → zcr_extended_text_input_v1 input type.
                let input_type = match purpose {
                    0 | 1 | 7 => 1,
                    2 | 3 => 2,
                    4 => 3,
                    5 => 4,
                    6 => 5,
                    8 => 6,
                    _ => 1,
                };

                let mut b = MessageBuilder::new();
                b.write_u32(input_type);
                b.write_u32(0);
                b.write_u32(0);
                b.write_u32(0);
                b.write_u32(0);
                queue_client_msg(ctx, a.host_ext_id, 6, b);
            }

            if let Some((x, y, w, h)) = a.cursor_rect {
                let mut b = MessageBuilder::new();
                b.write_i32(x);
                b.write_i32(y);
                b.write_i32(w);
                b.write_i32(h);
                queue_client_msg(ctx, a.host_v1_id, 7, b);
            }

            let mut b = MessageBuilder::new();
            b.write_u32(a.host_serial);
            queue_client_msg(ctx, a.host_v1_id, 9, b);
        }

        Action::Drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::text_input_unstable_v1::zwp_text_input_v1::ZwpTextInputV1Handler;
    use crate::protocols::text_input_extension_unstable_v1::zcr_extended_text_input_v1::ZcrExtendedTextInputV1Handler;
    use crate::protocols::text_input_unstable_v3::zwp_text_input_v3::ZwpTextInputV3Handler;

    /// Helper: extract opcode from a wire message at the given index in a queue.
    fn msg_opcode(queue: &[(Vec<u8>, Vec<std::os::unix::io::RawFd>)], idx: usize) -> u16 {
        let word2 = u32::from_ne_bytes(queue[idx].0[4..8].try_into().unwrap());
        (word2 & 0xffff) as u16
    }

    /// Helper: extract sender_id from a wire message.
    fn msg_sender(queue: &[(Vec<u8>, Vec<std::os::unix::io::RawFd>)], idx: usize) -> u32 {
        u32::from_ne_bytes(queue[idx].0[0..4].try_into().unwrap())
    }

    /// Helper: set up a context with a host→guest mapping for text input testing.
    fn setup_v1_ctx() -> (Context, u32, u32) {
        let mut ctx = Context::new_for_test(false, false, vec![]);
        let host_v1_id = 10u32;
        let guest_id = 20u32;
        let host_ext_id = 30u32;
        ctx.shadow_table.map_id(guest_id, host_v1_id);
        ctx.text_inputs.insert(
            guest_id,
            crate::state::TextInputState {
                host_v1_id,
                host_ext_id,
                guest_seat: 0,
                active_surface: None,
                enabled: true,
                enabled_changed: false,
                surrounding_text: None,
                surrounding_text_dirty: false,
                content_hint: 0,
                content_purpose: 0,
                cursor_rect: None,
                text_change_cause: 0,
                current_preedit: String::new(),
                commit_serial: 0,
                host_serial: 0,
                host_activated: false,
            },
        );
        (ctx, host_v1_id, guest_id)
    }

    fn msg_done_serial(queue: &[(Vec<u8>, Vec<std::os::unix::io::RawFd>)], idx: usize) -> u32 {
        let payload = &queue[idx].0[8..];
        u32::from_ne_bytes(payload[0..4].try_into().unwrap())
    }

    #[test]
    fn on_preedit_string_sends_v3_preedit_and_done() {
        let (mut ctx, host_v1_id, guest_id) = setup_v1_ctx();
        ctx.last_sender_id = host_v1_id;

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.commit_serial = 42;
        }

        let mut handler = TextInputV1Handler;
        let text = "こんにちは".to_string();
        let action = handler.on_preedit_string(&mut ctx, 0, &text, &String::new());
        assert_eq!(action, Action::Drop);

        // Should produce exactly 2 messages: preedit_string (opcode 2) + done (opcode 5)
        assert_eq!(ctx.host_to_client_queue.len(), 2);

        assert_eq!(msg_sender(&ctx.host_to_client_queue, 0), guest_id);
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 0), 2); // preedit_string

        assert_eq!(msg_sender(&ctx.host_to_client_queue, 1), guest_id);
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 1), 5); // done
        assert_eq!(msg_done_serial(&ctx.host_to_client_queue, 1), 42); // serial
    }

    #[test]
    fn on_commit_string_sends_preedit_clear_then_commit_then_done() {
        let (mut ctx, host_v1_id, guest_id) = setup_v1_ctx();
        ctx.last_sender_id = host_v1_id;

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.commit_serial = 42;
        }

        let mut handler = TextInputV1Handler;
        let text = "確定".to_string();
        let action = handler.on_commit_string(&mut ctx, 0, &text);
        assert_eq!(action, Action::Drop);

        // Should produce 3 messages: preedit_string("") + commit_string + done
        assert_eq!(ctx.host_to_client_queue.len(), 3);

        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 0), 2); // preedit_string (clear)
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 1), 3); // commit_string
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 2), 5); // done
        assert_eq!(msg_done_serial(&ctx.host_to_client_queue, 2), 42); // serial

        // All messages should target the guest_id
        for i in 0..3 {
            assert_eq!(msg_sender(&ctx.host_to_client_queue, i), guest_id);
        }
    }

    #[test]
    fn on_delete_surrounding_text_negative_index_spanning_cursor() {
        let (mut ctx, host_v1_id, guest_id) = setup_v1_ctx();
        ctx.last_sender_id = host_v1_id;

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.commit_serial = 42;
        }

        let mut handler = TextInputV1Handler;
        let action = handler.on_delete_surrounding_text(&mut ctx, -3, 5);
        assert_eq!(action, Action::Drop);

        // Should produce 2 messages: delete_surrounding_text + done
        assert_eq!(ctx.host_to_client_queue.len(), 2);

        assert_eq!(msg_sender(&ctx.host_to_client_queue, 0), guest_id);
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 0), 4); // delete_surrounding_text

        // Parse payload: before_length (u32), after_length (u32)
        let payload = &ctx.host_to_client_queue[0].0[8..];
        let before_length = u32::from_ne_bytes(payload[0..4].try_into().unwrap());
        let after_length = u32::from_ne_bytes(payload[4..8].try_into().unwrap());
        assert_eq!(before_length, 3);
        assert_eq!(after_length, 2);

        assert_eq!(msg_sender(&ctx.host_to_client_queue, 1), guest_id);
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 1), 5); // done
        assert_eq!(msg_done_serial(&ctx.host_to_client_queue, 1), 42); // serial
    }

    #[test]
    fn on_delete_surrounding_text_entirely_before_cursor() {
        let (mut ctx, host_v1_id, _guest_id) = setup_v1_ctx();
        ctx.last_sender_id = host_v1_id;

        let mut handler = TextInputV1Handler;
        let action = handler.on_delete_surrounding_text(&mut ctx, -5, 3);
        assert_eq!(action, Action::Drop);

        assert_eq!(ctx.host_to_client_queue.len(), 2);
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 0), 4); // delete_surrounding_text

        let payload = &ctx.host_to_client_queue[0].0[8..];
        let before_length = u32::from_ne_bytes(payload[0..4].try_into().unwrap());
        let after_length = u32::from_ne_bytes(payload[4..8].try_into().unwrap());
        assert_eq!(before_length, 5);
        assert_eq!(after_length, 0);
    }

    #[test]
    fn on_set_preedit_region_translates_correctly() {
        let (mut ctx, _host_v1_id, guest_id) = setup_v1_ctx();
        let host_ext_id = 30u32;
        ctx.last_sender_id = host_ext_id;

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.surrounding_text = Some(("가나다".to_string(), 6, 6)); // "가나" is 6 bytes
            state.commit_serial = 42;
        }

        let mut handler = ExtendedTextInputV1Handler;
        let action = handler.on_set_preedit_region(&mut ctx, -3, 3); // "나" (3 bytes)
        assert_eq!(action, Action::Drop);

        // Should produce 3 messages: delete_surrounding_text, preedit_string, done
        assert_eq!(ctx.host_to_client_queue.len(), 3);

        // 1. delete_surrounding_text (opcode 4)
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 0), 4);
        let payload = &ctx.host_to_client_queue[0].0[8..];
        let before_length = u32::from_ne_bytes(payload[0..4].try_into().unwrap());
        let after_length = u32::from_ne_bytes(payload[4..8].try_into().unwrap());
        assert_eq!(before_length, 3);
        assert_eq!(after_length, 0);

        // 2. preedit_string (opcode 2)
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 1), 2);
        let payload = &ctx.host_to_client_queue[1].0[8..];
        let str_len = u32::from_ne_bytes(payload[0..4].try_into().unwrap()) as usize;
        let preedit_str = String::from_utf8(payload[4..4 + str_len - 1].to_vec()).unwrap();
        assert_eq!(preedit_str, "나");

        // 3. done (opcode 5)
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 2), 5);
        assert_eq!(msg_done_serial(&ctx.host_to_client_queue, 2), 42); // serial

        // Cached preedit should be updated
        if let Some(state) = ctx.text_inputs.get(&guest_id) {
            assert_eq!(state.current_preedit, "나");
        } else {
            panic!("state not found");
        }
    }

    #[test]
    fn on_confirm_preedit_commits_cached_preedit() {
        let (mut ctx, _host_v1_id, guest_id) = setup_v1_ctx();
        let host_ext_id = 30u32;
        ctx.last_sender_id = host_ext_id;

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.current_preedit = "나".to_string();
            state.commit_serial = 42;
        }

        let mut handler = ExtendedTextInputV1Handler;
        let action = handler.on_confirm_preedit(&mut ctx, 0);
        assert_eq!(action, Action::Drop);

        // Should produce 2 messages: commit_string, done
        assert_eq!(ctx.host_to_client_queue.len(), 2);

        // 1. commit_string (opcode 3)
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 0), 3);
        let payload = &ctx.host_to_client_queue[0].0[8..];
        let str_len = u32::from_ne_bytes(payload[0..4].try_into().unwrap()) as usize;
        let commit_str = String::from_utf8(payload[4..4 + str_len - 1].to_vec()).unwrap();
        assert_eq!(commit_str, "나");

        // 2. done (opcode 5)
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 1), 5);
        assert_eq!(msg_done_serial(&ctx.host_to_client_queue, 1), 42); // serial

        // Cached preedit should be cleared
        if let Some(state) = ctx.text_inputs.get(&guest_id) {
            assert_eq!(state.current_preedit, "");
        } else {
            panic!("state not found");
        }
    }

    #[test]
    fn host_serial_updated_and_propagated_to_commit_state() {
        let (mut ctx, host_v1_id, guest_id) = setup_v1_ctx();
        ctx.last_sender_id = host_v1_id;

        let mut handler = TextInputV1Handler;

        // 1. Send preedit_string from host with serial 99
        let text = "あ".to_string();
        let action = handler.on_preedit_string(&mut ctx, 99, &text, &String::new());
        assert_eq!(action, Action::Drop);

        // State should store host_serial = 99
        if let Some(state) = ctx.text_inputs.get(&guest_id) {
            assert_eq!(state.host_serial, 99);
        } else {
            panic!("state not found");
        }

        // 2. Client calls on_commit
        ctx.last_sender_id = guest_id;
        let mut v3_handler = TextInputV3Handler;
        let action = v3_handler.on_commit(&mut ctx);
        assert_eq!(action, Action::Drop);

        // Find the client_to_host_queue messages
        // Opcode 9 is commit_state. The serial should be 99.
        let mut found_commit_state = false;
        for (msg, _) in &ctx.client_to_host_queue {
            let opcode = u32::from_ne_bytes(msg[4..8].try_into().unwrap()) & 0xffff;
            if opcode == 9 {
                let payload = &msg[8..];
                let serial = u32::from_ne_bytes(payload[0..4].try_into().unwrap());
                assert_eq!(serial, 99);
                found_commit_state = true;
            }
        }
        assert!(found_commit_state);
    }

    #[test]
    fn on_keysym_forwards_serial_and_time_to_wl_keyboard() {
        let (mut ctx, host_v1_id, _guest_id) = setup_v1_ctx();
        ctx.last_sender_id = host_v1_id;

        // Register a guest wl_keyboard ID to capture the forwarded key
        let guest_keyboard_id = 999u32;
        ctx.shadow_table.map_id(guest_keyboard_id, 888);
        ctx.shadow_table.track_interface(guest_keyboard_id, "wl_keyboard".to_string());

        let mut handler = TextInputV1Handler;
        // 0xff08 is KEY_BackSpace
        let action = handler.on_keysym(&mut ctx, 123, 456, 0xff08, 1, 0);
        assert_eq!(action, Action::Drop);

        // State should store host_serial = 123
        if let Some(state) = ctx.text_inputs.values().next() {
            assert_eq!(state.host_serial, 123);
        } else {
            panic!("state not found");
        }

        // Should produce 1 message on host_to_client_queue: wl_keyboard::key (opcode 3)
        assert_eq!(ctx.host_to_client_queue.len(), 1);
        assert_eq!(msg_opcode(&ctx.host_to_client_queue, 0), 3);
        assert_eq!(msg_sender(&ctx.host_to_client_queue, 0), guest_keyboard_id);

        let payload = &ctx.host_to_client_queue[0].0[8..];
        let serial = u32::from_ne_bytes(payload[0..4].try_into().unwrap());
        let time = u32::from_ne_bytes(payload[4..8].try_into().unwrap());
        assert_eq!(serial, 123);
        assert_eq!(time, 456);
    }

    #[test]
    fn test_activation_state_machine() {
        let (mut ctx, _host_v1_id, guest_id) = setup_v1_ctx();

        // 1. Initially enabled is true (from setup_v1_ctx), active_surface is None.
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.enabled = true;
            state.host_activated = false;
        }

        // Guest calls commit before focus (active_surface is None).
        ctx.last_sender_id = guest_id;
        let mut v3_handler = TextInputV3Handler;
        let action = v3_handler.on_commit(&mut ctx);
        assert_eq!(action, Action::Drop);

        // Should NOT send activate (opcode 0) to host because active_surface is None.
        let mut found_activate = false;
        for (msg, _) in &ctx.client_to_host_queue {
            let opcode = u32::from_ne_bytes(msg[4..8].try_into().unwrap()) & 0xffff;
            if opcode == 0 {
                found_activate = true;
            }
        }
        assert!(!found_activate, "Should not send activate when active_surface is None");

        // 2. Keyboard enter is received from host. Set active_surface to a mock guest surface ID (1234).
        let guest_surface = 1234u32;
        let host_surface = 5678u32;
        ctx.shadow_table.map_id(guest_surface, host_surface);

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.active_surface = Some(guest_surface);
        }
        update_host_activation(&mut ctx, guest_id);

        // Now it should have sent activate (opcode 0) with surface host_surface (5678).
        let mut found_activate = false;
        let mut activate_surface = 0u32;
        for (msg, _) in &ctx.client_to_host_queue {
            let opcode = u32::from_ne_bytes(msg[4..8].try_into().unwrap()) & 0xffff;
            if opcode == 0 {
                found_activate = true;
                let payload = &msg[8..];
                // activate(seat, surface) -> seat is first u32, surface is second u32 (offset 4)
                activate_surface = u32::from_ne_bytes(payload[4..8].try_into().unwrap());
            }
        }
        assert!(found_activate, "Should send activate when focused");
        assert_eq!(activate_surface, host_surface);

        // Clear the queue to check next transition.
        ctx.client_to_host_queue.clear();

        // 3. Keyboard leave is received (active_surface is None).
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.active_surface = None;
        }
        update_host_activation(&mut ctx, guest_id);

        // Now it should have sent deactivate (opcode 1).
        let mut found_deactivate = false;
        for (msg, _) in &ctx.client_to_host_queue {
            let opcode = u32::from_ne_bytes(msg[4..8].try_into().unwrap()) & 0xffff;
            if opcode == 1 {
                found_deactivate = true;
            }
        }
        assert!(found_deactivate, "Should send deactivate when focus is lost");
    }

    #[test]
    fn test_cjk_backspace_holding() {
        let (mut ctx, host_v1_id, guest_id) = setup_v1_ctx();
        ctx.last_sender_id = host_v1_id;

        // Register a guest wl_keyboard ID to capture the forwarded key
        let guest_keyboard_id = 999u32;
        ctx.shadow_table.map_id(guest_keyboard_id, 888);
        ctx.shadow_table.track_interface(guest_keyboard_id, "wl_keyboard".to_string());

        let mut v1_handler = TextInputV1Handler;
        let mut v3_handler = TextInputV3Handler;

        // 1. Focus is enter.
        let guest_surface = 1234u32;
        let host_surface = 5678u32;
        ctx.shadow_table.map_id(guest_surface, host_surface);
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.active_surface = Some(guest_surface);
        }
        update_host_activation(&mut ctx, guest_id);

        // 2. Client types "가나다라" and commits it.
        ctx.last_sender_id = guest_id;
        v3_handler.on_set_surrounding_text(&mut ctx, &"가나다라".to_string(), 12, 12);
        v3_handler.on_commit(&mut ctx);

        // Verify state has UTF-8 byte len 12
        if let Some(state) = ctx.text_inputs.get(&guest_id) {
            assert_eq!(state.surrounding_text.as_ref().unwrap().0, "가나다라");
            assert_eq!(state.surrounding_text.as_ref().unwrap().1, 12);
        } else {
            panic!("state not found");
        }

        // 3. User holds Backspace. We simulate multiple backspace repeat events.
        let mut text = "가나다라".to_string();
        
        for expected_len in (0..4).rev() {
            ctx.last_sender_id = host_v1_id;
            ctx.host_to_client_queue.clear();

            // 0xff08 is KEY_BackSpace. Simulate key press (state=1)
            v1_handler.on_keysym(&mut ctx, 100 + expected_len, 200 + expected_len, 0xff08, 1, 0);

            // Verify a synthetic backspace pressed key event is forwarded
            assert_eq!(ctx.host_to_client_queue.len(), 1);
            assert_eq!(msg_opcode(&ctx.host_to_client_queue, 0), 3); // wl_keyboard::key
            assert_eq!(msg_sender(&ctx.host_to_client_queue, 0), guest_keyboard_id);

            // Simulating client-side buffer deletion
            let popped = text.pop();
            assert!(popped.is_some());

            // Client sets surrounding text and commits
            ctx.last_sender_id = guest_id;
            v3_handler.on_set_surrounding_text(&mut ctx, &text, text.len() as i32, text.len() as i32);
            v3_handler.on_commit(&mut ctx);

            // Verify the surrounding text updated in state
            if let Some(state) = ctx.text_inputs.get(&guest_id) {
                assert_eq!(state.surrounding_text.as_ref().unwrap().0, text);
                assert_eq!(state.surrounding_text.as_ref().unwrap().1, text.len() as i32);
            }
        }

        // Final verification: buffer is completely empty
        assert_eq!(text, "");
        if let Some(state) = ctx.text_inputs.get(&guest_id) {
            assert_eq!(state.surrounding_text.as_ref().unwrap().0, "");
            assert_eq!(state.surrounding_text.as_ref().unwrap().1, 0);
        }
    }
}


