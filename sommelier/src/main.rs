use clap::Parser;

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
        "/xdg_decoration_unstable_v1_protocol.rs"
    ));
    include!(concat!(env!("OUT_DIR"), "/fractional_scale_v1_protocol.rs"));
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Connect to a local compositor at PATH
    #[arg(long)]
    local_compositor: Option<String>,

    /// Enable GPU acceleration
    #[arg(long)]
    gpu_accel: bool,

    /// Disable XDG Decoration support
    #[arg(long)]
    no_xdg_decoration: bool,

    /// Use virtio-wayland channel at PATH (defaults to /dev/wl0 if --local-compositor is not specified)
    #[arg(long)]
    virtio_wayland: Option<String>,

    /// The display name (e.g. wayland-proxy-0)
    #[arg(default_value = "wayland-proxy-0")]
    display: String,
}

#[tokio::main]
async fn main() {
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env).init();

    let args = Args::parse();

    let local_compositor = args.local_compositor;
    let gpu_accel = args.gpu_accel;
    let disable_xdg_decoration = args.no_xdg_decoration;
    let mut virtio_wayland = args.virtio_wayland;

    if local_compositor.is_none() && virtio_wayland.is_none() {
        virtio_wayland = Some("/dev/wl0".to_string());
    }

    if virtio_wayland.is_some() && gpu_accel {
        log::error!("--virtio-wayland and --gpu-accel cannot be used together");
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
        disable_xdg_decoration,
        virtio_wayland,
    )
    .await;
}
