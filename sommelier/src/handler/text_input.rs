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
use crate::wire::Action;

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
            let commit_serial = if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
                state.host_serial = serial;
                state.current_preedit = text.clone();
                state.commit_serial
            } else {
                0
            };

            // v3 preedit_string (opcode 2)
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_string(text);
            builder.write_i32(0); // cursor_begin
            builder.write_i32(text.len() as i32); // cursor_end (end of string)

            let mut msg = Vec::new();
            msg.extend_from_slice(&guest_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 2u32;
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);
            ctx.host_to_client_queue.push((msg, Vec::new()));

            // v3 done (opcode 5)
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_u32(commit_serial); // serial

            let mut msg = Vec::new();
            msg.extend_from_slice(&guest_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 5u32;
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);
            ctx.host_to_client_queue.push((msg, Vec::new()));
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

            // v3 preedit_string (opcode 2) - explicitly clear preedit before commit
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_string("");
            builder.write_i32(0); // cursor_begin
            builder.write_i32(0); // cursor_end

            let mut msg = Vec::new();
            msg.extend_from_slice(&guest_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 2u32;
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);
            ctx.host_to_client_queue.push((msg, Vec::new()));

            // v3 commit_string (opcode 3)
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_string(text);

            let mut msg = Vec::new();
            msg.extend_from_slice(&guest_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 3u32;
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);
            ctx.host_to_client_queue.push((msg, Vec::new()));

            // v3 done (opcode 5)
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_u32(commit_serial); // serial

            let mut msg = Vec::new();
            msg.extend_from_slice(&guest_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 5u32;
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);
            ctx.host_to_client_queue.push((msg, Vec::new()));
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
        if let Some(keymap) = xkbcommon::xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            "",
            "",
            None,
            xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) {
            let mut found_keycode = None;
            for keycode_raw in keymap.min_keycode().raw()..=keymap.max_keycode().raw() {
                let keycode = keycode_raw.into();
                let syms = keymap.key_get_syms_by_level(keycode, 0, 0);
                if syms.iter().any(|s| s.raw() == sym) {
                    found_keycode = Some(keycode_raw - 8);
                    break;
                }
            }

            if let Some(keycode) = found_keycode {
                let keyboards = ctx.shadow_table.find_by_interface("wl_keyboard");
                if let Some(&keyboard_id) = keyboards.first() {
                    // Send wl_keyboard::key (opcode 3)
                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_u32(serial); // serial
                    builder.write_u32(time); // time
                    builder.write_u32(keycode); // key
                    builder.write_u32(state); // state (0: released, 1: pressed)

                    let mut msg = Vec::new();
                    msg.extend_from_slice(&keyboard_id.to_ne_bytes());
                    let len = (builder.payload.len() + 8) as u32;
                    let word2 = (len << 16) | 3u32;
                    msg.extend_from_slice(&word2.to_ne_bytes());
                    msg.extend_from_slice(&builder.payload);
                    ctx.host_to_client_queue.push((msg, Vec::new()));
                }
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

            let length_i32 = i32::try_from(length).unwrap_or_else(|_| {
                log::warn!(
                    "on_delete_surrounding_text: length {} exceeds i32::MAX, clamping",
                    length
                );
                i32::MAX
            });

            // Safety: negate via i64 to avoid i32::MIN overflow.
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

            // v3 delete_surrounding_text (opcode 4)
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_u32(before_length);
            builder.write_u32(after_length);

            let mut msg = Vec::new();
            msg.extend_from_slice(&guest_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 4u32;
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);
            ctx.host_to_client_queue.push((msg, Vec::new()));

            // v3 done (opcode 5)
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_u32(commit_serial); // serial

            let mut msg = Vec::new();
            msg.extend_from_slice(&guest_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 5u32;
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);
            ctx.host_to_client_queue.push((msg, Vec::new()));
        }
        Action::Drop
    }

    fn on_language(&mut self, ctx: &mut Context, serial: u32, _language: &String) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            if let Some(s) = ctx.text_inputs.get_mut(&guest_id) {
                s.host_serial = serial;
            }
        }
        Action::Drop
    }

    fn on_text_direction(&mut self, ctx: &mut Context, serial: u32, _direction: u32) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
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
    fn on_set_preedit_region(&mut self, ctx: &mut Context, index: i32, length: u32) -> Action {
        let host_ext_id = ctx.last_sender_id;
        if let Some((&guest_id, state)) = ctx
            .text_inputs
            .iter_mut()
            .find(|(_, s)| s.host_ext_id == host_ext_id)
        {
            let commit_serial = state.commit_serial;
            if let Some((text, cursor, _anchor)) = &state.surrounding_text {
                let cursor_i64 = *cursor as i64;
                let index_i64 = index as i64;
                let start_idx = cursor_i64 + index_i64;
                let length_i64 = length as i64;

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

                    // v3 delete_surrounding_text (opcode 4)
                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_u32(before_length);
                    builder.write_u32(after_length);

                    let mut msg = Vec::new();
                    msg.extend_from_slice(&guest_id.to_ne_bytes());
                    let len = (builder.payload.len() + 8) as u32;
                    let word2 = (len << 16) | 4u32;
                    msg.extend_from_slice(&word2.to_ne_bytes());
                    msg.extend_from_slice(&builder.payload);
                    ctx.host_to_client_queue.push((msg, Vec::new()));

                    // v3 preedit_string (opcode 2)
                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_string(&preedit_text);
                    builder.write_i32(0); // cursor_begin
                    builder.write_i32(preedit_text.len() as i32); // cursor_end

                    let mut msg = Vec::new();
                    msg.extend_from_slice(&guest_id.to_ne_bytes());
                    let len = (builder.payload.len() + 8) as u32;
                    let word2 = (len << 16) | 2u32;
                    msg.extend_from_slice(&word2.to_ne_bytes());
                    msg.extend_from_slice(&builder.payload);
                    ctx.host_to_client_queue.push((msg, Vec::new()));

                    // v3 done (opcode 5)
                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_u32(commit_serial); // serial

                    let mut msg = Vec::new();
                    msg.extend_from_slice(&guest_id.to_ne_bytes());
                    let len = (builder.payload.len() + 8) as u32;
                    let word2 = (len << 16) | 5u32;
                    msg.extend_from_slice(&word2.to_ne_bytes());
                    msg.extend_from_slice(&builder.payload);
                    ctx.host_to_client_queue.push((msg, Vec::new()));
                } else {
                    log::warn!(
                        "on_set_preedit_region: calculated range [{}, {}] is out of bounds or invalid for text of length {}",
                        start_idx, start_idx + length_i64, text.len()
                    );
                }
            } else {
                log::warn!("on_set_preedit_region: no cached surrounding text available");
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
            let commit_serial = state.commit_serial;
            let preedit_text = state.current_preedit.clone();
            if !preedit_text.is_empty() {
                // v3 commit_string (opcode 3)
                let mut builder = crate::wire::MessageBuilder::new();
                builder.write_string(&preedit_text);

                let mut msg = Vec::new();
                msg.extend_from_slice(&guest_id.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = (len << 16) | 3u32;
                msg.extend_from_slice(&word2.to_ne_bytes());
                msg.extend_from_slice(&builder.payload);
                ctx.host_to_client_queue.push((msg, Vec::new()));

                state.current_preedit.clear();

                // v3 done (opcode 5)
                let mut builder = crate::wire::MessageBuilder::new();
                builder.write_u32(commit_serial); // serial

                let mut msg = Vec::new();
                msg.extend_from_slice(&guest_id.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = (len << 16) | 5u32;
                msg.extend_from_slice(&word2.to_ne_bytes());
                msg.extend_from_slice(&builder.payload);
                ctx.host_to_client_queue.push((msg, Vec::new()));
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
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_u32(host_v1_id);

            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&host_manager_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = len << 16; // opcode 0: create_text_input
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);

            ctx.client_to_host_queue.push((full_msg, Vec::new()));
        }

        if let Some(host_ext_manager_id) = ctx.host_text_input_extension_v1_id {
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_u32(host_ext_id);
            builder.write_u32(host_v1_id);

            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&host_ext_manager_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = len << 16; // opcode 0: get_extended_text_input
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);

            ctx.client_to_host_queue.push((full_msg, Vec::new()));
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
            },
        );

        Action::Drop
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

    fn on_commit(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.commit_serial += 1;
            let host_v1_id = state.host_v1_id;
            let host_seat = ctx.shadow_table.get_host_id(state.guest_seat).unwrap_or(0);
            let host_surface = state
                .active_surface
                .and_then(|s| ctx.shadow_table.get_host_id(s))
                .unwrap_or(0);

            if state.enabled_changed {
                if state.enabled {
                    // activate: opcode 0
                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_u32(host_seat);
                    builder.write_u32(host_surface);

                    let mut full_msg = Vec::new();
                    full_msg.extend_from_slice(&host_v1_id.to_ne_bytes());
                    let len = (builder.payload.len() + 8) as u32;
                    let word2 = len << 16;
                    full_msg.extend_from_slice(&word2.to_ne_bytes());
                    full_msg.extend_from_slice(&builder.payload);
                    ctx.client_to_host_queue.push((full_msg, Vec::new()));
                } else {
                    // deactivate: opcode 1
                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_u32(host_seat);

                    let mut full_msg = Vec::new();
                    full_msg.extend_from_slice(&host_v1_id.to_ne_bytes());
                    let len = (builder.payload.len() + 8) as u32;
                    let word2 = (len << 16) | 1u32;
                    full_msg.extend_from_slice(&word2.to_ne_bytes());
                    full_msg.extend_from_slice(&builder.payload);
                    ctx.client_to_host_queue.push((full_msg, Vec::new()));
                }
                state.enabled_changed = false;
            }

            if state.surrounding_text_dirty {
                if let Some((text, cursor, anchor)) = &state.surrounding_text {
                    // set_surrounding_text: opcode 5
                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_string(text);
                    builder.write_u32(*cursor as u32);
                    builder.write_u32(*anchor as u32);

                    let mut full_msg = Vec::new();
                    full_msg.extend_from_slice(&host_v1_id.to_ne_bytes());
                    let len = (builder.payload.len() + 8) as u32;
                    let word2 = (len << 16) | 5u32;
                    full_msg.extend_from_slice(&word2.to_ne_bytes());
                    full_msg.extend_from_slice(&builder.payload);
                    ctx.client_to_host_queue.push((full_msg, Vec::new()));
                }
                state.surrounding_text_dirty = false;
            }

            if state.content_hint != 0 || state.content_purpose != 0 {
                // set_content_type: opcode 6 (on zwp_text_input_v1)
                let mut builder = crate::wire::MessageBuilder::new();
                builder.write_u32(state.content_hint);
                builder.write_u32(state.content_purpose);

                let mut full_msg = Vec::new();
                full_msg.extend_from_slice(&host_v1_id.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = (len << 16) | 6u32;
                full_msg.extend_from_slice(&word2.to_ne_bytes());
                full_msg.extend_from_slice(&builder.payload);
                ctx.client_to_host_queue.push((full_msg, Vec::new()));

                // map to zcr_extended_text_input_v1::set_input_type
                // 0: normal->text(1), 1: alpha->text(1), 2: digits->number(2), 3: number->number(2),
                // 4: phone->telephone(3), 5: url->url(4), 6: email->email(5), 7: name->text(1), 8: password->password(6)
                let input_type = match state.content_purpose {
                    0 | 1 | 7 => 1, // TEXT
                    2 | 3 => 2,     // NUMBER
                    4 => 3,         // TELEPHONE
                    5 => 4,         // URL
                    6 => 5,         // EMAIL
                    8 => 6,         // PASSWORD
                    // terminal (9)
                    _ => 1, // TEXT
                };

                let input_mode = 0; // default
                let input_flags = 0;
                let learning_mode = 0;
                let inline_composition_support = 0;

                let mut ext_builder = crate::wire::MessageBuilder::new();
                ext_builder.write_u32(input_type);
                ext_builder.write_u32(input_mode);
                ext_builder.write_u32(input_flags);
                ext_builder.write_u32(learning_mode);
                ext_builder.write_u32(inline_composition_support);

                let mut ext_msg = Vec::new();
                ext_msg.extend_from_slice(&state.host_ext_id.to_ne_bytes());
                let ext_len = (ext_builder.payload.len() + 8) as u32;
                let ext_word2 = (ext_len << 16) | 6u32; // REQ_SET_INPUT_TYPE
                ext_msg.extend_from_slice(&ext_word2.to_ne_bytes());
                ext_msg.extend_from_slice(&ext_builder.payload);
                ctx.client_to_host_queue.push((ext_msg, Vec::new()));

                state.content_hint = 0;
                state.content_purpose = 0;
            }

            if let Some((x, y, w, h)) = state.cursor_rect.take() {
                // set_cursor_rectangle: opcode 7
                let mut builder = crate::wire::MessageBuilder::new();
                builder.write_i32(x);
                builder.write_i32(y);
                builder.write_i32(w);
                builder.write_i32(h);

                let mut full_msg = Vec::new();
                full_msg.extend_from_slice(&host_v1_id.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = (len << 16) | 7u32;
                full_msg.extend_from_slice(&word2.to_ne_bytes());
                full_msg.extend_from_slice(&builder.payload);
                ctx.client_to_host_queue.push((full_msg, Vec::new()));
            }

            // commit_state: opcode 9
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_u32(state.host_serial); // serial

            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&host_v1_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 9u32;
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);
            ctx.client_to_host_queue.push((full_msg, Vec::new()));
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
}


