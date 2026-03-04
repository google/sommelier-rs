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

use gbm::{BufferObjectFlags, Format};
use std::fs::{File, OpenOptions};
use std::io;

/// Allocator for GBM buffers on the host.
///
/// This struct manages a GBM device and allows allocating buffers
/// that can be used for zero-copy sharing with the host compositor.
pub struct Allocator {
    pub device: gbm::Device<File>,
}

impl Allocator {
    /// Creates a new Allocator instance.
    ///
    /// This opens the render node at `/dev/dri/renderD128` and initializes
    /// a GBM device on top of it.
    pub fn new() -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/dri/renderD128")?;

        let device = gbm::Device::new(file).map_err(io::Error::other)?;

        Ok(Self { device })
    }

    /// Allocates a new GBM buffer object.
    ///
    /// # Arguments
    ///
    /// * `width` - The width of the buffer.
    /// * `height` - The height of the buffer.
    /// * `format` - The DRM format of the buffer.
    ///
    /// # Returns
    ///
    /// A `Result` containing the allocated `gbm::BufferObject` or an `io::Error`.
    pub fn allocate(
        &self,
        width: u32,
        height: u32,
        format: u32,
    ) -> io::Result<gbm::BufferObject<()>> {
        // Convert Wayland SHM format to GBM Format (DrmFourcc)
        // Wayland defines:
        // WL_SHM_FORMAT_ARGB8888 = 0
        // WL_SHM_FORMAT_XRGB8888 = 1

        let format = match format {
            0 => Format::Argb8888,
            1 => Format::Xrgb8888,
            val => {
                // If the value is large, it might be a FourCC code already (e.g. from dmabuf).
                // However, small values are likely Wayland SHM formats we don't support yet.
                // We'll try to parse it if it looks like a FourCC (usually ASCII chars).
                if val > 0xff {
                    gbm::Format::try_from(val).map_err(|e| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            format!("Invalid format: {}", e),
                        )
                    })?
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Unsupported Wayland SHM format: {}", val),
                    ));
                }
            }
        };

        // We request a linear buffer layout to ensure it can be mapped if necessary.
        // Using explicit flags instead of modifiers to guarantee LINEAR usage.
        self.device
            .create_buffer_object(
                width,
                height,
                format,
                BufferObjectFlags::RENDERING | BufferObjectFlags::LINEAR,
            )
            .map_err(io::Error::other)
    }
}
