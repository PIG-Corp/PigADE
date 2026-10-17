//! Bounded Unicode-aware editing for notes, role text, and task input.
use std::ops::Range;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use unicode_segmentation::UnicodeSegmentation;

const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_HISTORY_ENTRIES: usize = 100;
const MAX_HISTORY_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Movement {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    LineStart,
    LineEnd,
    DocumentStart,
    DocumentEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    text: String,
    anchor: usize,
    head: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Marked {
    range: Range<usize>,
    before: Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct EditBuffer {
    text: String,
    anchor: usize,
    head: usize,
    marked: Option<Marked>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    preferred_column: Option<usize>,
    revision: u64,
}

impl EditBuffer {
    /// Construct from trusted caller-bounded text. Deserialization and edits enforce 1 MiB.
    pub(crate) fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let end = text.len();
        Self {
            text,
            anchor: end,
            head: end,
            marked: None,
            undo: Vec::new(),
            redo: Vec::new(),
            preferred_column: None,
            revision: 0,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Monotonic revision for text-shaping caches. Selection-only changes do not bump it.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub(crate) fn cursor(&self) -> usize {
        self.head
    }

    pub(crate) fn reversed(&self) -> bool {
        self.anchor > self.head
    }

    pub(crate) fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }

    pub(crate) fn marked(&self) -> Option<Range<usize>> {
        self.marked.as_ref().map(|marked| marked.range.clone())
    }

    pub(crate) fn select_bytes(&mut self, range: Range<usize>, reversed: bool) {
        if range.is_empty() {
            let cursor = snap_grapheme(&self.text, range.start, false);
            self.anchor = cursor;
            self.head = cursor;
            self.preferred_column = None;
            return;
        }
        let start = snap_grapheme(&self.text, range.start.min(range.end), false);
        let end = snap_grapheme(&self.text, range.start.max(range.end), true);
        if reversed {
            self.anchor = end;
            self.head = start;
        } else {
            self.anchor = start;
            self.head = end;
        }
        self.preferred_column = None;
    }

    pub(crate) fn select_utf16(&mut self, range: Range<usize>) {
        let bytes = byte_range_from_utf16(&self.text, &range);
        self.select_bytes(bytes, range.start > range.end);
    }

    pub(crate) fn selected_utf16(&self) -> Range<usize> {
        let selection = self.selection();
        utf16_at_byte(&self.text, selection.start)..utf16_at_byte(&self.text, selection.end)
    }

    pub(crate) fn text_for_utf16(&self, range: Range<usize>) -> (String, Range<usize>) {
        let bytes = byte_range_from_utf16(&self.text, &range);
        (
            self.text[bytes.clone()].to_owned(),
            utf16_at_byte(&self.text, bytes.start)..utf16_at_byte(&self.text, bytes.end),
        )
    }

    pub(crate) fn len_utf16(&self) -> usize {
        self.text.encode_utf16().count()
    }

    pub(crate) fn replace(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        multiline: bool,
    ) -> Result<(), &'static str> {
        let inserted = normalize_input(text, multiline)?;
        let range = match range_utf16 {
            Some(range) => byte_range_from_utf16(&self.text, &range),
            None => self
                .marked
                .as_ref()
                .map(|marked| marked.range.clone())
                .unwrap_or_else(|| self.selection()),
        };
        let resulting_len = self
            .text
            .len()
            .checked_sub(range.len())
            .and_then(|length| length.checked_add(inserted.len()))
            .ok_or("text exceeds 1 MiB")?;
        if resulting_len > MAX_TEXT_BYTES {
            return Err("text exceeds 1 MiB");
        }

        let before = self
            .marked
            .as_ref()
            .map(|marked| marked.before.clone())
            .unwrap_or_else(|| self.snapshot());
        self.apply_replacement(range, &inserted);
        self.marked = None;
        self.preferred_column = None;
        self.push_undo(before);
        Ok(())
    }

    /// Stage an IME composition. Repeated marks remain one undo group from the
    /// state before the first mark; selection_utf16 is local to inserted text.
    pub(crate) fn mark(
        &mut self,
        range_utf16: Range<usize>,
        text: &str,
        selection_utf16: Option<Range<usize>>,
        multiline: bool,
    ) -> Result<(), &'static str> {
        let inserted = normalize_input(text, multiline)?;
        let byte_range = byte_range_from_utf16(&self.text, &range_utf16);
        let start = byte_range.start;
        let end = byte_range.end;
        let resulting_len = self
            .text
            .len()
            .checked_sub(end - start)
            .and_then(|length| length.checked_add(inserted.len()))
            .ok_or("text exceeds 1 MiB")?;
        if resulting_len > MAX_TEXT_BYTES {
            return Err("text exceeds 1 MiB");
        }

        let before = self
            .marked
            .as_ref()
            .map(|marked| marked.before.clone())
            .unwrap_or_else(|| self.snapshot());
        self.apply_replacement(start..end, &inserted);
        let marked_start = start;
        let marked_end = marked_start + inserted.len();
        self.marked = Some(Marked {
            range: marked_start..marked_end,
            before,
        });
        if let Some(selection) = selection_utf16 {
            let selection_bytes = byte_range_from_utf16(&inserted, &selection);
            let selection_start = selection_bytes.start;
            let selection_end = selection_bytes.end;
            self.anchor = marked_start + selection_start;
            self.head = marked_start + selection_end;
            if selection.start > selection.end {
                std::mem::swap(&mut self.anchor, &mut self.head);
            }
        } else {
            self.anchor = marked_end;
            self.head = marked_end;
        }
        self.preferred_column = None;
        Ok(())
    }

    /// Commit the current marked group to one undo entry.
    pub(crate) fn unmark(&mut self) -> bool {
        let Some(marked) = self.marked.take() else {
            return false;
        };
        self.push_undo(marked.before);
        true
    }

    /// Restore the text and selection from before the first mark.
    pub(crate) fn cancel_mark(&mut self) -> bool {
        let Some(marked) = self.marked.take() else {
            return false;
        };
        self.restore(marked.before);
        self.preferred_column = None;
        true
    }

    pub(crate) fn navigate(&mut self, movement: Movement, extend: bool) {
        self.unmark();
        let selection = self.selection();
        if !extend && selection.start != selection.end {
            match movement {
                Movement::Left | Movement::WordLeft => {
                    self.anchor = selection.start;
                    self.head = selection.start;
                    self.preferred_column = None;
                    return;
                }
                Movement::Right | Movement::WordRight => {
                    self.anchor = selection.end;
                    self.head = selection.end;
                    self.preferred_column = None;
                    return;
                }
                _ => (),
            }
        }
        let target = match movement {
            Movement::Left => {
                self.preferred_column = None;
                previous_grapheme(&self.text, self.head)
            }
            Movement::Right => {
                self.preferred_column = None;
                next_grapheme(&self.text, self.head)
            }
            Movement::WordLeft => {
                self.preferred_column = None;
                word_left(&self.text, self.head)
            }
            Movement::WordRight => {
                self.preferred_column = None;
                word_right(&self.text, self.head)
            }
            Movement::Up => self.vertical_target(false),
            Movement::Down => self.vertical_target(true),
            Movement::LineStart => {
                self.preferred_column = None;
                line_bounds(&self.text, self.head).0
            }
            Movement::LineEnd => {
                self.preferred_column = None;
                line_bounds(&self.text, self.head).1
            }
            Movement::DocumentStart => {
                self.preferred_column = None;
                0
            }
            Movement::DocumentEnd => {
                self.preferred_column = None;
                self.text.len()
            }
        };
        if extend {
            self.head = target;
        } else {
            self.anchor = target;
            self.head = target;
        }
    }

    pub(crate) fn backspace(&mut self, word: bool) -> bool {
        self.unmark();
        let selection = self.selection();
        let range = if selection.start != selection.end {
            selection
        } else if word {
            word_left(&self.text, self.head)..self.head
        } else {
            previous_grapheme(&self.text, self.head)..self.head
        };
        self.delete_range(range)
    }

    pub(crate) fn delete(&mut self, word: bool) -> bool {
        self.unmark();
        let selection = self.selection();
        let range = if selection.start != selection.end {
            selection
        } else if word {
            self.head..word_right(&self.text, self.head)
        } else {
            self.head..next_grapheme(&self.text, self.head)
        };
        self.delete_range(range)
    }

    pub(crate) fn select_all(&mut self) {
        self.unmark();
        self.anchor = 0;
        self.head = self.text.len();
        self.preferred_column = None;
    }

    pub(crate) fn undo(&mut self) -> bool {
        if self.cancel_mark() {
            return true;
        }
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.restore(snapshot);
        self.preferred_column = None;
        self.trim_history();
        true
    }

    pub(crate) fn redo(&mut self) -> bool {
        if self.marked.is_some() {
            return false;
        }
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.restore(snapshot);
        self.preferred_column = None;
        self.trim_history();
        true
    }

    fn vertical_target(&mut self, down: bool) -> usize {
        let (line_start, line_end) = line_bounds(&self.text, self.head);
        let column = *self
            .preferred_column
            .get_or_insert_with(|| grapheme_count(&self.text[line_start..self.head]));
        let target_start = if down {
            if line_end >= self.text.len() {
                return self.head;
            }
            line_end + 1
        } else {
            if line_start == 0 {
                return self.head;
            }
            self.text[..line_start - 1]
                .rfind('\n')
                .map_or(0, |index| index + 1)
        };
        let target_end = self.text[target_start..]
            .find('\n')
            .map_or(self.text.len(), |index| target_start + index);
        advance_graphemes(&self.text, target_start, target_end, column)
    }

    fn delete_range(&mut self, range: Range<usize>) -> bool {
        if range.start == range.end {
            return false;
        }
        let before = self.snapshot();
        self.text.replace_range(range.clone(), "");
        self.revision = self.revision.saturating_add(1);
        self.anchor = range.start;
        self.head = range.start;
        self.preferred_column = None;
        self.push_undo(before);
        true
    }

    fn apply_replacement(&mut self, range: Range<usize>, inserted: &str) {
        let changed = &self.text[range.clone()] != inserted;
        self.text.replace_range(range.clone(), inserted);
        if changed {
            self.revision = self.revision.saturating_add(1);
        }
        let cursor = range.start + inserted.len();
        self.anchor = cursor;
        self.head = cursor;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            anchor: self.anchor,
            head: self.head,
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        if self.text != snapshot.text {
            self.revision = self.revision.saturating_add(1);
        }
        self.text = snapshot.text;
        self.anchor = snapshot.anchor;
        self.head = snapshot.head;
    }

    fn push_undo(&mut self, snapshot: Snapshot) {
        if self.snapshot() == snapshot {
            return;
        }
        self.redo.clear();
        self.undo.push(snapshot);
        self.trim_history();
    }

    fn trim_history(&mut self) {
        let bytes = |snapshots: &[Snapshot]| {
            snapshots
                .iter()
                .map(|snapshot| snapshot.text.len() + 2 * std::mem::size_of::<usize>())
                .sum::<usize>()
        };
        while self.undo.len() + self.redo.len() > MAX_HISTORY_ENTRIES
            || bytes(&self.undo) + bytes(&self.redo) > MAX_HISTORY_BYTES
        {
            if !self.undo.is_empty() {
                self.undo.remove(0);
            } else if !self.redo.is_empty() {
                self.redo.remove(0);
            } else {
                break;
            }
        }
    }
}

impl From<String> for EditBuffer {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for EditBuffer {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl Serialize for EditBuffer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for EditBuffer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        if text.len() > MAX_TEXT_BYTES {
            return Err(serde::de::Error::custom("text exceeds 1 MiB"));
        }
        Ok(Self::new(text))
    }
}

fn normalize_input(input: &str, multiline: bool) -> Result<String, &'static str> {
    if input.contains('\0') {
        return Err("NUL is not allowed");
    }
    let normalized = if multiline {
        input.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        input.to_owned()
    };
    if normalized
        .chars()
        .any(|character| character.is_control() && !(multiline && matches!(character, '\n' | '\t')))
    {
        return Err("unsupported control character");
    }
    if normalized.len() > MAX_TEXT_BYTES {
        return Err("text exceeds 1 MiB");
    }
    Ok(normalized)
}

pub(crate) fn byte_from_utf16(text: &str, index: usize, round_up: bool) -> usize {
    let mut utf16 = 0usize;
    for (byte, character) in text.char_indices() {
        if index == utf16 {
            return snap_grapheme(text, byte, false);
        }
        let next = utf16 + character.len_utf16();
        if index < next {
            let byte = if round_up {
                byte + character.len_utf8()
            } else {
                byte
            };
            return snap_grapheme(text, byte, round_up);
        }
        utf16 = next;
    }
    text.len()
}

pub(crate) fn byte_range_from_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    if range.start == range.end {
        let cursor = byte_from_utf16(text, range.start, false);
        cursor..cursor
    } else {
        byte_from_utf16(text, range.start.min(range.end), false)
            ..byte_from_utf16(text, range.start.max(range.end), true)
    }
}

pub(crate) fn utf16_at_byte(text: &str, byte: usize) -> usize {
    text[..byte].encode_utf16().count()
}

fn snap_grapheme(text: &str, index: usize, round_up: bool) -> usize {
    let index = index.min(text.len());
    let mut previous = 0;
    for (start, grapheme) in text.grapheme_indices(true) {
        let end = start + grapheme.len();
        if index == start || index == end {
            return index;
        }
        if index < end {
            return if round_up { end } else { start };
        }
        previous = end;
    }
    previous
}

fn next_grapheme(text: &str, byte: usize) -> usize {
    text.get(byte..)
        .and_then(|suffix| suffix.graphemes(true).next())
        .map_or(text.len(), |grapheme| byte + grapheme.len())
}

fn previous_grapheme(text: &str, byte: usize) -> usize {
    text.get(..byte)
        .and_then(|prefix| prefix.graphemes(true).next_back())
        .map_or(0, |grapheme| byte - grapheme.len())
}

fn is_word(grapheme: &str) -> bool {
    grapheme
        .chars()
        .any(|character| character.is_alphanumeric() || character == '_')
}

fn is_whitespace(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace)
}

fn word_left(text: &str, mut byte: usize) -> usize {
    if byte == 0 {
        return 0;
    }
    let mut previous = previous_grapheme(text, byte);
    while previous < byte && is_whitespace(&text[previous..byte]) {
        byte = previous;
        if byte == 0 {
            return 0;
        }
        previous = previous_grapheme(text, byte);
    }
    if previous < byte && is_word(&text[previous..byte]) {
        byte = previous;
        loop {
            if byte == 0 {
                break;
            }
            let next = previous_grapheme(text, byte);
            if !is_word(&text[next..byte]) {
                break;
            }
            byte = next;
        }
        byte
    } else {
        previous
    }
}

fn word_right(text: &str, mut byte: usize) -> usize {
    let length = text.len();
    if byte >= length {
        return length;
    }
    let mut next = next_grapheme(text, byte);
    while next > byte && is_whitespace(&text[byte..next]) {
        byte = next;
        if byte >= length {
            return length;
        }
        next = next_grapheme(text, byte);
    }
    if next > byte && is_word(&text[byte..next]) {
        byte = next;
        loop {
            if byte >= length {
                break;
            }
            let after = next_grapheme(text, byte);
            if !is_word(&text[byte..after]) {
                break;
            }
            byte = after;
        }
        byte
    } else {
        next
    }
}

fn line_bounds(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let start = text[..byte].rfind('\n').map_or(0, |index| index + 1);
    let end = text[byte..]
        .find('\n')
        .map_or(text.len(), |index| byte + index);
    (start, end)
}

fn grapheme_count(text: &str) -> usize {
    text.graphemes(true).count()
}

fn advance_graphemes(text: &str, start: usize, end: usize, count: usize) -> usize {
    let mut byte = start;
    for grapheme in text[start..end].graphemes(true).take(count) {
        byte += grapheme.len();
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grapheme_navigation_and_reversed_selection_never_split_clusters() {
        let mut collapsed = EditBuffer::new("e\u{301}");
        collapsed.select_bytes(1..1, false);
        assert!(collapsed.selection().is_empty());
        assert_eq!(collapsed.cursor(), 0);

        let mut edit = EditBuffer::new("a\u{301}👩‍💻 z");
        let initial_revision = edit.revision();
        edit.navigate(Movement::DocumentStart, false);
        edit.navigate(Movement::Right, false);
        assert_eq!(edit.cursor(), "a\u{301}".len());
        assert_eq!(edit.revision(), initial_revision);
        edit.select_bytes(0.."a\u{301}".len() + "👩‍💻".len(), true);
        assert!(edit.reversed());
        assert_eq!(edit.selected_text(), "a\u{301}👩‍💻");
        assert!(edit.backspace(false));
        assert_eq!(edit.text(), " z");
        assert_eq!(edit.revision(), initial_revision + 1);
    }

    #[test]
    fn word_and_vertical_movement_keep_the_preferred_grapheme_column() {
        let mut edit = EditBuffer::new("one two\nabc\nx\n");
        edit.navigate(Movement::DocumentStart, false);
        edit.navigate(Movement::WordRight, false);
        assert_eq!(edit.cursor(), 3);
        edit.navigate(Movement::WordRight, false);
        assert_eq!(edit.cursor(), 7);
        edit.navigate(Movement::DocumentStart, false);
        for _ in 0..5 {
            edit.navigate(Movement::Right, false);
        }
        edit.navigate(Movement::Down, false);
        assert_eq!(edit.cursor(), 11); // clamps to the end of the shorter second line
        edit.navigate(Movement::Down, false);
        assert_eq!(edit.cursor(), 13); // preferred column is restored on the third line
    }

    #[test]
    fn utf16_replacement_composition_cancel_and_undo_are_grouped() {
        let mut edit = EditBuffer::new("A😀B");
        edit.select_utf16(2..2); // inside the surrogate pair, snap to its start
        assert_eq!(edit.cursor(), 1);
        assert_eq!(edit.selected_text(), "");
        edit.mark(1..3, "かな", Some(1..2), false).unwrap();
        assert_eq!(edit.marked(), Some(1..7));
        assert_eq!(edit.selected_utf16(), 2..3);
        edit.mark(1..3, "日本", None, false).unwrap();
        assert_eq!(edit.text(), "A日本B");
        assert!(edit.unmark());
        assert!(edit.undo());
        assert_eq!(edit.text(), "A😀B");

        edit.mark(1..3, "かな", None, false).unwrap();
        assert!(edit.cancel_mark());
        assert_eq!(edit.text(), "A😀B");
        assert!(!edit.undo());
        assert_eq!(edit.text(), "A😀B");
        let (selected, actual) = edit.text_for_utf16(2..3);
        assert_eq!(selected, "😀");
        assert_eq!(actual, 1..3);

        let mut insertion = EditBuffer::new("A😀B");
        insertion.replace(Some(2..2), "Z", false).unwrap();
        assert_eq!(insertion.text(), "AZ😀B");
    }

    #[test]
    fn rejected_and_oversized_edits_preserve_state_and_history_is_bounded() {
        let mut edit = EditBuffer::new("base");
        edit.select_all();
        edit.replace(None, "new\r\nline\t", true).unwrap();
        assert_eq!(edit.text(), "new\nline\t");
        let before = edit.clone();
        assert!(edit.replace(None, "bad\0input", true).is_err());
        assert!(edit.replace(None, "line\nfeed", false).is_err());
        assert_eq!(edit, before);
        edit.replace(None, &"x".repeat(MAX_TEXT_BYTES + 1), false)
            .unwrap_err();
        assert_eq!(edit, before);

        edit.select_all();
        for _ in 0..150 {
            edit.replace(None, "x", false).unwrap();
        }
        assert!(edit.undo.len() <= MAX_HISTORY_ENTRIES);
        assert!(
            edit.undo
                .iter()
                .map(|entry| entry.text.len())
                .sum::<usize>()
                <= MAX_HISTORY_BYTES
        );
    }

    #[test]
    fn serde_keeps_existing_plain_string_shape() -> Result<(), serde_json::Error> {
        let edit = EditBuffer::new("plain text");
        assert_eq!(serde_json::to_string(&edit)?, "\"plain text\"");
        assert_eq!(serde_json::from_str::<EditBuffer>("\"plain text\"")?, edit);
        let oversized = format!("\"{}\"", "x".repeat(MAX_TEXT_BYTES + 1));
        assert!(serde_json::from_str::<EditBuffer>(&oversized).is_err());
        Ok(())
    }

    #[test]
    fn text_revision_tracks_delete_undo_redo_but_not_selection() {
        let mut edit = EditBuffer::new("abcd");
        let initial = edit.revision();
        edit.select_bytes(1..3, false);
        assert_eq!(edit.revision(), initial);
        assert!(edit.backspace(false));
        assert_eq!(edit.text(), "ad");
        assert_eq!(edit.revision(), initial + 1);
        assert!(edit.undo());
        assert_eq!(edit.text(), "abcd");
        assert_eq!(edit.revision(), initial + 2);
        assert!(edit.redo());
        assert_eq!(edit.text(), "ad");
        assert_eq!(edit.revision(), initial + 3);

        edit.select_bytes(1..2, false);
        assert!(edit.delete(false));
        assert_eq!(edit.text(), "a");
        assert_eq!(edit.revision(), initial + 4);
    }
}
