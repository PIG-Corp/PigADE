//! Pure legacy terminal input encoding for the host's alacritty state.2 profile.
//! No output parsing lives here. Printable keydown text is intentionally deferred
//! to the platform text-input commit callback, including EN/RU and IME input.

// Mirrored from vendor/alacritty_terminal/src/term/mod.rs; profile changes must
// review these flags. Linking the terminal parser into the UI is unnecessary.
pub const SHOW_CURSOR: u32 = 1;
pub const APP_CURSOR: u32 = 1 << 1;
pub const LINE_FEED_NEW_LINE: u32 = 1 << 8;
const KITTY_KEYBOARD_PROTOCOL: u32 = 0b1_1111 << 18;

#[derive(Clone, Copy, Debug, Default)]
pub struct Modifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub platform: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct Key<'a> {
    /// Platform-independent key name, not the text produced by the active layout.
    pub name: &'a str,
    pub text: Option<&'a str>,
    pub modifiers: Modifiers,
}
#[derive(Debug, PartialEq)]
pub enum Encoded {
    /// Let GPUI/IME deliver committed text exactly once. Never send key_char here.
    PlatformText {
        meta: bool,
    },
    Bytes(Vec<u8>),
    /// Do not silently claim support for an enabled protocol we cannot encode.
    Unsupported(&'static str),
    Ignore,
}

pub fn encode(key: Key<'_>, modes: u32) -> Encoded {
    let modifiers = key.modifiers;
    if modifiers.platform || matches!(key.name, "shift" | "control" | "alt" | "super") {
        return Encoded::Ignore;
    }
    let named_special = matches!(
        key.name,
        "enter"
            | "escape"
            | "tab"
            | "backspace"
            | "up"
            | "down"
            | "right"
            | "left"
            | "home"
            | "end"
            | "delete"
            | "insert"
            | "pageup"
            | "pagedown"
            | "f1"
            | "f2"
            | "f3"
            | "f4"
            | "f5"
            | "f6"
            | "f7"
            | "f8"
            | "f9"
            | "f10"
            | "f11"
            | "f12"
    );
    let printable = !named_special
        && (key.text.is_some() || key.name.chars().count() == 1 || key.name == "space");
    if let Some(error) = protocol_error(modes) {
        return Encoded::Unsupported(error);
    }
    if printable && !modifiers.control {
        return Encoded::PlatformText {
            meta: modifiers.alt,
        };
    }
    let modifier = 1
        + u8::from(modifiers.shift)
        + 2 * u8::from(modifiers.alt)
        + 4 * u8::from(modifiers.control);
    let cursor_suffix = match key.name {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(suffix) = cursor_suffix {
        return Encoded::Bytes(if modifier != 1 {
            format!("\x1b[1;{modifier}{suffix}").into_bytes()
        } else if modes & APP_CURSOR != 0 {
            format!("\x1bO{suffix}").into_bytes()
        } else {
            format!("\x1b[{suffix}").into_bytes()
        });
    }
    let numbered = match key.name {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(number) = numbered {
        return Encoded::Bytes(
            if modifier == 1 {
                format!("\x1b[{number}~")
            } else {
                format!("\x1b[{number};{modifier}~")
            }
            .into_bytes(),
        );
    }
    if let Some(suffix) = match key.name {
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" => Some('R'),
        "f4" => Some('S'),
        _ => None,
    } {
        return Encoded::Bytes(
            if modifier == 1 {
                format!("\x1bO{suffix}")
            } else {
                format!("\x1b[1;{modifier}{suffix}")
            }
            .into_bytes(),
        );
    }
    let mut bytes = match key.name {
        "enter" => {
            if modes & LINE_FEED_NEW_LINE != 0 {
                b"\r\n".to_vec()
            } else {
                vec![13]
            }
        }
        "escape" => vec![27],
        "backspace" => vec![if modifiers.control { 8 } else { 127 }],
        "tab" => {
            if modifiers.shift {
                b"\x1b[Z".to_vec()
            } else {
                vec![9]
            }
        }
        "space" if modifiers.control => vec![0],
        name if modifiers.control => {
            let byte = match name.as_bytes() {
                [letter] if letter.is_ascii_alphabetic() => letter.to_ascii_lowercase() - b'a' + 1,
                [b'@' | b'2'] => 0,
                [b'[' | b'3'] => 27,
                [b'\\' | b'4'] => 28,
                [b']' | b'5'] => 29,
                [b'^' | b'6'] => 30,
                [b'_' | b'7' | b'/'] => 31,
                [b'?' | b'8'] => 127,
                _ => return Encoded::Ignore,
            };
            vec![byte]
        }
        _ => return Encoded::Ignore,
    };
    if modifiers.alt {
        bytes.insert(0, 27);
    }
    Encoded::Bytes(bytes)
}

pub fn protocol_error(modes: u32) -> Option<&'static str> {
    (modes & KITTY_KEYBOARD_PROTOCOL != 0)
        .then_some("Расширенный keyboard protocol пока не поддерживается")
}

/// Local composition buffer. Preedit never reaches the PTY. A commit consumes
/// the staged Meta prefix once; subsequent platform commits cannot duplicate it.
#[derive(Default)]
pub struct TextInput {
    pub preedit: String,
    meta: bool,
}
impl TextInput {
    pub fn key(&mut self, meta: bool) {
        self.meta = meta && self.preedit.is_empty();
    }
    pub fn cancel(&mut self) {
        self.preedit.clear();
        self.meta = false;
    }
    pub fn commit(&mut self, text: &str) -> Vec<u8> {
        self.preedit.clear();
        let meta = std::mem::take(&mut self.meta);
        let mut bytes = Vec::with_capacity(text.len() + usize::from(meta));
        if meta && !text.is_empty() {
            bytes.push(27);
        }
        bytes.extend_from_slice(text.as_bytes());
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(name: &str, modifiers: Modifiers, modes: u32) -> Encoded {
        encode(
            Key {
                name,
                text: None,
                modifiers,
            },
            modes,
        )
    }
    #[test]
    fn controls_do_not_become_ui_commands() {
        let ctrl = Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(key("c", ctrl, 0), Encoded::Bytes(vec![3]));
        assert_eq!(key("d", ctrl, 0), Encoded::Bytes(vec![4]));
        assert_eq!(key("space", ctrl, 0), Encoded::Bytes(vec![0]));
        assert_eq!(
            key("escape", Modifiers::default(), 0),
            Encoded::Bytes(vec![27])
        );
    }
    #[test]
    fn normal_and_application_cursor_and_modified_keys() {
        for (name, suffix) in [
            ("up", 'A'),
            ("down", 'B'),
            ("right", 'C'),
            ("left", 'D'),
            ("home", 'H'),
            ("end", 'F'),
        ] {
            assert_eq!(
                key(name, Modifiers::default(), 0),
                Encoded::Bytes(format!("\x1b[{suffix}").into_bytes())
            );
            assert_eq!(
                key(name, Modifiers::default(), APP_CURSOR),
                Encoded::Bytes(format!("\x1bO{suffix}").into_bytes())
            );
            assert_eq!(
                key(
                    name,
                    Modifiers {
                        control: true,
                        shift: true,
                        ..Default::default()
                    },
                    APP_CURSOR
                ),
                Encoded::Bytes(format!("\x1b[1;6{suffix}").into_bytes())
            );
        }
        assert_eq!(
            key("enter", Modifiers::default(), LINE_FEED_NEW_LINE),
            Encoded::Bytes(b"\r\n".to_vec())
        );
        assert_eq!(
            key(
                "tab",
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
                0
            ),
            Encoded::Bytes(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            key(
                "delete",
                Modifiers {
                    alt: true,
                    ..Default::default()
                },
                0
            ),
            Encoded::Bytes(b"\x1b[3;3~".to_vec())
        );
    }
    #[test]
    fn named_control_keys_take_precedence_over_platform_text() {
        assert_eq!(
            encode(
                Key {
                    name: "enter",
                    text: Some("\r"),
                    modifiers: Modifiers::default()
                },
                LINE_FEED_NEW_LINE
            ),
            Encoded::Bytes(b"\r\n".to_vec())
        );
    }
    #[test]
    fn en_ru_text_has_only_one_committed_delivery_path() {
        let mut input = TextInput::default();
        let mut bytes = Vec::new();
        for text in ["a", "ф", "界", "e\u{301}", " "] {
            let encoded = encode(
                Key {
                    name: "a",
                    text: Some(text),
                    modifiers: Modifiers::default(),
                },
                0,
            );
            assert_eq!(encoded, Encoded::PlatformText { meta: false });
            input.key(false);
            bytes.extend(input.commit(text));
        }
        assert_eq!(String::from_utf8(bytes).unwrap(), "aф界e\u{301} ");
    }
    #[test]
    fn ime_preedit_is_local_and_meta_is_consumed_once() {
        let mut input = TextInput {
            preedit: "に".into(),
            ..Default::default()
        };
        input.key(true);
        input.preedit = "日本".into();
        assert_eq!(input.commit("日本"), "日本".as_bytes());
        assert!(input.preedit.is_empty());
        input.key(true);
        assert_eq!(input.commit("b"), b"\x1bb");
        assert_eq!(input.commit("c"), b"c");
        input.key(true);
        input.cancel();
        assert_eq!(input.commit("d"), b"d");
    }
    #[test]
    fn platform_shortcuts_and_unimplemented_extensions_are_explicit() {
        assert_eq!(
            key(
                "c",
                Modifiers {
                    platform: true,
                    ..Default::default()
                },
                0
            ),
            Encoded::Ignore
        );
        assert!(matches!(
            key("up", Modifiers::default(), 1 << 18),
            Encoded::Unsupported(_)
        ));
    }
}
