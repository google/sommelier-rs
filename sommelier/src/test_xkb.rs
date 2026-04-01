#[test]
fn test_keysym_to_keycode() {
    use xkbcommon::xkb;
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(
        &context,
        "", "", "", "", None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    ).unwrap();
    
    let sym = xkb::keysyms::KEY_a;
    let mut found_keycode = None;
    let min_keycode = keymap.min_keycode().raw();
    let max_keycode = keymap.max_keycode().raw();
    
    for keycode_raw in min_keycode..=max_keycode {
        let keycode = keycode_raw.into();
        let syms = keymap.key_get_syms_by_level(keycode, 0, 0);
        if syms.iter().any(|s| s.raw() == sym) {
            found_keycode = Some(keycode);
            break;
        }
    }
    
    println!("keycode for a: {:?}", found_keycode);
    assert!(found_keycode.is_some());
}
