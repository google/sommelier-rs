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

use crate::protocols::aura_shell::zaura_shell::{
    REQ_GET_AURA_SURFACE, REQ_GET_AURA_TOPLEVEL_FOR_XDG_TOPLEVEL,
};
use crate::protocols::aura_shell::zaura_surface::REQ_RELEASE as REQ_RELEASE_AURA_SURFACE;
use crate::protocols::aura_shell::zaura_surface::REQ_SET_APPLICATION_ID;
use crate::protocols::aura_shell::zaura_toplevel::REQ_RELEASE as REQ_RELEASE_AURA_TOPLEVEL;
use crate::protocols::wayland::wl_compositor::WlCompositorHandler;
use crate::protocols::wayland::wl_region::WlRegionHandler;
use crate::protocols::wayland::wl_subcompositor::WlSubcompositorHandler;
use crate::protocols::wayland::wl_subsurface::WlSubsurfaceHandler;
use crate::protocols::wayland::wl_surface::WlSurfaceHandler;
use crate::state::{Context, OutputState, SurfaceState};
use crate::wire::Action;
use log::debug;
use std::cmp;
use std::ptr;

pub struct CompositorHandler;

impl WlCompositorHandler for CompositorHandler {}

/// Return the host-side `zaura_surface` associated with a guest wl_surface,
/// creating it lazily when the host advertises `zaura_shell`.
///
/// The app-id path used to own this allocation, but shortcut handling also
/// needs an Aura object for clients that never call `xdg_toplevel.set_app_id`.
/// Keeping creation in one helper makes both paths use the same mapping and
/// cleanup rules.
pub(crate) fn ensure_zaura_surface(ctx: &mut Context, wl_surface_guest_id: u32) -> Option<u32> {
    let wl_surface_host_id = ctx.shadow_table.get_host_id(wl_surface_guest_id)?;

    if let Some(&existing_zaura_id) = ctx.wl_surface_to_zaura_surface.get(&wl_surface_host_id) {
        return Some(existing_zaura_id);
    }

    let zaura_shell_host_id = ctx.host_zaura_shell_id?;
    let zaura_surface_host_id = ctx.shadow_table.allocate_host_id();
    ctx.shadow_table
        .track_host_interface(zaura_surface_host_id, "zaura_surface".to_string());

    let mut builder = crate::wire::MessageBuilder::new();
    builder.write_u32(zaura_surface_host_id);
    builder.write_u32(wl_surface_host_id);

    let msg = builder.build_message(zaura_shell_host_id, REQ_GET_AURA_SURFACE);
    ctx.client_to_host_queue.push((msg, Vec::new()));
    ctx.wl_surface_to_zaura_surface
        .insert(wl_surface_host_id, zaura_surface_host_id);

    log::debug!(
        "Created zaura_surface={} for guest wl_surface={} (host wl_surface={})",
        zaura_surface_host_id,
        wl_surface_guest_id,
        wl_surface_host_id
    );
    Some(zaura_surface_host_id)
}

/// Return the host-side `zaura_toplevel` associated with a guest
/// `xdg_toplevel`, creating it lazily once the window is active.
pub(crate) fn ensure_zaura_toplevel(ctx: &mut Context, xdg_toplevel_guest_id: u32) -> Option<u32> {
    if let Some(&existing_zaura_id) = ctx
        .xdg_toplevel_to_zaura_toplevel
        .get(&xdg_toplevel_guest_id)
    {
        return Some(existing_zaura_id);
    }

    let Some(xdg_toplevel_host_id) = ctx.shadow_table.get_host_id(xdg_toplevel_guest_id) else {
        log::debug!(
            "Cannot create zaura_toplevel for guest xdg_toplevel={}: host ID mapping not ready",
            xdg_toplevel_guest_id
        );
        return None;
    };
    let Some(zaura_shell_host_id) = ctx.host_zaura_shell_id else {
        log::debug!(
            "Cannot create zaura_toplevel for guest xdg_toplevel={}: zaura_shell not bound",
            xdg_toplevel_guest_id
        );
        return None;
    };
    if ctx.host_zaura_shell_version < 29 {
        log::debug!(
            "Cannot create zaura_toplevel for guest xdg_toplevel={}: zaura_shell version {} < 29",
            xdg_toplevel_guest_id,
            ctx.host_zaura_shell_version
        );
        return None;
    }

    let zaura_toplevel_host_id = ctx.shadow_table.allocate_host_id();
    ctx.shadow_table
        .track_host_interface(zaura_toplevel_host_id, "zaura_toplevel".to_string());

    let mut builder = crate::wire::MessageBuilder::new();
    builder.write_u32(zaura_toplevel_host_id);
    builder.write_u32(xdg_toplevel_host_id);
    let msg = builder.build_message(zaura_shell_host_id, REQ_GET_AURA_TOPLEVEL_FOR_XDG_TOPLEVEL);
    ctx.client_to_host_queue.push((msg, Vec::new()));
    ctx.xdg_toplevel_to_zaura_toplevel
        .insert(xdg_toplevel_guest_id, zaura_toplevel_host_id);

    // Screen-coordinate bounds are required for our grid layout. The host
    // protocol requires this request before the first surface commit.
    let msg = crate::wire::MessageBuilder::new().build_message(
        zaura_toplevel_host_id,
        crate::protocols::aura_shell::zaura_toplevel::REQ_SET_SUPPORTS_SCREEN_COORDINATES,
    );
    ctx.client_to_host_queue.push((msg, Vec::new()));

    log::debug!(
        "Created zaura_toplevel={} for guest xdg_toplevel={} (host xdg_toplevel={})",
        zaura_toplevel_host_id,
        xdg_toplevel_guest_id,
        xdg_toplevel_host_id
    );
    Some(zaura_toplevel_host_id)
}

impl WlSurfaceHandler for CompositorHandler {
    fn on_destroy(&mut self, ctx: &mut Context) -> Action {
        let wl_surface_guest_id = ctx.last_sender_id;
        // Clean up any host-side zaura_surface we created for this wl_surface.
        if let Some(wl_surface_host_id) = ctx.shadow_table.get_host_id(wl_surface_guest_id) {
            if let Some(zaura_surface_host_id) =
                ctx.wl_surface_to_zaura_surface.remove(&wl_surface_host_id)
            {
                if ctx.host_zaura_shell_version >= 38 {
                    let builder = crate::wire::MessageBuilder::new();
                    let msg =
                        builder.build_message(zaura_surface_host_id, REQ_RELEASE_AURA_SURFACE);
                    ctx.client_to_host_queue.push((msg, Vec::new()));
                }
                ctx.shadow_table
                    .remove_host_interface(zaura_surface_host_id);
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

impl crate::protocols::wayland::wl_output::WlOutputHandler for CompositorHandler {
    fn on_mode(
        &mut self,
        ctx: &mut Context,
        flags: u32,
        width: i32,
        height: i32,
        _refresh: i32,
    ) -> Action {
        // WL_OUTPUT_MODE_CURRENT is bit 0. Keep the most recent mode when a
        // compositor sends a mode list before marking the active mode.
        if flags & 1 != 0 || !ctx.output_states.contains_key(&ctx.last_sender_id) {
            let output = ctx
                .output_states
                .entry(ctx.last_sender_id)
                .or_insert_with(OutputState::default);
            output.mode_width = width;
            output.mode_height = height;
            log::debug!(
                "wl_output {} mode={}x{} flags={:#x}",
                ctx.last_sender_id,
                width,
                height,
                flags
            );
        }
        Action::Forward
    }

    fn on_scale(&mut self, ctx: &mut Context, factor: i32) -> Action {
        let output = ctx
            .output_states
            .entry(ctx.last_sender_id)
            .or_insert_with(OutputState::default);
        output.scale = factor;
        log::debug!("wl_output {} scale={}", ctx.last_sender_id, factor);
        Action::Forward
    }
}

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
            // Generated dispatch normally installs the guest→host mapping
            // after this callback, so the proxy retries creation immediately
            // after dispatch.  Keep this opportunistic attempt for callers
            // that have already installed the mapping (and for idempotence).
            let _ = ensure_zaura_toplevel(ctx, id);
        }
        Action::Forward
    }
}

impl crate::protocols::xdg_shell::xdg_toplevel::XdgToplevelHandler for CompositorHandler {
    fn on_destroy(&mut self, ctx: &mut Context) -> Action {
        let xdg_toplevel_id = ctx.last_sender_id;
        ctx.xdg_toplevel_to_wl_surface.remove(&xdg_toplevel_id);
        if let Some(zaura_toplevel_host_id) =
            ctx.xdg_toplevel_to_zaura_toplevel.remove(&xdg_toplevel_id)
        {
            if ctx.host_zaura_shell_version >= 38 {
                let msg = crate::wire::MessageBuilder::new()
                    .build_message(zaura_toplevel_host_id, REQ_RELEASE_AURA_TOPLEVEL);
                ctx.client_to_host_queue.push((msg, Vec::new()));
            }
            ctx.shadow_table
                .remove_host_interface(zaura_toplevel_host_id);
        }
        Action::Forward
    }

    fn on_set_app_id(&mut self, ctx: &mut Context, app_id: &String) -> Action {
        let xdg_toplevel_id = ctx.last_sender_id;

        // Resolve xdg_toplevel → wl_surface (guest) → wl_surface (host).
        if let Some(&wl_surface_guest_id) = ctx.xdg_toplevel_to_wl_surface.get(&xdg_toplevel_id) {
            if let Some(zaura_surface_host_id) = ensure_zaura_surface(ctx, wl_surface_guest_id) {
                if ctx.host_zaura_shell_version >= 5 {
                    // ChromeOS's Crostini security delegate currently ignores
                    // arbitrary `set_window_bounds` requests for guest
                    // windows. An opt-in experiment can mark the host surface
                    // as an ARC window, for which Exo permits bounds changes.
                    // This also applies ARC-specific host properties, so keep
                    // it disabled by default.
                    let formatted_app_id = if ctx.window_bounds_as_arc {
                        "org.chromium.arc.2147483647".to_string()
                    } else {
                        format!(
                            "org.chromium.guest_os.{}.wayland.{}",
                            ctx.vm_identifier, app_id
                        )
                    };

                    let mut builder = crate::wire::MessageBuilder::new();
                    builder.write_string(&formatted_app_id);

                    let msg = builder.build_message(zaura_surface_host_id, REQ_SET_APPLICATION_ID);
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

impl crate::protocols::aura_shell::zaura_toplevel::ZauraToplevelHandler for CompositorHandler {
    fn on_configure(
        &mut self,
        ctx: &mut Context,
        _x: i32,
        _y: i32,
        width: i32,
        height: i32,
        states: &[u8],
    ) -> Action {
        let host_zaura_toplevel_id = ctx.last_sender_id;
        let Some((&guest_xdg_toplevel_id, _)) = ctx
            .xdg_toplevel_to_zaura_toplevel
            .iter()
            .find(|(_, &host_id)| host_id == host_zaura_toplevel_id)
        else {
            log::debug!(
                "Dropping zaura_toplevel.configure for unknown host object {}",
                host_zaura_toplevel_id
            );
            return Action::Drop;
        };

        // Aura configure carries the same width/height/state information as
        // xdg_toplevel.configure, plus screen-space x/y. The guest xdg-shell
        // protocol has no x/y fields, so forward the compatible subset.
        let mut builder = crate::wire::MessageBuilder::new();
        builder.write_i32(width);
        builder.write_i32(height);
        builder.write_array(states);
        let msg = builder.build_message(
            guest_xdg_toplevel_id,
            crate::protocols::xdg_shell::xdg_toplevel::EVT_CONFIGURE,
        );
        ctx.host_to_client_queue.push((msg, Vec::new()));
        log::debug!(
            "Translated zaura_toplevel.configure host={} -> guest xdg_toplevel={} size={}x{}",
            host_zaura_toplevel_id,
            guest_xdg_toplevel_id,
            width,
            height
        );

        // The Aura object is internal; never let generated dispatch try to
        // translate its sender ID as a normal host/guest mapping.
        Action::Drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::aura_shell::zaura_shell::{
        REQ_GET_AURA_SURFACE, REQ_GET_AURA_TOPLEVEL_FOR_XDG_TOPLEVEL,
    };
    use crate::protocols::aura_shell::zaura_surface::REQ_RELEASE;
    use crate::protocols::aura_shell::zaura_surface::REQ_SET_APPLICATION_ID;
    use crate::protocols::aura_shell::zaura_toplevel::{
        REQ_RELEASE as REQ_RELEASE_AURA_TOPLEVEL, REQ_SET_SUPPORTS_SCREEN_COORDINATES,
    };
    use crate::protocols::xdg_shell::xdg_surface::XdgSurfaceHandler;
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
        ctx.shadow_table.map_id(xdg_toplevel_id, 401);
        ctx.host_zaura_shell_id = Some(zaura_shell_host);
        ctx.host_zaura_shell_version = 38;
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
        assert_eq!(msg_opcode(msg0), REQ_GET_AURA_SURFACE);

        let msg1 = &ctx.client_to_host_queue[1].0;
        assert_eq!(msg_opcode(msg1), REQ_SET_APPLICATION_ID);
        let payload = &msg1[8..];
        let str_len = u32::from_ne_bytes(payload[0..4].try_into().unwrap()) as usize;
        let app_id_str = String::from_utf8(payload[4..4 + str_len - 1].to_vec()).unwrap();
        assert!(app_id_str.starts_with("org.chromium.guest_os."));
        assert!(app_id_str.ends_with(".wayland.my_app"));
    }

    #[test]
    fn get_toplevel_creates_screen_coordinate_aura_child() {
        let (mut ctx, _xdg_toplevel_id, zaura_shell_host, _wl_surface_host) = setup_ctx();
        let xdg_surface_id = 500u32;
        ctx.shadow_table.map_id(600, 601);
        ctx.last_sender_id = xdg_surface_id;

        let mut handler = CompositorHandler;
        let action = handler.on_get_toplevel(&mut ctx, 600);
        assert_eq!(action, Action::Forward);
        assert_eq!(
            ctx.xdg_toplevel_to_zaura_toplevel.get(&600),
            Some(&2),
            "first free internal ID should be used for aura_toplevel"
        );
        assert_eq!(ctx.client_to_host_queue.len(), 2);
        assert_eq!(msg_sender(&ctx.client_to_host_queue[0].0), zaura_shell_host);
        assert_eq!(
            msg_opcode(&ctx.client_to_host_queue[0].0),
            REQ_GET_AURA_TOPLEVEL_FOR_XDG_TOPLEVEL
        );
        assert_eq!(msg_sender(&ctx.client_to_host_queue[1].0), 2,);
        assert_eq!(
            msg_opcode(&ctx.client_to_host_queue[1].0),
            REQ_SET_SUPPORTS_SCREEN_COORDINATES
        );
        assert_eq!(
            ctx.xdg_toplevel_to_wl_surface.get(&600),
            Some(&100),
            "xdg_toplevel must remain associated with its wl_surface"
        );
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
        assert_eq!(
            msg_opcode(&ctx.client_to_host_queue[0].0),
            REQ_SET_APPLICATION_ID
        );
        assert_eq!(
            msg_sender(&ctx.client_to_host_queue[0].0),
            zaura_surface_host
        );
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
    fn set_app_id_noop_when_version_below_5() {
        let (mut ctx, xdg_toplevel_id, _zaura_shell_host, wl_surface_host) = setup_ctx();
        ctx.host_zaura_shell_version = 4;
        ctx.last_sender_id = xdg_toplevel_id;

        let mut handler = CompositorHandler;
        let action = handler.on_set_app_id(&mut ctx, &"old_host".to_string());
        assert_eq!(action, Action::Forward);

        // get_aura_surface is sent (always available), but set_application_id is skipped.
        assert_eq!(ctx.client_to_host_queue.len(), 1);
        assert_eq!(
            msg_opcode(&ctx.client_to_host_queue[0].0),
            REQ_GET_AURA_SURFACE
        );

        // zaura_surface should still be tracked for cleanup
        assert!(ctx
            .wl_surface_to_zaura_surface
            .contains_key(&wl_surface_host));
    }

    #[test]
    fn xdg_toplevel_destroy_cleans_up_map() {
        let (mut ctx, xdg_toplevel_id, _zaura_shell_host, _wl_surface_host) = setup_ctx();

        let mut handler = CompositorHandler;
        ctx.last_sender_id = xdg_toplevel_id;

        assert!(ctx
            .xdg_toplevel_to_wl_surface
            .contains_key(&xdg_toplevel_id));

        ctx.last_sender_id = xdg_toplevel_id;
        let action = XdgToplevelHandler::on_destroy(&mut handler, &mut ctx);
        assert_eq!(action, Action::Forward);

        assert!(!ctx
            .xdg_toplevel_to_wl_surface
            .contains_key(&xdg_toplevel_id));
    }

    #[test]
    fn xdg_toplevel_destroy_releases_aura_child() {
        let (mut ctx, xdg_toplevel_id, _zaura_shell_host, _wl_surface_host) = setup_ctx();
        ctx.xdg_toplevel_to_zaura_toplevel
            .insert(xdg_toplevel_id, 77);
        ctx.shadow_table
            .track_host_interface(77, "zaura_toplevel".to_string());

        let mut handler = CompositorHandler;
        ctx.last_sender_id = xdg_toplevel_id;
        let action = XdgToplevelHandler::on_destroy(&mut handler, &mut ctx);
        assert_eq!(action, Action::Forward);
        let release = ctx
            .client_to_host_queue
            .iter()
            .find(|(msg, _)| msg_sender(msg) == 77);
        assert!(release.is_some());
        assert_eq!(msg_opcode(&release.unwrap().0), REQ_RELEASE_AURA_TOPLEVEL);
        assert!(ctx.shadow_table.get_host_interface(77).is_none());
    }

    #[test]
    fn aura_configure_is_translated_to_guest_xdg_configure() {
        let (mut ctx, xdg_toplevel_id, _zaura_shell_host, _wl_surface_host) = setup_ctx();
        let zaura_toplevel_host_id = 77u32;
        ctx.xdg_toplevel_to_zaura_toplevel
            .insert(xdg_toplevel_id, zaura_toplevel_host_id);
        ctx.last_sender_id = zaura_toplevel_host_id;

        let states = [1u8, 2, 3, 4];
        let mut handler = CompositorHandler;
        let action =
            crate::protocols::aura_shell::zaura_toplevel::ZauraToplevelHandler::on_configure(
                &mut handler,
                &mut ctx,
                10,
                20,
                1920,
                1080,
                &states,
            );
        assert_eq!(action, Action::Drop);
        assert_eq!(ctx.host_to_client_queue.len(), 1);

        let (msg, fds) = &ctx.host_to_client_queue[0];
        assert!(fds.is_empty());
        assert_eq!(msg_sender(msg), xdg_toplevel_id);
        assert_eq!(
            msg_opcode(msg),
            crate::protocols::xdg_shell::xdg_toplevel::EVT_CONFIGURE
        );

        let mut expected_payload = Vec::new();
        expected_payload.extend_from_slice(&1920i32.to_ne_bytes());
        expected_payload.extend_from_slice(&1080i32.to_ne_bytes());
        expected_payload.extend_from_slice(&4u32.to_ne_bytes());
        expected_payload.extend_from_slice(&states);
        assert_eq!(&msg[8..], &expected_payload);
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
            .find(|(msg, _)| msg_opcode(msg) == REQ_RELEASE);
        assert!(release_msg.is_some());
        assert_eq!(msg_sender(&release_msg.unwrap().0), zaura_surface_host);

        assert!(!ctx
            .wl_surface_to_zaura_surface
            .contains_key(&wl_surface_host));
    }
}
