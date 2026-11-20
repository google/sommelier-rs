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

use crate::protocols::fractional_scale_v1::ALLOWED_INTERFACES as FRACTIONAL_SCALE_ALLOWED;
use crate::protocols::linux_dmabuf_v1::ALLOWED_INTERFACES as DMABUF_ALLOWED;
use crate::protocols::text_input_unstable_v3::ALLOWED_INTERFACES as TEXT_INPUT_ALLOWED;
use crate::protocols::viewporter::ALLOWED_INTERFACES as VIEWPORTER_ALLOWED;
use crate::protocols::wayland::ALLOWED_INTERFACES as WL_ALLOWED;
use crate::protocols::wayland::wl_registry;
use crate::protocols::wayland::wl_shm;
use crate::protocols::xdg_decoration_unstable_v1::ALLOWED_INTERFACES as XDG_DECORATION_ALLOWED;
use crate::protocols::xdg_shell::ALLOWED_INTERFACES as XDG_ALLOWED;
use crate::state::{Context, HostId};
use crate::wire::{Action, MessageBuilder};
use log::error;

pub struct RegistryHandler;

impl RegistryHandler {
    fn enqueue_bind(
        ctx: &mut Context,
        registry_host_id: u32,
        name: u32,
        interface: &str,
        version: u32,
        host_new_id: u32,
    ) {
        let mut builder = MessageBuilder::new();
        builder.write_u32(name);
        builder.write_string(interface);
        builder.write_u32(version);
        builder.write_u32(host_new_id);

        let full_msg = builder.build_message(registry_host_id, wl_registry::REQ_BIND);
        ctx.client_to_host_queue.push((full_msg, Vec::new()));
    }

    fn bind_internal(ctx: &mut Context, name: u32, interface: &str, version: u32) -> u32 {
        let host_id = ctx.shadow_table.allocate_host_id();
        Self::enqueue_bind(ctx, ctx.last_sender_id, name, interface, version, host_id);
        host_id
    }
}

impl wl_registry::WlRegistryHandler for RegistryHandler {
    fn on_global(&mut self, ctx: &mut Context, name: u32, interface: &str, version: u32) -> Action {
        // Track host globals
        ctx.host_globals.insert(interface.to_string(), name);

        if interface == "zwp_linux_dmabuf_v1" {
            if !ctx.gpu_accel {
                return Action::Drop;
            }
            if ctx.host_dmabuf_id.is_none() {
                let host_id = Self::bind_internal(ctx, name, interface, version);
                ctx.host_dmabuf_id = Some(host_id);
                // Register for event dispatch: the host compositor sends format/modifier
                // events to the dmabuf factory object after we bind it.
                ctx.shadow_table
                    .track_host_interface(host_id, "zwp_linux_dmabuf_v1".to_string());
            }
        } else if interface == "zwp_text_input_manager_v1" {
            if ctx.host_text_input_manager_v1_id.is_none() {
                let host_id = Self::bind_internal(ctx, name, interface, version);
                ctx.host_text_input_manager_v1_id = Some(host_id);
                // The host does not send events to the text_input_manager factory; no
                // shadow table entry needed.
            }

            let client_version = 1;
            let mut global_builder = MessageBuilder::new();
            global_builder.write_u32(name);
            global_builder.write_string("zwp_text_input_manager_v3");
            global_builder.write_u32(client_version);

            // Translate host registry ID to guest registry ID
            let registry_guest_id = ctx
                .shadow_table
                .get_guest_id(ctx.last_sender_id)
                .unwrap_or(ctx.last_sender_id);

            let global_msg =
                global_builder.build_message(registry_guest_id, wl_registry::EVT_GLOBAL);
            ctx.host_to_client_queue.push((global_msg, Vec::new()));

            return Action::Drop;
        } else if interface == "zcr_text_input_extension_v1" {
            if ctx.host_text_input_extension_v1_id.is_none() {
                let host_id = Self::bind_internal(ctx, name, interface, version);
                ctx.host_text_input_extension_v1_id = Some(host_id);
                // The host does not send events to the text_input_extension factory; no
                // shadow table entry needed.
            }
            return Action::Drop;
        } else if interface == "zcr_keyboard_extension_v1" {
            // Bind zcr_keyboard_extension_v1 internally. This is a ChromeOS-
            // specific protocol that enables the ack-key mechanism for
            // controlling host accelerator processing. Not exposed to the guest.
            if ctx.host_keyboard_extension_id.is_none() {
                // Bind at v1: we only need ack_key. peek_key (added in v2) is
                // intentionally not used.
                let host_id = Self::bind_internal(ctx, name, interface, 1);
                ctx.host_keyboard_extension_id = Some(HostId::from_allocated(host_id));
                log::debug!("Bound zcr_keyboard_extension_v1 (host_id={})", host_id);
            }
            return Action::Drop;
        } else if interface == "zaura_shell" {
            // Bind zaura_shell internally for ChromeOS shelf integration.
            // We use this to create zaura_surface objects and set application
            // IDs so the shelf can match windows to .desktop entries.
            // Not exposed to the guest; capped at v38 (need v5 for
            // set_application_id, v38 for release destructor).
            if ctx.host_zaura_shell_id.is_none() {
                let bound_version = std::cmp::min(version, 38);
                let host_id = Self::bind_internal(ctx, name, interface, bound_version);
                ctx.host_zaura_shell_id = Some(host_id);
                ctx.host_zaura_shell_version = bound_version;
                ctx.shadow_table
                    .track_host_interface(host_id, "zaura_shell".to_string());
                log::debug!("Bound zaura_shell internally (host_id={})", host_id);
            }
            return Action::Drop;
        } else if interface == "wl_shm" && ctx.host_shm_id.is_none() {
            // wl_shm is emulated: on_bind drops the guest request and sends synthetic
            // format events, so the host never sends wl_shm events to us. No shadow
            // table entry is needed.
            ctx.host_shm_id = Some(Self::bind_internal(ctx, name, interface, 1));
        }

        let is_allowed = WL_ALLOWED.contains(&interface)
            || XDG_ALLOWED.contains(&interface)
            || DMABUF_ALLOWED.contains(&interface)
            || VIEWPORTER_ALLOWED.contains(&interface)
            || TEXT_INPUT_ALLOWED.contains(&interface)
            || (XDG_DECORATION_ALLOWED.contains(&interface) && ctx.xdg_decoration)
            || FRACTIONAL_SCALE_ALLOWED.contains(&interface)
            || interface == "wl_data_device_manager";

        if !is_allowed {
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
                let msg = builder.build_message(*guest_new_id, wl_shm::EVT_FORMAT);
                ctx.host_to_client_queue.push((msg, Vec::new()));
            }

            return Action::Drop;
        } else if interface == "zwp_text_input_manager_v3" {
            // Map the client's v3 ID to the host's v1 ID we already bound.
            if let Some(host_id) = ctx.host_text_input_manager_v1_id {
                ctx.shadow_table.map_id(*guest_new_id, host_id);
                ctx.shadow_table
                    .track_interface(*guest_new_id, "zwp_text_input_manager_v3".to_string());
            } else {
                error!("zwp_text_input_manager_v3 bound but host v1 manager not found");
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
            Self::enqueue_bind(
                ctx,
                registry_host_id,
                _name,
                interface,
                *_version,
                host_new_id,
            );
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
    fn test_internal_globals_bound_once_across_registries() {
        let mut handler = RegistryHandler;
        let mut ctx = Context::new_for_test(true, false, vec![]);
        let globals = [
            (1, "wl_shm", 1, Action::Forward),
            (2, "zwp_linux_dmabuf_v1", 4, Action::Forward),
            (3, "zwp_text_input_manager_v1", 1, Action::Drop),
            (4, "zcr_text_input_extension_v1", 1, Action::Drop),
            (5, "zcr_keyboard_extension_v1", 2, Action::Drop),
            (6, "zaura_shell", 40, Action::Drop),
        ];

        for registry_host_id in [10u32, 11u32] {
            ctx.last_sender_id = registry_host_id;
            for &(name, iface, ver, expected_action) in &globals {
                assert_eq!(
                    handler.on_global(&mut ctx, name, iface, ver),
                    expected_action
                );
            }
        }

        assert_eq!(ctx.client_to_host_queue.len(), 6);
        assert_eq!(ctx.host_to_client_queue.len(), 2); // v3 advertisement per registry
    }
}
