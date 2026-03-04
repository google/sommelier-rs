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

#![allow(non_camel_case_types)]
#![allow(dead_code)]

use nix::{ioctl_read, ioctl_readwrite, ioctl_write_ptr};
use zerocopy::{AsBytes, FromBytes, FromZeroes};

pub const VIRTWL_SEND_MAX_ALLOCS: usize = 28;

pub const VIRTWL_IOCTL_BASE: u8 = b'w';

// ioctl types
pub const VIRTWL_IOCTL_NEW_CTX: u32 = 0;
pub const VIRTWL_IOCTL_NEW_ALLOC: u32 = 1;
pub const VIRTWL_IOCTL_NEW_PIPE_READ: u32 = 2;
pub const VIRTWL_IOCTL_NEW_PIPE_WRITE: u32 = 3;
pub const VIRTWL_IOCTL_NEW_DMABUF: u32 = 4;
pub const VIRTWL_IOCTL_NEW_CTX_NAMED: u32 = 5;

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct virtwl_ioctl_new_dmabuf {
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub stride0: u32,
    pub stride1: u32,
    pub stride2: u32,
    pub offset0: u32,
    pub offset1: u32,
    pub offset2: u32,
}

#[repr(C)]
#[derive(Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct virtwl_ioctl_new {
    pub type_: u32,
    pub fd: i32,
    pub flags: u32,
    pub payload: [u8; 36],
}

impl std::fmt::Debug for virtwl_ioctl_new {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("virtwl_ioctl_new")
            .field("type_", &self.type_)
            .field("fd", &self.fd)
            .field("flags", &self.flags)
            .finish()
    }
}

impl virtwl_ioctl_new {
    pub fn new_alloc(size: u32) -> Self {
        let mut s = Self::new_zeroed();
        s.type_ = VIRTWL_IOCTL_NEW_ALLOC;
        s.fd = -1;
        // size is at offset 0 of the union
        s.payload[0..4].copy_from_slice(&size.to_ne_bytes());
        s
    }

    pub fn new_ctx() -> Self {
        let mut s = Self::new_zeroed();
        s.type_ = VIRTWL_IOCTL_NEW_CTX;
        s.fd = -1;
        s
    }

    pub fn new_dmabuf(width: u32, height: u32, format: u32) -> Self {
        let mut s = Self::new_zeroed();
        s.type_ = VIRTWL_IOCTL_NEW_DMABUF;
        s.fd = -1;

        let d = virtwl_ioctl_new_dmabuf {
            width,
            height,
            format,
            stride0: 0,
            stride1: 0,
            stride2: 0,
            offset0: 0,
            offset1: 0,
            offset2: 0,
        };
        s.payload[0..36].copy_from_slice(d.as_bytes());
        s
    }

    pub fn new_pipe_read() -> Self {
        let mut s = Self::new_zeroed();
        s.type_ = VIRTWL_IOCTL_NEW_PIPE_READ;
        s.fd = -1;
        s.flags = 0;
        s
    }

    pub fn new_pipe_write() -> Self {
        let mut s = Self::new_zeroed();
        s.type_ = VIRTWL_IOCTL_NEW_PIPE_WRITE;
        s.fd = -1;
        s.flags = 0;
        s
    }

    pub fn get_dmabuf(&self) -> virtwl_ioctl_new_dmabuf {
        let mut d = virtwl_ioctl_new_dmabuf::new_zeroed();
        d.as_bytes_mut().copy_from_slice(&self.payload[0..36]);
        d
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct virtwl_ioctl_txn {
    pub fds: [i32; VIRTWL_SEND_MAX_ALLOCS],
    pub len: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct virtwl_ioctl_dmabuf_sync {
    pub flags: u32,
}

// IOCTL macros

// VIRTWL_IOCTL_NEW is defined as _IOWR(0x00, ...)
ioctl_readwrite!(virtwl_ioctl_new, VIRTWL_IOCTL_BASE, 0x00, virtwl_ioctl_new);

// VIRTWL_IOCTL_SEND is defined as _IOR(0x01, ...) in kernel headers.
// Note: _IOR implies the kernel writes to the user buffer (Read from Kernel).
// This is counter-intuitive for "SEND" (User -> Kernel), but we must match the kernel definition.
// The variable length data follows the struct.
//
// Since we pass a pointer to a buffer larger than the struct, we use the generated function
// which takes a *mut T. `ioctl_read!` generates `unsafe fn name(fd, *mut T)`.
ioctl_read!(virtwl_ioctl_send, VIRTWL_IOCTL_BASE, 0x01, virtwl_ioctl_txn);

// VIRTWL_IOCTL_RECV is defined as _IOW(0x02, ...) in kernel headers.
// Note: _IOW implies the kernel reads from the user buffer (Write to Kernel).
// This is counter-intuitive for "RECV" (Kernel -> User), but we must match the kernel definition.
//
// Because it is defined as _IOW, nix's `ioctl_write_ptr!` generates a function taking *const T.
// However, the kernel will write to this buffer (despite the _IOW direction), so we must pass a pointer
// to mutable memory but cast it to *const T to satisfy the signature.
ioctl_write_ptr!(virtwl_ioctl_recv, VIRTWL_IOCTL_BASE, 0x02, virtwl_ioctl_txn);

// VIRTWL_IOCTL_DMABUF_SYNC is defined as _IOR(0x03, ...)
ioctl_read!(
    virtwl_ioctl_dmabuf_sync,
    VIRTWL_IOCTL_BASE,
    0x03,
    virtwl_ioctl_dmabuf_sync
);
