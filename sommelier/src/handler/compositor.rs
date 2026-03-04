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

pub struct CompositorHandler;

impl WlCompositorHandler for CompositorHandler {}

impl WlSurfaceHandler for CompositorHandler {
    fn on_destroy(&mut self, ctx: &mut Context) -> Action {
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
