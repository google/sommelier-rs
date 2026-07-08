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

/// Push a wire message built by `builder` onto `queue`, binding it to
/// `sender_id` / `opcode`. Centralizes the (header + payload) assembly that
/// used to be open-coded with `extend_from_slice` + `(len << 16) | opcode`.
pub(crate) fn push_msg(
    queue: &mut Vec<(Vec<u8>, Vec<RawFd>)>,
    sender_id: u32,
    opcode: u16,
    builder: MessageBuilder,
) {
    queue.push((builder.build_message(sender_id, opcode), Vec::new()));
}

pub struct TextInputManagerV1Handler;
impl zwp_text_input_manager_v1::ZwpTextInputManagerV1Handler for TextInputManagerV1Handler {}

pub struct TextInputV1Handler;
impl zwp_text_input_v1::ZwpTextInputV1Handler for TextInputV1Handler {
    fn on_preedit_string(
        &mut self,
        ctx: &mut Context,
        _serial: u32,
        text: &String,
        _commit: &String,
    ) -> Action {
        let host_id = ctx.last_sender_id;
        log::trace!(">>> on_preedit_string: host_id={}, text={:?}", host_id, text);
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            // v3 preedit_string (opcode 2)
            let mut builder = MessageBuilder::new();
            builder.write_string(text);
            builder.write_i32(0); // cursor_begin
            builder.write_i32(0); // cursor_end
            push_msg(&mut ctx.host_to_client_queue, guest_id, 2, builder);
        }
        Action::Drop
    }

    fn on_commit_string(&mut self, ctx: &mut Context, _serial: u32, text: &String) -> Action {
        let host_id = ctx.last_sender_id;
        log::trace!(
            ">>> on_commit_string: host_id={}, text={:?}",
            host_id,
            text
        );
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            // v3 preedit_string (opcode 2) - explicitly clear preedit before commit
            let mut builder = MessageBuilder::new();
            builder.write_string("");
            builder.write_i32(0); // cursor_begin
            builder.write_i32(0); // cursor_end
            push_msg(&mut ctx.host_to_client_queue, guest_id, 2, builder);

            // v3 commit_string (opcode 3)
            let mut builder = MessageBuilder::new();
            builder.write_string(text);
            push_msg(&mut ctx.host_to_client_queue, guest_id, 3, builder);

            // v3 done (opcode 5)
            // serial matches state but for simplicity we can send 0 or _serial
            let mut builder = MessageBuilder::new();
            builder.write_u32(0); // serial
            push_msg(&mut ctx.host_to_client_queue, guest_id, 5, builder);
        }
        Action::Drop
    }

    fn on_keysym(
        &mut self,
        ctx: &mut Context,
        _serial: u32,
        _time: u32,
        sym: u32,
        state: u32,
        _modifiers: u32,
    ) -> Action {
        let host_id = ctx.last_sender_id;
        let sym_char = std::char::from_u32(sym).map(|c| c.to_string()).unwrap_or_default();
        log::trace!(
            ">>> on_keysym: host_id={}, sym=0x{:x} ({:?}), state={}",
            host_id,
            sym,
            sym_char,
            state
        );

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
                    log::debug!(
                        "  -> forwarding wl_keyboard.key: keyboard_id={}, keycode={}, state={}",
                        keyboard_id,
                        keycode,
                        state
                    );
                    // Send wl_keyboard::key (opcode 3)
                    let mut builder = MessageBuilder::new();
                    builder.write_u32(0); // serial
                    builder.write_u32(0); // time
                    builder.write_u32(keycode); // key
                    builder.write_u32(state); // state (0: released, 1: pressed)
                    push_msg(&mut ctx.host_to_client_queue, keyboard_id, 3, builder);
                } else {
                    log::warn!("  -> no wl_keyboard found to forward keysym to");
                }
            } else {
                log::warn!(
                    "  -> could not find keycode for sym=0x{:x} ({:?})",
                    sym,
                    sym_char
                );
            }
        }
        Action::Drop
    }

    fn on_enter(&mut self, _ctx: &mut Context, surface: u32) -> Action {
        log::trace!(">>> on_enter: surface={}", surface);
        Action::Drop
    }

    fn on_leave(&mut self, _ctx: &mut Context) -> Action {
        log::trace!(">>> on_leave");
        Action::Drop
    }

    fn on_modifiers_map(&mut self, _ctx: &mut Context, _map: &[u8]) -> Action {
        log::trace!(">>> on_modifiers_map: len={}", _map.len());
        Action::Drop
    }

    fn on_input_panel_state(&mut self, _ctx: &mut Context, _state: u32) -> Action {
        log::trace!(">>> on_input_panel_state: state={}", _state);
        Action::Drop
    }

    fn on_preedit_styling(
        &mut self,
        _ctx: &mut Context,
        _index: u32,
        _length: u32,
        _style: u32,
    ) -> Action {
        log::trace!(
            ">>> on_preedit_styling: index={}, length={}, style={}",
            _index,
            _length,
            _style
        );
        Action::Drop
    }

    fn on_preedit_cursor(&mut self, _ctx: &mut Context, _index: i32) -> Action {
        log::trace!(">>> on_preedit_cursor: index={}", _index);
        Action::Drop
    }

    fn on_cursor_position(&mut self, _ctx: &mut Context, _index: i32, _anchor: i32) -> Action {
        log::trace!(
            ">>> on_cursor_position: index={}, anchor={}",
            _index,
            _anchor
        );
        Action::Drop
    }

    fn on_delete_surrounding_text(
        &mut self,
        ctx: &mut Context,
        _index: i32,
        _length: u32,
    ) -> Action {
        let host_id = ctx.last_sender_id;
        log::trace!(
            ">>> on_delete_surrounding_text: host_id={}, index={}, length={}",
            host_id,
            _index,
            _length
        );
        Action::Drop
    }

    fn on_language(&mut self, ctx: &mut Context, _serial: u32, _language: &String) -> Action {
        let host_id = ctx.last_sender_id;
        log::trace!(
            ">>> on_language: host_id={}, language={:?}",
            host_id,
            _language
        );
        Action::Drop
    }

    fn on_text_direction(&mut self, ctx: &mut Context, _serial: u32, _direction: u32) -> Action {
        let host_id = ctx.last_sender_id;
        log::trace!(
            ">>> on_text_direction: host_id={}, direction={}",
            host_id,
            _direction
        );
        Action::Drop
    }
}

pub struct TextInputExtensionV1Handler;
impl zcr_text_input_extension_v1::ZcrTextInputExtensionV1Handler for TextInputExtensionV1Handler {}

pub struct ExtendedTextInputV1Handler;
impl zcr_extended_text_input_v1::ZcrExtendedTextInputV1Handler for ExtendedTextInputV1Handler {
    fn on_set_preedit_region(
        &mut self,
        ctx: &mut Context,
        _index: i32,
        _length: u32,
    ) -> Action {
        let host_ext_id = ctx.last_sender_id;
        log::trace!(
            ">>> on_set_preedit_region: host_ext_id={}, index={}, length={}",
            host_ext_id,
            _index,
            _length
        );
        Action::Drop
    }
    fn on_clear_grammar_fragments(&mut self, _ctx: &mut Context, _start: u32, _end: u32) -> Action {
        log::trace!(
            ">>> on_clear_grammar_fragments: start={}, end={}",
            _start,
            _end
        );
        Action::Drop
    }
    fn on_add_grammar_fragment(
        &mut self,
        _ctx: &mut Context,
        _start: u32,
        _end: u32,
        _suggestion: &String,
    ) -> Action {
        log::trace!(
            ">>> on_add_grammar_fragment: start={}, end={}, suggestion={:?}",
            _start,
            _end,
            _suggestion
        );
        Action::Drop
    }
    fn on_set_autocorrect_range(&mut self, _ctx: &mut Context, _start: u32, _end: u32) -> Action {
        log::trace!(
            ">>> on_set_autocorrect_range: start={}, end={}",
            _start,
            _end
        );
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
        log::trace!(
            ">>> on_set_virtual_keyboard_occluded_bounds: x={}, y={}, w={}, h={}",
            _x,
            _y,
            _width,
            _height
        );
        Action::Drop
    }
    fn on_confirm_preedit(&mut self, ctx: &mut Context, _selection_behavior: u32) -> Action {
        let host_ext_id = ctx.last_sender_id;
        log::trace!(
            ">>> on_confirm_preedit: host_ext_id={}, selection_behavior={}",
            host_ext_id,
            _selection_behavior
        );
        Action::Drop
    }
}

pub struct TextInputManagerV3Handler;
impl zwp_text_input_manager_v3::ZwpTextInputManagerV3Handler for TextInputManagerV3Handler {
    fn on_get_text_input(&mut self, ctx: &mut Context, id: u32, seat: u32) -> Action {
        log::trace!(">>> v3 on_get_text_input: guest_id={}, seat={}", id, seat);
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
            push_msg(&mut ctx.client_to_host_queue, host_ext_manager_id, 0, builder);
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
                content_hint: 0,
                content_purpose: 0,
                cursor_rect: None,
                text_change_cause: 0,
            },
        );

        Action::Drop
    }
}

pub struct TextInputV3Handler;
impl zwp_text_input_v3::ZwpTextInputV3Handler for TextInputV3Handler {
    fn on_enable(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;
        log::info!(">>> v3 on_enable: guest_id={}", guest_id);
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.enabled = true;
            state.enabled_changed = true;
        }
        Action::Drop
    }

    fn on_disable(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;
        log::info!(">>> v3 on_disable: guest_id={}", guest_id);
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
        log::trace!(
            ">>> v3 on_set_surrounding_text: guest_id={}, text={:?}, cursor={}, anchor={}",
            guest_id,
            text,
            cursor,
            anchor
        );
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.surrounding_text = Some((text.clone(), cursor, anchor));
        }
        Action::Drop
    }

    fn on_set_text_change_cause(&mut self, ctx: &mut Context, cause: u32) -> Action {
        let guest_id = ctx.last_sender_id;
        log::trace!(
            ">>> v3 on_text_change_cause: guest_id={}, cause={}",
            guest_id,
            cause
        );
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.text_change_cause = cause;
        }
        Action::Drop
    }

    fn on_set_content_type(&mut self, ctx: &mut Context, hint: u32, purpose: u32) -> Action {
        let guest_id = ctx.last_sender_id;
        log::trace!(
            ">>> v3 on_set_content_type: guest_id={}, hint={}, purpose={}",
            guest_id,
            hint,
            purpose
        );
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
        log::trace!(
            ">>> v3 on_set_cursor_rectangle: guest_id={}, rect=({}, {}, {}, {})",
            guest_id,
            x,
            y,
            width,
            height
        );
        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            state.cursor_rect = Some((x, y, width, height));
        }
        Action::Drop
    }

    fn on_commit(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;
        log::trace!(">>> v3 on_commit: guest_id={}", guest_id);

        if let Some(state) = ctx.text_inputs.get_mut(&guest_id) {
            let host_v1_id = state.host_v1_id;
            let host_seat = ctx.shadow_table.get_host_id(state.guest_seat).unwrap_or(0);
            let host_surface = state
                .active_surface
                .and_then(|s| ctx.shadow_table.get_host_id(s))
                .unwrap_or(0);

            if state.enabled_changed {
                if state.enabled {
                    // activate: opcode 0
                    let mut builder = MessageBuilder::new();
                    builder.write_u32(host_seat);
                    builder.write_u32(host_surface);
                    push_msg(&mut ctx.client_to_host_queue, host_v1_id, 0, builder);
                } else {
                    // deactivate: opcode 1
                    let mut builder = MessageBuilder::new();
                    builder.write_u32(host_seat);
                    push_msg(&mut ctx.client_to_host_queue, host_v1_id, 1, builder);
                }
                state.enabled_changed = false;
            }

            if let Some((text, cursor, anchor)) = state.surrounding_text.take() {
                // set_surrounding_text: opcode 5
                let mut builder = MessageBuilder::new();
                builder.write_string(&text);
                builder.write_u32(cursor as u32);
                builder.write_u32(anchor as u32);
                push_msg(&mut ctx.client_to_host_queue, host_v1_id, 5, builder);
            }

            if state.content_hint != 0 || state.content_purpose != 0 {
                // set_content_type: opcode 6 (on zwp_text_input_v1)
                let mut builder = MessageBuilder::new();
                builder.write_u32(state.content_hint);
                builder.write_u32(state.content_purpose);
                push_msg(&mut ctx.client_to_host_queue, host_v1_id, 6, builder);

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

                let mut ext_builder = MessageBuilder::new();
                ext_builder.write_u32(input_type);
                ext_builder.write_u32(input_mode);
                ext_builder.write_u32(input_flags);
                ext_builder.write_u32(learning_mode);
                ext_builder.write_u32(inline_composition_support);
                push_msg(&mut ctx.client_to_host_queue, state.host_ext_id, 6, ext_builder);

                state.content_hint = 0;
                state.content_purpose = 0;
            }

            if let Some((x, y, w, h)) = state.cursor_rect.take() {
                // set_cursor_rectangle: opcode 7
                let mut builder = MessageBuilder::new();
                builder.write_i32(x);
                builder.write_i32(y);
                builder.write_i32(w);
                builder.write_i32(h);
                push_msg(&mut ctx.client_to_host_queue, host_v1_id, 7, builder);
            }

            // commit_state: opcode 9
            let mut builder = MessageBuilder::new();
            builder.write_u32(0); // serial
            push_msg(&mut ctx.client_to_host_queue, host_v1_id, 9, builder);
        }

        Action::Drop
    }
}
