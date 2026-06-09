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
use crate::state::{BufferState, Context, PoolInner, PoolState};
use crate::wire::{Action, MessageBuilder};
use log::{debug, error, warn};
use std::os::unix::io::{AsRawFd, IntoRawFd, RawFd};
use std::ptr;
use std::sync::{Arc, RwLock};

pub struct ShmHandler;

impl protocols::wayland::wl_shm::WlShmHandler for ShmHandler {
    fn on_format(&mut self, ctx: &mut Context, _format: u32) -> Action {
        if let Some(internal_id) = ctx.host_shm_id {
            if ctx.last_sender_id == internal_id {
                return Action::Drop;
            }
        }
        Action::Forward
    }

    fn on_create_pool(&mut self, ctx: &mut Context, id: u32, fd: RawFd, size: i32) -> Action {
        let pool_id = id;

        debug!("Creating pool, fd={}", fd);

        // Duplicate the FD for PoolState because proxy.rs closes the original 'fd'
        // after this handler returns.
        let pool_fd = unsafe { libc::dup(fd) };
        debug!("Dup result={}", pool_fd);

        if pool_fd < 0 {
            error!("Failed to dup FD for SHM pool");
            return Action::Drop;
        }

        // Mmap the file
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                pool_fd,
                0,
            )
        };
        debug!("Mmap result={:?}", ptr);

        if ptr == libc::MAP_FAILED {
            error!("Failed to mmap SHM pool");
            unsafe {
                libc::close(pool_fd);
            }
            return Action::Drop;
        }

        let pool = Arc::new(PoolState {
            client_fd: pool_fd, // Takes ownership of new FD
            inner: RwLock::new(PoolInner {
                client_ptr: ptr,
                size: size as usize,
            }),
        });

        ctx.pools.insert(pool_id, pool);
        ctx.shadow_table
            .track_interface(pool_id, "wl_shm_pool".to_string());

        Action::Drop
    }
}

impl protocols::wayland::wl_shm_pool::WlShmPoolHandler for ShmHandler {
    fn on_create_buffer(
        &mut self,
        ctx: &mut Context,
        id: u32,
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: u32,
    ) -> Action {
        debug!("Creating buffer id={}", id);
        let pool_id = ctx.last_sender_id;
        let pool = match ctx.pools.get(&pool_id) {
            Some(p) => p.clone(),
            None => {
                warn!("Unknown pool ID: {}", pool_id);
                return Action::Drop;
            }
        };

        debug!("Allocator present={}", ctx.allocator.is_some());
        debug!(
            "VirtWayland channel present={}",
            ctx.virtwayland_channel.is_some()
        );

        // Allocate buffer (GBM or VirtWayland)
        let alloc_res = if let Some(channel) = &ctx.virtwayland_channel {
            let size = (stride as u32) * (height as u32);
            debug!("Allocating VirtWayland buffer: size={}", size);
            match channel.allocate(size) {
                Ok((fd, _alloc_size)) => {
                    // virtwl allocation is a simple SHM-like buffer.
                    // No modifier, offset 0.
                    Some((None, stride as u32, fd, 0, 0, size as u64))
                }
                Err(e) => {
                    error!("Failed to allocate VirtWayland buffer: {}", e);
                    return Action::Drop;
                }
            }
        } else {
            None
        };

        let (bo, bo_stride, dmabuf_fd_owned, _modifier, blob_offset, total_size) =
            if let Some(res) = alloc_res {
                res
            } else if let Some(allocator) = &mut ctx.allocator {
                // Fallback to GBM allocator
                match allocator.allocate(
                    width as u32,
                    height as u32,
                    Self::wl_shm_format_to_drm_format(format),
                ) {
                    Ok(bo) => {
                        let bo_stride = bo.stride().unwrap_or(0);
                        debug!(
                            "Allocated GBM BO: width={}, height={}, stride={}, format={}",
                            width, height, bo_stride, format
                        );

                        let fd = match bo.fd() {
                            Ok(f) => f,
                            Err(e) => {
                                error!("Failed to get FD from BO: {}", e);
                                return Action::Drop;
                            }
                        };

                        // For GBM with LINEAR flag, modifier is likely 0 (LINEAR)
                        // We can try to get it from BO if needed, but for now we default to 0
                        // as that was the behavior and we want to be safe.
                        // If we want to be correct:
                        let modifier: u64 = match bo.modifier() {
                            Ok(m) => m.into(),
                            Err(_) => 0,
                        };

                        debug!("GBM BO modifier: {}", modifier);

                        // FIX: Add offset and calculate size to match the 6-element tuple
                        let offset = 0;
                        let total_size = (bo_stride as u64) * (height as u64);

                        (Some(bo), bo_stride, fd, modifier, offset, total_size)
                    }
                    Err(e) => {
                        error!("Failed to allocate GBM BO: {}", e);
                        return Action::Drop;
                    }
                }
            } else {
                error!("No allocator (VirtGpu or GBM) available");
                return Action::Drop;
            };

        // Create WL_SHM buffer on host
        if let Some(host_wl_shm_id) = ctx.host_shm_id {
            // Map the buffer for SHM synchronization
            let dest_size = total_size as usize;
            let dest_ptr = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    dest_size,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    dmabuf_fd_owned.as_raw_fd(),
                    blob_offset as i64,
                ) as *mut u8
            };

            if dest_ptr as *mut libc::c_void == libc::MAP_FAILED {
                error!(
                    "Failed to mmap buffer DMABUF: {:?}",
                    std::io::Error::last_os_error()
                );
                return Action::Drop;
            }

            let host_pool_id = ctx.shadow_table.allocate_host_id();

            // wl_shm.create_pool(new_id, fd, size)
            let mut builder = MessageBuilder::new();
            builder.write_u32(host_pool_id);
            builder.write_i32(total_size as i32);

            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&host_wl_shm_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = len << 16; // wl_shm.create_pool = 0
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);

            let fd_to_send = match dmabuf_fd_owned.try_clone() {
                Ok(f) => f.into_raw_fd(),
                Err(e) => {
                    error!("Failed to dup FD: {}", e);
                    unsafe {
                        libc::munmap(dest_ptr as *mut libc::c_void, dest_size);
                    }
                    return Action::Drop;
                }
            };
            debug!("Dup for send={} size={}", fd_to_send, total_size);

            ctx.client_to_host_queue.push((full_msg.into(), vec![fd_to_send]));

            // wl_shm_pool.create_buffer(new_id, offset, width, height, stride, format)
            let host_buffer_id = ctx.shadow_table.allocate_host_id();
            let mut builder = MessageBuilder::new();
            builder.write_u32(host_buffer_id);
            builder.write_i32(blob_offset); // offset
            builder.write_i32(width);
            builder.write_i32(height);
            builder.write_i32(bo_stride as i32); // stride
            builder.write_u32(format); // format (SHM format, not DRM format)

            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&host_pool_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = len << 16; // wl_shm_pool.create_buffer = 0
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);

            ctx.client_to_host_queue.push((full_msg.into(), Vec::new()));

            // wl_shm_pool.destroy()
            let builder = MessageBuilder::new();
            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&host_pool_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            let word2 = (len << 16) | 1_u32; // wl_shm_pool.destroy = 1
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);

            ctx.client_to_host_queue.push((full_msg.into(), Vec::new()));

            // Store mapping
            ctx.shadow_table.map_id(id, host_buffer_id);
            ctx.shadow_table
                .track_interface(id, "wl_buffer".to_string());

            // Save buffer state
            ctx.buffers.insert(
                id,
                BufferState {
                    pool: pool.clone(),
                    offset,
                    width,
                    height,
                    stride: stride as u32,
                    format,
                    host_buffer_id,
                    bo,
                    dmabuf_fd: Some(dmabuf_fd_owned),
                    bo_stride, // Store bo_stride
                    dest_ptr,
                    dest_size,
                },
            );
        } else {
            error!("wl_shm not available on host");
            return Action::Drop;
        }

        Action::Drop
    }

    fn on_destroy(&mut self, ctx: &mut Context) -> Action {
        let pool_id = ctx.last_sender_id;
        ctx.pools.remove(&pool_id);
        ctx.shadow_table.remove_id(pool_id);
        Action::Drop
    }

    fn on_resize(&mut self, ctx: &mut Context, size: i32) -> Action {
        let pool_id = ctx.last_sender_id;
        debug!("Resizing pool id={}", pool_id);
        if let Some(pool) = ctx.pools.get(&pool_id) {
            if let Ok(mut inner) = pool.inner.write() {
                debug!(
                    "Resizing pool id={} from {} to {}",
                    pool_id, inner.size, size
                );
                let new_size = size as usize;

                // mremap with MREMAP_MAYMOVE
                let new_ptr = unsafe {
                    libc::mremap(inner.client_ptr, inner.size, new_size, libc::MREMAP_MAYMOVE)
                };

                if new_ptr == libc::MAP_FAILED {
                    error!(
                        "Failed to mremap pool: {:?}",
                        std::io::Error::last_os_error()
                    );
                } else {
                    inner.client_ptr = new_ptr;
                    inner.size = new_size;
                    debug!("Pool resized successfully to {:?}", new_ptr);
                }
            } else {
                error!("Failed to acquire write lock on pool");
            }
        } else {
            warn!("Pool not found for resize: {}", pool_id);
        }
        Action::Drop
    }
}

impl protocols::wayland::wl_buffer::WlBufferHandler for ShmHandler {
    fn on_destroy(&mut self, ctx: &mut Context) -> Action {
        let guest_id = ctx.last_sender_id;
        if let Some(host_id) = ctx.shadow_table.get_host_id(guest_id) {
            let builder = MessageBuilder::new();
            let mut full_msg = Vec::new();
            full_msg.extend_from_slice(&host_id.to_ne_bytes());
            let len = (builder.payload.len() + 8) as u32;
            // REQ_DESTROY is 0 for wl_buffer
            let word2 = len << 16;
            full_msg.extend_from_slice(&word2.to_ne_bytes());
            full_msg.extend_from_slice(&builder.payload);

            ctx.client_to_host_queue.push((full_msg.into(), Vec::new()));
        }

        ctx.buffers.remove(&guest_id);
        ctx.shadow_table.remove_id(guest_id);
        Action::Drop
    }
}

impl ShmHandler {
    fn wl_shm_format_to_drm_format(format: u32) -> u32 {
        match format {
            0 => 0x34325241, // ARGB8888
            1 => 0x34325258, // XRGB8888
            _ => format,
        }
    }
}
