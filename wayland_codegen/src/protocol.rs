use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct Protocol {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "$value")]
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
    #[serde(rename = "$value")]
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
    pub allow_null: Option<String>,
    #[serde(rename = "@enum", default)]
    pub enum_name: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Enum {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@bitfield", default)]
    pub bitfield: Option<String>,
    #[serde(rename = "$value")]
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
