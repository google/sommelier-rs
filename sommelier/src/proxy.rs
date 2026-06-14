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
use crate::virtwl_channel::VirtWaylandChannel;
use crate::wire::{ProtocolError, WireMessage};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use std::os::fd::BorrowedFd;
use std::os::unix::io::{IntoRawFd, RawFd};
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};

type DispatchResult = Result<Option<(Vec<u8>, Vec<RawFd>)>, ProtocolError>;

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
    text_input_manager_v1: crate::handler::text_input::TextInputManagerV1Handler,
    text_input_v1: crate::handler::text_input::TextInputV1Handler,
    text_input_extension_v1: crate::handler::text_input::TextInputExtensionV1Handler,
    extended_text_input_v1: crate::handler::text_input::ExtendedTextInputV1Handler,
    text_input_manager_v3: crate::handler::text_input::TextInputManagerV3Handler,
    text_input_v3: crate::handler::text_input::TextInputV3Handler,
    keyboard: crate::handler::keyboard::KeyboardHandler,
    seat: crate::handler::seat::SeatHandler,
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
            data_device: crate::handler::data_device::DataDeviceHandler,
            text_input_manager_v1: crate::handler::text_input::TextInputManagerV1Handler,
            text_input_v1: crate::handler::text_input::TextInputV1Handler,
            text_input_extension_v1: crate::handler::text_input::TextInputExtensionV1Handler,
            extended_text_input_v1: crate::handler::text_input::ExtendedTextInputV1Handler,
            text_input_manager_v3: crate::handler::text_input::TextInputManagerV3Handler,
            text_input_v3: crate::handler::text_input::TextInputV3Handler,
            keyboard: crate::handler::keyboard::KeyboardHandler::new(),
            seat: crate::handler::seat::SeatHandler,
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
    fn new(
        client_conn: WaylandConnection,
        host_conn: WaylandConnection,
        gpu_accel: bool,
        xdg_decoration: bool,
    ) -> Self {
        Self {
            client_conn,
            host_conn,
            ctx: Context::new(gpu_accel, xdg_decoration),
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
                            if bytes == 0 { break; }
                            if !self.handle_msgs(Direction::ClientToHost).await { break; }
                        }
                        Err(_) => break,
                    }
                }
                res = self.host_conn.recv() => {
                    match res {
                        Ok(bytes) => {
                            if bytes == 0 { break; }
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
    ) -> DispatchResult {
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
        } else if protocols::text_input_unstable_v1::ALLOWED_INTERFACES.contains(&interface) {
            protocols::text_input_unstable_v1::dispatch_request(interface, msg, handler, ctx)
        } else if protocols::text_input_extension_unstable_v1::ALLOWED_INTERFACES
            .contains(&interface)
        {
            protocols::text_input_extension_unstable_v1::dispatch_request(
                interface, msg, handler, ctx,
            )
        } else if protocols::xdg_decoration_unstable_v1::ALLOWED_INTERFACES.contains(&interface) {
            protocols::xdg_decoration_unstable_v1::dispatch_request(interface, msg, handler, ctx)
        } else if protocols::fractional_scale_v1::ALLOWED_INTERFACES.contains(&interface) {
            protocols::fractional_scale_v1::dispatch_request(interface, msg, handler, ctx)
        } else if protocols::keyboard_extension_unstable_v1::ALLOWED_INTERFACES.contains(&interface)
        {
            protocols::keyboard_extension_unstable_v1::dispatch_request(
                interface, msg, handler, ctx,
            )
        } else {
            Ok(None)
        }
    }

    fn dispatch_event(
        handler: &mut SommelierHandler,
        ctx: &mut Context,
        interface: &str,
        msg: &mut WireMessage,
    ) -> DispatchResult {
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
        } else if protocols::text_input_unstable_v1::ALLOWED_INTERFACES.contains(&interface) {
            protocols::text_input_unstable_v1::dispatch_event(interface, msg, handler, ctx)
        } else if protocols::text_input_extension_unstable_v1::ALLOWED_INTERFACES
            .contains(&interface)
        {
            protocols::text_input_extension_unstable_v1::dispatch_event(
                interface, msg, handler, ctx,
            )
        } else if protocols::xdg_decoration_unstable_v1::ALLOWED_INTERFACES.contains(&interface) {
            protocols::xdg_decoration_unstable_v1::dispatch_event(interface, msg, handler, ctx)
        } else if protocols::fractional_scale_v1::ALLOWED_INTERFACES.contains(&interface) {
            protocols::fractional_scale_v1::dispatch_event(interface, msg, handler, ctx)
        } else if protocols::keyboard_extension_unstable_v1::ALLOWED_INTERFACES.contains(&interface)
        {
            protocols::keyboard_extension_unstable_v1::dispatch_event(interface, msg, handler, ctx)
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

            if len < 8 {
                log::error!("Invalid message length: {}", len);
                return false;
            }
            if offset + len > conn.read_buf.len() {
                break;
            }

            let packet = &conn.read_buf[offset..offset + len];

            let guest_id = match direction {
                Direction::ClientToHost => Some(sender_id),
                Direction::HostToClient => self.ctx.shadow_table.get_guest_id(sender_id),
            };

            let interface = match direction {
                Direction::ClientToHost => self.ctx.shadow_table.get_interface(sender_id).cloned(),
                Direction::HostToClient => self
                    .ctx
                    .shadow_table
                    .get_host_interface(sender_id)
                    .or_else(|| guest_id.and_then(|gid| self.ctx.shadow_table.get_interface(gid)))
                    .cloned(),
            };

            let mut consumed_fds = 0;
            let result = if let Some(interface) = interface {
                let mut msg =
                    WireMessage::new(sender_id, opcode, &packet[8..], &conn.read_fds[fd_offset..]);

                log::trace!("[{:?}] {}:{} (len={})", direction, interface, opcode, len);

                self.ctx.last_sender_id = sender_id;

                let res = match direction {
                    Direction::ClientToHost => Self::dispatch_request(
                        &mut self.handler,
                        &mut self.ctx,
                        &interface,
                        &mut msg,
                    ),
                    Direction::HostToClient => {
                        Self::dispatch_event(&mut self.handler, &mut self.ctx, &interface, &mut msg)
                    }
                };
                consumed_fds = msg.fd_offset;
                res
            } else {
                if guest_id.is_some() {
                    log::warn!(
                        "[{:?}] unknown id {} opcode {} (len={})",
                        direction,
                        sender_id,
                        opcode,
                        len
                    );
                } else {
                    log::debug!(
                        "[{:?}] untracked host id {} opcode {} (len={})",
                        direction,
                        sender_id,
                        opcode,
                        len
                    );
                }
                Ok(None)
            };

            match result {
                Ok(Some((mut data, fds))) => {
                    log::debug!("  -> translated ({} bytes, {} fds)", data.len(), fds.len());

                    // Patch ID in the forwarded message
                    if let Some(gid) = guest_id {
                        let target_id = match direction {
                            Direction::ClientToHost => self.ctx.shadow_table.get_host_id(gid),
                            Direction::HostToClient => Some(gid),
                        };

                        if let Some(tid) = target_id {
                            if data.len() >= 4 {
                                data[0..4].copy_from_slice(&tid.to_ne_bytes());
                            }
                        } else {
                            // If we can't map the ID, it might be a new object or error.
                            // For ClientToHost, it's usually sender which should be mapped.
                            // We log a warning but send anyway (might fail on host).
                            log::warn!("Could not map sender ID {} to host ID", gid);
                        }
                    }

                    out_buffer.extend_from_slice(&data);
                    out_fds.extend(fds);
                }
                Ok(None) => {
                    log::debug!("  -> dropped (unhandled)");
                }
                Err(e) => {
                    let guest_id_for_log = match direction {
                        Direction::ClientToHost => Some(sender_id),
                        Direction::HostToClient => self.ctx.shadow_table.get_guest_id(sender_id),
                    };
                    let interface = guest_id_for_log
                        .and_then(|gid| self.ctx.shadow_table.get_interface(gid).cloned())
                        .unwrap_or_else(|| "unknown".to_string());
                    log::error!("Protocol error: {} (direction={:?}, id={}, guest_id={:?}, interface={}, opcode={})", e, direction, sender_id, guest_id_for_log, interface, opcode);
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

        let mut fds_to_close: std::collections::HashSet<std::os::unix::io::RawFd> =
            std::collections::HashSet::new();
        fds_to_close.extend(out_fds.iter());

        // Bidirectional queue: when processing events in one direction, the
        // handler may queue messages for the OPPOSITE direction. For example,
        // processing a host→client wl_keyboard.key event queues an ack_key
        // message back to the host via client_to_host_queue. Flush that queue
        // now by sending it back through the source connection.
        //
        // Ordering note: the forwarded wl_keyboard.key arrives at the guest
        // *before* the ack_key reaches the host, because out_buffer is sent
        // first (above). This is intentional and safe: Exo holds the key in
        // pending_key_acks_ with a 1000 ms TTL, so the ack always arrives well
        // within the window. The C sommelier exhibits the same ordering.
        let mut reverse_out_fds = Vec::new();
        let reverse_queue = match direction {
            Direction::ClientToHost => &mut self.ctx.host_to_client_queue,
            Direction::HostToClient => &mut self.ctx.client_to_host_queue,
        };
        let mut reverse_out_buf = Vec::new();
        for (p_data, p_fds) in reverse_queue.drain(..) {
            log::debug!(
                "  -> adding reverse queued message ({} bytes, {} fds)",
                p_data.len(),
                p_fds.len()
            );
            reverse_out_buf.extend_from_slice(&p_data);
            reverse_out_fds.extend(p_fds);
        }
        if !reverse_out_buf.is_empty()
            && conn.send(&reverse_out_buf, &reverse_out_fds).await.is_err()
        {
            // success=false signals the caller (handle_msgs) to drop this client
            // connection. We cannot return early here because we still own
            // reverse_out_fds and must close them below to avoid fd leaks.
            success = false;
            // Do NOT return early here: we must still close `reverse_out_fds`
            // below. sendmsg(SCM_RIGHTS) copies FDs into the kernel cmsg buffer;
            // if the send fails the originals remain our responsibility and
            // must be closed to avoid fd leaks.
        }
        // FDs are closed unconditionally — we own them and must not leak
        // regardless of whether the send succeeded or was skipped. When
        // sendmsg(SCM_RIGHTS) succeeds it duplicates FDs into the kernel
        // cmsg buffer; the originals are still ours to close. When the send
        // fails or the buffer was empty, we obviously retain ownership.
        fds_to_close.extend(reverse_out_fds.iter());

        fds_to_close.extend(conn.read_fds.iter().take(fd_offset));

        for fd in fds_to_close {
            let _ = nix::unistd::close(fd);
        }

        // Remove consumed FDs and data
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
    wl_data_offer: data_device,
    wl_keyboard: keyboard,
    wl_seat: seat
});
impl protocols::wayland::ProtocolHandler for SommelierHandler {}

// XDG Shell Protocol
protocols::xdg_shell::impl_sommelier_delegates!(SommelierHandler, {
    xdg_wm_base: compositor,
    xdg_surface: compositor,
    xdg_toplevel: compositor
});
impl protocols::xdg_shell::ProtocolHandler for SommelierHandler {}

// Linux DMABuf Protocol
protocols::linux_dmabuf_v1::impl_sommelier_delegates!(SommelierHandler, {
    zwp_linux_dmabuf_v1: linux_dmabuf,
    zwp_linux_buffer_params_v1: linux_dmabuf,
    zwp_linux_dmabuf_feedback_v1: linux_dmabuf
});
impl protocols::linux_dmabuf_v1::ProtocolHandler for SommelierHandler {}

// Text Input unstable v1 Protocol
protocols::text_input_unstable_v1::impl_sommelier_delegates!(SommelierHandler, {
    zwp_text_input_manager_v1: text_input_manager_v1,
    zwp_text_input_v1: text_input_v1
});
impl protocols::text_input_unstable_v1::ProtocolHandler for SommelierHandler {}

// Text Input Extension unstable v1 Protocol
protocols::text_input_extension_unstable_v1::impl_sommelier_delegates!(SommelierHandler, {
    zcr_text_input_extension_v1: text_input_extension_v1,
    zcr_extended_text_input_v1: extended_text_input_v1
});
impl protocols::text_input_extension_unstable_v1::ProtocolHandler for SommelierHandler {}

// Text Input unstable v3 Protocol
protocols::text_input_unstable_v3::impl_sommelier_delegates!(SommelierHandler, {
    zwp_text_input_manager_v3: text_input_manager_v3,
    zwp_text_input_v3: text_input_v3
});
impl protocols::text_input_unstable_v3::ProtocolHandler for SommelierHandler {}

// Viewporter Protocol
protocols::viewporter::impl_sommelier_delegates!(SommelierHandler, {});
impl protocols::viewporter::ProtocolHandler for SommelierHandler {}

// XDG Decoration Protocol
protocols::xdg_decoration_unstable_v1::impl_sommelier_delegates!(SommelierHandler, {});
impl protocols::xdg_decoration_unstable_v1::ProtocolHandler for SommelierHandler {}

// Fractional Scale Protocol
protocols::fractional_scale_v1::impl_sommelier_delegates!(SommelierHandler, {});
impl protocols::fractional_scale_v1::ProtocolHandler for SommelierHandler {}

// Keyboard Extension Protocol (ChromeOS-specific)
protocols::keyboard_extension_unstable_v1::impl_sommelier_delegates!(SommelierHandler, {
    zcr_keyboard_extension_v1: keyboard,
    zcr_extended_keyboard_v1: keyboard
});
impl protocols::keyboard_extension_unstable_v1::ProtocolHandler for SommelierHandler {}

pub async fn run(
    display: &str,
    local_compositor: Option<String>,
    gpu_accel: bool,
    xdg_decoration: bool,
    virtio_wayland: Option<String>,
) {
    if let Some(path) = &virtio_wayland {
        if let Err(e) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
        {
            log::error!("Failed to open virtio-wayland device {}: {}", path, e);
            std::process::exit(1);
        }
    } else if let Some(path) = &local_compositor {
        if let Err(e) = std::os::unix::net::UnixStream::connect(path) {
            log::error!(
                "Failed to connect to local compositor socket {}: {}",
                path,
                e
            );
            std::process::exit(1);
        }
    }

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

                let mut virtwayland_channel_ref = None;

                let host_conn = {
                    let mut conn = None;

                    if let Some(path) = &virtio_wayland {
                        match VirtWaylandChannel::new(path) {
                            Ok(channel) => {
                                let channel_arc = Arc::new(channel);
                                virtwayland_channel_ref = Some(channel_arc.clone());
                                conn = Some(WaylandConnection::new_virtwayland(channel_arc));
                            }
                            Err(e) => {
                                log::error!(
                                    "Failed to open virtio-wayland channel at {}: {}",
                                    path,
                                    e
                                );
                            }
                        }
                    }

                    if conn.is_none() {
                        if let Some(path) = local_compositor.as_ref() {
                            match UnixStream::connect(path).await {
                                Ok(stream) => {
                                    let host_fd = stream.into_std().unwrap().into_raw_fd();
                                    unsafe {
                                        if let Err(e) = fcntl(
                                            BorrowedFd::borrow_raw(host_fd),
                                            FcntlArg::F_SETFL(OFlag::O_NONBLOCK),
                                        ) {
                                            log::error!(
                                                "Failed to set non-blocking on host fd: {}",
                                                e
                                            );
                                        } else {
                                            conn = Some(WaylandConnection::new(host_fd));
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::error!("Failed to connect to host: {}", e);
                                }
                            }
                        } else {
                            log::error!("Local compositor path required if not using virtio-wayland (or if it failed)");
                        }
                    }
                    conn
                };

                if let Some(host_conn) = host_conn {
                    let mut client = Client::new(client_conn, host_conn, gpu_accel, xdg_decoration);
                    if let Some(channel) = virtwayland_channel_ref {
                        client.ctx.virtwayland_channel = Some(channel);
                    }

                    tokio::spawn(async move {
                        client.run().await;
                        log::info!("Client disconnected");
                    });
                } else {
                    log::error!("Failed to establish host connection, closing client");
                }
            }
            Err(e) => {
                log::error!("Accept error: {}", e);
            }
        }
    }
}
