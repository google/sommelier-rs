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

use crate::protocols::wayland::wl_seat;
use crate::state::Context;
use crate::wire::Action;

pub struct SeatHandler;

impl wl_seat::WlSeatHandler for SeatHandler {
    fn on_get_keyboard(&mut self, ctx: &mut Context, id: u32) -> Action {
        let guest_seat_id = ctx.last_sender_id;
        ctx.keyboard_to_seat.insert(id, guest_seat_id);
        ctx.shadow_table.track_interface(id, "wl_keyboard".to_string());
        Action::Forward
    }
}
