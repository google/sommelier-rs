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

use crate::protocols::wayland::wl_keyboard;
use crate::state::Context;
use crate::wire::Action;

pub struct KeyboardHandler;

impl wl_keyboard::WlKeyboardHandler for KeyboardHandler {
    fn on_enter(
        &mut self,
        ctx: &mut Context,
        serial: u32,
        surface: u32,
        keys: &[u8],
    ) -> Action {
        let guest_keyboard_id = ctx.last_sender_id;
        
        if let Some(&guest_seat_id) = ctx.keyboard_to_seat.get(&guest_keyboard_id) {
            ctx.active_surface_for_seat.insert(guest_seat_id, surface);

            // Find the v3 text input for this seat
            for (guest_text_input_id, state) in ctx.text_inputs.iter_mut() {
                if state.guest_seat == guest_seat_id {
                    state.active_surface = Some(surface);
                    
                    // Send zwp_text_input_v3.enter (opcode 0)
                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_u32(surface);
                    
                    let mut msg = Vec::new();
                    msg.extend_from_slice(&guest_text_input_id.to_ne_bytes());
                    let len = (builder.payload.len() + 8) as u32;
                    let word2 = (len << 16) | 0u32;
                    msg.extend_from_slice(&word2.to_ne_bytes());
                    msg.extend_from_slice(&builder.payload);
                    ctx.host_to_client_queue.push((msg, Vec::new()));
                }
            }
        }
        
        Action::Forward
    }

    fn on_leave(&mut self, ctx: &mut Context, serial: u32, surface: u32) -> Action {
        let guest_keyboard_id = ctx.last_sender_id;
        
        if let Some(&guest_seat_id) = ctx.keyboard_to_seat.get(&guest_keyboard_id) {
            ctx.active_surface_for_seat.remove(&guest_seat_id);

            // Find the v3 text input for this seat
            for (guest_text_input_id, state) in ctx.text_inputs.iter_mut() {
                if state.guest_seat == guest_seat_id {
                    state.active_surface = None;
                    
                    // Send zwp_text_input_v3.leave (opcode 1)
                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_u32(surface);
                    
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
        
        Action::Forward
    }
}
