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
use smallvec::SmallVec;

/// A Wayland object ID allocated by the **guest** (client) side.
///
/// Request handlers (client→host) receive guest IDs. Use [`ShadowTable::host_id_of`]
/// to translate to the corresponding host ID. Passing a `GuestId` where a `HostId`
/// is expected (or vice versa) is a **compile error**.
///
/// The inner field is intentionally `pub(crate)` so that arbitrary `GuestId(host_id)`
/// constructions cannot be made from outside this crate, preserving the type invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GuestId(pub(crate) u32);

impl GuestId {
    /// Wrap the raw sender ID from a **client→host request** handler.
    /// Only call this in handlers where `ctx.last_sender_id` is a guest ID.
    #[inline]
    pub(crate) fn from_request_sender(ctx: &Context) -> Self {
        Self(ctx.last_sender_id)
    }

    /// Extract the raw u32 value (e.g. for wire serialization).
    #[inline]
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// A Wayland object ID allocated by the **host** compositor side.
///
/// Event handlers (host→client) receive host IDs. Use [`ShadowTable::guest_id_of`]
/// to translate to the corresponding guest ID. Passing a `HostId` where a `GuestId`
/// is expected (or vice versa) is a **compile error**.
///
/// The inner field is intentionally `pub(crate)` so that arbitrary `HostId(guest_id)`
/// constructions cannot be made from outside this crate, preserving the type invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostId(pub(crate) u32);

impl HostId {
    /// Wrap the raw sender ID from a **host→client event** handler.
    /// Only call this in handlers where `ctx.last_sender_id` is a host ID.
    #[inline]
    pub(crate) fn from_event_sender(ctx: &Context) -> Self {
        Self(ctx.last_sender_id)
    }

    /// Extract the raw u32 value (e.g. for wire serialization).
    #[inline]
    pub fn raw(self) -> u32 {
        self.0
    }
}

// The Wayland protocol reserves object IDs >= 0xFF000000 for server-allocated
// objects. Client-allocated IDs are constrained to [1, 0xFEFFFFFF] by the
// server: https://gitlab.freedesktop.org/wayland/wayland/-/blob/main/src/wayland-server.c
//
// We no longer use sentinel IDs at all. Instead, each internally-bound
// interface stores its host ID in a dedicated `ctx.host_*_id` field, and we
// register it with `track_host_interface` so proxy.rs can dispatch inbound
// host events without any magic number hackery.

#[allow(dead_code)]
pub struct ShadowTable {
    guest_to_host: HashMap<u32, u32>,
    host_to_guest: HashMap<u32, u32>,
    interfaces: HashMap<u32, String>,
    host_interfaces: HashMap<u32, String>,
    next_host_id: u32,
}

impl ShadowTable {
    pub fn new() -> Self {
        Self {
            guest_to_host: HashMap::new(),
            host_to_guest: HashMap::new(),
            interfaces: HashMap::new(),
            host_interfaces: HashMap::new(),
            // Start at 2 to mimic standard Wayland client behavior.
            // ID 1 is reserved for wl_display.
            next_host_id: 2,
        }
    }

    pub fn allocate_host_id(&mut self) -> u32 {
        loop {
            let id = self.next_host_id;
            // Advance and skip the Wayland-reserved IDs 0 (null) and 1 (wl_display).
            // wrapping_add(1).max(2) handles the u32::MAX → 0 → 2 wrap in one step.
            self.next_host_id = self.next_host_id.wrapping_add(1).max(2);
            if id >= 2 && !self.host_to_guest.contains_key(&id) {
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

    pub fn track_host_interface(&mut self, host_id: u32, interface: String) {
        self.host_interfaces.insert(host_id, interface);
    }

    /// Remove a host-side interface registration.
    ///
    /// Call this when a host object is destroyed (e.g. `zcr_extended_keyboard_v1.destroy`)
    /// to prevent stale events for the recycled ID from being dispatched.
    pub fn remove_host_interface(&mut self, host_id: u32) {
        self.host_interfaces.remove(&host_id);
    }

    pub fn get_interface(&self, guest_id: u32) -> Option<&String> {
        self.interfaces.get(&guest_id)
    }

    pub fn get_host_interface(&self, host_id: u32) -> Option<&String> {
        self.host_interfaces.get(&host_id)
    }

    /// Typed lookup: translate a guest-allocated object ID to its host counterpart.
    /// Use in **client→host request** handlers where `ctx.last_sender_id` is a guest ID.
    pub fn host_id_of(&self, guest: GuestId) -> Option<HostId> {
        self.guest_to_host.get(&guest.0).map(|&h| HostId(h))
    }

    /// Typed lookup: translate a host-allocated object ID to its guest counterpart.
    /// Use in **host→client event** handlers where `ctx.last_sender_id` is a host ID.
    pub fn guest_id_of(&self, host: HostId) -> Option<GuestId> {
        self.host_to_guest.get(&host.0).map(|&g| GuestId(g))
    }

    pub fn remove_id(&mut self, guest_id: u32) {
        if let Some(host_id) = self.guest_to_host.remove(&guest_id) {
            self.host_to_guest.remove(&host_id);
            self.host_interfaces.remove(&host_id);
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
    /// Pending messages to send from client→host (e.g. ack_key, bind requests).
    ///
    /// `SmallVec<[u8; 32]>` stores messages ≤ 32 bytes inline (all three
    /// keyboard extension messages are ≤ 16 bytes), so the hot-path
    /// `send_ack_key` call incurs **zero heap allocations** for the message
    /// buffer. Larger messages (registry bind, text-input, etc.) fall back
    /// to heap automatically, matching the previous `Vec<u8>` behaviour.
    pub client_to_host_queue: Vec<(SmallVec<[u8; 32]>, Vec<RawFd>)>,
    pub host_to_client_queue: Vec<(SmallVec<[u8; 32]>, Vec<RawFd>)>,
    pub allocator: Option<Allocator>,
    pub virtwayland_channel: Option<Arc<VirtWaylandChannel>>,
    pub host_dmabuf_id: Option<u32>,
    pub host_shm_id: Option<u32>,
    pub host_text_input_manager_v1_id: Option<u32>,
    pub host_text_input_extension_v1_id: Option<u32>,
    /// Host-side zcr_keyboard_extension_v1 object ID (bound internally on startup).
    pub host_keyboard_extension_id: Option<HostId>,
    /// Maps host keyboard ID → host extended-keyboard ID for `ack_key`.
    ///
    /// Both key and value are [`HostId`]s intentionally — using [`GuestId`] here
    /// by mistake is a **compile error**, preventing the direction bug where a
    /// client→host request handler reads `ctx.last_sender_id` (a guest ID) and
    /// uses it to look up a host-keyed map.
    pub keyboard_to_extended_keyboard: HashMap<HostId, HostId>,
    /// Parsed SOMMELIER_ACCELERATORS: keys the host should handle.
    pub accelerators: Vec<crate::accelerator::Accelerator>,
    pub supported_formats: HashSet<u32>,
    pub host_globals: HashMap<String, u32>,
    pub pending_params: HashMap<u32, Vec<PendingParam>>,
    pub feedback_index_maps: HashMap<u32, HashMap<u16, u16>>,
    pub gpu_accel: bool,
    pub xdg_decoration: bool,
}

impl Context {
    pub fn new(gpu_accel: bool, xdg_decoration: bool) -> Self {
        // Initialize allocator
        let allocator = match Allocator::new() {
            Ok(alloc) => Some(alloc),
            Err(e) => {
                warn!("Failed to initialize GBM allocator: {}", e);
                None
            }
        };

        let accelerators_env = std::env::var("SOMMELIER_ACCELERATORS").unwrap_or_default();
        let accelerators = match crate::accelerator::parse_accelerators(&accelerators_env) {
            Ok(list) => list,
            Err(e) => {
                // A malformed accelerator config should not crash the proxy — that
                // would break every app in the container. Degrade to no filtering
                // (all keys forwarded to guest) and log a clear error.
                warn!(
                    "Invalid SOMMELIER_ACCELERATORS '{}': {}. \
                     Accelerator filtering disabled.",
                    accelerators_env, e
                );
                Vec::new()
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
            host_keyboard_extension_id: None,
            keyboard_to_extended_keyboard: HashMap::new(),
            accelerators,
            supported_formats: HashSet::new(),
            host_globals: HashMap::new(),
            pending_params: HashMap::new(),
            feedback_index_maps: HashMap::new(),
            gpu_accel,
            xdg_decoration,
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_host_id_skips_zero_and_one() {
        let mut table = ShadowTable::new();
        // Drain the normal range and force a wrap-around.
        // We fill IDs 2..=u32::MAX (not practical), so instead we simulate
        // the internal state just after a wrap by directly setting next_host_id.
        table.next_host_id = u32::MAX;
        // Allocate once: should skip MAX itself only if it's in the map.
        // MAX is not in the map so it gets returned.
        let id = table.allocate_host_id();
        assert!(id >= 2, "must never return 0 or 1, got {}", id);
    }

    #[test]
    fn allocate_host_id_wraps_correctly() {
        let mut table = ShadowTable::new();
        // Simulate state right after wrapping: next_host_id is 0 → adjusted to 2.
        table.next_host_id = 0;
        let id = table.allocate_host_id();
        assert!(id >= 2, "post-wrap allocation must skip reserved IDs, got {}", id);
    }

    #[test]
    fn guest_id_and_host_id_raw_round_trip() {
        // Verify the typed constructors and raw() round-trip through the same u32.
        let ctx = Context::new(false, false);
        // GuestId::from_request_sender reads ctx.last_sender_id.
        let mut ctx = ctx;
        ctx.last_sender_id = 42;
        let gid = GuestId::from_request_sender(&ctx);
        assert_eq!(gid.raw(), 42);

        ctx.last_sender_id = 99;
        let hid = HostId::from_event_sender(&ctx);
        assert_eq!(hid.raw(), 99);
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new(false, false)
    }
}
