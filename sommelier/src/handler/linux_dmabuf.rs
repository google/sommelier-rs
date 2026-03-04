use crate::protocols::linux_dmabuf_v1::zwp_linux_buffer_params_v1;
use crate::protocols::linux_dmabuf_v1::zwp_linux_dmabuf_feedback_v1;
use crate::protocols::linux_dmabuf_v1::zwp_linux_dmabuf_v1;
use crate::state::{Context, PendingParam};
use crate::wire::{Action, MessageBuilder};
use log::{debug, error};
use std::os::fd::BorrowedFd;
use std::os::unix::io::{IntoRawFd, RawFd};

pub struct LinuxDmabufHandler;

impl LinuxDmabufHandler {
    fn process_params(
        &self,
        ctx: &mut Context,
        params_id: u32,
        _width: i32,
        _height: i32,
        _format: u32,
        send_create: impl FnOnce(&mut Context, u32),
    ) {
        if let Some(params) = ctx.pending_params.remove(&params_id) {
            let host_id = if let Some(id) = ctx.shadow_table.get_host_id(params_id) {
                id
            } else {
                error!("Unknown host ID for params {}", params_id);
                for p in params {
                    unsafe { libc::close(p.fd) };
                }
                return;
            };

            // Send ADDs
            for p in params {
                let mut builder = MessageBuilder::new();

                // PASS THE GUEST'S EXACT METADATA. NO MINIGBM OVERRIDES.
                builder.write_u32(p.plane_idx);
                builder.write_u32(p.offset);
                builder.write_u32(p.stride);
                builder.write_u32(p.modifier_hi);
                builder.write_u32(p.modifier_lo);

                let mut full_msg = Vec::new();
                full_msg.extend_from_slice(&host_id.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = (len << 16) | (zwp_linux_buffer_params_v1::REQ_ADD as u32);
                full_msg.extend_from_slice(&word2.to_ne_bytes());
                full_msg.extend_from_slice(&builder.payload);

                ctx.client_to_host_queue.push((full_msg, vec![p.fd]));
            }

            // Send CREATE
            send_create(ctx, host_id);
        }
    }
}

impl zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1Handler for LinuxDmabufHandler {
    fn on_destroy(&mut self, _ctx: &mut Context) -> Action {
        Action::Forward
    }

    fn on_create_params(&mut self, ctx: &mut Context, params_id: u32) -> Action {
        ctx.pending_params.insert(params_id, Vec::new());
        Action::Forward
    }

    fn on_get_default_feedback(&mut self, _ctx: &mut Context, _id: u32) -> Action {
        Action::Forward
    }

    fn on_get_surface_feedback(&mut self, _ctx: &mut Context, _id: u32, _surface: u32) -> Action {
        Action::Forward
    }

    fn on_format(&mut self, ctx: &mut Context, format: u32) -> Action {
        ctx.supported_formats.insert(format);

        if let Some(internal_id) = ctx.host_dmabuf_id {
            if ctx.last_sender_id == internal_id {
                return Action::Drop;
            }
        }

        // if format == 0x34324241 || format == 0x34324258 {
        //     return Action::Drop;
        // }

        Action::Forward
    }

    fn on_modifier(
        &mut self,
        ctx: &mut Context,
        format: u32,
        modifier_hi: u32,
        modifier_lo: u32,
    ) -> Action {
        let modifier = ((modifier_hi as u64) << 32) | (modifier_lo as u64);
        debug!(
            "Host advertises format: {:#010x}, modifier: {:#018x}",
            format, modifier
        );
        // We forward all modifiers to the guest.
        // We also track them, though this set is currently unused.
        ctx.supported_formats.insert(format);

        if let Some(internal_id) = ctx.host_dmabuf_id {
            if ctx.last_sender_id == internal_id {
                return Action::Drop;
            }
        }
        // if format == 0x34324241 || format == 0x34324258 {
        //     return Action::Drop;
        // }
        // if modifier == 0 || modifier == 0x00ffffffffffffff {
        //     return Action::Drop;
        // }

        // Force linear buffers (0) or invalid (0x00ffffffffffffff) to avoid
        // garbled output from mismatched host tiling and guest linear rendering.
        // guest driver does not support complicated hw-specific tiling
        // if modifier != 0 && modifier != 0x00ffffffffffffff {
        //     return Action::Drop;
        // }

        Action::Forward
    }
}

impl zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1Handler for LinuxDmabufHandler {
    fn on_main_device(&mut self, ctx: &mut Context, _device: &[u8]) -> Action {
        let guest_id = match ctx.shadow_table.get_guest_id(ctx.last_sender_id) {
            Some(id) => id,
            None => return Action::Drop,
        };

        // Hardcoded dev_t for /dev/dri/renderD128
        let dev_id_bytes: [u8; 8] = [0x80, 0xE2, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

        let mut builder = MessageBuilder::new();
        builder.write_array(&dev_id_bytes);

        let mut full_msg = Vec::new();
        full_msg.extend_from_slice(&guest_id.to_ne_bytes());
        let len = (builder.payload.len() + 8) as u32;
        let word2 = (len << 16) | (zwp_linux_dmabuf_feedback_v1::EVT_MAIN_DEVICE as u32);
        full_msg.extend_from_slice(&word2.to_ne_bytes());
        full_msg.extend_from_slice(&builder.payload);

        ctx.host_to_client_queue.push((full_msg, Vec::new()));
        Action::Drop
    }

    fn on_tranche_target_device(&mut self, ctx: &mut Context, _device: &[u8]) -> Action {
        let guest_id = match ctx.shadow_table.get_guest_id(ctx.last_sender_id) {
            Some(id) => id,
            None => return Action::Drop,
        };

        // Hardcoded dev_t for /dev/dri/renderD128
        let dev_id_bytes: [u8; 8] = [0x80, 0xE2, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

        let mut builder = MessageBuilder::new();
        builder.write_array(&dev_id_bytes);

        let mut full_msg = Vec::new();
        full_msg.extend_from_slice(&guest_id.to_ne_bytes());
        let len = (builder.payload.len() + 8) as u32;
        let word2 = (len << 16) | (zwp_linux_dmabuf_feedback_v1::EVT_TRANCHE_TARGET_DEVICE as u32);
        full_msg.extend_from_slice(&word2.to_ne_bytes());
        full_msg.extend_from_slice(&builder.payload);

        ctx.host_to_client_queue.push((full_msg, Vec::new()));
        Action::Drop
    }

    fn on_format_table(&mut self, ctx: &mut Context, fd: RawFd, size: u32) -> Action {
        let guest_id = match ctx.shadow_table.get_guest_id(ctx.last_sender_id) {
            Some(id) => id,
            None => return Action::Drop,
        };

        let host_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size as usize,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };

        if host_ptr == libc::MAP_FAILED {
            return Action::Drop;
        }

        let entry_count = (size / 16) as usize;
        let host_entries =
            unsafe { std::slice::from_raw_parts(host_ptr as *const u32, entry_count * 4) };

        let mut filtered_entries: Vec<u32> = Vec::new();
        let mut index_map = std::collections::HashMap::new();

        let mut get_or_insert = |format: u32, mod_hi: u32, mod_lo: u32| -> u16 {
            for (i, chunk) in filtered_entries.chunks(4).enumerate() {
                if chunk[0] == format && chunk[2] == mod_hi && chunk[3] == mod_lo {
                    return i as u16;
                }
            }
            let idx = (filtered_entries.len() / 4) as u16;
            filtered_entries.push(format);
            filtered_entries.push(0);
            filtered_entries.push(mod_hi);
            filtered_entries.push(mod_lo);
            idx
        };

        for i in 0..entry_count {
            let format = host_entries[i * 4];
            let mod_hi = host_entries[i * 4 + 2];
            let mod_lo = host_entries[i * 4 + 3];

            // // Hide AB24 (RGBA8888) and XB24 (RGBX8888) to prevent Chromium panics,
            // // forcing it to fall back to AR24 (BGRA_8888).
            // if format == 0x34324241 || format == 0x34324258 {
            //     continue; // Skips adding to index_map, effectively dropping it from tranches
            // }
            let modifier: u64 = (mod_hi as u64) << 32 | mod_lo as u64;
            // if modifier != 0 && modifier != 0x00ffffffffffffff {
            //     continue;
            // }

            if modifier == 0 {
                continue;
            }

            // KEEP the host's true modifier! Do NOT force linear.
            // This allows Chrome to see Tiled modifiers and successfully
            // allocate buffers with the Texturing usage flag.
            let new_idx = get_or_insert(format, mod_hi, mod_lo);

            // Map the host's format index to our new index
            index_map.insert(i as u16, new_idx);
        }
        unsafe {
            libc::munmap(host_ptr, size as usize);
        }
        ctx.feedback_index_maps.insert(guest_id, index_map);

        let memfd_name = std::ffi::CString::new("sommelier-format-table").unwrap();
        let new_fd = unsafe { libc::memfd_create(memfd_name.as_ptr(), libc::MFD_CLOEXEC) };
        if new_fd < 0 {
            return Action::Drop;
        }

        let new_size = (filtered_entries.len() * 4) as u32;
        unsafe {
            libc::ftruncate(new_fd, new_size as i64);
            let new_ptr = libc::mmap(
                std::ptr::null_mut(),
                new_size as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                new_fd,
                0,
            );
            std::ptr::copy_nonoverlapping(
                filtered_entries.as_ptr(),
                new_ptr as *mut u32,
                filtered_entries.len(),
            );
            libc::munmap(new_ptr, new_size as usize);
        }

        let mut builder = MessageBuilder::new();
        builder.write_u32(new_size);

        let mut full_msg = Vec::new();
        full_msg.extend_from_slice(&guest_id.to_ne_bytes());
        let len = (builder.payload.len() + 8) as u32;
        let word2 = (len << 16) | (zwp_linux_dmabuf_feedback_v1::EVT_FORMAT_TABLE as u32);
        full_msg.extend_from_slice(&word2.to_ne_bytes());
        full_msg.extend_from_slice(&builder.payload);

        ctx.host_to_client_queue.push((full_msg, vec![new_fd]));

        Action::Drop
    }

    fn on_tranche_formats(&mut self, ctx: &mut Context, indices: &[u8]) -> Action {
        let guest_id = match ctx.shadow_table.get_guest_id(ctx.last_sender_id) {
            Some(id) => id,
            None => return Action::Drop,
        };

        let index_map = match ctx.feedback_index_maps.get(&guest_id) {
            Some(map) => map,
            None => return Action::Drop,
        };

        let mut new_indices = Vec::new();
        for i in 0..(indices.len() / 2) {
            let idx = u16::from_ne_bytes([indices[i * 2], indices[i * 2 + 1]]);
            if let Some(&new_idx) = index_map.get(&idx) {
                new_indices.extend_from_slice(&new_idx.to_ne_bytes());
            }
        }

        let mut builder = MessageBuilder::new();
        builder.write_array(&new_indices);

        let mut full_msg = Vec::new();
        full_msg.extend_from_slice(&guest_id.to_ne_bytes());
        let len = (builder.payload.len() + 8) as u32;
        let word2 = (len << 16) | (zwp_linux_dmabuf_feedback_v1::EVT_TRANCHE_FORMATS as u32);
        full_msg.extend_from_slice(&word2.to_ne_bytes());
        full_msg.extend_from_slice(&builder.payload);

        ctx.host_to_client_queue.push((full_msg, Vec::new()));

        Action::Drop
    }
}

impl zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1Handler for LinuxDmabufHandler {
    fn on_destroy(&mut self, ctx: &mut Context) -> Action {
        let id = ctx.last_sender_id;
        if let Some(params) = ctx.pending_params.remove(&id) {
            for p in params {
                unsafe { libc::close(p.fd) };
            }
        }
        Action::Forward
    }

    fn on_add(
        &mut self,
        ctx: &mut Context,
        fd: RawFd,
        plane_idx: u32,
        offset: u32,
        stride: u32,
        modifier_hi: u32,
        modifier_lo: u32,
    ) -> Action {
        let dup_fd = match nix::unistd::dup(unsafe { BorrowedFd::borrow_raw(fd) }) {
            Ok(d) => d.into_raw_fd(),
            Err(e) => {
                error!("Failed to dup FD for add: {}", e);
                return Action::Drop;
            }
        };

        let modifier = ((modifier_hi as u64) << 32) | (modifier_lo as u64);

        // --- DEBUG PRINT ---
        let format_hex = format!("{:x}", modifier);
        let mod_str = if modifier == 0 {
            "LINEAR".to_string()
        } else if modifier == 0x00ffffffffffffff {
            "INVALID (Implicit)".to_string()
        } else {
            format!("EXPLICIT (0x{})", format_hex)
        };

        eprintln!(
            "[PROXY] Client creating buffer -> Plane: {}, Stride: {}, Modifier: {}",
            plane_idx, stride, mod_str
        );

        let params_id = ctx.last_sender_id;
        if let Some(list) = ctx.pending_params.get_mut(&params_id) {
            list.push(PendingParam {
                fd: dup_fd,
                plane_idx,
                offset,
                stride,
                modifier_hi,
                modifier_lo,
            });
        } else {
            // If create_params wasn't tracked (e.g. before connection), we can't buffer.
            // But we should have tracked it.
            error!("Unknown params ID {} in add", params_id);
            unsafe { libc::close(dup_fd) };
        }

        Action::Drop
    }

    fn on_create(
        &mut self,
        ctx: &mut Context,
        width: i32,
        height: i32,
        format: u32,
        flags: u32,
    ) -> Action {
        self.process_params(
            ctx,
            ctx.last_sender_id,
            width,
            height,
            format,
            |ctx, host_id| {
                let mut builder = MessageBuilder::new();
                builder.write_i32(width);
                builder.write_i32(height);
                builder.write_u32(format);
                builder.write_u32(flags);

                let mut full_msg = Vec::new();
                full_msg.extend_from_slice(&host_id.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = (len << 16) | (zwp_linux_buffer_params_v1::REQ_CREATE as u32);
                full_msg.extend_from_slice(&word2.to_ne_bytes());
                full_msg.extend_from_slice(&builder.payload);

                ctx.client_to_host_queue.push((full_msg, Vec::new()));
            },
        );
        Action::Drop
    }

    fn on_create_immed(
        &mut self,
        ctx: &mut Context,
        buffer_id: u32,
        width: i32,
        height: i32,
        format: u32,
        flags: u32,
    ) -> Action {
        // We need to map buffer_id manually because we are dropping the request.
        // Codegen would map it if we forwarded.
        // buffer_id is new_id wl_buffer.
        let host_buffer_id = ctx.shadow_table.allocate_host_id();
        ctx.shadow_table.map_id(buffer_id, host_buffer_id);
        ctx.shadow_table
            .track_interface(buffer_id, "wl_buffer".to_string());

        self.process_params(
            ctx,
            ctx.last_sender_id,
            width,
            height,
            format,
            |ctx, host_id| {
                let mut builder = MessageBuilder::new();
                builder.write_u32(host_buffer_id);
                builder.write_i32(width);
                builder.write_i32(height);
                builder.write_u32(format);
                builder.write_u32(flags);

                let mut full_msg = Vec::new();
                full_msg.extend_from_slice(&host_id.to_ne_bytes());
                let len = (builder.payload.len() + 8) as u32;
                let word2 = (len << 16) | (zwp_linux_buffer_params_v1::REQ_CREATE_IMMED as u32);
                full_msg.extend_from_slice(&word2.to_ne_bytes());
                full_msg.extend_from_slice(&builder.payload);

                ctx.client_to_host_queue.push((full_msg, Vec::new()));
            },
        );
        Action::Drop
    }
}
