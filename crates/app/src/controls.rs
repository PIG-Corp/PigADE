//! Panel-management hotkeys and their UI-local modifier preference.
//!
//! This module is intentionally independent of GPUI and terminal input. It
//! recognizes only a small explicit set of panel keys; every other key remains
//! available to the focused terminal.

use crate::input::Modifiers;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const CONFIG_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 4096;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Modifier held before a panel-management key. The serialized names are a
/// stable local-config contract, independent of the display labels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modifier {
    #[default]
    Control,
    Alt,
    Super,
    ControlAlt,
    Disabled,
}

impl Modifier {
    pub fn matches(self, state: Modifiers) -> bool {
        match self {
            Self::Control => state.control && !state.alt && !state.platform,
            Self::Alt => !state.control && state.alt && !state.platform,
            Self::Super => !state.control && !state.alt && state.platform,
            Self::ControlAlt => state.control && state.alt && !state.platform,
            Self::Disabled => false,
        }
    }
    pub const ALL: [Self; 5] = [
        Self::Control,
        Self::Alt,
        Self::Super,
        Self::ControlAlt,
        Self::Disabled,
    ];

    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Alt => "alt",
            Self::Super => "super",
            Self::ControlAlt => "control_alt",
            Self::Disabled => "disabled",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Control => "Ctrl",
            Self::Alt => "Alt",
            Self::Super => "Super",
            Self::ControlAlt => "Ctrl+Alt",
            Self::Disabled => "Prefix",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Close,
    FocusMode,
    Focus(Direction),
    Swap(Direction),
    FocusLeft,
    FocusRight,
    SelectTab(u8),
    TransferToTab(u8),
    NextTab,
    PreviousTab,
    LaunchShell,
    ResizeLeft,
    ResizeRight,
}

/// Classifies a key using a configured modifier and exact modifier state.
/// Shift is recognized only where it selects a distinct action variant;
/// platform/meta and all other unexpected modifiers reject the binding.
pub fn classify(modifier: Modifier, key: &str, state: Modifiers) -> Option<Action> {
    let key = normalize_key(key);
    let shifted = state.shift;
    if !modifier.matches(state) {
        return None;
    }
    // GPUI's Linux XKB adapter emits the US shifted digit symbol and removes
    // Shift from punctuation keystrokes. Recover only our explicit transfer
    // bindings here; terminal input and dialog editors keep their native keys.
    if let Some(number) = match key {
        "!" => Some(1),
        "@" => Some(2),
        "#" => Some(3),
        "$" => Some(4),
        "%" => Some(5),
        "^" => Some(6),
        "&" => Some(7),
        "*" => Some(8),
        "(" => Some(9),
        _ => None,
    } {
        return Some(Action::TransferToTab(number));
    }
    if shifted {
        return match key {
            "up" => Some(Action::Swap(Direction::Up)),
            "down" => Some(Action::Swap(Direction::Down)),
            "left" => Some(Action::Swap(Direction::Left)),
            "right" => Some(Action::Swap(Direction::Right)),
            key if key.len() == 1 && matches!(key.as_bytes()[0], b'1'..=b'9') => {
                Some(Action::TransferToTab(key.parse().ok()?))
            }
            "tab" => Some(Action::PreviousTab),
            _ => None,
        };
    }
    match key {
        "q" => Some(Action::Close),
        "f" => Some(Action::FocusMode),
        "up" => Some(Action::Focus(Direction::Up)),
        "down" => Some(Action::Focus(Direction::Down)),
        "left" => Some(Action::Focus(Direction::Left)),
        "right" => Some(Action::Focus(Direction::Right)),
        "[" => Some(Action::FocusLeft),
        "]" => Some(Action::FocusRight),
        key if key.len() == 1 && matches!(key.as_bytes()[0], b'1'..=b'9') => {
            Some(Action::SelectTab(key.parse().ok()?))
        }
        "tab" => Some(Action::NextTab),
        "enter" | "return" => Some(Action::LaunchShell),
        ";" => Some(Action::ResizeLeft),
        "'" => Some(Action::ResizeRight),
        _ => None,
    }
}

fn normalize_key(key: &str) -> &str {
    match key {
        // On layouts where GPUI reports the produced Cyrillic character rather
        // than the physical key name, retain the same physical shortcuts.
        "й" | "Й" | "Q" => "q",
        "а" | "А" | "F" => "f",
        "х" => "[",
        "ъ" => "]",
        "ж" => ";",
        "э" => "'",
        other => other,
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredConfig {
    version: u32,
    modifier: Modifier,
}

pub fn config_path() -> Result<PathBuf, String> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(".config"),
    };
    Ok(base.join("pigade").join("controls.json"))
}

pub fn load() -> Result<Modifier, String> {
    load_from(&config_path()?)
}

pub fn save(modifier: Modifier) -> Result<(), String> {
    save_to(&config_path()?, modifier)
}

/// Load from an explicit path for isolated tests and callers that own the path.
pub fn load_from(path: &Path) -> Result<Modifier, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Modifier::default());
        }
        Err(error) => return Err(format!("could not inspect controls: {error}")),
        Ok(metadata) if !metadata.is_file() => {
            return Err("controls config must be a regular file".into());
        }
        Ok(_) => (),
    }
    let mut file = File::open(path).map_err(|error| format!("could not open controls: {error}"))?;
    let size = file
        .metadata()
        .map_err(|error| format!("could not inspect controls: {error}"))?
        .len();
    if size > MAX_CONFIG_BYTES {
        return Err("controls config exceeds 4096 bytes".into());
    }
    let mut bytes = Vec::with_capacity(size as usize);
    std::io::Read::by_ref(&mut file)
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read controls: {error}"))?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("controls config exceeds 4096 bytes".into());
    }
    let config: StoredConfig = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid controls config: {error}"))?;
    if config.version != CONFIG_VERSION {
        return Err(format!(
            "unsupported controls config version {}",
            config.version
        ));
    }
    Ok(config.modifier)
}

/// Atomically replace only this PigADE-local setting. The temporary file is
/// created exclusively with mode 0600, then file and directory are synced.
pub fn save_to(path: &Path, modifier: Modifier) -> Result<(), String> {
    let parent = path.parent().ok_or("controls path has no parent")?;
    let mut new_directories = Vec::new();
    let mut ancestor = parent;
    while !ancestor.exists() {
        new_directories.push(ancestor.to_owned());
        let Some(next) = ancestor.parent() else {
            break;
        };
        ancestor = next;
    }
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create controls directory: {error}"))?;
    let bytes = serde_json::to_vec(&StoredConfig {
        version: CONFIG_VERSION,
        modifier,
    })
    .map_err(|error| format!("could not encode controls config: {error}"))?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".controls-{}-{sequence}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut temp_created = false;
    let result = (|| {
        let mut file = options
            .open(&temp)
            .map_err(|error| format!("could not create controls temp file: {error}"))?;
        temp_created = true;
        file.write_all(&bytes)
            .map_err(|error| format!("could not write controls: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("could not sync controls: {error}"))?;
        drop(file);
        fs::rename(&temp, path).map_err(|error| format!("could not replace controls: {error}"))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("could not sync controls directory: {error}"))?;
        for created in &new_directories {
            if let Some(parent) = created.parent() {
                File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|error| {
                        format!("could not sync new controls directory entry: {error}")
                    })?;
            }
        }
        Ok(())
    })();
    if result.is_err() && temp_created {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn mods(control: bool, alt: bool, shift: bool, platform: bool) -> Modifiers {
        Modifiers {
            control,
            alt,
            shift,
            platform,
        }
    }

    fn temp_config_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "pigade-controls-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn stable_names_and_labels_are_explicit() {
        assert_eq!(
            Modifier::ALL.map(Modifier::stable_name),
            ["control", "alt", "super", "control_alt", "disabled"]
        );
        assert_eq!(
            Modifier::ALL.map(Modifier::label),
            ["Ctrl", "Alt", "Super", "Ctrl+Alt", "Prefix"]
        );
        assert_eq!(Modifier::default(), Modifier::Control);
    }

    #[test]
    fn control_shortcuts_and_shift_variants_are_exact() {
        let ctrl = mods(true, false, false, false);
        assert_eq!(classify(Modifier::Control, "q", ctrl), Some(Action::Close));
        assert_eq!(
            classify(Modifier::Control, "F", ctrl),
            Some(Action::FocusMode)
        );
        assert_eq!(
            classify(Modifier::Control, "left", ctrl),
            Some(Action::Focus(Direction::Left))
        );
        assert_eq!(
            classify(Modifier::Control, "left", mods(true, false, true, false)),
            Some(Action::Swap(Direction::Left))
        );
        assert_eq!(
            classify(Modifier::Control, "3", ctrl),
            Some(Action::SelectTab(3))
        );
        assert_eq!(
            classify(Modifier::Control, "3", mods(true, false, true, false)),
            Some(Action::TransferToTab(3))
        );
        assert_eq!(
            classify(Modifier::Control, "tab", mods(true, false, true, false)),
            Some(Action::PreviousTab)
        );
        assert_eq!(
            classify(Modifier::Control, "tab", ctrl),
            Some(Action::NextTab)
        );
        assert_eq!(
            classify(Modifier::Control, "enter", ctrl),
            Some(Action::LaunchShell)
        );
    }

    #[test]
    fn other_terminal_keys_and_extra_modifiers_are_passthrough() {
        let ctrl = mods(true, false, false, false);
        for key in ["c", "d", "z", "r", "h", "j", "k", "l", "x"] {
            assert_eq!(classify(Modifier::Control, key, ctrl), None, "{key}");
        }
        for state in [
            mods(true, true, false, false),
            mods(true, false, false, true),
            mods(false, false, false, true),
        ] {
            assert_eq!(classify(Modifier::Control, "q", state), None);
        }
        assert_eq!(
            classify(Modifier::Control, "q", mods(true, false, true, false)),
            None
        );
    }

    #[test]
    fn gpui_shifted_digit_symbols_select_transfer_without_rewriting_terminal_keys() {
        for (index, key) in ["!", "@", "#", "$", "%", "^", "&", "*", "("]
            .into_iter()
            .enumerate()
        {
            for shift in [false, true] {
                assert_eq!(
                    classify(Modifier::Control, key, mods(true, false, shift, false)),
                    Some(Action::TransferToTab(index as u8 + 1))
                );
            }
            assert_eq!(
                classify(Modifier::Control, key, mods(false, false, false, false)),
                None
            );
            assert_eq!(
                classify(Modifier::Control, key, mods(true, true, false, false)),
                None
            );
            assert_eq!(
                classify(Modifier::Disabled, key, mods(true, false, false, false)),
                None
            );
        }
        // Do not guess ambiguous RU punctuation or consume arbitrary Ctrl keys.
        assert_eq!(
            classify(Modifier::Control, ";", mods(true, false, false, false)),
            Some(Action::ResizeLeft)
        );
        assert_eq!(
            classify(Modifier::Control, "0", mods(true, false, true, false)),
            None
        );
    }

    #[test]
    fn alternative_modifiers_and_ru_physical_keys_work() {
        assert_eq!(
            classify(Modifier::Alt, "й", mods(false, true, false, false)),
            Some(Action::Close)
        );
        assert_eq!(
            classify(Modifier::ControlAlt, "а", mods(true, true, false, false)),
            Some(Action::FocusMode)
        );
        assert_eq!(
            classify(Modifier::Super, "q", mods(false, false, false, true)),
            Some(Action::Close)
        );
        assert_eq!(
            classify(Modifier::Disabled, "q", mods(false, false, false, false)),
            None
        );
        for (key, expected) in [
            ("х", Action::FocusLeft),
            ("ъ", Action::FocusRight),
            ("ж", Action::ResizeLeft),
            ("э", Action::ResizeRight),
        ] {
            assert_eq!(
                classify(Modifier::Control, key, mods(true, false, false, false)),
                Some(expected)
            );
        }
    }

    #[test]
    fn config_round_trip_is_atomic_private_and_strict() {
        let dir = temp_config_dir();
        let path = dir.join("pigade/controls.json");
        assert_eq!(load_from(&path).unwrap(), Modifier::Control);
        save_to(&path, Modifier::ControlAlt).unwrap();
        assert_eq!(load_from(&path).unwrap(), Modifier::ControlAlt);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(&path, br#"{"version":2,"modifier":"control"}"#).unwrap();
        assert!(
            load_from(&path)
                .unwrap_err()
                .contains("unsupported controls config version")
        );
        fs::write(&path, br#"{"version":1,"modifier":"control","extra":true}"#).unwrap();
        assert!(load_from(&path).is_err());
        fs::write(&path, vec![b'x'; 4097]).unwrap();
        assert!(load_from(&path).unwrap_err().contains("exceeds 4096 bytes"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn disabled_never_captures_any_key_or_mouse_modifiers() {
        for mask in 0..16 {
            let state = mods(mask & 1 != 0, mask & 2 != 0, mask & 4 != 0, mask & 8 != 0);
            assert!(!Modifier::Disabled.matches(state));
            for key in ["q", "f", "1", "tab", "enter", "left"] {
                assert_eq!(classify(Modifier::Disabled, key, state), None);
            }
        }
    }
}
