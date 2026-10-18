//! Explicit, generation-scoped terminal paste review.
//!
//! This module only constructs bytes for a later user-confirmed input command.
//! File paths are rendered as inert POSIX shell words; it does not inspect,
//! canonicalize, open, execute, or upload those paths.

use crate::{command_worker::CommandTarget, input, protocol::View};
use std::path::PathBuf;

const MAX_INSERT_BYTES: usize = 16 * 1024;
const MAX_FILES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Clipboard,
    Files { count: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Review {
    pub target: CommandTarget,
    pub label: String,
    pub text: String,
    pub origin: Origin,
    bracketed: bool,
}

impl Review {
    pub(crate) fn new(
        target: CommandTarget,
        label: String,
        text: String,
        origin: Origin,
        modes: u32,
    ) -> Result<Self, String> {
        if text.is_empty() {
            return Err("Нет текста для вставки".into());
        }
        if let Origin::Files { count } = &origin
            && (*count == 0 || *count > MAX_FILES)
        {
            return Err("Недопустимое число файлов".into());
        }
        input::paste(&text, modes).map_err(str::to_owned)?;
        Ok(Self {
            target,
            label,
            text,
            origin,
            bracketed: modes & input::BRACKETED_PASTE != 0,
        })
    }

    /// Encode only for the exact live generation reviewed by the user.
    pub(crate) fn encode(&self, view: &View) -> Result<Vec<u8>, String> {
        if view.session != self.target.session || view.generation != self.target.generation {
            return Err("Панель или поколение изменились после подготовки".into());
        }
        if view.state != "running" || view.storage_error.is_some() {
            return Err("Терминал недоступен для ввода".into());
        }
        if (view.frame.modes & input::BRACKETED_PASTE != 0) != self.bracketed {
            return Err("Режим вставки изменился после подготовки".into());
        }
        input::paste(&self.text, view.frame.modes).map_err(str::to_owned)
    }
}

pub(crate) fn needs_review(text: &str) -> bool {
    text.contains('\r') || text.contains('\n')
}

/// Convert absolute UTF-8 paths into a bounded shell command fragment.
/// Every path is a single-quoted word; the returned string ends in a space so
/// the user can continue editing it. No path is resolved or read from disk.
pub(crate) fn file_paths(paths: &[PathBuf]) -> Result<String, String> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        return Err("Выберите от 1 до 32 файлов".into());
    }

    let mut output_len = 1usize; // final space
    for (index, path) in paths.iter().enumerate() {
        // Reject oversized platform payloads before UTF-8/control scans or
        // allocating quoted output. Quotes, final space and framing need15B.
        if path.as_os_str().len() > MAX_INSERT_BYTES - 15 {
            return Err("Список путей превышает 16 KiB".into());
        }
        let Some(value) = path.to_str() else {
            return Err("Путь файла не является допустимым UTF-8".into());
        };
        if !path.is_absolute() {
            return Err("Путь файла должен быть абсолютным".into());
        }
        if value.chars().any(char::is_control) {
            return Err("Путь содержит управляющий символ".into());
        }
        output_len = output_len
            .checked_add(2) // surrounding quotes
            .and_then(|length| length.checked_add(value.len()))
            .and_then(|length| {
                value
                    .bytes()
                    .filter(|byte| *byte == b'\'')
                    .count()
                    .checked_mul(3)
                    .and_then(|extra| length.checked_add(extra))
            })
            .and_then(|length| length.checked_add(usize::from(index > 0)))
            .ok_or_else(|| "Список путей превышает 16 KiB".to_owned())?;
        if output_len > MAX_INSERT_BYTES - 12 {
            return Err("Список путей превышает 16 KiB".into());
        }
    }

    let mut text = String::with_capacity(output_len);
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            text.push(' ');
        }
        text.push('\'');
        for character in path.to_str().expect("validated UTF-8 path").chars() {
            if character == '\'' {
                text.push_str("'\\''");
            } else {
                text.push(character);
            }
        }
        text.push('\'');
    }
    text.push(' ');
    debug_assert_eq!(text.len(), output_len);
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Cell, Frame, Geometry};
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    fn target() -> CommandTarget {
        CommandTarget {
            session: "session-a".into(),
            generation: "generation-a".into(),
        }
    }

    fn view() -> View {
        View {
            session: "session-a".into(),
            generation: "generation-a".into(),
            sequence: 1,
            state: "running".into(),
            recovery_path: "live".into(),
            pid: Some(7),
            storage_error: None,
            frame: Frame {
                geometry: Geometry { rows: 1, cols: 1 },
                cells: vec![Cell {
                    text: "x".into(),
                    fg: String::new(),
                    bg: String::new(),
                    flags: 0,
                    underline: None,
                    hyperlink: None,
                }],
                hyperlinks: Vec::new(),
                cursor: (0, 0),
                cursor_style: "Block".into(),
                cursor_presentation: None,
                modes: 0,
                colors: String::new(),
                palette_overrides: Vec::new(),
                history_lines: 0,
                sync_pending_bytes: 0,
                title: String::new(),
            },
        }
    }

    fn review(text: &str, modes: u32) -> Review {
        Review::new(
            target(),
            "Terminal A".into(),
            text.into(),
            Origin::Clipboard,
            modes,
        )
        .unwrap()
    }

    #[test]
    fn file_paths_quote_shell_metacharacters_spaces_and_unicode_as_words() {
        let paths = [
            PathBuf::from("/tmp/a b"),
            PathBuf::from("/tmp/it's $(not-run)"),
            PathBuf::from("/tmp/日本語😀"),
        ];
        assert_eq!(
            file_paths(&paths).unwrap(),
            "'/tmp/a b' '/tmp/it'\\''s $(not-run)' '/tmp/日本語😀' "
        );
    }

    #[test]
    fn posix_shell_recovers_exact_paths_without_expansion_or_word_splitting() {
        let paths = [
            PathBuf::from("/tmp/a b"),
            PathBuf::from("/tmp/it's$(printf EXPANDED)"),
            PathBuf::from("/tmp/日本語😀;$HOME*"),
        ];
        let fragment = file_paths(&paths).unwrap();
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("set -- {fragment}; printf '%s\\0' \"$@\""))
            .output()
            .unwrap();
        assert!(output.status.success());
        let mut expected = Vec::new();
        for path in paths {
            expected.extend_from_slice(path.to_str().unwrap().as_bytes());
            expected.push(0);
        }
        assert_eq!(output.stdout, expected);
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn file_paths_reject_invalid_count_relative_control_non_utf8_and_oversize() {
        assert!(file_paths(&[]).is_err());
        assert!(file_paths(&vec![PathBuf::from("/a"); MAX_FILES + 1]).is_err());
        assert!(file_paths(&[PathBuf::from("relative")]).is_err());
        assert!(file_paths(&[PathBuf::from("/tmp/line\nbreak")]).is_err());
        assert!(file_paths(&[PathBuf::from(OsString::from_vec(vec![b'/', 0xff]))]).is_err());
        assert!(
            file_paths(&[PathBuf::from(format!("/{}", "x".repeat(MAX_INSERT_BYTES)))]).is_err()
        );
    }

    #[test]
    fn file_paths_reserve_bracketed_paste_overhead() {
        let within = PathBuf::from(format!("/{}", "x".repeat(MAX_INSERT_BYTES - 16)));
        let text = file_paths(&[within]).unwrap();
        assert_eq!(text.len(), MAX_INSERT_BYTES - 12);
        assert!(input::paste(&text, input::BRACKETED_PASTE).is_ok());
        let too_large = PathBuf::from(format!("/{}", "x".repeat(MAX_INSERT_BYTES - 15)));
        assert!(file_paths(&[too_large]).is_err());
    }

    #[test]
    fn review_encodes_exact_bracketed_or_plain_payload_without_appending_enter() {
        assert_eq!(
            review("first\r\nsecond", 0).encode(&view()).unwrap(),
            b"first\rsecond"
        );
        assert_eq!(
            review("first\r\nsecond", input::BRACKETED_PASTE)
                .encode(&view_with_modes(input::BRACKETED_PASTE))
                .unwrap(),
            b"\x1b[200~first\nsecond\x1b[201~"
        );
        assert_eq!(review("plain", 0).encode(&view()).unwrap(), b"plain");
    }

    fn view_with_modes(modes: u32) -> View {
        let mut view = view();
        view.frame.modes = modes;
        view
    }

    #[test]
    fn review_rejects_empty_invalid_text_changed_target_state_or_mode() {
        assert!(Review::new(target(), "A".into(), String::new(), Origin::Clipboard, 0).is_err());
        assert!(
            Review::new(
                target(),
                "A".into(),
                "\x1b[201~".into(),
                Origin::Clipboard,
                0
            )
            .is_err()
        );
        assert!(
            Review::new(
                target(),
                "A".into(),
                "x".repeat(MAX_INSERT_BYTES + 1),
                Origin::Clipboard,
                0
            )
            .is_err()
        );
        assert!(
            Review::new(
                target(),
                "A".into(),
                "x".into(),
                Origin::Files { count: 0 },
                0
            )
            .is_err()
        );

        let original = view();
        let paste = review("hello", 0);
        let mut wrong = original.clone();
        wrong.session = "other".into();
        assert!(paste.encode(&wrong).is_err());
        let mut wrong = original.clone();
        wrong.generation = "new-generation".into();
        assert!(paste.encode(&wrong).is_err());
        let mut stopped = original.clone();
        stopped.state = "exited".into();
        assert!(paste.encode(&stopped).is_err());
        let mut blocked = original.clone();
        blocked.storage_error = Some("readonly".into());
        assert!(paste.encode(&blocked).is_err());
        let mut changed_mode = original.clone();
        changed_mode.frame.modes = input::BRACKETED_PASTE;
        assert!(paste.encode(&changed_mode).is_err());
        assert_eq!(original, view());
        assert_eq!(paste.text, "hello");
    }

    #[test]
    fn multiline_needs_explicit_review() {
        assert!(!needs_review("one line\ttab"));
        assert!(needs_review("one\ntwo"));
        assert!(needs_review("one\rtwo"));
        assert!(!needs_review(""));
    }
}
