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

use crate::protocols::wayland::wl_compositor::WlCompositorHandler;
use crate::protocols::wayland::wl_region::WlRegionHandler;
use crate::protocols::wayland::wl_subcompositor::WlSubcompositorHandler;
use crate::protocols::wayland::wl_subsurface::WlSubsurfaceHandler;
use crate::protocols::wayland::wl_surface::WlSurfaceHandler;
use crate::state::{Context, SurfaceState};
use crate::wire::Action;
use log::debug;
use std::cmp;
use std::ptr;

/// Opcodes for internally-constructed zaura_shell wire messages.
/// These are stable per Wayland protocol versioning rules (append-only).
/// Source: chromiumos/platform2 aura-shell.xml
///
/// Version requirements:
/// - set_application_id (opcode 4): available since v5
/// - release (opcode 27): available since v38
/// We bind at min(version, 38) in registry.rs, so all opcodes are valid.
const ZAURA_SHELL_GET_AURA_SURFACE: u16 = 0;
const ZAURA_SURFACE_SET_APPLICATION_ID: u16 = 4;
const ZAURA_SURFACE_RELEASE: u16 = 27;

pub struct CompositorHandler;

impl WlCompositorHandler for CompositorHandler {}

impl WlSurfaceHandler for CompositorHandler {
    fn on_destroy(&mut self, ctx: &mut Context) -> Action {
        let wl_surface_guest_id = ctx.last_sender_id;
        // Clean up any host-side zaura_surface we created for this wl_surface.
        if let Some(wl_surface_host_id) = ctx.shadow_table.get_host_id(wl_surface_guest_id) {
            if let Some(zaura_surface_host_id) = ctx.wl_surface_to_zaura_surface.remove(&wl_surface_host_id) {
                let builder = crate::wire::MessageBuilder::new();
                let msg = builder.build_message(
                    zaura_surface_host_id,
                    ZAURA_SURFACE_RELEASE,
                );
                ctx.client_to_host_queue.push((msg, Vec::new()));
                ctx.shadow_table.remove_host_interface(zaura_surface_host_id);
            }
        }
        ctx.surfaces.remove(&ctx.last_sender_id);
        Action::Forward
    }

    fn on_attach(&mut self, ctx: &mut Context, buffer: u32, _x: i32, _y: i32) -> Action {
        let surface_id = ctx.last_sender_id;
        let surface_state = ctx.surfaces.entry(surface_id).or_insert(SurfaceState {
            pending_buffer_id: None,
        });
        surface_state.pending_buffer_id = if buffer == 0 { None } else { Some(buffer) };
        Action::Forward
    }

    fn on_commit(&mut self, ctx: &mut Context) -> Action {
        let surface_id = ctx.last_sender_id;

        let pending_buffer_id = if let Some(surface_state) = ctx.surfaces.get_mut(&surface_id) {
            surface_state.pending_buffer_id.take()
        } else {
            None
        };

        if let Some(buffer_id) = pending_buffer_id {
            // Get buffers mutable reference
            let buffers = &mut ctx.buffers;

            if let Some(buffer) = buffers.get_mut(&buffer_id) {
                // Check if this is a SHM buffer (has a dest_ptr)
                if !buffer.dest_ptr.is_null() {
                    let pool = &buffer.pool;

                    if let Ok(inner) = pool.inner.read() {
                        unsafe {
                            if !inner.client_ptr.is_null() && inner.client_ptr != libc::MAP_FAILED {
                                let src_ptr =
                                    (inner.client_ptr as *const u8).add(buffer.offset as usize);
                                let src_stride = buffer.stride;
                                let height = buffer.height as u32;
                                // let width = buffer.width as u32; // Not strictly needed for loop if we use stride
                                let bo_stride = buffer.bo_stride;
                                let offset = buffer.offset;
                                let pool_size = inner.size;

                                let dst_ptr = buffer.dest_ptr;
                                let dest_size = buffer.dest_size;

                                // Destination stride is the allocated buffer stride (bo_stride)
                                let dst_stride = bo_stride;

                                // The copy length should be the minimum of source and destination strides
                                // to avoid buffer overflows. Typically src_stride (tight) <= dst_stride (aligned).
                                let copy_len = cmp::min(src_stride, dst_stride) as usize;

                                debug!("Copying buffer: dst_stride={}, src_stride={}, height={}, buffer.offset={}, pool_size={}, dest_size={}", 
                                    dst_stride, src_stride, height, offset, pool_size, dest_size);

                                // Always perform row-by-row copy to handle potential stride mismatches
                                // or GPU padding correctly.
                                for i in 0..height {
                                    let src_offset = (i * src_stride) as usize;
                                    let dst_offset = (i * dst_stride) as usize;

                                    // Ensure we don't read/write out of bounds
                                    if (src_offset + copy_len + offset as usize) <= pool_size
                                        && dst_offset + copy_len <= dest_size
                                    {
                                        ptr::copy_nonoverlapping(
                                            src_ptr.add(src_offset),
                                            dst_ptr.add(dst_offset),
                                            copy_len,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Action::Forward
    }
}

impl WlRegionHandler for CompositorHandler {}
impl WlSubcompositorHandler for CompositorHandler {}
impl WlSubsurfaceHandler for CompositorHandler {}

// --- XDG Shell → wl_surface tracking for zaura_shell integration ---
//
// ChromeOS needs a zaura_surface (from zaura_shell) to set the application ID
// that the shelf uses for icon matching. But the app ID arrives via
// xdg_toplevel::set_app_id, which doesn't carry a wl_surface reference.
//
// We bridge this gap by tracking the chain:
//   xdg_wm_base::get_xdg_surface(xdg_surface, wl_surface)
//   xdg_surface::get_toplevel(xdg_toplevel)
// so that when set_app_id fires on an xdg_toplevel, we can resolve back to
// the underlying wl_surface and create/reuse a host zaura_surface on it.

impl crate::protocols::xdg_shell::xdg_wm_base::XdgWmBaseHandler for CompositorHandler {
    fn on_get_xdg_surface(&mut self, ctx: &mut Context, id: u32, surface: u32) -> Action {
        ctx.xdg_surface_to_wl_surface.insert(id, surface);
        Action::Forward
    }
}

impl crate::protocols::xdg_shell::xdg_surface::XdgSurfaceHandler for CompositorHandler {
    fn on_destroy(&mut self, ctx: &mut Context) -> Action {
        let xdg_surface_id = ctx.last_sender_id;
        ctx.xdg_surface_to_wl_surface.remove(&xdg_surface_id);
        Action::Forward
    }

    fn on_get_toplevel(&mut self, ctx: &mut Context, id: u32) -> Action {
        let xdg_surface_id = ctx.last_sender_id;
        if let Some(&wl_surface_id) = ctx.xdg_surface_to_wl_surface.get(&xdg_surface_id) {
            ctx.xdg_toplevel_to_wl_surface.insert(id, wl_surface_id);
        }
        Action::Forward
    }
}

impl crate::protocols::xdg_shell::xdg_toplevel::XdgToplevelHandler for CompositorHandler {
    fn on_destroy(&mut self, ctx: &mut Context) -> Action {
        let xdg_toplevel_id = ctx.last_sender_id;
        ctx.xdg_toplevel_to_wl_surface.remove(&xdg_toplevel_id);
        Action::Forward
    }

    fn on_set_app_id(&mut self, ctx: &mut Context, app_id: &String) -> Action {
        let xdg_toplevel_id = ctx.last_sender_id;

        // Resolve xdg_toplevel → wl_surface (guest) → wl_surface (host).
        if let Some(&wl_surface_guest_id) = ctx.xdg_toplevel_to_wl_surface.get(&xdg_toplevel_id) {
            if let Some(wl_surface_host_id) = ctx.shadow_table.get_host_id(wl_surface_guest_id) {
                // Lazily create a host zaura_surface for this wl_surface, or reuse
                // an existing one. This avoids overhead for surfaces that never
                // set an app ID (subsurfaces, popups, etc.).
                let zaura_surface_host_id = if let Some(&existing_zaura_id) =
                    ctx.wl_surface_to_zaura_surface.get(&wl_surface_host_id)
                {
                    existing_zaura_id
                } else if let Some(zaura_shell_host_id) = ctx.host_zaura_shell_id {
                    let zaura_surface_host_id = ctx.shadow_table.allocate_host_id();
                    ctx.shadow_table.track_host_interface(
                        zaura_surface_host_id,
                        "zaura_surface".to_string(),
                    );

                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_u32(zaura_surface_host_id);
                    builder.write_u32(wl_surface_host_id);

                    let msg = builder.build_message(
                        zaura_shell_host_id,
                        ZAURA_SHELL_GET_AURA_SURFACE,
                    );
                    ctx.client_to_host_queue.push((msg, Vec::new()));

                    ctx.wl_surface_to_zaura_surface
                        .insert(wl_surface_host_id, zaura_surface_host_id);

                    zaura_surface_host_id
                } else {
                    0
                };

                if zaura_surface_host_id != 0 {
                    let formatted_app_id = format!(
                        "org.chromium.guest_os.{}.wayland.{}",
                        ctx.vm_identifier, app_id
                    );

                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_string(&formatted_app_id);

                    let msg = builder.build_message(
                        zaura_surface_host_id,
                        ZAURA_SURFACE_SET_APPLICATION_ID,
                    );
                    ctx.client_to_host_queue.push((msg, Vec::new()));
                    log::debug!(
                        "Set application ID to {} (formatted: {}) on zaura_surface (host_id={})",
                        app_id,
                        formatted_app_id,
                        zaura_surface_host_id
                    );
                }
            }
        }
        Action::Forward
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::xdg_shell::xdg_toplevel::XdgToplevelHandler;
    use crate::state::Context;

    fn msg_sender(msg: &[u8]) -> u32 {
        u32::from_ne_bytes(msg[0..4].try_into().unwrap())
    }

    fn msg_opcode(msg: &[u8]) -> u16 {
        let word2 = u32::from_ne_bytes(msg[4..8].try_into().unwrap());
        (word2 & 0xffff) as u16
    }

    fn setup_ctx() -> (Context, u32, u32, u32) {
        let mut ctx = Context::new_for_test(false, false, vec![]);
        let wl_surface_guest = 100u32;
        let wl_surface_host = 200u32;
        let zaura_shell_host = 300u32;
        let xdg_toplevel_id = 400u32;
        let xdg_surface_id = 500u32;

        ctx.shadow_table.map_id(wl_surface_guest, wl_surface_host);
        ctx.host_zaura_shell_id = Some(zaura_shell_host);
        ctx.xdg_surface_to_wl_surface
            .insert(xdg_surface_id, wl_surface_guest);
        ctx.xdg_toplevel_to_wl_surface
            .insert(xdg_toplevel_id, wl_surface_guest);

        (ctx, xdg_toplevel_id, zaura_shell_host, wl_surface_host)
    }

    #[test]
    fn set_app_id_creates_zaura_surface_and_sets_app_id() {
        let (mut ctx, xdg_toplevel_id, zaura_shell_host, _wl_surface_host) = setup_ctx();
        ctx.last_sender_id = xdg_toplevel_id;

        let mut handler = CompositorHandler;
        let action = handler.on_set_app_id(&mut ctx, &"my_app".to_string());
        assert_eq!(action, Action::Forward);

        assert_eq!(ctx.client_to_host_queue.len(), 2);

        let msg0 = &ctx.client_to_host_queue[0].0;
        assert_eq!(msg_sender(msg0), zaura_shell_host);
        assert_eq!(msg_opcode(msg0), 0);

        let msg1 = &ctx.client_to_host_queue[1].0;
        assert_eq!(msg_opcode(msg1), 4);
        let payload = &msg1[8..];
        let str_len = u32::from_ne_bytes(payload[0..4].try_into().unwrap()) as usize;
        let app_id_str = String::from_utf8(payload[4..4 + str_len - 1].to_vec()).unwrap();
        assert!(app_id_str.starts_with("org.chromium.guest_os."));
        assert!(app_id_str.ends_with(".wayland.my_app"));
    }

    #[test]
    fn set_app_id_reuses_existing_zaura_surface() {
        let (mut ctx, xdg_toplevel_id, _zaura_shell_host, wl_surface_host) = setup_ctx();
        ctx.last_sender_id = xdg_toplevel_id;

        let zaura_surface_host = 99u32;
        ctx.wl_surface_to_zaura_surface
            .insert(wl_surface_host, zaura_surface_host);

        let mut handler = CompositorHandler;
        let action = handler.on_set_app_id(&mut ctx, &"reused".to_string());
        assert_eq!(action, Action::Forward);

        assert_eq!(ctx.client_to_host_queue.len(), 1);
        assert_eq!(msg_opcode(&ctx.client_to_host_queue[0].0), 4);
        assert_eq!(msg_sender(&ctx.client_to_host_queue[0].0), zaura_surface_host);
    }

    #[test]
    fn set_app_id_noop_when_no_zaura_shell() {
        let (mut ctx, xdg_toplevel_id, _zaura_shell_host, _wl_surface_host) = setup_ctx();
        ctx.host_zaura_shell_id = None;
        ctx.last_sender_id = xdg_toplevel_id;

        let mut handler = CompositorHandler;
        let action = handler.on_set_app_id(&mut ctx, &"no_shell".to_string());
        assert_eq!(action, Action::Forward);
        assert_eq!(ctx.client_to_host_queue.len(), 0);
    }

    #[test]
    fn xdg_toplevel_destroy_cleans_up_map() {
        let (mut ctx, xdg_toplevel_id, _zaura_shell_host, _wl_surface_host) = setup_ctx();

        let mut handler = CompositorHandler;
        ctx.last_sender_id = xdg_toplevel_id;

        assert!(ctx.xdg_toplevel_to_wl_surface.contains_key(&xdg_toplevel_id));

        ctx.last_sender_id = xdg_toplevel_id;
        let action = XdgToplevelHandler::on_destroy(&mut handler, &mut ctx);
        assert_eq!(action, Action::Forward);

        assert!(!ctx.xdg_toplevel_to_wl_surface.contains_key(&xdg_toplevel_id));
    }

    #[test]
    fn wl_surface_destroy_releases_zaura_surface() {
        let (mut ctx, xdg_toplevel_id, _zaura_shell_host, wl_surface_host) = setup_ctx();

        ctx.last_sender_id = xdg_toplevel_id;
        let mut handler = CompositorHandler;
        handler.on_set_app_id(&mut ctx, &"app".to_string());

        let zaura_surface_host = *ctx
            .wl_surface_to_zaura_surface
            .get(&wl_surface_host)
            .unwrap();

        let wl_surface_guest = 100u32;
        ctx.last_sender_id = wl_surface_guest;
        let action = WlSurfaceHandler::on_destroy(&mut handler, &mut ctx);
        assert_eq!(action, Action::Forward);

        let release_msg = ctx
            .client_to_host_queue
            .iter()
            .find(|(msg, _)| msg_opcode(msg) == 27);
        assert!(release_msg.is_some());
        assert_eq!(msg_sender(&release_msg.unwrap().0), zaura_surface_host);

        assert!(!ctx.wl_surface_to_zaura_surface.contains_key(&wl_surface_host));
    }
}
