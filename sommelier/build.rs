use std::env;
use std::fs;
use std::path::Path;
use wayland_codegen::generator::generate;
use wayland_codegen::parse;

fn main() {
    let out_dir = env::var_os("OUT_DIR").unwrap();

    let protocols = [
        ("wayland", "../third_party/protocols/wayland.xml"),
        ("xdg_shell", "../third_party/protocols/xdg-shell.xml"),
        ("linux_dmabuf_v1", "../third_party/protocols/linux-dmabuf-v1.xml"),
        ("viewporter", "../third_party/protocols/viewporter.xml"),
        (
            "text-input-unstable-v3",
            "../third_party/protocols/text-input-unstable-v3.xml",
        ),
        (
            "xdg_decoration_unstable_v1",
            "../third_party/protocols/xdg-decoration-unstable-v1.xml",
        ),
        (
            "fractional_scale_v1",
            "../third_party/protocols/fractional-scale-v1.xml",
        ),
    ];

    for (name, path_str) in &protocols {
        let dest_path = Path::new(&out_dir).join(format!("{}_protocol.rs", name));
        let protocol_path = Path::new(path_str);

        println!("cargo:rerun-if-changed={}", protocol_path.display());

        let protocol =
            parse(protocol_path).unwrap_or_else(|_| panic!("Failed to parse {}", path_str));
        let code = generate(&protocol);

        fs::write(&dest_path, code)
            .unwrap_or_else(|_| panic!("Failed to write {}", dest_path.display()));
    }
}
