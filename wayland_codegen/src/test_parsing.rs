#[cfg(test)]
mod tests {
    use crate::protocol::*;

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

        if let ProtocolItem::Interface(interface) = &protocol.items[0] {
            assert_eq!(interface.name, "wl_display");
            if let InterfaceItem::Request(req) = &interface.items[0] {
                assert_eq!(req.name, "get_registry");
                if let MessageItem::Arg(arg) = &req.items[0] {
                    assert_eq!(arg.name, "registry");
                    assert_eq!(arg.typ, "new_id");
                    assert_eq!(arg.interface, Some("wl_registry".to_string()));
                    return;
                }
            }
        }
        panic!("Structure mismatch");
    }
}
