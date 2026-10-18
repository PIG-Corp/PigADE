//! Frozen, review-first capture of OSC 8 links present in one visible frame.
//!
//! This module never opens a URL. The UI must show a row in `Review` and call
//! `confirm_once` from an explicit confirmation action before it receives a
//! canonical target.

use crate::protocol::View;
use gpui::http_client::Url;
use std::collections::HashMap;
use uuid::Uuid;

const MAX_ROWS: usize = 64;
const MAX_OWNED_BYTES: usize = 128 * 1024;
const MAX_URL_BYTES: usize = 8192;
const MAX_LABEL_BYTES: usize = 1024;
const MAX_ESCAPED_LITERAL_BYTES: usize = 49_152;
const MAX_VIEW_CELLS: usize = 32_768;
const WIDE_CHAR_SPACER: u16 = 0x0040;
const LEADING_WIDE_CHAR_SPACER: u16 = 0x0400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkSource {
    pub session: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkTarget {
    pub literal: String,
    pub decision: LinkDecision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkDecision {
    Allowed { canonical: Url, domain: String },
    Blocked(BlockReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlockReason {
    UnsupportedScheme,
    InvalidAddress,
    Credentials,
    MissingHost,
    UnsafeCharacters,
    Oversized,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkRow {
    pub label: String,
    pub label_truncated: bool,
    pub target: LinkTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SheetStage {
    List,
    Review,
    Cancelled,
}

#[derive(Clone, Debug)]
pub(crate) struct LinkSheet {
    pub dialog_id: Uuid,
    pub source: LinkSource,
    pub rows: Vec<LinkRow>,
    /// Partial-capture indicator: omitted cell/destination records, not unique links.
    pub omitted: usize,
    pub stage: SheetStage,
    pub selected: usize,
    consumed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureError {
    InvalidSource,
    InvalidGeometry,
    DanglingReference,
}

impl LinkSheet {
    pub fn select(&mut self, index: usize) -> bool {
        if self.stage == SheetStage::List && index < self.rows.len() {
            self.selected = index;
            true
        } else {
            false
        }
    }

    /// Enter always opens review; it never authorizes an external action.
    pub fn enter_review(&mut self) -> bool {
        if self.stage != SheetStage::List {
            return false;
        }
        if self.selected < self.rows.len() {
            self.stage = SheetStage::Review;
            true
        } else {
            false
        }
    }

    pub fn back_to_list(&mut self) -> bool {
        if self.stage == SheetStage::Review {
            self.stage = SheetStage::List;
            true
        } else {
            false
        }
    }

    pub fn cancel(&mut self) {
        self.consumed = true;
        self.stage = SheetStage::Cancelled;
    }

    /// Consume the explicit review confirmation once for this dialog identity.
    pub fn confirm_once(&mut self, dialog_id: Uuid) -> Option<Url> {
        if self.consumed || self.dialog_id != dialog_id {
            return None;
        }
        if self.stage != SheetStage::Review {
            return None;
        }
        match &self.rows.get(self.selected)?.target.decision {
            LinkDecision::Allowed { canonical, .. } => {
                self.consumed = true;
                Some(canonical.clone())
            }
            LinkDecision::Blocked(_) => None,
        }
    }

    /// The allowed canonical address can be copied from review; blocked input
    /// is never placed on the clipboard by this model.
    pub fn copy_target(&self, dialog_id: Uuid) -> Option<&str> {
        if self.consumed || self.dialog_id != dialog_id || self.stage != SheetStage::Review {
            return None;
        }
        match &self.rows.get(self.selected)?.target.decision {
            LinkDecision::Allowed { canonical, .. } => Some(canonical.as_str()),
            LinkDecision::Blocked(_) => None,
        }
    }

    /// Safely escaped, bounded text for displaying a blocked literal address.
    /// A `true` truncation flag requires the UI to show an ellipsis.
    pub fn blocked_literal_display(&self, dialog_id: Uuid) -> Option<(String, bool)> {
        if self.consumed || self.dialog_id != dialog_id || self.stage != SheetStage::Review {
            return None;
        }
        let row = self.rows.get(self.selected)?;
        matches!(&row.target.decision, LinkDecision::Blocked(_))
            .then(|| display_literal(&row.target.literal))
    }
}

#[derive(Default)]
struct Candidate {
    label: String,
    label_bytes: usize,
    label_truncated: bool,
}

/// Capture only links referenced by visible cells. The resulting sheet is a
/// value snapshot and cannot be retargeted by subsequent frame updates.
pub(crate) fn capture(view: &View, dialog_id: Uuid) -> Result<LinkSheet, CaptureError> {
    let session = Uuid::parse_str(&view.session).map_err(|_| CaptureError::InvalidSource)?;
    let generation = Uuid::parse_str(&view.generation).map_err(|_| CaptureError::InvalidSource)?;
    if session.is_nil() || generation.is_nil() || dialog_id.is_nil() || view.state.len() > 128 {
        return Err(CaptureError::InvalidSource);
    }
    let rows = usize::from(view.frame.geometry.rows);
    let cols = usize::from(view.frame.geometry.cols);
    let Some(cell_count) = rows.checked_mul(cols) else {
        return Err(CaptureError::InvalidGeometry);
    };
    if rows == 0 || cols == 0 || cell_count > MAX_VIEW_CELLS || cell_count != view.frame.cells.len()
    {
        return Err(CaptureError::InvalidGeometry);
    }

    let mut unique = Vec::<(String, Candidate)>::new();
    let mut indexes = HashMap::<String, usize>::new();
    // A long URI can cover every cell. Resolve each referenced pool entry
    // once instead of hashing its full bytes again for every glyph. This
    // numeric cache has at most one entry per validated visible cell.
    let mut resolved_pool = HashMap::<u32, Option<usize>>::new();
    let mut omitted = 0usize;
    // Count state, labels, both in-flight URI copies (ordered vector + map),
    // and eventual canonical/domain strings under construction.
    let mut owned_bytes = view.state.len();
    for cell in &view.frame.cells {
        let Some(link_index) = cell.hyperlink else {
            continue;
        };
        let (_, uri) = view
            .frame
            .hyperlinks
            .get(link_index as usize)
            .ok_or(CaptureError::DanglingReference)?;
        if cell.flags & (WIDE_CHAR_SPACER | LEADING_WIDE_CHAR_SPACER) != 0 {
            continue;
        }
        let index = if let Some(index) = resolved_pool.get(&link_index) {
            *index
        } else {
            let index = if let Some(index) = indexes.get(uri).copied() {
                Some(index)
            } else if unique.len() == MAX_ROWS
                || uri.len() > MAX_URL_BYTES
                || owned_bytes.saturating_add(uri.len().saturating_mul(2)) > MAX_OWNED_BYTES
            {
                None
            } else {
                let index = unique.len();
                indexes.insert(uri.clone(), index);
                // Ordered rows and the temporary URI map own two copies.
                owned_bytes += uri.len().saturating_mul(2);
                unique.push((uri.clone(), Candidate::default()));
                Some(index)
            };
            resolved_pool.insert(link_index, index);
            index
        };
        let Some(index) = index else {
            omitted = omitted.saturating_add(1);
            continue;
        };
        let candidate = &mut unique[index].1;
        let remaining = MAX_OWNED_BYTES
            .saturating_sub(owned_bytes)
            .min(MAX_LABEL_BYTES.saturating_sub(candidate.label_bytes));
        let (label, truncated) = safe_display(&cell.text, remaining);
        candidate.label_truncated |= truncated;
        if candidate.label_bytes + label.len() <= MAX_LABEL_BYTES
            && owned_bytes.saturating_add(label.len()) <= MAX_OWNED_BYTES
        {
            candidate.label_bytes += label.len();
            owned_bytes += label.len();
            candidate.label.push_str(&label);
        }
    }

    let mut output = Vec::new();
    for (literal, candidate) in unique {
        let decision = validate_uri(&literal);
        let expansion_bytes = match &decision {
            LinkDecision::Allowed { canonical, domain } => {
                canonical.as_str().len().saturating_add(domain.len())
            }
            LinkDecision::Blocked(_) => 0,
        };
        if owned_bytes.saturating_add(expansion_bytes) > MAX_OWNED_BYTES {
            omitted = omitted.saturating_add(1);
            continue;
        }
        owned_bytes += expansion_bytes;
        output.push(LinkRow {
            label: candidate.label,
            label_truncated: candidate.label_truncated,
            target: LinkTarget { literal, decision },
        });
    }

    Ok(LinkSheet {
        dialog_id,
        source: LinkSource {
            session,
            generation,
            sequence: view.sequence,
            state: view.state.clone(),
        },
        rows: output,
        omitted,
        stage: SheetStage::List,
        selected: 0,
        consumed: false,
    })
}

fn validate_uri(literal: &str) -> LinkDecision {
    let blocked = |reason| LinkDecision::Blocked(reason);
    if literal.len() > MAX_URL_BYTES {
        return blocked(BlockReason::Oversized);
    }
    if literal.is_empty()
        || literal.chars().any(|ch| {
            ch.is_whitespace()
                || ch.is_control()
                || matches!(ch, '\\' | '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        || has_bad_percent_escape(literal)
    {
        return blocked(BlockReason::UnsafeCharacters);
    }
    let Some((scheme, rest)) = literal.split_once("://") else {
        return blocked(BlockReason::UnsupportedScheme);
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return blocked(BlockReason::UnsupportedScheme);
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return blocked(BlockReason::MissingHost);
    }
    if authority.contains('@') {
        return blocked(BlockReason::Credentials);
    }
    let Ok(parsed) = Url::parse(literal) else {
        return blocked(BlockReason::InvalidAddress);
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return blocked(BlockReason::Credentials);
    }
    let domain = parsed.host_str().unwrap_or_default().to_owned();
    if domain.is_empty() {
        return blocked(BlockReason::MissingHost);
    }
    LinkDecision::Allowed {
        canonical: parsed,
        domain,
    }
}

fn has_bad_percent_escape(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return true;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    false
}

fn safe_display(value: &str, byte_limit: usize) -> (String, bool) {
    let mut output = String::new();
    for ch in value.chars() {
        if ch.is_control()
            || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            let escaped = format!("\\u{{{:x}}}", ch as u32);
            if output.len().saturating_add(escaped.len()) > byte_limit {
                return (output, true);
            }
            output.push_str(&escaped);
        } else {
            if output.len().saturating_add(ch.len_utf8()) > byte_limit {
                return (output, true);
            }
            output.push(ch);
        }
    }
    (output, false)
}

/// Escaped form suitable for displaying an original literal URI. The input is
/// limited to `MAX_URL_BYTES`; the displayed form is limited to 48 KiB.
pub(crate) fn display_literal(value: &str) -> (String, bool) {
    let mut end = value.len().min(MAX_URL_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let (shown, escaped_truncated) = safe_display(&value[..end], MAX_ESCAPED_LITERAL_BYTES);
    (shown, escaped_truncated || end < value.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Cell, Frame, Geometry};

    fn view(cells: Vec<Cell>, links: Vec<(String, String)>, cols: u16) -> View {
        View {
            session: "00000000-0000-4000-8000-000000000001".into(),
            generation: "00000000-0000-4000-8000-000000000002".into(),
            sequence: 7,
            state: "running".into(),
            recovery_path: String::new(),
            pid: None,
            storage_error: None,
            stop_error: None,
            stop_scope: None,
            frame: Frame {
                geometry: Geometry { rows: 1, cols },
                cells,
                hyperlinks: links,
                cursor: (0, 0),
                cursor_style: "block".into(),
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

    fn cell(text: &str, hyperlink: Option<u32>, flags: u16) -> Cell {
        Cell {
            text: text.into(),
            fg: String::new(),
            bg: String::new(),
            flags,
            underline: None,
            hyperlink,
        }
    }

    #[test]
    fn captures_visible_unique_targets_and_reviews_explicitly_once() {
        let source = view(
            vec![
                cell("界", Some(0), 0),
                cell("", Some(0), WIDE_CHAR_SPACER),
                cell("!", Some(1), 0),
            ],
            vec![
                ("one".into(), "https://example.com/a".into()),
                ("two".into(), "https://example.com/a".into()),
                ("unused".into(), "https://unused.test".into()),
            ],
            3,
        );
        let dialog = Uuid::new_v4();
        let mut sheet = capture(&source, dialog).unwrap();
        assert_eq!(sheet.rows.len(), 1);
        assert_eq!(sheet.rows[0].label, "界!");
        assert!(!sheet.rows[0].label_truncated);
        assert!(
            sheet.confirm_once(dialog).is_none(),
            "list confirmation cannot open"
        );
        assert!(sheet.enter_review());
        assert!(
            sheet.confirm_once(Uuid::new_v4()).is_none(),
            "another dialog cannot consume the target"
        );
        assert_eq!(sheet.copy_target(dialog), Some("https://example.com/a"));
        assert_eq!(
            sheet.confirm_once(dialog).unwrap().as_str(),
            "https://example.com/a"
        );
        assert!(sheet.confirm_once(dialog).is_none());
        assert!(sheet.confirm_once(Uuid::new_v4()).is_none());
        let mut cancelled = capture(&source, dialog).unwrap();
        assert!(cancelled.enter_review());
        cancelled.cancel();
        assert!(cancelled.confirm_once(dialog).is_none());
        assert!(cancelled.copy_target(dialog).is_none());
    }

    #[test]
    fn blocks_unsafe_targets_and_freezes_the_captured_frame() {
        let bad = [
            "javascript:alert(1)",
            "https://user:pass@example.com/",
            "https://example.com/a\\b",
            "https://example.com/a b",
            "https://example.com/\u{202e}txt",
            "https://example.com/%zz",
        ];
        for literal in bad {
            let source = view(
                vec![cell("label", Some(0), 0)],
                vec![(String::new(), literal.into())],
                1,
            );
            let dialog = Uuid::new_v4();
            let mut sheet = capture(&source, dialog).unwrap();
            assert!(matches!(
                sheet.rows[0].target.decision,
                LinkDecision::Blocked(_)
            ));
            assert!(sheet.enter_review());
            assert!(sheet.copy_target(dialog).is_none());
            assert!(sheet.confirm_once(dialog).is_none());
            assert!(sheet.blocked_literal_display(dialog).is_some());
        }

        let original = view(
            vec![cell("old", Some(0), 0)],
            vec![(String::new(), "https://old.test".into())],
            1,
        );
        let dialog = Uuid::new_v4();
        let mut frozen = capture(&original, dialog).unwrap();
        let updated = view(
            vec![cell("new", Some(0), 0)],
            vec![(String::new(), "https://new.test".into())],
            1,
        );
        let _ = updated;
        assert!(frozen.enter_review());
        assert_eq!(
            frozen.confirm_once(dialog).unwrap().as_str(),
            "https://old.test/"
        );
    }

    #[test]
    fn url_normalization_is_visible_and_never_enables_other_schemes() {
        for literal in [
            "file:///tmp/a",
            "data:text/html,hello",
            "mailto:user@example.test",
            "http:///missing",
            "https://@example.test/",
            "https://example.test/\n",
            "https://example.test/%",
            "https://example.test/a\\b",
        ] {
            assert!(
                matches!(validate_uri(literal), LinkDecision::Blocked(_)),
                "{literal:?}"
            );
        }
        let LinkDecision::Allowed { canonical, domain } =
            validate_uri("HTTPS://ПРИМЕР.РФ:443/путь?q=界")
        else {
            panic!("valid Unicode web address rejected")
        };
        assert!(domain.starts_with("xn--") && domain.is_ascii());
        assert_eq!(canonical.host_str(), Some(domain.as_str()));
        assert!(canonical.as_str().starts_with("https://xn--"));
        assert!(canonical.as_str().contains('%'));
        assert!(!canonical.as_str().contains(":443"));
        assert!(matches!(
            validate_uri("http://localhost:3000/"),
            LinkDecision::Allowed { .. }
        ));
    }

    #[test]
    fn capture_budget_marks_omissions_and_keeps_final_strings_bounded() {
        let uris = (0..80)
            .map(|index| {
                (
                    String::new(),
                    format!("https://example.test/{index}/{}", "界".repeat(500)),
                )
            })
            .collect::<Vec<_>>();
        let cells = (0..80).map(|index| cell("label", Some(index), 0)).collect();
        let sheet = capture(&view(cells, uris, 80), Uuid::new_v4()).unwrap();
        let strings = sheet
            .rows
            .iter()
            .map(|row| {
                row.label.len()
                    + row.target.literal.len()
                    + match &row.target.decision {
                        LinkDecision::Allowed { canonical, domain } => {
                            canonical.as_str().len() + domain.len()
                        }
                        LinkDecision::Blocked(_) => 0,
                    }
            })
            .sum::<usize>()
            + sheet.source.state.len();
        assert!(strings <= MAX_OWNED_BYTES);
        assert!(sheet.rows.len() <= MAX_ROWS && sheet.omitted > 0);
        let (escaped, truncated) = display_literal(&"\u{202e}".repeat(MAX_URL_BYTES));
        assert!(truncated && escaped.len() <= MAX_ESCAPED_LITERAL_BYTES);
        assert!(!escaped.contains('\u{202e}'));
    }

    #[test]
    fn full_viewport_with_one_long_uri_keeps_one_complete_destination() {
        let prefix = "https://example.test/";
        let literal = format!("{prefix}{}", "x".repeat(MAX_URL_BYTES - prefix.len()));
        let mut source = view(
            vec![cell("x", Some(0), 0); MAX_VIEW_CELLS],
            vec![(String::new(), literal.clone())],
            256,
        );
        source.frame.geometry.rows = 128;
        let dialog = Uuid::new_v4();
        let mut sheet = capture(&source, dialog).unwrap();
        assert_eq!(sheet.rows.len(), 1);
        assert_eq!(sheet.omitted, 0);
        assert!(sheet.rows[0].label_truncated);
        assert!(sheet.enter_review());
        assert_eq!(sheet.confirm_once(dialog).unwrap().as_str(), literal);
    }

    #[test]
    fn rejects_dangling_cells_and_bounds_rows_and_urls() {
        let invalid = view(vec![cell("x", Some(1), 0)], vec![], 1);
        assert_eq!(
            capture(&invalid, Uuid::new_v4()).unwrap_err(),
            CaptureError::DanglingReference
        );
        let spacer_invalid = view(vec![cell("", Some(1), WIDE_CHAR_SPACER)], vec![], 1);
        assert_eq!(
            capture(&spacer_invalid, Uuid::new_v4()).unwrap_err(),
            CaptureError::DanglingReference
        );
        let oversized = format!("https://example.test/{}", "x".repeat(MAX_URL_BYTES));
        let source = view(
            vec![cell("x", Some(0), 0)],
            vec![(String::new(), oversized)],
            1,
        );
        let sheet = capture(&source, Uuid::new_v4()).unwrap();
        assert_eq!(sheet.rows.len(), 0);
        assert_eq!(sheet.omitted, 1);
    }

    #[test]
    fn labels_and_blocked_literals_mark_safe_truncation() {
        let long_label = "界".repeat(400);
        let source = view(
            vec![cell(&long_label, Some(0), 0)],
            vec![(String::new(), "javascript:\u{202e}".into())],
            1,
        );
        let dialog = Uuid::new_v4();
        let mut sheet = capture(&source, dialog).unwrap();
        assert!(sheet.rows[0].label_truncated);
        assert!(sheet.enter_review());
        let (shown, truncated) = sheet.blocked_literal_display(dialog).unwrap();
        assert!(!truncated, "escaping a complete literal is not truncation");
        assert!(shown.contains("\\u{202e}"));
        assert!(sheet.confirm_once(dialog).is_none());
        assert!(sheet.blocked_literal_display(dialog).is_some());
    }
}
