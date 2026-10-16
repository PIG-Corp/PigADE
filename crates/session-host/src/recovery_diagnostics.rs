//! Bounded artifact diagnostics when no trustworthy terminal frame can be built.
use std::{fmt::Write, path::Path};

use serde::{Deserialize, Serialize};

pub const MAX_ISSUES: usize = 16;
pub const MAX_TEXT_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryIssue {
    /// A display filename, not an authenticated session/generation identity.
    pub journal: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryDiagnostics {
    pub issues: Vec<RecoveryIssue>,
    pub omitted: u64,
}
impl RecoveryDiagnostics {
    pub(crate) fn record(&mut self, path: &Path, error: &anyhow::Error) {
        if self.issues.len() >= MAX_ISSUES {
            self.omitted = self.omitted.saturating_add(1);
            return;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let mut journal = BoundedText::default();
        // Unix basenames can contain controls and backslashes. Escape those
        // display bytes so one damaged artifact cannot invalidate every poll
        // in a client which enforces portable, single-line display names.
        for character in name.chars() {
            let result = if character.is_control() || matches!(character, '\\' | '%') {
                let mut bytes = [0; 4];
                character
                    .encode_utf8(&mut bytes)
                    .bytes()
                    .try_for_each(|byte| write!(journal, "%{byte:02X}"))
            } else {
                journal.write_char(character)
            };
            if result.is_err() {
                break;
            }
        }
        let mut message = BoundedText::default();
        let _ = write!(message, "{error:#}");
        self.issues.push(RecoveryIssue {
            journal: journal.finish(),
            message: message.finish(),
        });
    }

    pub(crate) fn response(&self) -> Option<Self> {
        (!self.issues.is_empty() || self.omitted != 0).then(|| self.clone())
    }
}

#[derive(Default)]
struct BoundedText {
    text: String,
    truncated: bool,
}
impl std::fmt::Write for BoundedText {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        let remaining = (MAX_TEXT_BYTES - '…'.len_utf8()).saturating_sub(self.text.len());
        let mut take = remaining.min(value.len());
        while !value.is_char_boundary(take) {
            take -= 1;
        }
        self.text.push_str(&value[..take]);
        if take < value.len() {
            self.truncated = true;
            return Err(std::fmt::Error);
        }
        Ok(())
    }
}
impl BoundedText {
    fn finish(mut self) -> String {
        if self.truncated {
            self.text.push('…');
        }
        self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_bound_count_and_utf8_formatting_without_losing_omission_count() {
        let mut diagnostics = RecoveryDiagnostics::default();
        assert!(diagnostics.response().is_none());
        let error = anyhow::anyhow!("Ж".repeat(10_000));
        for index in 0..(MAX_ISSUES + 3) {
            diagnostics.record(Path::new(&format!("{index}.journal")), &error);
        }
        assert_eq!(diagnostics.issues.len(), MAX_ISSUES);
        assert_eq!(diagnostics.omitted, 3);
        for issue in &diagnostics.issues {
            assert!(issue.message.len() <= MAX_TEXT_BYTES);
            assert!(issue.message.ends_with('…'));
            assert!(issue.journal.ends_with(".journal"));
        }
        assert!(diagnostics.response().is_some());
    }

    #[test]
    fn legal_unix_basenames_remain_safe_single_line_display_names() {
        let mut diagnostics = RecoveryDiagnostics::default();
        diagnostics.record(Path::new("bad\\name\n%Ж.journal"), &anyhow::anyhow!("bad"));
        assert_eq!(diagnostics.issues[0].journal, "bad%5Cname%0A%25Ж.journal");
        assert!(!diagnostics.issues[0].journal.chars().any(char::is_control));
    }
}
