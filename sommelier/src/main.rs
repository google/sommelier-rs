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

use clap::Parser;

mod accelerator;
mod allocator;
mod connection;
mod handler;
mod proxy;
mod state;
mod virtwl;
mod virtwl_channel;
mod wire;

mod protocols {
    include!(concat!(env!("OUT_DIR"), "/wayland_protocol.rs"));
    include!(concat!(env!("OUT_DIR"), "/xdg_shell_protocol.rs"));
    include!(concat!(env!("OUT_DIR"), "/linux_dmabuf_v1_protocol.rs"));
    include!(concat!(env!("OUT_DIR"), "/viewporter_protocol.rs"));
    include!(concat!(
        env!("OUT_DIR"),
        "/text-input-unstable-v3_protocol.rs"
    ));
    include!(concat!(
        env!("OUT_DIR"),
        "/text-input-unstable-v1_protocol.rs"
    ));
    include!(concat!(
        env!("OUT_DIR"),
        "/text-input-extension-unstable-v1_protocol.rs"
    ));
    include!(concat!(
        env!("OUT_DIR"),
        "/xdg_decoration_unstable_v1_protocol.rs"
    ));
    include!(concat!(env!("OUT_DIR"), "/fractional_scale_v1_protocol.rs"));
    include!(concat!(
        env!("OUT_DIR"),
        "/keyboard_extension_unstable_v1_protocol.rs"
    ));
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Connect to a local compositor at PATH, used for debug only
    #[arg(long)]
    local_compositor: Option<String>,

    /// Enable GPU acceleration (virtio-gpu). Currently broken in this branch!
    #[arg(long, hide = true, default_value_t = false)]
    gpu_accel: bool,

    /// Enable XDG Decoration support
    #[arg(long)]
    xdg_decoration: bool,

    /// Use virtio-wayland channel at PATH (defaults to /dev/wl0 if --local-compositor is not specified)
    #[arg(long)]
    virtio_wl: Option<String>,

    /// The display name (e.g. wayland-proxy-0)
    #[arg(default_value = "wayland-proxy-0")]
    display: String,
}

// SAFETY INVARIANT for `unsafe impl Send for KeyboardHandler` (keyboard.rs):
// KeyboardHandler holds xkb::Context/Keymap/State which are !Send. The impl is
// sound only when client tasks are never migrated across OS threads. We enforce
// this by using a single-threaded Tokio runtime — all tasks run on one thread,
// so no cross-thread migration can occur.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env).init();

    let args = Args::parse();

    let local_compositor = args.local_compositor;
    let gpu_accel = args.gpu_accel;
    let xdg_decoration = args.xdg_decoration;
    let mut virtio_wl = args.virtio_wl;

    if local_compositor.is_none() && virtio_wl.is_none() {
        virtio_wl = Some("/dev/wl0".to_string());
    }

    if virtio_wl.is_some() && gpu_accel {
        log::error!("--virtio-wl and --gpu-accel cannot be used together");
        return;
    }

    // Need XDG_RUNTIME_DIR
    let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR not set");
    let socket_path = format!("{}/{}", xdg_runtime, args.display);

    // Clean up old socket
    let _ = std::fs::remove_file(&socket_path);

    proxy::run(
        &socket_path,
        local_compositor,
        gpu_accel,
        xdg_decoration,
        virtio_wl,
    )
    .await;
}
mod test_xkb;
