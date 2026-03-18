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

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Protocol {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "$value", default)]
    pub items: Vec<ProtocolItem>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolItem {
    Copyright(String),
    Description(Description),
    Interface(Interface),
}

#[derive(Debug, Deserialize, Clone)]
pub struct Description {
    #[serde(rename = "@summary", default)]
    pub summary: Option<String>,
    #[serde(rename = "$value", default)]
    pub text: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Interface {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@version")]
    pub version: u32,
    #[serde(rename = "$value", default)]
    pub items: Vec<InterfaceItem>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum InterfaceItem {
    Description(Description),
    Request(Message),
    Event(Message),
    Enum(Enum),
}

#[derive(Debug, Deserialize, Clone)]
pub struct Message {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@type", default)]
    pub msg_type: Option<String>,
    #[serde(rename = "@since", default)]
    pub since: Option<u32>,
    #[serde(rename = "$value", default)]
    pub items: Vec<MessageItem>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum MessageItem {
    Description(Description),
    Arg(Arg),
}

#[derive(Debug, Deserialize, Clone)]
pub struct Arg {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@type")]
    pub typ: String,
    #[serde(rename = "@interface", default)]
    pub interface: Option<String>,
    #[serde(rename = "@summary", default)]
    pub summary: Option<String>,
    #[serde(rename = "@allow-null", default)]
    pub allow_null: Option<bool>,
    #[serde(rename = "@enum", default)]
    pub enum_name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Enum {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@bitfield", default)]
    pub bitfield: Option<String>,
    #[serde(rename = "$value", default)]
    pub items: Vec<EnumItem>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub enum EnumItem {
    Description(Description),
    Entry(Entry),
}

#[derive(Debug, Deserialize, Clone)]
pub struct Entry {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@value")]
    pub value: String,
    #[serde(rename = "@summary", default)]
    pub summary: Option<String>,
    #[serde(rename = "@since", default)]
    pub since: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_get_registry() {
        let xml = r#"
        <protocol name="wayland">
          <interface name="wl_display" version="1">
            <request name="get_registry">
              <arg name="registry" type="new_id" interface="wl_registry"/>
            </request>
          </interface>
        </protocol>
        "#;

        let protocol: Protocol = quick_xml::de::from_str(xml).expect("Failed to parse XML");
        assert_eq!(protocol.name, "wayland");
        assert_eq!(protocol.items.len(), 1);

        let ProtocolItem::Interface(interface) = &protocol.items[0] else {
            panic!("Expected ProtocolItem::Interface");
        };
        assert_eq!(interface.name, "wl_display");
        assert_eq!(interface.version, 1);
        assert_eq!(interface.items.len(), 1);

        let InterfaceItem::Request(req) = &interface.items[0] else {
            panic!("Expected InterfaceItem::Request");
        };
        assert_eq!(req.name, "get_registry");
        assert_eq!(req.items.len(), 1);

        let MessageItem::Arg(arg) = &req.items[0] else {
            panic!("Expected MessageItem::Arg");
        };
        assert_eq!(arg.name, "registry");
        assert_eq!(arg.typ, "new_id");
        assert_eq!(arg.interface.as_deref(), Some("wl_registry"));
    }
}
