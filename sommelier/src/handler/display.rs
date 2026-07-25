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

use crate::protocols::wayland::wl_display;
use crate::state::Context;
use crate::wire::{Action, MessageBuilder};
use log::error;

pub struct DisplayHandler;

impl wl_display::WlDisplayHandler for DisplayHandler {
    fn on_get_registry(&mut self, ctx: &mut Context, registry: u32) -> Action {
        let host_registry_id = ctx.shadow_table.allocate_host_id();
        ctx.shadow_table.map_id(registry, host_registry_id);
        ctx.shadow_table
            .track_interface(registry, "wl_registry".to_string());

        // Send get_registry to host (opcode 1)
        let mut builder = MessageBuilder::new();
        builder.write_u32(host_registry_id);

        let mut full_msg = Vec::new();
        full_msg.extend_from_slice(&1u32.to_ne_bytes());
        let len = (builder.payload.len() + 8) as u32;
        let word2 = (len << 16) | (wl_display::REQ_GET_REGISTRY as u32);
        full_msg.extend_from_slice(&word2.to_ne_bytes());
        full_msg.extend_from_slice(&builder.payload);

        ctx.client_to_host_queue.push((full_msg, Vec::new()));

        Action::Drop
    }

    fn on_sync(&mut self, ctx: &mut Context, callback: u32) -> Action {
        let host_callback_id = ctx.shadow_table.allocate_host_id();
        ctx.shadow_table.map_id(callback, host_callback_id);
        ctx.shadow_table
            .track_interface(callback, "wl_callback".to_string());

        // Send sync to host (opcode 0)
        let mut builder = MessageBuilder::new();
        builder.write_u32(host_callback_id);

        let mut full_msg = Vec::new();
        full_msg.extend_from_slice(&1u32.to_ne_bytes());
        let len = (builder.payload.len() + 8) as u32;
        let word2 = (len << 16) | (wl_display::REQ_SYNC as u32);
        full_msg.extend_from_slice(&word2.to_ne_bytes());
        full_msg.extend_from_slice(&builder.payload);

        ctx.client_to_host_queue.push((full_msg, Vec::new()));

        Action::Drop
    }

    fn on_delete_id(&mut self, ctx: &mut Context, id: u32) -> Action {
        let guest_id = ctx.shadow_table.get_guest_id(id).unwrap_or(0);
        if guest_id != 0 {
            ctx.shadow_table.remove_id(guest_id);

            // Forward the corrected delete_id event to the client
            let mut builder = MessageBuilder::new();
            builder.write_u32(guest_id);

            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&1u32.to_ne_bytes()); // wl_display is ID 1
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | (wl_display::EVT_DELETE_ID as u32);
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);

            ctx.host_to_client_queue.push((full_msg, Vec::new()));
        }
        Action::Drop
    }

    fn on_error(&mut self, ctx: &mut Context, object_id: u32, code: u32, message: &str) -> Action {
        let guest_id = ctx.shadow_table.get_guest_id(object_id).unwrap_or(0);
        error!(
            "Wayland Error from Host: object_id={} (guest_id={}), code={}, message={}",
            object_id, guest_id, code, message
        );

        let mut builder = MessageBuilder::new();
        builder.write_u32(guest_id);
        builder.write_u32(code);
        builder.write_string(message);

        let mut full_msg = Vec::new();
        full_msg.extend_from_slice(&1u32.to_ne_bytes()); // wl_display is ID 1
        let len = (builder.payload.len() + 8) as u32;
        let word2 = (len << 16) | (wl_display::EVT_ERROR as u32);
        full_msg.extend_from_slice(&word2.to_ne_bytes());
        full_msg.extend_from_slice(&builder.payload);

        ctx.host_to_client_queue.push((full_msg, Vec::new()));

        Action::Drop
    }
}
