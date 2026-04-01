use xkbcommon::xkb;

fn main() {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = xkb::Keymap::new_from_names(
        &context,
        "", "", "", "", "",
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    ).unwrap();
    
    let sym = xkb::keysyms::KEY_a;
    // How to get keycode? There is no direct `key_for_sym` in xkbcommon, 
    // usually we iterate through all keycodes and check if `keymap.key_get_syms_by_level` contains our sym.
    
    let mut found_keycode = None;
    let min_keycode = keymap.min_keycode();
    let max_keycode = keymap.max_keycode();
    
    for keycode in min_keycode..=max_keycode {
        let syms = keymap.key_get_syms_by_level(keycode, 0, 0);
        if syms.contains(&sym) {
            found_keycode = Some(keycode);
            break;
        }
    }
    
    println!("keycode for a: {:?}", found_keycode);
}
