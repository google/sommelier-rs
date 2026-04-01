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
        _serial: u32,
        text: &String,
        _commit: &String,
    ) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            // v3 preedit_string (opcode 2)
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_string(text);
            builder.write_i32(0); // cursor_begin
            builder.write_i32(0); // cursor_end
            
            let mut msg = Vec::new();
            msg.extend_from_slice(&guest_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 2u32;
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);
            ctx.host_to_client_queue.push((msg, Vec::new()));
        }
        Action::Drop
    }

    fn on_commit_string(
        &mut self,
        ctx: &mut Context,
        _serial: u32,
        text: &String,
    ) -> Action {
        let host_id = ctx.last_sender_id;
        if let Some(guest_id) = ctx.shadow_table.get_guest_id(host_id) {
            // v3 preedit_string (opcode 2) - explicitly clear preedit before commit
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_string(&String::new());
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
            // serial matches state but for simplicity we can send 0 or _serial
            let mut builder = crate::wire::MessageBuilder::new();
            builder.write_u32(0); // serial
            
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
        _serial: u32,
        _time: u32,
        sym: u32,
        state: u32,
        _modifiers: u32,
    ) -> Action {
        let context = xkbcommon::xkb::Context::new(xkbcommon::xkb::CONTEXT_NO_FLAGS);
        if let Some(keymap) = xkbcommon::xkb::Keymap::new_from_names(
            &context, "", "", "", "", None, xkbcommon::xkb::KEYMAP_COMPILE_NO_FLAGS,
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
                    builder.write_u32(0); // serial
                    builder.write_u32(0); // time
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
    
    fn on_cursor_position(
        &mut self,
        _ctx: &mut Context,
        _index: i32,
        _anchor: i32,
    ) -> Action {
        Action::Drop
    }
    
    fn on_delete_surrounding_text(
        &mut self,
        _ctx: &mut Context,
        _index: i32,
        _length: u32,
    ) -> Action {
        Action::Drop
    }
    
    fn on_language(
        &mut self,
        _ctx: &mut Context,
        _serial: u32,
        _language: &String,
    ) -> Action {
        Action::Drop
    }
    
    fn on_text_direction(
        &mut self,
        _ctx: &mut Context,
        _serial: u32,
        _direction: u32,
    ) -> Action {
        Action::Drop
    }
}

pub struct TextInputExtensionV1Handler;
impl zcr_text_input_extension_v1::ZcrTextInputExtensionV1Handler for TextInputExtensionV1Handler {}

pub struct ExtendedTextInputV1Handler;
impl zcr_extended_text_input_v1::ZcrExtendedTextInputV1Handler for ExtendedTextInputV1Handler {
    fn on_set_preedit_region(
        &mut self,
        _ctx: &mut Context,
        _index: i32,
        _length: u32,
    ) -> Action {
        Action::Drop
    }
    fn on_clear_grammar_fragments(
        &mut self,
        _ctx: &mut Context,
        _start: u32,
        _end: u32,
    ) -> Action {
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
    fn on_set_autocorrect_range(
        &mut self,
        _ctx: &mut Context,
        _start: u32,
        _end: u32,
    ) -> Action {
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
    fn on_confirm_preedit(&mut self, _ctx: &mut Context, _selection_behavior: u32) -> Action {
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
            let word2 = (len << 16) | 0u32; // opcode 0: create_text_input
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
            let word2 = (len << 16) | 0u32; // opcode 0: get_extended_text_input
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);
            
            ctx.client_to_host_queue.push((full_msg, Vec::new()));
        }

        ctx.shadow_table.map_id(id, host_v1_id);
        ctx.shadow_table.track_interface(id, "zwp_text_input_v3".to_string());
        ctx.shadow_table.track_host_interface(host_v1_id, "zwp_text_input_v1".to_string());
        ctx.shadow_table.track_host_interface(host_ext_id, "zcr_extended_text_input_v1".to_string());

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
            let host_v1_id = state.host_v1_id;
            let host_seat = ctx.shadow_table.get_host_id(state.guest_seat).unwrap_or(0);
            let host_surface = state.active_surface.and_then(|s| ctx.shadow_table.get_host_id(s)).unwrap_or(0);
            
            if state.enabled_changed {
                if state.enabled {
                    // activate: opcode 0
                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_u32(host_seat);
                    builder.write_u32(host_surface);
                    
                    let mut full_msg = Vec::new();
                    full_msg.extend_from_slice(&host_v1_id.to_ne_bytes());
                    let len = (builder.payload.len() + 8) as u32;
                    let word2 = (len << 16) | 0u32;
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
            
            if let Some((text, cursor, anchor)) = state.surrounding_text.take() {
                // set_surrounding_text: opcode 5
                let mut builder = crate::wire::MessageBuilder::new();
                builder.write_string(&text);
                builder.write_u32(cursor as u32);
                builder.write_u32(anchor as u32);
                
                let mut full_msg = Vec::new();
                full_msg.extend_from_slice(&host_v1_id.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = (len << 16) | 5u32;
                full_msg.extend_from_slice(&word2.to_ne_bytes());
                full_msg.extend_from_slice(&builder.payload);
                ctx.client_to_host_queue.push((full_msg, Vec::new()));
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
                    _ => 1,         // TEXT
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
            builder.write_u32(0); // serial
            
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