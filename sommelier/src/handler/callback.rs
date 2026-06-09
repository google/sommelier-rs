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

use crate::protocols;
use crate::protocols::wayland::wl_callback::WlCallbackHandler;
use crate::state::Context;
use crate::wire::{Action, MessageBuilder};
use log::debug;

pub struct CallbackHandler;

impl WlCallbackHandler for CallbackHandler {
    fn on_done(&mut self, ctx: &mut Context, callback_data: u32) -> Action {
        let host_id = ctx.last_sender_id;
        let guest_id = ctx.shadow_table.get_guest_id(host_id).unwrap_or(0);

        if guest_id != 0 {
            debug!(
                "wl_callback.done for guest_id {}, sending done and delete_id",
                guest_id
            );

            // 1. Send done event to client
            let mut builder = MessageBuilder::new();
            builder.write_u32(callback_data);

            let mut done_msg = Vec::new();
            done_msg.extend_from_slice(&guest_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | (protocols::wayland::wl_callback::EVT_DONE as u32);
            done_msg.extend_from_slice(&word2.to_ne_bytes());
            done_msg.extend_from_slice(&builder.payload);
            ctx.host_to_client_queue.push((done_msg.into(), Vec::new()));

            // 2. Send delete_id to client
            let mut builder2 = MessageBuilder::new();
            builder2.write_u32(guest_id);

            let mut del_msg = Vec::new();
            del_msg.extend_from_slice(&1u32.to_ne_bytes()); // wl_display
            let len = (builder2.payload.len() + 8) as u32;
            let word2 = (len << 16) | (protocols::wayland::wl_display::EVT_DELETE_ID as u32);
            del_msg.extend_from_slice(&word2.to_ne_bytes());
            del_msg.extend_from_slice(&builder2.payload);
            ctx.host_to_client_queue.push((del_msg.into(), Vec::new()));

            // 3. Remove from shadow table
            ctx.shadow_table.remove_id(guest_id);
        }
        Action::Drop
    }
}
