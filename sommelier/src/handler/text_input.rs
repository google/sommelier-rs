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
use std::os::unix::io::RawFd;

fn push_msg(
    queue: &mut Vec<(Vec<u8>, Vec<RawFd>)>,
    sender_id: u32,
    opcode: u16,
    builder: MessageBuilder,
) {
    queue.push((builder.build_message(sender_id, opcode), Vec::new()));
}

fn with_state<F>(ctx: &mut Context, guest_id: u32, f: F) -> u32
where
    F: FnOnce(&mut crate::state::TextInputState),
{
    match ctx.text_inputs.get_mut(&guest_id) {
        Some(state) => {
            f(state);
            let serial = state.done_serial;
            state.done_serial = state.done_serial.wrapping_add(1).max(1);
            serial
        }
        None => 0,
    }
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
        commit: &String,
    ) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            let done_serial = with_state(ctx, guest_id, |s| {
                s.host_serial = serial;
                s.current_preedit = text.clone();
            });

            log::info!(
                ">>> on_preedit_string: serial={}, text={:?}, commit={:?}, guest_id={}, done_serial={}",
                serial, text, commit, guest_id, done_serial
            );

            // TODO: Korean IME drops intermediate syllables in continuous input
            // (e.g., "가나다라마바사" → "가다마사"). The v1 `commit` parameter changes with
            // every keystroke, but same-syllable composition and syllable transition are
            // indistinguishable at the protocol level. v3 has no `commit` parameter or
            // implicit commit mechanism, so these events are lost in translation.

            // v3 preedit_string (opcode 2): cursor_end = text.len() selects entire preedit.
            let mut builder = MessageBuilder::new();
            builder.write_string(text);
            builder.write_i32(0);
            builder.write_i32(text.len() as i32);
            push_msg(&mut ctx.host_to_client_queue, guest_id, 2, builder);

            // v3 done (opcode 5): signals the guest that the preedit update is complete.
            log::info!("  -> sending v3 preedit_string({:?}) + done({})", text, done_serial);
            let mut builder = MessageBuilder::new();
            builder.write_u32(done_serial);
            push_msg(&mut ctx.host_to_client_queue, guest_id, 5, builder);
        }
        Action::Drop
    }

    fn on_commit_string(&mut self, ctx: &mut Context, serial: u32, text: &String) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            let done_serial = with_state(ctx, guest_id, |s| {
                s.host_serial = serial;
                s.current_preedit.clear();
            });

            log::info!(
                ">>> on_commit_string: serial={}, text={:?}, guest_id={}, done_serial={}",
                serial, text, guest_id, done_serial
            );
            log::info!("  -> sending v3 preedit_string(\"\") + commit_string({:?}) + done({})", text, done_serial);

            // Explicitly clear preedit before commit (v3 preedit_string "").
            // Without this, the guest may keep stale underline after commit.
            let mut builder = MessageBuilder::new();
            builder.write_string("");
            builder.write_i32(0);
            builder.write_i32(0);
            push_msg(&mut ctx.host_to_client_queue, guest_id, 2, builder);

            // v3 commit_string (opcode 3).
            let mut builder = MessageBuilder::new();
            builder.write_string(text);
            push_msg(&mut ctx.host_to_client_queue, guest_id, 3, builder);

            // v3 done (opcode 5) with serial.
            let mut builder = MessageBuilder::new();
            builder.write_u32(done_serial);
            push_msg(&mut ctx.host_to_client_queue, guest_id, 5, builder);
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

        let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
        let found_keycode =
            xkbcommon::xkb::Keymap::new_from_names(
                &context,
                "",
                "",
                "",
                "",
                None,
                xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
            .and_then(|keymap| {
                for keycode_raw in keymap.min_keycode().raw()..=keymap.max_keycode().raw() {
                    let keycode = keycode_raw.into();
                    let syms = keymap.key_get_syms_by_level(keycode, 0, 0);
                    if syms.iter().any(|s| s.raw() == sym) {
                        return Some(keycode_raw - 8);
                    }
                }
                None
            });

        // Forward the key event to the guest wl_keyboard with the real serial/time.
        // Previously hardcoded 0 for both, which caused ChromeOS/Exo to reject events.
        if let Some(keycode) = found_keycode {
            let keyboards = ctx.shadow_table.find_by_interface("wl_keyboard");
            if let Some(&keyboard_id) = keyboards.first() {
                let mut builder = MessageBuilder::new();
                builder.write_u32(serial);
                builder.write_u32(time);
                builder.write_u32(keycode);
                builder.write_u32(state);
                push_msg(&mut ctx.host_to_client_queue, keyboard_id, 3, builder);
            }
        }
        Action::Drop
    }

    fn on_enter(&mut self, _ctx: &mut Context, surface: u32) -> Action {
        log::info!(">>> on_enter: surface={}", surface);
        Action::Drop
    }

    fn on_leave(&mut self, _ctx: &mut Context) -> Action {
        log::info!(">>> on_leave");
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

    fn on_delete_surrounding_text(&mut self, ctx: &mut Context, index: i32, length: u32) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            let done_serial = ctx.text_inputs.get_mut(&guest_id).map_or(0, |s| {
                let serial = s.done_serial;
                s.done_serial = s.done_serial.wrapping_add(1).max(1);
                serial
            });

            // Convert v1's (index, length) to v3's (before_length, after_length).
            // v1: delete `length` bytes starting at cursor + index.
            // v3: delete `before_length` bytes before cursor, `after_length` after cursor.
            // use i64 to avoid i32::MIN overflow when negating.
            let (before_length, after_length) = {
                let start = index as i64;
                let end = start + length as i64;
                let before = if start < 0 { (-start).min(length as i64) as u32 } else { 0 };
                let after = if end > 0 { end as u32 } else { 0 };
                (before, after)
            };

            // v1 delete_surrounding_text (opcode 4): before_length, after_length.
            let mut builder = MessageBuilder::new();
            builder.write_u32(before_length);
            builder.write_u32(after_length);
            push_msg(&mut ctx.host_to_client_queue, guest_id, 4, builder);

            // v3 done (opcode 5) to commit the delete.
            let mut builder = MessageBuilder::new();
            builder.write_u32(done_serial);
            push_msg(&mut ctx.host_to_client_queue, guest_id, 5, builder);
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

        if let Some((&guest_id, state)) = ctx
            .text_inputs
            .iter_mut()
            .find(|(_, s)| s.host_ext_id == host_ext_id)
        {
            let done_serial = {
                let serial = state.done_serial;
                state.done_serial = state.done_serial.wrapping_add(1).max(1);
                serial
            };
            if let Some((text, cursor, _anchor)) = state.surrounding_text.as_ref() {
                let cursor_i64 = *cursor as i64;
                let index_i64 = index as i64;
                let start_idx = cursor_i64 + index_i64;
                let length_i64 = length as i64;

                if start_idx >= 0
                    && start_idx + length_i64 <= text.len() as i64
                    && text.is_char_boundary(start_idx as usize)
                    && text.is_char_boundary((start_idx + length_i64) as usize)
                {
                    let preedit_text =
                        text[start_idx as usize..(start_idx + length_i64) as usize].to_string();
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

                    let mut builder = MessageBuilder::new();
                    builder.write_u32(before_length);
                    builder.write_u32(after_length);
                    push_msg(&mut ctx.host_to_client_queue, guest_id, 4, builder);

                    let mut builder = MessageBuilder::new();
                    builder.write_string(&preedit_text);
                    builder.write_i32(0);
                    builder.write_i32(preedit_text.len() as i32);
                    push_msg(&mut ctx.host_to_client_queue, guest_id, 2, builder);

                    let mut builder = MessageBuilder::new();
                    builder.write_u32(done_serial);
                    push_msg(&mut ctx.host_to_client_queue, guest_id, 5, builder);
                } else {
                    log::warn!(
                        "on_set_preedit_region: calculated range [{}, {}] is out of bounds or invalid for text of length {}",
                        start_idx, start_idx + length_i64, text.len()
                    );
                }
            }
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
    fn on_confirm_preedit(&mut self, ctx: &mut Context, _selection_behavior: u32) -> Action {
        let host_ext_id = ctx.last_sender_id;
        if let Some((&guest_id, state)) = ctx
            .text_inputs
            .iter_mut()
            .find(|(_, s)| s.host_ext_id == host_ext_id)
        {
            let preedit_text = state.current_preedit.clone();
            let done_serial = {
                let serial = state.done_serial;
                state.done_serial = state.done_serial.wrapping_add(1).max(1);
                serial
            };
            state.current_preedit.clear();

            if !preedit_text.is_empty() {
                let mut builder = MessageBuilder::new();
                builder.write_string(&preedit_text);
                push_msg(&mut ctx.host_to_client_queue, guest_id, 3, builder);

                let mut builder = MessageBuilder::new();
                builder.write_u32(done_serial);
                push_msg(&mut ctx.host_to_client_queue, guest_id, 5, builder);
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
            let mut builder = MessageBuilder::new();
            builder.write_u32(host_v1_id);
            push_msg(&mut ctx.client_to_host_queue, host_manager_id, 0, builder);
        }

        if let Some(host_ext_manager_id) = ctx.host_text_input_extension_v1_id {
            let mut builder = MessageBuilder::new();
            builder.write_u32(host_ext_id);
            builder.write_u32(host_v1_id);
            push_msg(
                &mut ctx.client_to_host_queue,
                host_ext_manager_id,
                0,
                builder,
            );
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
                surrounding_text: None,
                surrounding_text_dirty: false,
                content_hint: 0,
                content_purpose: 0,
                cursor_rect: None,
                text_change_cause: 0,
                current_preedit: String::new(),
                done_serial: 1,
                host_serial: 0,
                host_activated: false,
            },
        );

        Action::Drop
    }
}

pub(crate) fn update_host_activation(ctx: &mut Context, guest_id: u32) {
    if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
        let host_seat = ctx.shadow_table.get_host_id(state.guest_seat).unwrap_or(0);
        let host_surface = state
            .active_surface
            .and_then(|s| ctx.shadow_table.get_host_id(s))
            .unwrap_or(0);
        let target_activated = state.enabled && host_surface != 0;

        if target_activated == state.host_activated {
            return;
        }
        state.host_activated = target_activated;

        if target_activated {
            log::info!(
                "update_host_activation: activating text input v1 (guest_id={}, host_v1_id={}, host_surface={})",
                guest_id,
                state.host_v1_id,
                host_surface,
            );
            let mut builder = MessageBuilder::new();
            builder.write_u32(host_seat);
            builder.write_u32(host_surface);
            push_msg(&mut ctx.client_to_host_queue, state.host_v1_id, 0, builder);
        } else {
            log::info!(
                "update_host_activation: deactivating text input v1 (guest_id={}, host_v1_id={})",
                guest_id,
                state.host_v1_id
            );
            let mut builder = MessageBuilder::new();
            builder.write_u32(host_seat);
            push_msg(&mut ctx.client_to_host_queue, state.host_v1_id, 1, builder);
        }
    }
}

pub struct TextInputV3Handler;
impl zwp_text_input_v3::ZwpTextInputV3Handler for TextInputV3Handler {
    fn on_enable(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;
        log::info!(">>> v3 on_enable: guest_id={}", guest_id);
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.enabled = true;
        }
        Action::Drop
    }

    fn on_disable(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;
        log::info!(">>> v3 on_disable: guest_id={}", guest_id);
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.enabled = false;
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

    fn on_commit(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;
        log::info!(">>> v3 on_commit: guest_id={}", guest_id);

        update_host_activation(ctx, guest_id);

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            if state.surrounding_text_dirty {
                state.surrounding_text_dirty = false;
                if let Some((text, cursor, anchor)) = &state.surrounding_text {
                    let mut builder = MessageBuilder::new();
                    builder.write_string(text);
                    builder.write_u32(*cursor as u32);
                    builder.write_u32(*anchor as u32);
                    push_msg(&mut ctx.client_to_host_queue, state.host_v1_id, 5, builder);
                }
            }

            if state.content_hint != 0 || state.content_purpose != 0 {
                let hint = state.content_hint;
                let purpose = state.content_purpose;
                state.content_hint = 0;
                state.content_purpose = 0;

                let mut builder = MessageBuilder::new();
                builder.write_u32(hint);
                builder.write_u32(purpose);
                push_msg(&mut ctx.client_to_host_queue, state.host_v1_id, 6, builder);

                let input_type = match purpose {
                    0 | 1 | 7 => 1,
                    2 | 3 => 2,
                    4 => 3,
                    5 => 4,
                    6 => 5,
                    8 => 6,
                    _ => 1,
                };
                let mut builder = MessageBuilder::new();
                builder.write_u32(input_type);
                builder.write_u32(0);
                builder.write_u32(0);
                builder.write_u32(0);
                builder.write_u32(0);
                push_msg(&mut ctx.client_to_host_queue, state.host_ext_id, 6, builder);
            }

            if let Some((x, y, w, h)) = state.cursor_rect.take() {
                let mut builder = MessageBuilder::new();
                builder.write_i32(x);
                builder.write_i32(y);
                builder.write_i32(w);
                builder.write_i32(h);
                push_msg(&mut ctx.client_to_host_queue, state.host_v1_id, 7, builder);
            }

            let mut builder = MessageBuilder::new();
            builder.write_u32(state.host_serial);
            push_msg(&mut ctx.client_to_host_queue, state.host_v1_id, 9, builder);
        }

        Action::Drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::text_input_extension_unstable_v1::zcr_extended_text_input_v1::ZcrExtendedTextInputV1Handler;
    use crate::protocols::text_input_unstable_v1::zwp_text_input_v1::ZwpTextInputV1Handler;
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
                surrounding_text: None,
                surrounding_text_dirty: false,
                content_hint: 0,
                content_purpose: 0,
                cursor_rect: None,
                text_change_cause: 0,
                current_preedit: String::new(),
                done_serial: 1,
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
            state.done_serial = 42;
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
            state.done_serial = 42;
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
            state.done_serial = 42;
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
        assert_eq!(before_length, 3);
        assert_eq!(after_length, 0);
    }

    #[test]
    fn on_set_preedit_region_translates_correctly() {
        let (mut ctx, _host_v1_id, guest_id) = setup_v1_ctx();
        let host_ext_id = 30u32;
        ctx.last_sender_id = host_ext_id;

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.surrounding_text = Some(("가나다".to_string(), 6, 6)); // "가나" is 6 bytes
            state.done_serial = 42;
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
            state.done_serial = 42;
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
        ctx.shadow_table
            .track_interface(guest_keyboard_id, "wl_keyboard".to_string());

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
        assert!(
            !found_activate,
            "Should not send activate when active_surface is None"
        );

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
        assert!(
            found_deactivate,
            "Should send deactivate when focus is lost"
        );
    }

    #[test]
    fn test_cjk_backspace_holding() {
        let (mut ctx, host_v1_id, guest_id) = setup_v1_ctx();
        ctx.last_sender_id = host_v1_id;

        // Register a guest wl_keyboard ID to capture the forwarded key
        let guest_keyboard_id = 999u32;
        ctx.shadow_table.map_id(guest_keyboard_id, 888);
        ctx.shadow_table
            .track_interface(guest_keyboard_id, "wl_keyboard".to_string());

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
            v1_handler.on_keysym(
                &mut ctx,
                100 + expected_len,
                200 + expected_len,
                0xff08,
                1,
                0,
            );

            // Verify a synthetic backspace pressed key event is forwarded
            assert_eq!(ctx.host_to_client_queue.len(), 1);
            assert_eq!(msg_opcode(&ctx.host_to_client_queue, 0), 3); // wl_keyboard::key
            assert_eq!(msg_sender(&ctx.host_to_client_queue, 0), guest_keyboard_id);

            // Simulating client-side buffer deletion
            let popped = text.pop();
            assert!(popped.is_some());

            // Client sets surrounding text and commits
            ctx.last_sender_id = guest_id;
            v3_handler.on_set_surrounding_text(
                &mut ctx,
                &text,
                text.len() as i32,
                text.len() as i32,
            );
            v3_handler.on_commit(&mut ctx);

            // Verify the surrounding text updated in state
            if let Some(state) = ctx.text_inputs.get(&guest_id) {
                assert_eq!(state.surrounding_text.as_ref().unwrap().0, text);
                assert_eq!(
                    state.surrounding_text.as_ref().unwrap().1,
                    text.len() as i32
                );
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
