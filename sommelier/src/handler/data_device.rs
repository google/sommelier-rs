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

use crate::protocols::wayland::wl_data_device::WlDataDeviceHandler;
use crate::protocols::wayland::wl_data_device_manager::WlDataDeviceManagerHandler;
use crate::protocols::wayland::wl_data_offer::WlDataOfferHandler;
use crate::protocols::wayland::wl_data_source::WlDataSourceHandler;
use crate::state::Context;
use crate::wire::{Action, MessageBuilder};
use std::os::unix::io::RawFd;

pub struct DataDeviceHandler {}

impl DataDeviceHandler {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for DataDeviceHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl WlDataDeviceManagerHandler for DataDeviceHandler {
    fn on_create_data_source(&mut self, ctx: &mut Context, id: u32) -> Action {
        log::debug!("Tracking data source id={}", id);
        ctx.shadow_table.map_id(id, id);
        ctx.shadow_table
            .track_interface(id, "wl_data_source".to_string());
        Action::Forward
    }
}

impl WlDataDeviceHandler for DataDeviceHandler {
    fn on_data_offer(&mut self, ctx: &mut Context, id: u32) -> Action {
        let host_id = id;
        let guest_id = ctx.shadow_table.allocate_guest_server_id();

        log::debug!(
            "Remapping data_offer: host_id={} -> guest_id={}",
            host_id,
            guest_id
        );

        ctx.shadow_table.map_id(guest_id, host_id);
        ctx.shadow_table
            .track_interface(guest_id, "wl_data_offer".to_string());

        let host_sender_id = ctx.last_sender_id;
        if let Some(guest_sender_id) = ctx.shadow_table.get_guest_id(host_sender_id) {
            let mut builder = MessageBuilder::new();
            builder.write_u32(guest_id);

            // Construct the event header
            // sender_id (4 bytes) + size|opcode (4 bytes) + payload
            let len = (builder.payload.len() + 8) as u32;
            let opcode: u16 = 0; // data_offer
            let word2 = (len << 16) | (opcode as u32);

            let mut msg = Vec::new();
            msg.extend_from_slice(&guest_sender_id.to_ne_bytes());
            msg.extend_from_slice(&word2.to_ne_bytes());
            msg.extend_from_slice(&builder.payload);

            ctx.host_to_client_queue.push((msg, builder.fds));
        } else {
            log::error!(
                "Failed to find guest ID for wl_data_device sender host_id={}",
                host_sender_id
            );
        }

        Action::Drop
    }
}

impl WlDataSourceHandler for DataDeviceHandler {
    fn on_target(&mut self, _ctx: &mut Context, _mime_type: &str) -> Action {
        Action::Forward
    }
    fn on_send(&mut self, _ctx: &mut Context, mime_type: &str, fd: RawFd) -> Action {
        log::info!(
            "WlDataSourceHandler::on_send mime_type={} fd={}",
            mime_type,
            fd
        );
        Action::Forward
    }
    fn on_cancelled(&mut self, _ctx: &mut Context) -> Action {
        Action::Forward
    }
    fn on_dnd_drop_performed(&mut self, _ctx: &mut Context) -> Action {
        Action::Forward
    }
    fn on_dnd_finished(&mut self, _ctx: &mut Context) -> Action {
        Action::Forward
    }
    fn on_action(&mut self, _ctx: &mut Context, _dnd_action: u32) -> Action {
        Action::Forward
    }
}

impl WlDataOfferHandler for DataDeviceHandler {
    fn on_offer(&mut self, _ctx: &mut Context, _mime_type: &str) -> Action {
        Action::Forward
    }
    fn on_source_actions(&mut self, _ctx: &mut Context, _source_actions: u32) -> Action {
        Action::Forward
    }
    fn on_action(&mut self, _ctx: &mut Context, _dnd_action: u32) -> Action {
        Action::Forward
    }
}
