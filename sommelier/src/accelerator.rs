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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    InvalidModifier(String),
    InvalidKeysym(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidModifier(tok) => write!(f, "Invalid modifier syntax or tag: '{}'", tok),
            Self::InvalidKeysym(sym) => write!(f, "Unknown or invalid keysym: '{}'", sym),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parses a single token into an `Accelerator`, or returns a `ParseError`.
pub fn parse_accelerator(token: &str) -> Result<Accelerator, ParseError> {
    let mut token = token.trim();
    let mut modifiers = 0;

    while token.starts_with('<') {
        let end_idx = token.find('>').ok_or_else(|| ParseError::InvalidModifier(token.to_string()))?;
        let mod_tag = &token[..=end_idx];
        match mod_tag.to_ascii_lowercase().as_str() {
            "<control>" => modifiers |= CONTROL_MASK,
            "<alt>" => modifiers |= ALT_MASK,
            "<shift>" => modifiers |= SHIFT_MASK,
            _ => return Err(ParseError::InvalidModifier(token.to_string())),
        }
        token = &token[end_idx + 1..];
    }

    if token.is_empty() {
        return Err(ParseError::InvalidKeysym("Empty keysym".to_string()));
    }

    let sym = xkb::keysym_from_name(token, xkb::KEYSYM_CASE_INSENSITIVE);
    if sym.raw() == xkb::keysyms::KEY_NoSymbol {
        return Err(ParseError::InvalidKeysym(token.to_string()));
    }

    Ok(Accelerator {
        modifiers,
        symbol: sym.raw(),
    })
}

/// Parse a `SOMMELIER_ACCELERATORS`-style string into a list of accelerators.
pub fn parse_accelerators(s: &str) -> Result<Vec<Accelerator>, ParseError> {
    let mut result = Vec::new();
    for token in s.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        result.push(parse_accelerator(token)?);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accelerators_standard_list() {
        let list = parse_accelerators(
            "Super_L,<Alt>bracketleft,<ALT>bracketright,<Control>space",
        )
        .unwrap();
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
        assert!(parse_accelerators("").unwrap().is_empty());
    }

    #[test]
    fn parse_fails_on_invalid() {
        // Unknown keysym
        assert_eq!(
            parse_accelerators("invalid_key_name"),
            Err(ParseError::InvalidKeysym("invalid_key_name".to_string()))
        );

        // Invalid modifier tag
        assert_eq!(
            parse_accelerators("<Ctrl>a"),
            Err(ParseError::InvalidModifier("<Ctrl>a".to_string()))
        );

        // Missing closing bracket
        assert_eq!(
            parse_accelerators("<Controla"),
            Err(ParseError::InvalidModifier("<Controla".to_string()))
        );

        // Multiple modifiers + missing keysym
        assert_eq!(
            parse_accelerators("<Control><Alt>"),
            Err(ParseError::InvalidKeysym("Empty keysym".to_string()))
        );
    }
}
