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

use crate::protocols::linux_dmabuf_v1::ALLOWED_INTERFACES as DMABUF_ALLOWED;
use crate::protocols::text_input_unstable_v3::ALLOWED_INTERFACES as TEXT_INPUT_ALLOWED;
use crate::protocols::viewporter::ALLOWED_INTERFACES as VIEWPORTER_ALLOWED;
use crate::protocols::wayland::wl_registry;
use crate::protocols::wayland::wl_shm;
use crate::protocols::wayland::ALLOWED_INTERFACES as WL_ALLOWED;
use crate::protocols::xdg_decoration_unstable_v1::ALLOWED_INTERFACES as XDG_DECORATION_ALLOWED;
use crate::protocols::xdg_shell::ALLOWED_INTERFACES as XDG_ALLOWED;
use crate::state::Context;
use crate::wire::{Action, MessageBuilder};
use log::error;

pub struct RegistryHandler;

impl wl_registry::WlRegistryHandler for RegistryHandler {
    fn on_global(&mut self, ctx: &mut Context, name: u32, interface: &str, version: u32) -> Action {
        // Track host globals
        ctx.host_globals.insert(interface.to_string(), name);

        if interface == "zxdg_decoration_manager_v1" {
            if !ctx.xdg_decoration {
                return Action::Drop;
            }
        }

        if interface == "zwp_linux_dmabuf_v1" {
            if !ctx.gpu_accel {
                return Action::Drop;
            }
            let host_id = ctx.shadow_table.allocate_host_id();
            ctx.host_dmabuf_id = Some(host_id);
            let placeholder_guest_id = 0xFE00_0000 | host_id;
            ctx.shadow_table.map_id(placeholder_guest_id, host_id);
            ctx.shadow_table
                .track_interface(placeholder_guest_id, "zwp_linux_dmabuf_v1".to_string());

            let client_version = version;
            let mut global_builder = MessageBuilder::new();
            global_builder.write_u32(name);
            global_builder.write_string(interface);
            global_builder.write_u32(client_version);

            // Translate host registry ID to guest registry ID
            let registry_guest_id = ctx
                .shadow_table
                .get_guest_id(ctx.last_sender_id)
                .unwrap_or(ctx.last_sender_id);

            let mut global_msg = Vec::new();
            global_msg.extend_from_slice(&registry_guest_id.to_ne_bytes());
            let len = (global_builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | (wl_registry::EVT_GLOBAL as u32);
            global_msg.extend_from_slice(&word2.to_ne_bytes());
            global_msg.extend_from_slice(&global_builder.payload);
            ctx.host_to_client_queue.push((global_msg, Vec::new()));

            // 2. Bind internally
            let registry_host_id = ctx.last_sender_id;
            let mut builder = MessageBuilder::new();
            builder.write_u32(name);
            builder.write_string(interface);
            builder.write_u32(client_version);
            builder.write_u32(host_id); // new_id

            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&registry_host_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | (wl_registry::REQ_BIND as u32);
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);

            ctx.client_to_host_queue.push((full_msg, Vec::new()));

            // Drop the original global event so we don't send the v4 advertisement
            return Action::Drop;
        } else if interface == "wl_shm" {
            let host_id = ctx.shadow_table.allocate_host_id();
            ctx.host_shm_id = Some(host_id);
            // Map to a high-bit placeholder guest ID
            let placeholder_guest_id = 0xFD00_0000 | host_id;
            ctx.shadow_table.map_id(placeholder_guest_id, host_id);
            ctx.shadow_table
                .track_interface(placeholder_guest_id, "wl_shm".to_string());

            // Bind to wl_shm
            let registry_host_id = ctx.last_sender_id;
            let mut builder = MessageBuilder::new();
            builder.write_u32(name);
            builder.write_string(interface);
            builder.write_u32(1); // Bind version 1
            builder.write_u32(host_id); // new_id

            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&registry_host_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | (wl_registry::REQ_BIND as u32);
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);

            ctx.client_to_host_queue.push((full_msg, Vec::new()));
        }

        if !WL_ALLOWED.contains(&interface)
            && !XDG_ALLOWED.contains(&interface)
            && !DMABUF_ALLOWED.contains(&interface)
            && !VIEWPORTER_ALLOWED.contains(&interface)
            && !TEXT_INPUT_ALLOWED.contains(&interface)
            && !XDG_DECORATION_ALLOWED.contains(&interface)
        {
            return Action::Drop;
        }
        Action::Forward
    }

    fn on_bind(&mut self, ctx: &mut Context, _name: u32, id: &(String, u32, u32)) -> Action {
        // id is (interface, version, new_id)
        let (interface, _version, guest_new_id) = id;

        if interface == "wl_shm" {
            // Do NOT forward wl_shm to host. We emulate it.
            // Just track it so we know this guest ID is wl_shm.
            ctx.shadow_table
                .track_interface(*guest_new_id, "wl_shm".to_string());

            // Send standard formats to client: ARGB8888 (0) and XRGB8888 (1)
            for format in [0u32, 1u32] {
                let mut builder = MessageBuilder::new();
                builder.write_u32(format);

                let mut full_msg = Vec::new();
                full_msg.extend_from_slice(&guest_new_id.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = (len << 16) | (wl_shm::EVT_FORMAT as u32);
                full_msg.extend_from_slice(&word2.to_ne_bytes());
                full_msg.extend_from_slice(&builder.payload);

                // Send to client
                ctx.host_to_client_queue.push((full_msg, Vec::new()));
            }

            return Action::Drop;
        }

        // Translation logic for other interfaces
        let host_new_id = ctx.shadow_table.allocate_host_id();
        ctx.shadow_table.map_id(*guest_new_id, host_new_id);
        ctx.shadow_table
            .track_interface(*guest_new_id, interface.clone());

        // We need to send the bind request to the host.
        // The sender is the registry object.
        let registry_guest_id = ctx.last_sender_id;

        if let Some(registry_host_id) = ctx.shadow_table.get_host_id(registry_guest_id) {
            let mut builder = MessageBuilder::new();
            builder.write_u32(_name);
            builder.write_string(interface);
            builder.write_u32(*_version);
            builder.write_u32(host_new_id);

            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&registry_host_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | (wl_registry::REQ_BIND as u32);
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);

            ctx.client_to_host_queue.push((full_msg, Vec::new()));
        } else {
            error!("Registry not mapped! Guest ID: {}", registry_guest_id);
        }

        Action::Drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::wayland::wl_registry::WlRegistryHandler;

    #[test]
    fn test_xdg_decoration_on_global_enabled() {
        let mut handler = RegistryHandler;
        let mut ctx = Context::new(false, true);
        let action = handler.on_global(&mut ctx, 1, "zxdg_decoration_manager_v1", 1);
        assert_eq!(action, Action::Forward);
        assert_eq!(ctx.host_globals.get("zxdg_decoration_manager_v1"), Some(&1));
    }

    #[test]
    fn test_xdg_decoration_on_global_disabled() {
        let mut handler = RegistryHandler;
        let mut ctx = Context::new(false, false);
        let action = handler.on_global(&mut ctx, 1, "zxdg_decoration_manager_v1", 1);
        assert_eq!(action, Action::Drop);
        assert_eq!(ctx.host_globals.get("zxdg_decoration_manager_v1"), Some(&1));
    }
}
