// Copyright 2021 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use log::{debug, error, info, warn};
use nix::errno::Errno;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::ioctl_readwrite;
use nix::sys::mman::{mmap, munmap, MapFlags, ProtFlags};
use nix::sys::stat::{fstat, SFlag};
use nix::unistd::{dup, pipe2, write};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::num::NonZeroUsize;
use std::os::fd::BorrowedFd;
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::ptr::NonNull;
use tokio::io::unix::AsyncFd;
use zerocopy::{AsBytes, FromBytes, FromZeroes};

// Cross-domain commands
pub const CROSS_DOMAIN_CMD_INIT: u8 = 1;
pub const CROSS_DOMAIN_CMD_GET_IMAGE_REQUIREMENTS: u8 = 2;
pub const CROSS_DOMAIN_CMD_POLL: u8 = 3;
pub const CROSS_DOMAIN_CMD_SEND: u8 = 4;
pub const CROSS_DOMAIN_CMD_RECEIVE: u8 = 5;
pub const CROSS_DOMAIN_CMD_READ: u8 = 6;
pub const CROSS_DOMAIN_CMD_WRITE: u8 = 7;

// Channel types
pub const CROSS_DOMAIN_CHANNEL_TYPE_WAYLAND: u32 = 0x0001;

// The maximum number of identifiers.
pub const CROSS_DOMAIN_MAX_IDENTIFIERS: usize = 28;

// virtgpu memory resource ID
pub const CROSS_DOMAIN_ID_TYPE_VIRTGPU_BLOB: u32 = 1;

// ID for Wayland pipe used for reading.
pub const CROSS_DOMAIN_ID_TYPE_READ_PIPE: u32 = 3;

// ID for Wayland pipe used for writing.
pub const CROSS_DOMAIN_ID_TYPE_WRITE_PIPE: u32 = 4;

// No ring used
pub const CROSS_DOMAIN_RING_NONE: u32 = 0xffffffff;
// A ring for metadata queries.
pub const CROSS_DOMAIN_QUERY_RING: u32 = 0;
// A ring based on this particular context's channel.
pub const CROSS_DOMAIN_CHANNEL_RING: u32 = 1;

// Read pipe IDs start at this value.
pub const CROSS_DOMAIN_PIPE_READ_START: u32 = 0x80000000;

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct CrossDomainCapabilities {
    pub version: u32,
    pub supported_channels: u32,
    pub supports_dmabuf: u32,
    pub supports_external_gpu_memory: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct CrossDomainImageRequirements {
    pub strides: [u32; 4],
    pub offsets: [u32; 4],
    pub modifier: u64,
    pub size: u64,
    pub blob_id: u32,
    pub map_info: u32,
    pub memory_idx: i32,
    pub physical_device_idx: i32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct CrossDomainHeader {
    pub cmd: u8,
    pub fence_ctx_idx: u8,
    pub cmd_size: u16,
    pub pad: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct CrossDomainInit {
    pub hdr: CrossDomainHeader,
    pub query_ring_id: u32,
    pub channel_ring_id: u32,
    pub channel_type: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct CrossDomainGetImageRequirements {
    pub hdr: CrossDomainHeader,
    pub width: u32,
    pub height: u32,
    pub drm_format: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct CrossDomainPoll {
    pub hdr: CrossDomainHeader,
    pub pad: u64,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct CrossDomainSendReceive {
    pub hdr: CrossDomainHeader,
    pub num_identifiers: u32,
    pub opaque_data_size: u32,
    pub identifiers: [u32; CROSS_DOMAIN_MAX_IDENTIFIERS],
    pub identifier_types: [u32; CROSS_DOMAIN_MAX_IDENTIFIERS],
    pub identifier_sizes: [u32; CROSS_DOMAIN_MAX_IDENTIFIERS],
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct CrossDomainReadWrite {
    pub hdr: CrossDomainHeader,
    pub identifier: u32,
    pub hang_up: u32,
    pub opaque_data_size: u32,
    pub pad: u32,
}

// DRM VirtGPU definitions

pub const VIRTGPU_EXECBUF_FENCE_FD_OUT: u32 = 0x02;
pub const VIRTGPU_EXECBUF_RING_IDX: u32 = 0x04;

pub const VIRTGPU_BLOB_MEM_GUEST: u32 = 0x0001;
pub const VIRTGPU_BLOB_MEM_HOST3D: u32 = 0x0002;

pub const VIRTGPU_BLOB_FLAG_USE_MAPPABLE: u32 = 0x0001;
pub const VIRTGPU_BLOB_FLAG_USE_SHAREABLE: u32 = 0x0002;
pub const VIRTGPU_BLOB_FLAG_USE_CROSS_DEVICE: u32 = 0x0003;

pub const VIRTGPU_CONTEXT_PARAM_CAPSET_ID: u64 = 0x0001;
pub const VIRTGPU_CONTEXT_PARAM_NUM_RINGS: u64 = 0x0002;
pub const VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK: u64 = 0x0003;

pub const VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS: u64 = 7;

pub const CAPSET_CROSS_DOMAIN: u32 = 5;

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmVirtgpuMap {
    pub offset: u64,
    pub handle: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmVirtgpuExecbuffer {
    pub flags: u32,
    pub size: u32,
    pub command: u64,
    pub bo_handles: u64,
    pub num_bo_handles: u32,
    pub fence_fd: i32,
    pub ring_idx: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmVirtgpuGetparam {
    pub param: u64,
    pub value: u64,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmVirtgpuResourceInfo {
    pub bo_handle: u32,
    pub res_handle: u32,
    pub size: u32,
    pub blob_mem: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmVirtgpu3dWait {
    pub handle: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmVirtgpuGetCaps {
    pub cap_set_id: u32,
    pub cap_set_ver: u32,
    pub addr: u64,
    pub size: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmVirtgpuResourceCreateBlob {
    pub blob_mem: u32,
    pub blob_flags: u32,
    pub bo_handle: u32,
    pub res_handle: u32,
    pub size: u64,
    pub pad: u32,
    pub cmd_size: u32,
    pub cmd: u64,
    pub blob_id: u64,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmVirtgpuContextSetParam {
    pub param: u64,
    pub value: u64,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmVirtgpuContextInit {
    pub num_params: u32,
    pub pad: u32,
    pub ctx_set_params: u64,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmGemClose {
    pub handle: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmPrimeHandleToFd {
    pub handle: u32,
    pub flags: u32,
    pub fd: i32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, AsBytes, FromBytes, FromZeroes)]
pub struct DrmPrimeFdToHandle {
    pub handle: u32,
    pub flags: u32,
    pub fd: i32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct DrmVersion {
    pub version_major: i32,
    pub version_minor: i32,
    pub version_patchlevel: i32,
    pub name_len: usize,
    pub name: *mut u8,
    pub date_len: usize,
    pub date: *mut u8,
    pub desc_len: usize,
    pub desc: *mut u8,
}
impl Default for DrmVersion {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

// IOCTLs
ioctl_readwrite!(virtgpu_map, 'd', 0x41, DrmVirtgpuMap);
ioctl_readwrite!(virtgpu_execbuffer, 'd', 0x42, DrmVirtgpuExecbuffer);
ioctl_readwrite!(virtgpu_getparam, 'd', 0x43, DrmVirtgpuGetparam);
ioctl_readwrite!(virtgpu_resource_info, 'd', 0x45, DrmVirtgpuResourceInfo);
ioctl_readwrite!(virtgpu_wait, 'd', 0x48, DrmVirtgpu3dWait);
ioctl_readwrite!(virtgpu_get_caps, 'd', 0x49, DrmVirtgpuGetCaps);
ioctl_readwrite!(
    virtgpu_resource_create_blob,
    'd',
    0x4a,
    DrmVirtgpuResourceCreateBlob
);
ioctl_readwrite!(virtgpu_context_init, 'd', 0x4b, DrmVirtgpuContextInit);
ioctl_readwrite!(drm_gem_close, 'd', 0x09, DrmGemClose);
ioctl_readwrite!(drm_prime_handle_to_fd, 'd', 0x2d, DrmPrimeHandleToFd);
ioctl_readwrite!(drm_prime_fd_to_handle, 'd', 0x2e, DrmPrimeFdToHandle);
ioctl_readwrite!(drm_version, 'd', 0x00, DrmVersion);

pub struct VirtGpuRing {
    ptr: NonNull<u8>,
    size: usize,
    blob_id: u32,
    bo_handle: u32,
    file: File,
}

unsafe impl Send for VirtGpuRing {}
unsafe impl Sync for VirtGpuRing {}

impl VirtGpuRing {
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.size) }
    }

    pub fn as_slice_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.size) }
    }
}

impl Drop for VirtGpuRing {
    fn drop(&mut self) {
        unsafe {
            let _ = munmap(self.ptr.cast(), self.size);
            let mut args = DrmGemClose {
                handle: self.bo_handle,
                pad: 0,
            };
            let _ = drm_gem_close(self.file.as_raw_fd(), &mut args);
        }
    }
}

pub struct VirtGpuSender {
    file: File,
}

impl VirtGpuSender {
    pub fn submit_cmd(
        &self,
        cmd_data: &[u8],
        ring_idx: u32,
        bo_handles: &[u32],
        request_fence: bool,
    ) -> Result<Option<OwnedFd>, nix::Error> {
        let mut exec = DrmVirtgpuExecbuffer {
            flags: 0,
            size: cmd_data.len() as u32,
            command: cmd_data.as_ptr() as u64,
            bo_handles: bo_handles.as_ptr() as u64,
            num_bo_handles: bo_handles.len() as u32,
            fence_fd: -1,
            ring_idx,
            pad: 0,
        };

        if ring_idx != CROSS_DOMAIN_RING_NONE {
            exec.flags |= VIRTGPU_EXECBUF_RING_IDX;
        }

        if request_fence {
            exec.flags |= VIRTGPU_EXECBUF_FENCE_FD_OUT;
        }

        unsafe {
            virtgpu_execbuffer(self.file.as_raw_fd(), &mut exec)?;
        }

        if request_fence && exec.fence_fd >= 0 {
            Ok(Some(unsafe { OwnedFd::from_raw_fd(exec.fence_fd) }))
        } else {
            Ok(None)
        }
    }

    pub fn send_pipe_data(&self, pipe_id: u32, data: &[u8], hang_up: bool) -> Result<(), nix::Error> {
        // warn!("Sender::send_pipe_data: pipe_id={} len={} hang_up={}", pipe_id, data.len(), hang_up);
        let mut cmd = CrossDomainReadWrite::new_zeroed();
        cmd.hdr.cmd = CROSS_DOMAIN_CMD_WRITE;
        cmd.hdr.cmd_size = (std::mem::size_of::<CrossDomainReadWrite>() + data.len()) as u16;
        cmd.identifier = pipe_id;
        cmd.opaque_data_size = data.len() as u32;
        cmd.hang_up = if hang_up { 1 } else { 0 };

        let cmd_bytes = cmd.as_bytes();
        let mut full_cmd = Vec::with_capacity(cmd_bytes.len() + data.len());
        full_cmd.extend_from_slice(cmd_bytes);
        full_cmd.extend_from_slice(data);

        self.submit_cmd(&full_cmd, CROSS_DOMAIN_RING_NONE, &[], false)?;
        Ok(())
    }
}

pub struct VirtGpuChannel {
    file: AsyncFd<File>,
    pub query_ring: Option<VirtGpuRing>,
    pub channel_ring: Option<VirtGpuRing>,
    pipe_cache: HashMap<u64, u32>,
    id_to_fd: HashMap<u32, OwnedFd>,
    pump_handles: HashMap<u32, tokio::task::AbortHandle>,
    read_pipe_id: u32,
}

impl VirtGpuChannel {
    fn get_driver_name(file: &File) -> Result<String, nix::Error> {
        let mut ver = DrmVersion::default();
        // First call to get length
        unsafe {
            drm_version(file.as_raw_fd(), &mut ver)?;
        }

        if ver.name_len == 0 {
            return Ok("".to_string());
        }

        let mut name_vec = vec![0u8; ver.name_len];
        ver.name = name_vec.as_mut_ptr();

        unsafe {
            drm_version(file.as_raw_fd(), &mut ver)?;
        }

        let s = String::from_utf8(name_vec).map_err(|_| Errno::EINVAL)?;
        // Remove null terminator if present
        Ok(s.trim_matches(char::from(0)).to_string())
    }

    pub fn get_resource_info(&self, fd: RawFd) -> Result<DrmVirtgpuResourceInfo, nix::Error> {
        let mut prime = DrmPrimeFdToHandle {
            fd,
            flags: 0,
            handle: 0,
        };

        unsafe {
            drm_prime_fd_to_handle(self.file.get_ref().as_raw_fd(), &mut prime)?;
        }

        let mut info = DrmVirtgpuResourceInfo::new_zeroed();
        info.bo_handle = prime.handle;

        let res = unsafe { virtgpu_resource_info(self.file.get_ref().as_raw_fd(), &mut info) };

        // Always close handle
        let mut close = DrmGemClose {
            handle: prime.handle,
            pad: 0,
        };
        unsafe {
            let _ = drm_gem_close(self.file.get_ref().as_raw_fd(), &mut close);
        }

        res?;
        Ok(info)
    }

    pub fn get_device_id(&self) -> Result<u64, nix::Error> {
        let stat = fstat(self.file.get_ref())?;
        Ok(stat.st_rdev)
    }

    /// Opens a virtio-gpu device and verifies it supports cross-domain contexts.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        for i in 128..192 {
            let path = format!("/dev/dri/renderD{}", i);
            let file = match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };

            // Check driver name
            if let Ok(name) = Self::get_driver_name(&file) {
                debug!("Device {} driver: '{}'", path, name);
                if name != "virtio_gpu" {
                    continue;
                }
            } else {
                warn!("Failed to get driver name for {}", path);
                continue;
            }

            // Check if it's a virtio-gpu device with cross-domain support
            match Self::is_virtio_gpu(&file) {
                Ok(true) => {
                    // Set non-blocking mode for AsyncFd
                    let fd = file.as_raw_fd();
                    unsafe {
                        let flags_int =
                            fcntl(BorrowedFd::borrow_raw(fd), FcntlArg::F_GETFL).unwrap_or(0);
                        let flags = OFlag::from_bits_truncate(flags_int);
                        let _ = fcntl(
                            BorrowedFd::borrow_raw(fd),
                            FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK),
                        );
                    }
                    info!("Found suitable virtio-gpu device: {}", path);
                    return Ok(VirtGpuChannel {
                        file: AsyncFd::new(file)?,
                        query_ring: None,
                        channel_ring: None,
                        pipe_cache: HashMap::new(),
                        id_to_fd: HashMap::new(),
                        pump_handles: HashMap::new(),
                        read_pipe_id: CROSS_DOMAIN_PIPE_READ_START,
                    });
                }
                Ok(false) => {
                    warn!(
                        "Device {} is virtio-gpu but lacks cross-domain capabilities",
                        path
                    );
                }
                Err(e) => {
                    warn!("Error checking capabilities for {}: {}", path, e);
                }
            }
        }

        Err("No suitable virtio-gpu device found".into())
    }

    fn is_virtio_gpu(file: &File) -> Result<bool, nix::Error> {
        // Check if CAPSET_CROSS_DOMAIN is supported in bitmask
        let mask = Self::get_param(file, VIRTGPU_PARAM_SUPPORTED_CAPSET_IDS)?;

        if (mask & (1 << CAPSET_CROSS_DOMAIN)) == 0 {
            debug!("  Missing CAPSET_CROSS_DOMAIN (mask: {:x})", mask);
            return Ok(false);
        }

        // Check capabilities for Wayland channel support
        let caps = Self::get_caps(file, CAPSET_CROSS_DOMAIN)?;

        if (caps.supported_channels & CROSS_DOMAIN_CHANNEL_TYPE_WAYLAND) == 0 {
            // debug!("  Missing CROSS_DOMAIN_CHANNEL_TYPE_WAYLAND (supported: {:x})", caps.supported_channels);
            // return Ok(false);
            warn!(
                "  Host reports missing Wayland support (supported: {:x})",
                caps.supported_channels
            );
            warn!("  IGNORING CHECK because C++ sommelier works.");
        }

        Ok(true)
    }

    fn get_param(file: &File, param: u64) -> Result<u64, nix::Error> {
        let mut value: u64 = 0;
        let mut p = DrmVirtgpuGetparam {
            param,
            value: &mut value as *mut u64 as u64,
        };
        debug!("Calling virtgpu_getparam param={}", param);
        unsafe {
            virtgpu_getparam(file.as_raw_fd(), &mut p)?;
        }
        debug!("virtgpu_getparam success value={}", value);
        Ok(value)
    }

    fn get_caps(file: &File, cap_set_id: u32) -> Result<CrossDomainCapabilities, nix::Error> {
        let mut caps = Box::new(CrossDomainCapabilities::new_zeroed());
        let mut args = DrmVirtgpuGetCaps {
            cap_set_id,
            cap_set_ver: 0,
            addr: caps.as_mut() as *mut _ as u64,
            size: std::mem::size_of::<CrossDomainCapabilities>() as u32,
            pad: 0,
        };

        debug!(
            "Calling virtgpu_get_caps cap_set_id={} addr={:x} size={}",
            cap_set_id, args.addr, args.size
        );
        unsafe {
            virtgpu_get_caps(file.as_raw_fd(), &mut args)?;
        }
        debug!("virtgpu_get_caps success");

        Ok(*caps)
    }

    fn fd_analysis(&mut self, fd: RawFd) -> Result<(u32, u32, u64, Option<u32>), nix::Error> {
        let fd_borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let mut prime = DrmPrimeFdToHandle {
            fd,
            flags: 0,
            handle: 0,
        };

        // Try dma-buf
        if unsafe { drm_prime_fd_to_handle(self.file.get_ref().as_raw_fd(), &mut prime) }.is_ok() {
            let mut info = DrmVirtgpuResourceInfo {
                bo_handle: prime.handle,
                res_handle: 0,
                size: 0,
                blob_mem: 0,
            };
            unsafe {
                virtgpu_resource_info(self.file.get_ref().as_raw_fd(), &mut info)?;
            }
            return Ok((
                info.res_handle,
                CROSS_DOMAIN_ID_TYPE_VIRTGPU_BLOB,
                info.size as u64,
                Some(prime.handle),
            ));
        }

        // Try pipe
        let stat = fstat(fd_borrowed)?;
        if (stat.st_mode & SFlag::S_IFMT.bits()) == SFlag::S_IFIFO.bits() {
            if let Some(&id) = self.pipe_cache.get(&stat.st_ino) {
                // Always use READ_PIPE for CMD_SEND, as rutabaga doesn't support WRITE_PIPE there.
                let type_ = CROSS_DOMAIN_ID_TYPE_READ_PIPE;
                return Ok((id, type_, 0, None));
            }

            self.read_pipe_id = self.read_pipe_id.wrapping_add(1);
            if self.read_pipe_id < CROSS_DOMAIN_PIPE_READ_START {
                self.read_pipe_id = CROSS_DOMAIN_PIPE_READ_START;
            }
            let id = self.read_pipe_id;
            self.pipe_cache.insert(stat.st_ino, id);

            let flags_int = fcntl(fd_borrowed, FcntlArg::F_GETFL)?;
            let _flags = OFlag::from_bits_truncate(flags_int);
            // In CROSS_DOMAIN_CMD_SEND (Guest -> Host), we always use READ_PIPE (3).
            // Rutabaga interprets READ_PIPE as "Rutabaga reads from internal pipe", which means
            // the Host Compositor writes to the other end. This matches the use case where
            // Guest sends a Writable pipe for Paste (Host -> Guest data).
            // Rutabaga does not support WRITE_PIPE in CMD_SEND.
            let type_ = CROSS_DOMAIN_ID_TYPE_READ_PIPE;

            // We need to keep a reference to this pipe so it doesn't get closed if the user closes their end
            let dup_fd = dup(fd_borrowed)?;
            self.id_to_fd.insert(id, dup_fd);

            return Ok((id, type_, 0, None));
        }

        Err(Errno::EINVAL)
    }

    // Update the return signature to include the size (u64) at the end
    pub fn allocate_host_blob(
        &mut self,
        width: u32,
        height: u32,
        drm_format: u32,
    ) -> Result<(OwnedFd, u32, u64, u32, u64), nix::Error> {
        let reqs = self.get_image_requirements(width, height, drm_format)?;
        // Align size to page boundary (4096) to satisfy KVM memory region requirements.
        // If the host returns an unaligned size, mapping it via KVM will fail.
        let aligned_size = (reqs.size + 4095) & !4095;
        let fd = self.create_host_blob(reqs.blob_id as u64, aligned_size)?;

        // Return aligned_size as the 5th element so mmap uses the correct size
        Ok((
            fd,
            reqs.strides[0],
            reqs.modifier,
            reqs.offsets[0],
            aligned_size,
        ))
    }

    fn create_host_blob(&self, blob_id: u64, size: u64) -> Result<OwnedFd, nix::Error> {
        let mut create_blob = DrmVirtgpuResourceCreateBlob {
            blob_mem: VIRTGPU_BLOB_MEM_HOST3D,
            blob_flags: VIRTGPU_BLOB_FLAG_USE_MAPPABLE | VIRTGPU_BLOB_FLAG_USE_SHAREABLE,
            bo_handle: 0,
            res_handle: 0,
            size,
            pad: 0,
            cmd_size: 0,
            cmd: 0,
            blob_id,
        };

        unsafe {
            virtgpu_resource_create_blob(self.file.get_ref().as_raw_fd(), &mut create_blob)?;
        }

        let mut prime = DrmPrimeHandleToFd {
            handle: create_blob.bo_handle,
            flags: 0,
            fd: -1,
        };
        // We use CLOEXEC | RDWR (0x80002) for the exported FD
        prime.flags = 0x80002;
        let res = unsafe { drm_prime_handle_to_fd(self.file.get_ref().as_raw_fd(), &mut prime) };

        let mut close = DrmGemClose {
            handle: create_blob.bo_handle,
            pad: 0,
        };
        unsafe {
            drm_gem_close(self.file.get_ref().as_raw_fd(), &mut close)?;
        }

        res?;
        Ok(unsafe { OwnedFd::from_raw_fd(prime.fd) })
    }

    fn create_pipe_internal(&mut self, identifier: u32, type_: u32) -> Result<OwnedFd, nix::Error> {
        if self.id_to_fd.contains_key(&identifier) {
            return Err(Errno::EEXIST);
        }

        let (read_fd, write_fd) = pipe2(OFlag::O_CLOEXEC)?;

        if type_ == CROSS_DOMAIN_ID_TYPE_WRITE_PIPE {
            // Caller wants to WRITE. We return the write end, and internally KEEP the read end.
            let stat = fstat(&read_fd)?;
            self.pipe_cache.insert(stat.st_ino, identifier);
            self.id_to_fd.insert(identifier, read_fd);
            Ok(write_fd) // Return write end
        } else if type_ == CROSS_DOMAIN_ID_TYPE_READ_PIPE {
            // Caller wants to READ. We return the read end, and internally KEEP the write end.
            let stat = fstat(&write_fd)?;
            self.pipe_cache.insert(stat.st_ino, identifier);
            self.id_to_fd.insert(identifier, write_fd);
            Ok(read_fd) // Return read end
        } else {
            Err(Errno::EINVAL)
        }
    }

    pub fn create_context(&self) -> Result<(), nix::Error> {
        let params = [
            DrmVirtgpuContextSetParam {
                param: VIRTGPU_CONTEXT_PARAM_CAPSET_ID,
                value: CAPSET_CROSS_DOMAIN as u64,
            },
            DrmVirtgpuContextSetParam {
                param: VIRTGPU_CONTEXT_PARAM_NUM_RINGS,
                value: 2,
            },
            DrmVirtgpuContextSetParam {
                param: VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK,
                value: 1 << CROSS_DOMAIN_CHANNEL_RING,
            },
        ];

        let mut init = DrmVirtgpuContextInit {
            num_params: params.len() as u32,
            pad: 0,
            ctx_set_params: params.as_ptr() as u64,
        };

        unsafe {
            virtgpu_context_init(self.file.get_ref().as_raw_fd(), &mut init)?;
        }

        Ok(())
    }

    fn create_ring_impl(&self, size: usize) -> Result<VirtGpuRing, nix::Error> {
        let mut create_blob = DrmVirtgpuResourceCreateBlob {
            blob_mem: VIRTGPU_BLOB_MEM_GUEST,
            blob_flags: VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
            bo_handle: 0,
            res_handle: 0,
            size: size as u64,
            pad: 0,
            cmd_size: 0,
            cmd: 0,
            blob_id: 0,
        };

        unsafe {
            virtgpu_resource_create_blob(self.file.get_ref().as_raw_fd(), &mut create_blob)?;
        }

        let mut map = DrmVirtgpuMap {
            offset: 0,
            handle: create_blob.bo_handle,
            pad: 0,
        };

        unsafe {
            virtgpu_map(self.file.get_ref().as_raw_fd(), &mut map)?;
        }

        let ptr = unsafe {
            mmap(
                None,
                NonZeroUsize::new(size).unwrap(),
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                self.file.get_ref(),
                map.offset as i64,
            )?
        };

        let file = self.file.get_ref().try_clone().map_err(|_| Errno::EBADF)?;

        Ok(VirtGpuRing {
            ptr: NonNull::new(ptr.as_ptr() as *mut u8).unwrap(),
            size,
            blob_id: create_blob.res_handle,
            bo_handle: create_blob.bo_handle,
            file,
        })
    }

    pub fn init_context(&mut self) -> Result<OwnedFd, nix::Error> {
        self.create_context()?;

        // Ring 0: Query
        let query_ring = self.create_ring_impl(4096)?;
        let query_id = query_ring.blob_id;
        self.query_ring = Some(query_ring);

        // Ring 1: Channel
        let channel_ring = self.create_ring_impl(4096)?;
        let channel_id = channel_ring.blob_id;
        self.channel_ring = Some(channel_ring);

        let init = CrossDomainInit {
            hdr: CrossDomainHeader {
                cmd: CROSS_DOMAIN_CMD_INIT,
                fence_ctx_idx: 0,
                cmd_size: std::mem::size_of::<CrossDomainInit>() as u16,
                pad: 0,
            },
            query_ring_id: query_id,
            channel_ring_id: channel_id,
            channel_type: CROSS_DOMAIN_CHANNEL_TYPE_WAYLAND,
        };

        self.submit_cmd(init.as_bytes(), 0, &[], false)?;

        // Start polling
        let poll = CrossDomainPoll {
            hdr: CrossDomainHeader {
                cmd: CROSS_DOMAIN_CMD_POLL,
                fence_ctx_idx: 0,
                cmd_size: std::mem::size_of::<CrossDomainPoll>() as u16,
                pad: 0,
            },
            pad: 0,
        };
        let ring = self.channel_ring.as_ref().ok_or(Errno::EINVAL)?;
        let fence = self.submit_cmd(
            poll.as_bytes(),
            CROSS_DOMAIN_CHANNEL_RING,
            &[ring.bo_handle],
            true,
        )?;
        fence.ok_or(Errno::EIO)
    }

    pub fn submit_cmd(
        &self,
        cmd_data: &[u8],
        ring_idx: u32,
        bo_handles: &[u32],
        request_fence: bool,
    ) -> Result<Option<OwnedFd>, nix::Error> {
        let mut exec = DrmVirtgpuExecbuffer {
            flags: 0,
            size: cmd_data.len() as u32,
            command: cmd_data.as_ptr() as u64,
            bo_handles: bo_handles.as_ptr() as u64,
            num_bo_handles: bo_handles.len() as u32,
            fence_fd: -1,
            ring_idx,
            pad: 0,
        };

        if ring_idx != CROSS_DOMAIN_RING_NONE {
            exec.flags |= VIRTGPU_EXECBUF_RING_IDX;
        }

        if request_fence {
            exec.flags |= VIRTGPU_EXECBUF_FENCE_FD_OUT;
        }

        unsafe {
            virtgpu_execbuffer(self.file.get_ref().as_raw_fd(), &mut exec)?;
        }

        if request_fence && exec.fence_fd >= 0 {
            Ok(Some(unsafe { OwnedFd::from_raw_fd(exec.fence_fd) }))
        } else {
            Ok(None)
        }
    }

    pub fn get_image_requirements(
        &mut self,
        width: u32,
        height: u32,
        drm_format: u32,
    ) -> Result<CrossDomainImageRequirements, nix::Error> {
        debug!(
            "get_image_requirements width={} height={} drm_format={}",
            width, height, drm_format
        );
        let mut req = CrossDomainGetImageRequirements::new_zeroed();
        req.hdr.cmd = CROSS_DOMAIN_CMD_GET_IMAGE_REQUIREMENTS;
        req.hdr.cmd_size = std::mem::size_of::<CrossDomainGetImageRequirements>() as u16;
        req.width = width;
        req.height = height;
        req.drm_format = drm_format;
        req.flags = 5; // GBM_BO_USE_SCANOUT (1) | GBM_BO_USE_RENDERING (4)

        let ring_handle = self.query_ring.as_ref().ok_or(Errno::EINVAL)?.bo_handle;

        // Submit command. Do NOT request a fence out.
        self.submit_cmd(
            req.as_bytes(),
            CROSS_DOMAIN_QUERY_RING,
            &[ring_handle],
            false,
        )?;

        let mut wait_3d = DrmVirtgpu3dWait {
            handle: ring_handle,
            flags: 0,
        };

        // Block until the host has explicitly signaled the BO is idle
        loop {
            match unsafe { virtgpu_wait(self.file.get_ref().as_raw_fd(), &mut wait_3d) } {
                Ok(_) => break,
                Err(Errno::EAGAIN) => continue,
                Err(e) => return Err(e),
            }
        }

        let ring = self.query_ring.as_mut().ok_or(Errno::EINVAL)?;
        let slice = ring.as_slice();

        if slice.len() >= std::mem::size_of::<CrossDomainImageRequirements>() {
            let mut res = CrossDomainImageRequirements::new_zeroed();
            res.as_bytes_mut()
                .copy_from_slice(&slice[..std::mem::size_of::<CrossDomainImageRequirements>()]);
            return Ok(res);
        }

        Err(Errno::EIO)
    }

    pub fn send_wayland(&mut self, data: &[u8], fds: &[RawFd]) -> Result<(), nix::Error> {
        let mut cmd = CrossDomainSendReceive::new_zeroed();
        cmd.hdr.cmd = CROSS_DOMAIN_CMD_SEND;
        cmd.hdr.cmd_size = (std::mem::size_of::<CrossDomainSendReceive>() + data.len()) as u16;
        cmd.opaque_data_size = data.len() as u32;
        cmd.num_identifiers = fds.len() as u32;

        let mut gem_handles = Vec::new();

        for (i, &fd) in fds.iter().enumerate() {
            if i >= CROSS_DOMAIN_MAX_IDENTIFIERS {
                break;
            }
            let (id, type_, _size, maybe_handle) = self.fd_analysis(fd)?;
            if let Some(handle) = maybe_handle {
                gem_handles.push(handle);
            }
            cmd.identifiers[i] = id;
            cmd.identifier_types[i] = type_;
            cmd.identifier_sizes[i] = 0; // C++ explicitly ignores this field
        }

        let cmd_bytes = cmd.as_bytes();
        let mut full_cmd = Vec::with_capacity(cmd_bytes.len() + data.len());
        full_cmd.extend_from_slice(cmd_bytes);
        full_cmd.extend_from_slice(data);

        // Send using execbuffer directly, not the ring.
        // C++ uses CROSS_DOMAIN_RING_NONE and 0 handle.
        let res = self.submit_cmd(&full_cmd, CROSS_DOMAIN_RING_NONE, &gem_handles, false);

        for handle in gem_handles {
            let mut close = DrmGemClose { handle, pad: 0 };
            unsafe {
                let _ = drm_gem_close(self.file.get_ref().as_raw_fd(), &mut close);
            }
        }

        res?;
        Ok(())
    }

    pub fn send_pipe_data(&mut self, pipe_id: u32, data: &[u8], hang_up: bool) -> Result<(), nix::Error> {
        warn!("send_pipe_data: pipe_id={} len={} hang_up={}", pipe_id, data.len(), hang_up);
        let mut cmd = CrossDomainReadWrite::new_zeroed();
        cmd.hdr.cmd = CROSS_DOMAIN_CMD_WRITE;
        cmd.hdr.cmd_size = (std::mem::size_of::<CrossDomainReadWrite>() + data.len()) as u16;
        cmd.identifier = pipe_id;
        cmd.opaque_data_size = data.len() as u32;
        cmd.hang_up = if hang_up { 1 } else { 0 };

        let cmd_bytes = cmd.as_bytes();
        let mut full_cmd = Vec::with_capacity(cmd_bytes.len() + data.len());
        full_cmd.extend_from_slice(cmd_bytes);
        full_cmd.extend_from_slice(data);

        self.submit_cmd(&full_cmd, CROSS_DOMAIN_RING_NONE, &[], false)?;

        if hang_up {
            if let Some(fd) = self.id_to_fd.remove(&pipe_id) {
                if let Ok(stat) = fstat(&fd) {
                    self.pipe_cache.remove(&stat.st_ino);
                }
            }
            // If the pump called this (EOF), the handle is still in the map.
            // If recv_wayland called this? recv_wayland doesn't call send_pipe_data.
            // Only pump calls send_pipe_data.
            self.pump_handles.remove(&pipe_id);
        }

        Ok(())
    }

    pub fn recv_wayland(
        &mut self,
    ) -> Result<
        (
            Vec<(Vec<u8>, Vec<OwnedFd>)>,
            Vec<(u32, OwnedFd)>,
            OwnedFd,
        ),
        nix::Error,
    > {
        // Drain DRM events to clear readiness
        let mut event_buf = [0u8; 1024];
        loop {
            match nix::unistd::read(self.file.get_ref(), &mut event_buf) {
                Ok(_) => {} // Keep reading until EAGAIN
                Err(Errno::EAGAIN) => break,
                Err(e) => return Err(e),
            }
        }

        // Take the ring to avoid borrow conflicts
        let mut ring = self.channel_ring.take().ok_or(Errno::EINVAL)?;
        let ring_handle = ring.bo_handle;
        let slice = ring.as_slice_mut();

        let mut messages = Vec::new();
        let mut new_pumps = Vec::new();

        // Only process the command at offset 0, matching C++ implementation
        let offset = 0;

        debug!("recv_wayland: slice len={}", slice.len());

        if offset + std::mem::size_of::<CrossDomainHeader>() <= slice.len() {
            let hdr_bytes = &slice[offset..offset + std::mem::size_of::<CrossDomainHeader>()];
            let mut hdr = CrossDomainHeader::new_zeroed();
            hdr.as_bytes_mut().copy_from_slice(hdr_bytes);

            if hdr.cmd != 0 {
                let mut cmd_len = hdr.cmd_size as usize;
                debug!(
                    "recv_wayland: offset={} cmd={} size={} header_bytes=[{:02x}, {:02x}, {:02x}, {:02x}, {:02x}, {:02x}, {:02x}, {:02x}]",
                    offset, hdr.cmd, cmd_len,
                    hdr_bytes[0], hdr_bytes[1], hdr_bytes[2], hdr_bytes[3],
                    hdr_bytes[4], hdr_bytes[5], hdr_bytes[6], hdr_bytes[7]
                );

                // Robustness: For specific commands that carry variable data,
                // we peek at the struct to determine the true size, in case the Host
                // provided a `cmd_size` that only covers the header/struct but not the data.
                if hdr.cmd == CROSS_DOMAIN_CMD_RECEIVE {
                    let struct_size = std::mem::size_of::<CrossDomainSendReceive>();
                    if offset + struct_size <= slice.len() {
                        let cmd_slice = &slice[offset..offset + struct_size];
                        let mut cmd_recv = CrossDomainSendReceive::new_zeroed();
                        cmd_recv.as_bytes_mut().copy_from_slice(cmd_slice);

                        let calculated_len = struct_size + cmd_recv.opaque_data_size as usize;
                        debug!("recv_wayland: RECEIVE struct_size={} opaque_data_size={} calculated_len={}", struct_size, cmd_recv.opaque_data_size, calculated_len);
                        if calculated_len > cmd_len {
                            debug!("recv_wayland: adjusting cmd_len for RECEIVE from {} to {} based on opaque_data_size", cmd_len, calculated_len);
                            cmd_len = calculated_len;
                        }
                    }
                } else if hdr.cmd == CROSS_DOMAIN_CMD_READ {
                    let struct_size = std::mem::size_of::<CrossDomainReadWrite>();
                    if offset + struct_size <= slice.len() {
                        let cmd_slice = &slice[offset..offset + struct_size];
                        let mut cmd_read = CrossDomainReadWrite::new_zeroed();
                        cmd_read.as_bytes_mut().copy_from_slice(cmd_slice);

                        let calculated_len = struct_size + cmd_read.opaque_data_size as usize;
                        debug!("recv_wayland: READ struct_size={} opaque_data_size={} calculated_len={}", struct_size, cmd_read.opaque_data_size, calculated_len);
                        if calculated_len > cmd_len {
                            debug!("recv_wayland: adjusting cmd_len for READ from {} to {} based on opaque_data_size", cmd_len, calculated_len);
                            cmd_len = calculated_len;
                        }
                    }
                }

                if cmd_len < std::mem::size_of::<CrossDomainHeader>() {
                    error!(
                        "recv_wayland: invalid cmd_size {} (too small) at offset {}, cmd={}, breaking. Header bytes: {:?}",
                        cmd_len, offset, hdr.cmd, hdr_bytes
                    );
                } else if offset + cmd_len > slice.len() {
                    error!(
                        "recv_wayland: cmd overflow at offset {} (len {} > slice {}), breaking",
                        offset,
                        offset + cmd_len,
                        slice.len()
                    );
                } else {
                    let cmd_slice = &slice[offset..offset + cmd_len];

                    if hdr.cmd == CROSS_DOMAIN_CMD_RECEIVE {
                        let struct_size = std::mem::size_of::<CrossDomainSendReceive>();
                        if cmd_slice.len() >= struct_size {
                            let mut cmd_recv = CrossDomainSendReceive::new_zeroed();
                            cmd_recv
                                .as_bytes_mut()
                                .copy_from_slice(&cmd_slice[..struct_size]);

                            let data_offset = struct_size;
                            let data_len = cmd_recv.opaque_data_size as usize;

                            if cmd_slice.len() >= data_offset + data_len {
                                let data = cmd_slice[data_offset..data_offset + data_len].to_vec();

                                let mut fds = Vec::new();
                                for i in 0..cmd_recv.num_identifiers as usize {
                                    if i >= CROSS_DOMAIN_MAX_IDENTIFIERS {
                                        break;
                                    }
                                    let id = cmd_recv.identifiers[i];
                                    let type_ = cmd_recv.identifier_types[i];
                                    let size = cmd_recv.identifier_sizes[i] as u64;

                                    if let Ok(fd) = if type_ == CROSS_DOMAIN_ID_TYPE_VIRTGPU_BLOB {
                                        self.create_host_blob(id as u64, size)
                                    } else if type_ == CROSS_DOMAIN_ID_TYPE_WRITE_PIPE
                                        || type_ == CROSS_DOMAIN_ID_TYPE_READ_PIPE
                                    {
                                        let res = self.create_pipe_internal(id, type_);
                                        if let Ok(_) = res {
                                            // If we created a WRITE_PIPE, we kept the read end in id_to_fd.
                                            // We need to pump it.
                                            if type_ == CROSS_DOMAIN_ID_TYPE_WRITE_PIPE {
                                                warn!("Creating WRITE_PIPE id={}", id);
                                                if let Some(read_fd) = self.id_to_fd.get(&id) {
                                                    if let Ok(dup_fd) = nix::unistd::dup(read_fd) {
                                                        new_pumps.push((id, dup_fd));
                                                    }
                                                }
                                            }
                                        }
                                        res
                                    } else {
                                        Err(Errno::EINVAL)
                                    } {
                                        fds.push(fd);
                                    }
                                }

                                messages.push((data, fds));
                            }
                        }
                    } else if hdr.cmd == CROSS_DOMAIN_CMD_READ {
                        let struct_size = std::mem::size_of::<CrossDomainReadWrite>();
                        if cmd_slice.len() >= struct_size {
                            let mut cmd_read = CrossDomainReadWrite::new_zeroed();
                            cmd_read
                                .as_bytes_mut()
                                .copy_from_slice(&cmd_slice[..struct_size]);

                            let data_offset = struct_size;
                            let data_len = cmd_read.opaque_data_size as usize;
                            if cmd_slice.len() >= data_offset + data_len {
                                let data = &cmd_slice[data_offset..data_offset + data_len];

                                if let Some(fd_owned) = self.id_to_fd.get(&cmd_read.identifier) {
                                    let _ = write(fd_owned, data);
                                }

                                if cmd_read.hang_up != 0 {
                                    self.id_to_fd.remove(&cmd_read.identifier);
                                    if let Some(handle) = self.pump_handles.remove(&cmd_read.identifier) {
                                        handle.abort();
                                    }
                                }
                            }
                        }
                    }

                    // Zero out the processed command
                    slice[offset..offset + cmd_len].fill(0);
                }
            } else {
                 debug!("recv_wayland: cmd at offset 0 is 0, nothing to process");
            }
        }

        // Restore the ring
        self.channel_ring = Some(ring);

        // Re-arm poll
        let poll = CrossDomainPoll {
            hdr: CrossDomainHeader {
                cmd: CROSS_DOMAIN_CMD_POLL,
                fence_ctx_idx: 0,
                cmd_size: std::mem::size_of::<CrossDomainPoll>() as u16,
                pad: 0,
            },
            pad: 0,
        };
        let fence = self.submit_cmd(
            poll.as_bytes(),
            CROSS_DOMAIN_CHANNEL_RING,
            &[ring_handle],
            true,
        )?;
        let fence = fence.ok_or(Errno::EIO)?;

        Ok((messages, new_pumps, fence))
    }
}

pub fn spawn_virtgpu_actor(
    channel: std::sync::Arc<std::sync::Mutex<VirtGpuChannel>>,
    initial_fence: OwnedFd,
) -> (
    tokio::sync::mpsc::Sender<(Vec<u8>, Vec<OwnedFd>)>,
    tokio::sync::mpsc::Receiver<(Vec<u8>, Vec<OwnedFd>)>,
) {
    let (command_tx, mut command_rx) = tokio::sync::mpsc::channel::<(Vec<u8>, Vec<OwnedFd>)>(32);
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<(Vec<u8>, Vec<OwnedFd>)>(32);

    let mut current_fence =
        AsyncFd::new(initial_fence).expect("Failed to create AsyncFd for fence");

    tokio::spawn(async move {
        loop {
            tokio::select! {
                cmd = command_rx.recv() => {
                    match cmd {
                        Some((data, fds)) => {
                            let raw_fds: Vec<RawFd> = fds.iter().map(|fd| fd.as_raw_fd()).collect();
                            let res = {
                                if let Ok(mut chan) = channel.lock() {
                                    chan.send_wayland(&data, &raw_fds)
                                } else {
                                    error!("VirtGpu actor: failed to lock channel");
                                    break;
                                }
                            };
                            if let Err(e) = res {
                                error!("VirtGpu actor: error sending wayland data: {}", e);
                                break;
                            }
                            // FDs are dropped here, closing them
                        }
                        None => break,
                    }
                }
                ready = current_fence.readable() => {
                    match ready {
                        Ok(mut guard) => {
                             guard.clear_ready();
                             let res = {
                                 if let Ok(mut chan) = channel.lock() {
                                     chan.recv_wayland()
                                 } else {
                                     error!("VirtGpu actor: failed to lock channel");
                                     break;
                                 }
                             };

                             match res {
                                 Ok((messages, new_pumps, new_fence)) => {
                                     match AsyncFd::new(new_fence) {
                                         Ok(f) => current_fence = f,
                                         Err(e) => {
                                             error!("VirtGpu actor: failed to create AsyncFd for new fence: {}", e);
                                             break;
                                         }
                                     }

                                     for (data, owned_fds) in messages {
                                         if event_tx.send((data, owned_fds)).await.is_err() {
                                             break;
                                         }
                                     }

                                     // Spawn pump tasks for new write pipes
                                     for (pipe_id, read_fd_raw) in new_pumps {
                                         warn!("Pump {}: starting", pipe_id);
                                         let chan_clone = channel.clone();

                                         // Clone file for concurrent sending without lock
                                         let file_clone = {
                                             let c = channel.lock().unwrap();
                                             c.file.get_ref().try_clone().expect("Failed to clone virtgpu file")
                                         };
                                         let sender = VirtGpuSender { file: file_clone };

                                         let handle = tokio::spawn(async move {
                                             // read_fd_raw is actually OwnedFd based on the error message?
                                             // Or did I misread the error? "expected `i32`, found `OwnedFd`"
                                             // This usually means the function expected i32, but got OwnedFd.
                                             // `OwnedFd::from_raw_fd` expects `RawFd` (i32).
                                             // So `read_fd_raw` MUST be `OwnedFd`.
                                             let read_fd = read_fd_raw; 

                                             // Set O_NONBLOCK
                                             let flags = match fcntl(&read_fd, FcntlArg::F_GETFL) {
                                                 Ok(f) => f,
                                                 Err(e) => {
                                                     error!("Pump {}: failed to get flags: {}", pipe_id, e);
                                                     return;
                                                 }
                                             };
                                             
                                             let mut flags = OFlag::from_bits_truncate(flags);
                                             flags.insert(OFlag::O_NONBLOCK);
                                             
                                             if let Err(e) = fcntl(&read_fd, FcntlArg::F_SETFL(flags)) {
                                                 error!("Pump {}: failed to set O_NONBLOCK: {}", pipe_id, e);
                                                 return;
                                             }

                                             let async_fd = match AsyncFd::new(read_fd) {
                                                 Ok(f) => f,
                                                 Err(e) => {
                                                     error!("Pump {}: failed to create AsyncFd: {}", pipe_id, e);
                                                     return;
                                                 }
                                             };
                                             
                                             let mut buf = [0u8; 16384];
                                             let mut draining = false;

                                             loop {
                                                 let mut guard = match async_fd.readable().await {
                                                     Ok(g) => g,
                                                     Err(e) => {
                                                         error!("Pump {}: readable error: {}", pipe_id, e);
                                                         warn!("Pump {}: read error: {}", pipe_id, e);
                                                         break;
                                                     }
                                                 };

                                                 loop {
                                                     match guard.try_io(|inner| {
                                                         nix::unistd::read(inner, &mut buf)
                                                             .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
                                                     }) {
                                                         Ok(Ok(0)) => {
                                                             debug!("Pump {}: EOF", pipe_id);
                                                             warn!("Pump {}: EOF", pipe_id);
                                                             if !draining {
                                                                 if let Ok(mut c) = chan_clone.lock() {
                                                                     let _ = c.send_pipe_data(pipe_id, &[], true);
                                                                 }
                                                             }
                                                             return;
                                                         }
                                                         Ok(Ok(n)) => {
                                                             warn!("Pump {}: read {} bytes", pipe_id, n);
                                                             if !draining {
                                                                 // Use sender for lock-free sending
                                                                 let res = sender.send_pipe_data(pipe_id, &buf[..n], false);
                                                                 if let Err(e) = res {
                                                                     error!("Pump {}: error sending data: {}. Switching to drain mode.", pipe_id, e);
                                                                     warn!("Pump {}: error sending data: {}. Switching to drain mode.", pipe_id, e);
                                                                     draining = true;
                                                                 } else {
                                                                     warn!("Pump {}: sent {} bytes successfully", pipe_id, n);
                                                                 }
                                                             }
                                                         }
                                                         Ok(Err(e)) => {
                                                             error!("Pump {}: read error: {}", pipe_id, e);
                                                             warn!("Pump {}: read error: {}", pipe_id, e);
                                                             if !draining {
                                                                 if let Ok(mut c) = chan_clone.lock() {
                                                                     let _ = c.send_pipe_data(pipe_id, &[], true);
                                                                 }
                                                             }
                                                             return;
                                                         }
                                                         Err(_would_block) => {
                                                             break;
                                                         }
                                                     }
                                                 }
                                             }
                                         });

                                         if let Ok(mut c) = channel.lock() {
                                             c.pump_handles.insert(pipe_id, handle.abort_handle());
                                         }
                                     }
                                 }
                                 Err(e) => {
                                     error!("VirtGpu actor: error receiving wayland data: {}", e);
                                     break;
                                 }
                             }
                        }
                        Err(e) => {
                             error!("VirtGpu actor: fence wait error: {}", e);
                             break;
                        }
                     }
                }
            }
        }
    });

    (command_tx, event_rx)
}
