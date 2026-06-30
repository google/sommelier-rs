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
        (
            "linux_dmabuf_v1",
            "../third_party/protocols/linux-dmabuf-v1.xml",
        ),
        ("viewporter", "../third_party/protocols/viewporter.xml"),
        (
            "text-input-unstable-v3",
            "../third_party/protocols/text-input-unstable-v3.xml",
        ),
        (
            "text-input-unstable-v1",
            "../third_party/protocols/text-input-unstable-v1.xml",
        ),
        (
            "text-input-extension-unstable-v1",
            "../third_party/protocols/text-input-extension-unstable-v1.xml",
        ),
        (
            "xdg_decoration_unstable_v1",
            "../third_party/protocols/xdg-decoration-unstable-v1.xml",
        ),
        (
            "fractional_scale_v1",
            "../third_party/protocols/fractional-scale-v1.xml",
        ),
        (
            "keyboard_extension_unstable_v1",
            "../third_party/protocols/keyboard-extension-unstable-v1.xml",
        ),
        ("aura_shell", "../third_party/protocols/aura-shell.xml"),
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
