use clap::Parser;

mod allocator;
mod connection;
mod handler;
mod proxy;
mod state;
mod virtgpu_channel;
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
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Wayland display name to listen on.
    #[arg(default_value = "wayland-proxy-0")]
    display: String,

    /// Use virtgpu channel for Wayland proxying (default).
    #[arg(long, conflicts_with = "local_compositor")]
    virtgpu_channel: bool,

    /// Use local compositor socket path for Wayland proxying.
    #[arg(long, conflicts_with = "virtgpu_channel")]
    local_compositor: Option<String>,

    /// Enable GPU acceleration.
    #[arg(long)]
    gpu_accel: bool,
}

#[tokio::main]
async fn main() {
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env).init();

    let args = Args::parse();

    let display = args.display;
    let local_compositor = args.local_compositor;
    let gpu_accel = args.gpu_accel;
    // Default to virtgpu unless local-compositor is specified.
    let use_virtgpu = local_compositor.is_none();

    // Need XDG_RUNTIME_DIR
    let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR not set");
    let socket_path = format!("{}/{}", xdg_runtime, display);

    // Clean up old socket
    let _ = std::fs::remove_file(&socket_path);

    proxy::run(&socket_path, use_virtgpu, local_compositor, gpu_accel).await;
}
