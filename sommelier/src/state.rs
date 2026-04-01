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

use std::collections::{HashMap, HashSet};
use std::os::unix::io::{OwnedFd, RawFd};
use std::sync::{Arc, RwLock};

use crate::allocator::Allocator;
use crate::virtwl_channel::VirtWaylandChannel;
use log::warn;

#[allow(dead_code)]
pub struct ShadowTable {
    guest_to_host: HashMap<u32, u32>,
    host_to_guest: HashMap<u32, u32>,
    interfaces: HashMap<u32, String>,
    next_host_id: u32,
}

impl ShadowTable {
    pub fn new() -> Self {
        Self {
            guest_to_host: HashMap::new(),
            host_to_guest: HashMap::new(),
            interfaces: HashMap::new(),
            // Start at 2 to mimic standard Wayland client behavior.
            // ID 1 is reserved for wl_display.
            next_host_id: 2,
        }
    }

    pub fn allocate_host_id(&mut self) -> u32 {
        loop {
            let id = self.next_host_id;
            self.next_host_id = self.next_host_id.wrapping_add(1);
            if self.next_host_id < 2 {
                self.next_host_id = 2; // Prevent 0 (null) and 1 (wl_display)
            }
            if !self.host_to_guest.contains_key(&id) {
                return id;
            }
        }
    }

    pub fn map_id(&mut self, guest_id: u32, host_id: u32) {
        if let Some(old_host_id) = self.guest_to_host.insert(guest_id, host_id) {
            if old_host_id != host_id {
                self.host_to_guest.remove(&old_host_id);
            }
        }
        self.host_to_guest.insert(host_id, guest_id);
    }

    pub fn get_host_id(&self, guest_id: u32) -> Option<u32> {
        self.guest_to_host.get(&guest_id).cloned()
    }

    pub fn get_guest_id(&self, host_id: u32) -> Option<u32> {
        self.host_to_guest.get(&host_id).cloned()
    }

    pub fn track_interface(&mut self, guest_id: u32, interface: String) {
        self.interfaces.insert(guest_id, interface);
    }

    pub fn get_interface(&self, guest_id: u32) -> Option<&String> {
        self.interfaces.get(&guest_id)
    }

    pub fn remove_id(&mut self, guest_id: u32) {
        if let Some(host_id) = self.guest_to_host.remove(&guest_id) {
            self.host_to_guest.remove(&host_id);
        }
        self.interfaces.remove(&guest_id);
    }

    #[allow(dead_code)]
    pub fn find_by_interface(&self, interface_name: &str) -> Vec<u32> {
        self.interfaces
            .iter()
            .filter_map(|(id, name)| {
                if name == interface_name {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect()
    }
}

impl Default for ShadowTable {
    fn default() -> Self {
        Self::new()
    }
}

pub struct PoolInner {
    pub client_ptr: *mut libc::c_void,
    pub size: usize,
}

unsafe impl Send for PoolInner {}
unsafe impl Sync for PoolInner {}

pub struct PoolState {
    pub client_fd: RawFd,
    pub inner: RwLock<PoolInner>,
}

impl Drop for PoolState {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.write() {
            unsafe {
                if !inner.client_ptr.is_null() && inner.client_ptr != libc::MAP_FAILED {
                    libc::munmap(inner.client_ptr, inner.size);
                    inner.client_ptr = std::ptr::null_mut();
                }
            }
        }
        unsafe {
            if self.client_fd >= 0 {
                libc::close(self.client_fd);
            }
        }
    }
}

pub struct BufferState {
    pub pool: Arc<PoolState>,
    pub offset: i32,
    #[allow(dead_code)]
    pub width: i32,
    pub height: i32,
    pub stride: u32,
    #[allow(dead_code)]
    pub format: u32,
    #[allow(dead_code)]
    pub host_buffer_id: u32,
    #[allow(dead_code)]
    pub bo: Option<gbm::BufferObject<()>>,
    #[allow(dead_code)]
    pub dmabuf_fd: Option<OwnedFd>,
    pub bo_stride: u32,
    pub dest_ptr: *mut u8,
    pub dest_size: usize,
}

unsafe impl Send for BufferState {}
unsafe impl Sync for BufferState {}

impl Drop for BufferState {
    fn drop(&mut self) {
        if !self.dest_ptr.is_null() && self.dest_ptr as *mut libc::c_void != libc::MAP_FAILED {
            unsafe {
                libc::munmap(self.dest_ptr as *mut libc::c_void, self.dest_size);
                self.dest_ptr = std::ptr::null_mut();
            }
        }
    }
}

pub struct SurfaceState {
    pub pending_buffer_id: Option<u32>,
}

pub struct PendingParam {
    pub fd: RawFd,
    pub plane_idx: u32,
    pub offset: u32,
    pub stride: u32,
    pub modifier_hi: u32,
    pub modifier_lo: u32,
}

pub struct TextInputState {
    pub host_v1_id: u32,
    pub host_ext_id: u32,
    pub guest_seat: u32,
    pub active_surface: Option<u32>,
    pub enabled: bool,
    pub enabled_changed: bool,
    pub surrounding_text: Option<(String, i32, i32)>,
    pub content_hint: u32,
    pub content_purpose: u32,
    pub cursor_rect: Option<(i32, i32, i32, i32)>,
    pub text_change_cause: u32,
}

pub struct Context {
    pub shadow_table: ShadowTable,
    pub pools: HashMap<u32, Arc<PoolState>>,
    pub buffers: HashMap<u32, BufferState>,
    pub surfaces: HashMap<u32, SurfaceState>,
    pub text_inputs: HashMap<u32, TextInputState>,
    pub keyboard_to_seat: HashMap<u32, u32>,
    pub active_surface_for_seat: HashMap<u32, u32>,
    pub last_sender_id: u32,
    pub client_to_host_queue: Vec<(Vec<u8>, Vec<RawFd>)>,
    pub host_to_client_queue: Vec<(Vec<u8>, Vec<RawFd>)>,
    pub allocator: Option<Allocator>,
    pub virtwayland_channel: Option<Arc<VirtWaylandChannel>>,
    pub host_dmabuf_id: Option<u32>,
    pub host_shm_id: Option<u32>,
    pub host_text_input_manager_v1_id: Option<u32>,
    pub host_text_input_extension_v1_id: Option<u32>,
    pub supported_formats: HashSet<u32>,
    pub host_globals: HashMap<String, u32>,
    pub pending_params: HashMap<u32, Vec<PendingParam>>,
    pub feedback_index_maps: HashMap<u32, HashMap<u16, u16>>,
    pub gpu_accel: bool,
    pub disable_xdg_decoration: bool,
}

impl Context {
    pub fn new(gpu_accel: bool, disable_xdg_decoration: bool) -> Self {
        // Initialize allocator
        let allocator = match Allocator::new() {
            Ok(alloc) => Some(alloc),
            Err(e) => {
                warn!("Failed to initialize GBM allocator: {}", e);
                None
            }
        };

        Self {
            shadow_table: ShadowTable::new(),
            pools: HashMap::new(),
            buffers: HashMap::new(),
            surfaces: HashMap::new(),
            text_inputs: HashMap::new(),
            keyboard_to_seat: HashMap::new(),
            active_surface_for_seat: HashMap::new(),
            last_sender_id: 0,
            client_to_host_queue: Vec::new(),
            host_to_client_queue: Vec::new(),
            allocator,
            virtwayland_channel: None,
            host_dmabuf_id: None,
            host_shm_id: None,
            host_text_input_manager_v1_id: None,
            host_text_input_extension_v1_id: None,
            supported_formats: HashSet::new(),
            host_globals: HashMap::new(),
            pending_params: HashMap::new(),
            feedback_index_maps: HashMap::new(),
            gpu_accel,
            disable_xdg_decoration,
        }
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new(false, false)
    }
}
