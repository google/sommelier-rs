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

use crate::virtwl_channel::VirtWaylandChannel;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::sys::socket::{recvmsg, sendmsg, ControlMessage, ControlMessageOwned, MsgFlags};
use std::io::{self, IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::sync::Arc;
use tokio::io::unix::AsyncFd;

pub struct WaylandConnection {
    transport: ConnectionTransport,
    pub read_buf: Vec<u8>,
    pub read_fds: Vec<RawFd>,
}

enum ConnectionTransport {
    Unix(AsyncFd<OwnedFd>),
    VirtWayland(Arc<VirtWaylandChannel>),
}

impl WaylandConnection {
    pub fn new(fd: RawFd) -> Self {
        // Safety: We assume we own the fd passed in
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };

        // Ensure non-blocking mode for AsyncFd
        let raw_fd = owned.as_raw_fd();
        unsafe {
            let borrowed_fd = std::os::fd::BorrowedFd::borrow_raw(raw_fd);
            let flags_int = fcntl(borrowed_fd, FcntlArg::F_GETFL).unwrap_or(0);
            let flags = OFlag::from_bits_truncate(flags_int);
            let _ = fcntl(borrowed_fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK));
        }

        Self {
            transport: ConnectionTransport::Unix(
                AsyncFd::new(owned).expect("Failed to create AsyncFd"),
            ),
            read_buf: Vec::new(),
            read_fds: Vec::new(),
        }
    }

    pub fn new_virtwayland(channel: Arc<VirtWaylandChannel>) -> Self {
        Self {
            transport: ConnectionTransport::VirtWayland(channel),
            read_buf: Vec::new(),
            read_fds: Vec::new(),
        }
    }

    pub async fn send(&mut self, data: &[u8], fds: &[RawFd]) -> io::Result<()> {
        if data.is_empty() && fds.is_empty() {
            return Ok(());
        }

        match &mut self.transport {
            ConnectionTransport::Unix(fd) => {
                let mut data_offset = 0;
                let mut fds_to_send = fds;

                while data_offset < data.len() || !fds_to_send.is_empty() {
                    let mut guard = fd.writable().await?;

                    let remaining_data = &data[data_offset..];
                    let iov = [IoSlice::new(remaining_data)];

                    let cmsgs = if !fds_to_send.is_empty() {
                        vec![ControlMessage::ScmRights(fds_to_send)]
                    } else {
                        vec![]
                    };

                    let result = guard.try_io(|inner| {
                        sendmsg::<()>(
                            inner.get_ref().as_raw_fd(),
                            &iov,
                            &cmsgs,
                            MsgFlags::empty(),
                            None,
                        )
                        .map_err(io::Error::from)
                    });

                    match result {
                        Ok(Ok(bytes_sent)) => {
                            // If sendmsg succeeds, FDs (if any) are sent.
                            // We must ensure we don't send them again in a retry loop.
                            fds_to_send = &[];

                            data_offset += bytes_sent;

                            if bytes_sent == 0 && !remaining_data.is_empty() {
                                return Err(io::Error::new(
                                    io::ErrorKind::WriteZero,
                                    "failed to write whole buffer",
                                ));
                            }
                        }
                        Ok(Err(e)) => return Err(e),
                        Err(_would_block) => continue,
                    }
                }
                Ok(())
            }
            ConnectionTransport::VirtWayland(channel) => channel.send(data, fds).await,
        }
    }

    pub async fn recv(&mut self) -> io::Result<usize> {
        match &mut self.transport {
            ConnectionTransport::Unix(fd) => {
                let mut buf = [0u8; 4096];
                let mut cmsg_space = nix::cmsg_space!([RawFd; 32]);

                loop {
                    let mut guard = fd.readable().await?;

                    let result = guard.try_io(|inner| {
                        let mut iov = [IoSliceMut::new(&mut buf)];
                        match recvmsg::<()>(
                            inner.get_ref().as_raw_fd(),
                            &mut iov,
                            Some(&mut cmsg_space),
                            MsgFlags::empty(),
                        ) {
                            Ok(msg) => {
                                let bytes = msg.bytes;
                                let mut fds = Vec::new();
                                for cmsg in msg.cmsgs().map_err(io::Error::other)? {
                                    if let ControlMessageOwned::ScmRights(recv_fds) = cmsg {
                                        fds.extend(recv_fds.into_iter().map(|o| o.into_raw_fd()));
                                    }
                                }
                                Ok((bytes, fds))
                            }
                            Err(e) => Err(io::Error::from(e)),
                        }
                    });

                    match result {
                        Ok(Ok((bytes, fds))) => {
                            self.read_buf.extend_from_slice(&buf[..bytes]);
                            self.read_fds.extend(fds);
                            return Ok(bytes);
                        }
                        Ok(Err(e)) => return Err(e),
                        Err(_would_block) => continue,
                    }
                }
            }
            ConnectionTransport::VirtWayland(channel) => {
                let (data, fds) = channel.recv().await?;
                let len = data.len();
                self.read_buf.extend(data);
                self.read_fds
                    .extend(fds.into_iter().map(|fd| fd.into_raw_fd()));
                Ok(len)
            }
        }
    }
}

impl AsRawFd for WaylandConnection {
    fn as_raw_fd(&self) -> RawFd {
        match &self.transport {
            ConnectionTransport::Unix(fd) => fd.as_raw_fd(),
            ConnectionTransport::VirtWayland(_) => -1,
        }
    }
}
