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

use crate::connection::WaylandConnection;
use crate::protocols;
use crate::state::Context;
use crate::virtgpu_channel::VirtGpuChannel;
use crate::wire::{ProtocolError, WireMessage};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use std::os::fd::BorrowedFd;
use std::os::unix::io::{IntoRawFd, RawFd};
use std::sync::{Arc, Mutex};
use tokio::net::{UnixListener, UnixStream};

#[derive(Debug, Clone, Copy)]
enum Direction {
    ClientToHost,
    HostToClient,
}

struct SommelierHandler {
    display: crate::handler::display::DisplayHandler,
    registry: crate::handler::registry::RegistryHandler,
    compositor: crate::handler::compositor::CompositorHandler,
    callback: crate::handler::callback::CallbackHandler,
    shm: crate::handler::shm::ShmHandler,
    linux_dmabuf: crate::handler::linux_dmabuf::LinuxDmabufHandler,
    data_device: crate::handler::data_device::DataDeviceHandler,
}

impl SommelierHandler {
    fn new() -> Self {
        Self {
            display: crate::handler::display::DisplayHandler,
            registry: crate::handler::registry::RegistryHandler,
            compositor: crate::handler::compositor::CompositorHandler,
            callback: crate::handler::callback::CallbackHandler,
            shm: crate::handler::shm::ShmHandler,
            linux_dmabuf: crate::handler::linux_dmabuf::LinuxDmabufHandler,
            data_device: crate::handler::data_device::DataDeviceHandler::new(),
        }
    }
}

struct Client {
    client_conn: WaylandConnection,
    host_conn: WaylandConnection,
    ctx: Context,
    handler: SommelierHandler,
}

impl Client {
    fn new(client_conn: WaylandConnection, host_conn: WaylandConnection, gpu_accel: bool) -> Self {
        Self {
            client_conn,
            host_conn,
            ctx: Context::new(gpu_accel),
            handler: SommelierHandler::new(),
        }
    }

    async fn run(mut self) {
        self.ctx.shadow_table.map_id(1, 1);
        self.ctx
            .shadow_table
            .track_interface(1, "wl_display".to_string());

        loop {
            tokio::select! {
                res = self.client_conn.recv() => {
                    match res {
                        Ok(bytes) => {
                            if bytes == 0 && self.client_conn.read_fds.is_empty() { break; }
                            if !self.handle_msgs(Direction::ClientToHost).await { break; }
                        }
                        Err(_) => break,
                    }
                }
                res = self.host_conn.recv() => {
                    match res {
                        Ok(bytes) => {
                            if bytes == 0 && self.host_conn.read_fds.is_empty() { break; }
                            if !self.handle_msgs(Direction::HostToClient).await { break; }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    }

    fn dispatch_request(
        handler: &mut SommelierHandler,
        ctx: &mut Context,
        interface: &str,
        msg: &mut WireMessage,
    ) -> Result<Option<(Vec<u8>, Vec<RawFd>)>, ProtocolError> {
        if protocols::wayland::ALLOWED_INTERFACES.contains(&interface) {
            protocols::wayland::dispatch_request(interface, msg, handler, ctx)
        } else if protocols::xdg_shell::ALLOWED_INTERFACES.contains(&interface) {
            protocols::xdg_shell::dispatch_request(interface, msg, handler, ctx)
        } else if protocols::linux_dmabuf_v1::ALLOWED_INTERFACES.contains(&interface) {
            protocols::linux_dmabuf_v1::dispatch_request(interface, msg, handler, ctx)
        } else if protocols::viewporter::ALLOWED_INTERFACES.contains(&interface) {
            protocols::viewporter::dispatch_request(interface, msg, handler, ctx)
        } else if protocols::text_input_unstable_v3::ALLOWED_INTERFACES.contains(&interface) {
            protocols::text_input_unstable_v3::dispatch_request(interface, msg, handler, ctx)
        } else {
            Ok(None)
        }
    }

    fn dispatch_event(
        handler: &mut SommelierHandler,
        ctx: &mut Context,
        interface: &str,
        msg: &mut WireMessage,
    ) -> Result<Option<(Vec<u8>, Vec<RawFd>)>, ProtocolError> {
        if protocols::wayland::ALLOWED_INTERFACES.contains(&interface) {
            protocols::wayland::dispatch_event(interface, msg, handler, ctx)
        } else if protocols::xdg_shell::ALLOWED_INTERFACES.contains(&interface) {
            protocols::xdg_shell::dispatch_event(interface, msg, handler, ctx)
        } else if protocols::linux_dmabuf_v1::ALLOWED_INTERFACES.contains(&interface) {
            protocols::linux_dmabuf_v1::dispatch_event(interface, msg, handler, ctx)
        } else if protocols::viewporter::ALLOWED_INTERFACES.contains(&interface) {
            protocols::viewporter::dispatch_event(interface, msg, handler, ctx)
        } else if protocols::text_input_unstable_v3::ALLOWED_INTERFACES.contains(&interface) {
            protocols::text_input_unstable_v3::dispatch_event(interface, msg, handler, ctx)
        } else {
            Ok(None)
        }
    }

    async fn handle_msgs(&mut self, direction: Direction) -> bool {
        let (conn, other_conn) = match direction {
            Direction::ClientToHost => (&mut self.client_conn, &mut self.host_conn),
            Direction::HostToClient => (&mut self.host_conn, &mut self.client_conn),
        };

        let mut out_buffer = Vec::new();
        let mut out_fds = Vec::new();
        let mut offset = 0;
        let mut fd_offset = 0;

        while offset + 8 <= conn.read_buf.len() {
            let sender_id =
                u32::from_ne_bytes(conn.read_buf[offset..offset + 4].try_into().unwrap());
            let word2 =
                u32::from_ne_bytes(conn.read_buf[offset + 4..offset + 8].try_into().unwrap());
            let len = (word2 >> 16) as usize;
            let opcode = (word2 & 0xFFFF) as u16;

            if len < 8 || offset + len > conn.read_buf.len() {
                break;
            }

            let packet = &conn.read_buf[offset..offset + len];

            let guest_id = match direction {
                Direction::ClientToHost => Some(sender_id),
                Direction::HostToClient => self.ctx.shadow_table.get_guest_id(sender_id),
            };

            let mut consumed_fds = 0;
            let result = if let Some(gid) = guest_id {
                if let Some(interface) = self.ctx.shadow_table.get_interface(gid).cloned() {
                    let mut msg = WireMessage::new(
                        sender_id,
                        opcode,
                        &packet[8..],
                        &conn.read_fds[fd_offset..],
                    );

                    log::trace!("[{:?}] {}:{} (len={})", direction, interface, opcode, len);

                    let res = match direction {
                        Direction::ClientToHost => Self::dispatch_request(
                            &mut self.handler,
                            &mut self.ctx,
                            &interface,
                            &mut msg,
                        ),
                        Direction::HostToClient => Self::dispatch_event(
                            &mut self.handler,
                            &mut self.ctx,
                            &interface,
                            &mut msg,
                        ),
                    };
                    consumed_fds = msg.fd_offset;
                    res
                } else {
                    log::warn!(
                        "[{:?}] unknown id {} opcode {} (len={})",
                        direction,
                        sender_id,
                        opcode,
                        len
                    );
                    Ok(None)
                }
            } else {
                log::warn!(
                    "[{:?}] untracked host id {} opcode {} (len={})",
                    direction,
                    sender_id,
                    opcode,
                    len
                );
                Ok(None)
            };

            match result {
                Ok(Some((data, fds))) => {
                    log::debug!("  -> translated ({} bytes, {} fds)", data.len(), fds.len());

                    out_buffer.extend_from_slice(&data);
                    out_fds.extend(fds);
                }
                Ok(None) => {
                    log::debug!("  -> dropped (unhandled)");
                }
                Err(e) => {
                    log::error!("Protocol error: {}", e);
                    return false;
                }
            }

            fd_offset += consumed_fds;

            // Add pending messages from context
            let queue = match direction {
                Direction::ClientToHost => &mut self.ctx.client_to_host_queue,
                Direction::HostToClient => &mut self.ctx.host_to_client_queue,
            };
            for (p_data, p_fds) in queue.drain(..) {
                log::debug!(
                    "  -> adding queued message ({} bytes, {} fds)",
                    p_data.len(),
                    p_fds.len()
                );
                out_buffer.extend_from_slice(&p_data);
                out_fds.extend(p_fds);
            }

            offset += len;
        }

        let mut success = true;
        if !out_buffer.is_empty() && other_conn.send(&out_buffer, &out_fds).await.is_err() {
            success = false;
        }

        // Collect all FDs to close (both sent and consumed) into a set to avoid double-closing
        // FDs that are in both lists (e.g. forwarded FDs). Double-closing is dangerous as it
        // creates a race where a newly allocated FD (e.g. from dup in another thread) could be
        // closed accidentally.
        let mut fds_to_close: std::collections::HashSet<RawFd> = std::collections::HashSet::new();
        fds_to_close.extend(out_fds.iter());
        fds_to_close.extend(conn.read_fds.iter().take(fd_offset));

        for fd in fds_to_close {
            let _ = nix::unistd::close(fd);
        }

        // Remove consumed data and FDs
        conn.read_fds.drain(..fd_offset);
        conn.read_buf.drain(..offset);

        success
    }
}

// Wayland Protocol
protocols::wayland::impl_sommelier_delegates!(SommelierHandler, {
    wl_display: display,
    wl_registry: registry,
    wl_callback: callback,
    wl_compositor: compositor,
    wl_surface: compositor,
    wl_subcompositor: compositor,
    wl_subsurface: compositor,
    wl_region: compositor,
    wl_shm: shm,
    wl_shm_pool: shm,
    wl_buffer: shm,
    wl_data_device_manager: data_device,
    wl_data_device: data_device,
    wl_data_source: data_device,
    wl_data_offer: data_device
});
impl protocols::wayland::ProtocolHandler for SommelierHandler {}

// XDG Shell Protocol
protocols::xdg_shell::impl_sommelier_delegates!(SommelierHandler, {});
impl protocols::xdg_shell::ProtocolHandler for SommelierHandler {}

// Linux DMABuf Protocol
protocols::linux_dmabuf_v1::impl_sommelier_delegates!(SommelierHandler, {
    zwp_linux_dmabuf_v1: linux_dmabuf,
    zwp_linux_buffer_params_v1: linux_dmabuf,
    zwp_linux_dmabuf_feedback_v1: linux_dmabuf
});
impl protocols::linux_dmabuf_v1::ProtocolHandler for SommelierHandler {}

// Text Input unstable v3 Protocol
protocols::text_input_unstable_v3::impl_sommelier_delegates!(SommelierHandler, {});
impl protocols::text_input_unstable_v3::ProtocolHandler for SommelierHandler {}

// Viewporter Protocol
protocols::viewporter::impl_sommelier_delegates!(SommelierHandler, {});
impl protocols::viewporter::ProtocolHandler for SommelierHandler {}

pub async fn run(
    display: &str,
    use_virtgpu: bool,
    local_compositor: Option<String>,
    gpu_accel: bool,
) {
    let listener = UnixListener::bind(display).expect("Failed to bind socket");
    log::info!("Listening on {}", display);

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                log::info!("New client connected");

                let client_fd = stream.into_std().unwrap().into_raw_fd();
                unsafe {
                    if let Err(e) = fcntl(
                        BorrowedFd::borrow_raw(client_fd),
                        FcntlArg::F_SETFL(OFlag::O_NONBLOCK),
                    ) {
                        log::error!("Failed to set non-blocking on client fd: {}", e);
                        continue;
                    }
                }
                let client_conn = WaylandConnection::new(client_fd);

                let mut virtgpu_channel_ref = None;

                let host_conn = if use_virtgpu {
                    match VirtGpuChannel::new() {
                        Ok(mut channel) => match channel.init_context() {
                            Ok(fence) => {
                                let channel_arc = Arc::new(Mutex::new(channel));
                                virtgpu_channel_ref = Some(channel_arc.clone());
                                WaylandConnection::new_virtgpu(channel_arc, fence)
                            }
                            Err(e) => {
                                log::error!("Failed to init virtgpu context: {}", e);
                                continue;
                            }
                        },
                        Err(e) => {
                            log::error!("Failed to connect to virtgpu: {}", e);
                            continue;
                        }
                    }
                } else {
                    let path = local_compositor
                        .as_ref()
                        .expect("Local compositor path required if not using virtgpu");
                    match UnixStream::connect(path).await {
                        Ok(host_stream) => {
                            let host_fd = host_stream.into_std().unwrap().into_raw_fd();
                            unsafe {
                                if let Err(e) = fcntl(
                                    BorrowedFd::borrow_raw(host_fd),
                                    FcntlArg::F_SETFL(OFlag::O_NONBLOCK),
                                ) {
                                    log::error!("Failed to set non-blocking on host fd: {}", e);
                                    continue;
                                }
                            }
                            WaylandConnection::new(host_fd)
                        }
                        Err(e) => {
                            log::error!("Failed to connect to host: {}", e);
                            continue;
                        }
                    }
                };

                let mut client = Client::new(client_conn, host_conn, gpu_accel);
                if let Some(channel) = virtgpu_channel_ref {
                    client.ctx.virtgpu_channel = Some(channel);
                }

                tokio::spawn(async move {
                    client.run().await;
                    log::info!("Client disconnected");
                });
            }
            Err(e) => {
                log::error!("Accept error: {}", e);
            }
        }
    }
}
