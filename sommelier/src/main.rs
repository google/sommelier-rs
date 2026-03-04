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

#[tokio::main]
async fn main() {
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env).init();

    let args: Vec<String> = std::env::args().collect();
    let mut display = "wayland-proxy-0".to_string();
    let mut use_virtgpu = true;
    let mut local_compositor = None;
    let mut gpu_accel = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--virtgpu-channel" => {
                use_virtgpu = true;
                local_compositor = None;
            }
            "--local-compositor" => {
                if i + 1 < args.len() {
                    use_virtgpu = false;
                    local_compositor = Some(args[i + 1].clone());
                    i += 1;
                } else {
                    log::error!("--local-compositor requires a path argument");
                    return;
                }
            }
            "--gpu-accel" => {
                gpu_accel = true;
            }
            arg => {
                // If it's not a flag, assume it's the display name
                if !arg.starts_with("--") {
                    display = arg.to_string();
                } else {
                    log::error!("Unknown argument: {}", arg);
                }
            }
        }
        i += 1;
    }

    // Need XDG_RUNTIME_DIR
    let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR not set");
    let socket_path = format!("{}/{}", xdg_runtime, display);

    // Clean up old socket
    let _ = std::fs::remove_file(&socket_path);

    proxy::run(&socket_path, use_virtgpu, local_compositor, gpu_accel).await;
}
