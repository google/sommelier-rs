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

pub mod generator;
pub mod protocol;

use crate::protocol::Protocol;
use quick_xml::de::from_reader;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

pub fn parse<P: AsRef<Path>>(path: P) -> Result<Protocol, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let protocol: Protocol = from_reader(reader)?;
    Ok(protocol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_parse_wayland() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let protocol_path = manifest_dir.join("../third_party/protocols/wayland.xml");
        let protocol = parse(protocol_path).expect("Failed to parse wayland.xml");
        assert_eq!(protocol.name, "wayland");

        let interfaces: Vec<&crate::protocol::Interface> = protocol
            .items
            .iter()
            .filter_map(|item| {
                if let crate::protocol::ProtocolItem::Interface(i) = item {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();

        assert!(!interfaces.is_empty());
    }

    #[test]
    fn test_generate_wayland() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let protocol_path = manifest_dir.join("../third_party/protocols/wayland.xml");
        let protocol = parse(protocol_path).expect("Failed to parse wayland.xml");
        let code = generator::generate(&protocol);
        assert!(code.contains("pub mod wl_display"));
        assert!(code.contains("const REQ_SYNC"));
        assert!(code.contains("pub enum Request"));
        assert!(code.contains("fn from_wire"));
    }
}
