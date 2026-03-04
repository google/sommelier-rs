// Copyright 2026 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::protocols::wayland::wl_data_device;
use crate::protocols::wayland::wl_data_device_manager;
use crate::protocols::wayland::wl_data_offer;
use crate::protocols::wayland::wl_data_source;
use crate::state::Context;
use crate::wire::{Action, MessageBuilder};
use log::{debug, error};
use std::os::unix::io::IntoRawFd;

pub struct DataDeviceHandler;

impl wl_data_device_manager::WlDataDeviceManagerHandler for DataDeviceHandler {
    fn on_create_data_source(&mut self, ctx: &mut Context, id: u32) -> Action {
        debug!("Tracking data source id={}", id);
        // Track the data source
        ctx.shadow_table.map_id(id, id);
        ctx.shadow_table
            .track_interface(id, "wl_data_source".to_string());
        Action::Forward
    }

    fn on_get_data_device(&mut self, ctx: &mut Context, id: u32, _seat: u32) -> Action {
        // Track the data device
        ctx.shadow_table
            .track_interface(id, "wl_data_device".to_string());
        Action::Forward
    }
}

impl wl_data_device::WlDataDeviceHandler for DataDeviceHandler {
    fn on_data_offer(&mut self, ctx: &mut Context, id: u32) -> Action {
        // Track the data offer
        ctx.shadow_table.map_id(id, id);
        ctx.shadow_table
            .track_interface(id, "wl_data_offer".to_string());
        Action::Forward
    }
}

impl wl_data_offer::WlDataOfferHandler for DataDeviceHandler {
    fn on_receive(&mut self, ctx: &mut Context, mime_type: &String, fd: i32) -> Action {
        debug!("wl_data_offer.receive: mime_type={}, fd={}", mime_type, fd);

        if let Some(virtwl) = &ctx.virtwayland_channel {
            debug!("Using virtwl for clipboard receive");

            // Create a pipe where we read (so host must write to it)
            // VIRTWL_IOCTL_NEW_PIPE_READ creates a pipe that is readable via the returned FD.
            let virtwl_pipe = match virtwl.create_pipe(true) {
                Ok(p) => p,
                Err(e) => {
                    error!("Failed to create virtwl pipe: {}", e);
                    return Action::Forward;
                }
            };

            // We need to send this virtwl FD to the host.
            // The guest client provided 'fd' to write data into.
            // We pump: virtwl_pipe (read) -> fd (write)

            let sender_id = ctx.last_sender_id;
            let host_sender_id = ctx.shadow_table.get_host_id(sender_id).unwrap_or(sender_id);
            debug!(
                "on_receive: sender_id={}, host_sender_id={}",
                sender_id, host_sender_id
            );

            let mut builder = MessageBuilder::new();
            builder.write_string(mime_type);

            let mut msg_data = Vec::new();
            msg_data.extend_from_slice(&host_sender_id.to_ne_bytes());
            let msg_len = (builder.payload.len() + 8) as u32;
            let word2 = (msg_len << 16) | (wl_data_offer::REQ_RECEIVE as u32);
            msg_data.extend_from_slice(&word2.to_ne_bytes());
            msg_data.extend_from_slice(&builder.payload);

            // Dup virtwl FD
            // virtwl_pipe is OwnedFd, so it implements AsFd.
            let virtwl_fd_dup = match virtwl_pipe.try_clone() {
                Ok(f) => f,
                Err(e) => {
                    error!("Failed to dup virtwl fd: {}", e);
                    return Action::Forward;
                }
            };

            // Push to queue, converting OwnedFd to RawFd
            ctx.client_to_host_queue
                .push((msg_data, vec![virtwl_fd_dup.into_raw_fd()]));

            // Spawn pump task
            // DUP client FD because proxy.rs will close the original one
            let client_fd =
                match nix::unistd::dup(unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }) {
                    Ok(f) => f,
                    Err(e) => {
                        error!("Failed to dup client fd: {}", e);
                        return Action::Forward;
                    }
                };

            let pump_task = async move {
                let mut input = tokio::fs::File::from_std(std::fs::File::from(virtwl_pipe));
                let mut output = tokio::fs::File::from_std(std::fs::File::from(client_fd));

                if let Err(e) = tokio::io::copy(&mut input, &mut output).await {
                    error!("Clipboard pump error: {}", e);
                }
                debug!("Clipboard pump finished");
            };

            tokio::spawn(pump_task);

            // Drop original action (we handled it)
            return Action::Drop;
        }

        Action::Forward
    }
}

impl wl_data_source::WlDataSourceHandler for DataDeviceHandler {}
