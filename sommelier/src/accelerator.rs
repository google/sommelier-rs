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

//! Parser for the `SOMMELIER_ACCELERATORS` environment variable.
//!
//! The variable holds a comma-separated list of key combinations that the
//! **host compositor** should handle. Each entry is a keysym name optionally
//! preceded by modifier tags:
//!
//! ```text
//! SOMMELIER_ACCELERATORS="Super_L,<Alt>bracketleft,<Alt>bracketright,<Control>space"
//! ```
//!
//! Keys matching this list are acked as `NOT_HANDLED` via
//! `zcr_extended_keyboard_v1.ack_key`, causing the host to process the
//! accelerator. All other keys are acked as `HANDLED`, keeping them in the
//! guest.

use xkbcommon::xkb;

/// Modifier bitmask constants matching the sommelier C convention.
pub const CONTROL_MASK: u32 = 1 << 0;
pub const ALT_MASK: u32 = 1 << 1;
pub const SHIFT_MASK: u32 = 1 << 2;

// We need xkb_keysym_to_lower for case-insensitive keysym matching.
// The xkbcommon-rs crate does not expose this, so import directly.
#[link(name = "xkbcommon")]
extern "C" {
    pub fn xkb_keysym_to_lower(sym: u32) -> u32;
}

/// A parsed accelerator: modifier bitmask + lowercase keysym.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accelerator {
    pub modifiers: u32,
    pub symbol: u32,
}

/// Parse a `SOMMELIER_ACCELERATORS`-style string into a list of accelerators.
pub fn parse_accelerators(s: &str) -> Vec<Accelerator> {
    let mut result = Vec::new();
    for token in s.split(',') {
        let mut token = token.trim();
        if token.is_empty() {
            continue;
        }
        let mut modifiers = 0;
        loop {
            if token.starts_with("<Control>") {
                modifiers |= CONTROL_MASK;
                token = &token[9..];
            } else if token.starts_with("<Alt>") {
                modifiers |= ALT_MASK;
                token = &token[5..];
            } else if token.starts_with("<Shift>") {
                modifiers |= SHIFT_MASK;
                token = &token[7..];
            } else {
                break;
            }
        }
        let sym = xkb::keysym_from_name(token, xkb::KEYSYM_CASE_INSENSITIVE);
        if sym.raw() != xkb::keysyms::KEY_NoSymbol {
            result.push(Accelerator {
                modifiers,
                symbol: sym.raw(),
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accelerators_standard_list() {
        let list = parse_accelerators(
            "Super_L,<Alt>bracketleft,<Alt>bracketright,<Control>space,invalid_key_name",
        );
        assert_eq!(list.len(), 4);

        assert_eq!(list[0].modifiers, 0);
        assert_eq!(list[0].symbol, xkb::keysyms::KEY_Super_L);

        assert_eq!(list[1].modifiers, ALT_MASK);
        assert_eq!(list[1].symbol, xkb::keysyms::KEY_bracketleft);

        assert_eq!(list[2].modifiers, ALT_MASK);
        assert_eq!(list[2].symbol, xkb::keysyms::KEY_bracketright);

        assert_eq!(list[3].modifiers, CONTROL_MASK);
        assert_eq!(list[3].symbol, xkb::keysyms::KEY_space);
    }

    #[test]
    fn parse_empty_string() {
        assert!(parse_accelerators("").is_empty());
    }
}
