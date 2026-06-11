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
    fn on_create_data_source(&mut self, _ctx: &mut Context, id: u32) -> Action {
        debug!("Tracking data source id={}", id);
        Action::Forward
    }

    fn on_get_data_device(&mut self, _ctx: &mut Context, _id: u32, _seat: u32) -> Action {
        Action::Forward
    }
}

impl wl_data_device::WlDataDeviceHandler for DataDeviceHandler {
    fn on_data_offer(&mut self, _ctx: &mut Context, _id: u32) -> Action {
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
                .push((msg_data.into(), vec![virtwl_fd_dup.into_raw_fd()]));

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
