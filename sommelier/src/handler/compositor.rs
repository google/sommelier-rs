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
    fn on_get_xdg_surface(&mut self, ctx: &mut Context, _id: u32, _surface: u32) -> Action {
        ctx.xdg_surface_to_wl_surface.insert(_id, _surface);
        Action::Forward
    }
}

impl crate::protocols::xdg_shell::xdg_surface::XdgSurfaceHandler for CompositorHandler {
    fn on_destroy(&mut self, ctx: &mut Context) -> Action {
        let xdg_surface_id = ctx.last_sender_id;
        ctx.xdg_surface_to_wl_surface.remove(&xdg_surface_id);
        Action::Forward
    }

    fn on_get_toplevel(&mut self, ctx: &mut Context, _id: u32) -> Action {
        let xdg_surface_id = ctx.last_sender_id;
        if let Some(&wl_surface_id) = ctx.xdg_surface_to_wl_surface.get(&xdg_surface_id) {
            ctx.xdg_toplevel_to_wl_surface.insert(_id, wl_surface_id);
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

    fn on_set_app_id(&mut self, ctx: &mut Context, _app_id: &String) -> Action {
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
                        ctx.vm_identifier, _app_id
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
                        _app_id,
                        formatted_app_id,
                        zaura_surface_host_id
                    );
                }
            }
        }
        Action::Forward
    }
}
