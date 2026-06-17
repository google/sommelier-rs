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

use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

/// Fallback allocator for CPU-accessible SHM buffers in local placeholder mode.
pub struct Allocator;

pub struct AllocatedBuffer {
    pub fd: OwnedFd,
    pub stride: u32,
    pub size: usize,
}

impl Allocator {
    pub fn new() -> io::Result<Self> {
        Ok(Self)
    }

    pub fn allocate(&self, width: u32, height: u32, format: u32) -> io::Result<AllocatedBuffer> {
        // Supported Wayland SHM formats:
        // WL_SHM_FORMAT_ARGB8888 = 0
        // WL_SHM_FORMAT_XRGB8888 = 1
        let bpp = match format {
            0 | 1 => 4,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Unsupported Wayland SHM format: {}", format),
                ));
            }
        };

        let stride = width
            .checked_mul(bpp)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Width overflow"))?;
        let size = (stride as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Size overflow"))?;

        let memfd_name = b"sommelier-shm\0";
        let raw_fd = unsafe {
            libc::memfd_create(
                memfd_name.as_ptr() as *const libc::c_char,
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw_fd < 0 {
            return Err(io::Error::last_os_error());
        }

        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        if let Err(e) = nix::unistd::ftruncate(&fd, size as i64) {
            return Err(e.into());
        }

        // Apply file seals to prevent truncation (SIGBUS vulnerabilities)
        let seals = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(AllocatedBuffer { fd, stride, size })
    }
}
