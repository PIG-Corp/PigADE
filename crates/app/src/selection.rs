//! Frame-local cell selection and plain-text extraction.
//!
//! A selection is valid only for the exact displayed session, generation, and
//! sequence. Until the UI has a stable historical frame owner, any new frame
//! invalidates it. Extraction copies only cell text; styles, links, and other
//! host metadata are never included.

use crate::protocol::{Cell, View};

// These wire bits mirror alacritty_terminal::term::cell::Flags. The app's host
// protocol intentionally carries flag bits without depending on the core.
const WRAPLINE: u16 = 0x0010;
const WIDE_CHAR: u16 = 0x0020;
const WIDE_CHAR_SPACER: u16 = 0x0040;
const LEADING_WIDE_CHAR_SPACER: u16 = 0x0400;
const SPACER_FLAGS: u16 = WIDE_CHAR_SPACER | LEADING_WIDE_CHAR_SPACER;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FrameIdentity {
    session: String,
    generation: String,
    sequence: u64,
}

impl FrameIdentity {
    fn from_view(view: &View) -> Self {
        Self {
            session: view.session.clone(),
            generation: view.generation.clone(),
            sequence: view.sequence,
        }
    }

    fn matches(&self, view: &View) -> bool {
        self.session == view.session
            && self.generation == view.generation
            && self.sequence == view.sequence
    }
}

/// Zero-based row and column in the visible frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPosition {
    pub row: usize,
    pub column: usize,
}

/// Inclusive anchor/focus range tied to one immutable displayed frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellSelection {
    frame: FrameIdentity,
    anchor: CellPosition,
    focus: CellPosition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionError {
    InvalidFrameGeometry,
    OutOfBounds,
    FrameChanged,
}

impl CellSelection {
    pub fn new(
        view: &View,
        anchor: CellPosition,
        focus: CellPosition,
    ) -> Result<Self, SelectionError> {
        validate_position(view, anchor)?;
        validate_position(view, focus)?;
        Ok(Self {
            frame: FrameIdentity::from_view(view),
            anchor: glyph_origin(view, anchor),
            focus: glyph_origin(view, focus),
        })
    }

    #[cfg(test)]
    pub fn anchor(&self) -> CellPosition {
        self.anchor
    }

    #[cfg(test)]
    pub fn focus(&self) -> CellPosition {
        self.focus
    }

    /// Move the drag focus. A frame refresh invalidates the entire selection.
    pub fn set_focus(&mut self, view: &View, focus: CellPosition) -> Result<(), SelectionError> {
        self.ensure_current(view)?;
        validate_position(view, focus)?;
        self.focus = glyph_origin(view, focus);
        Ok(())
    }

    /// Whether a visible cell belongs to this selection, without allocating.
    ///
    /// Both cells of a valid wide glyph are considered selected when its
    /// leading cell is selected. Leading wide spacers are wrap padding and
    /// never map to a glyph.
    pub fn contains(&self, view: &View, position: CellPosition) -> bool {
        if !self.frame.matches(view) || validate_position(view, position).is_err() {
            return false;
        }
        let cols = view.frame.geometry.cols as usize;
        let cell = &view.frame.cells[position.row * cols + position.column];
        if cell.flags & LEADING_WIDE_CHAR_SPACER != 0 {
            return false;
        }
        let position = glyph_origin(view, position);
        let (start, end) = if position_key(self.anchor) <= position_key(self.focus) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        };
        position_key(start) <= position_key(position) && position_key(position) <= position_key(end)
    }

    pub fn is_current(&self, view: &View) -> bool {
        self.frame.matches(view)
    }

    /// Extract selected text from the exact frame that created this selection.
    ///
    /// Row boundaries marked WRAPLINE are joined without a newline. Other row
    /// boundaries are hard breaks. Cell spaces inside nonblank rows are kept.
    /// A row consisting only of default ASCII space cells is a blank row: such
    /// interior rows are preserved as empty lines, while trailing blank rows
    /// are removed (except that one selected row remains). This policy avoids
    /// copying screen-filling padding after a selection while retaining blank
    /// lines and intentional spaces within lines.
    pub fn text(&self, view: &View) -> Result<String, SelectionError> {
        self.ensure_current(view)?;
        validate_frame(view)?;

        let (start, end) = if position_key(self.anchor) <= position_key(self.focus) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        };
        let cols = view.frame.geometry.cols as usize;
        let mut rows = Vec::with_capacity(end.row - start.row + 1);
        for row in start.row..=end.row {
            let first_col = if row == start.row { start.column } else { 0 };
            let last_col = if row == end.row { end.column } else { cols - 1 };
            let cells = &view.frame.cells[row * cols + first_col..=row * cols + last_col];
            let blank = cells.iter().all(is_default_blank_or_spacer);
            let text = if blank {
                String::new()
            } else {
                cells.iter().filter_map(selected_cell_text).collect()
            };
            let soft_wrap =
                row < end.row && view.frame.cells[row * cols + cols - 1].flags & WRAPLINE != 0;
            rows.push(SelectedRow {
                text,
                blank,
                soft_wrap,
            });
        }

        while rows.len() > 1 && rows.last().is_some_and(|row| row.blank) {
            rows.pop();
        }

        let mut result = rows[0].text.clone();
        for index in 1..rows.len() {
            if !rows[index - 1].soft_wrap {
                result.push('\n');
            }
            result.push_str(&rows[index].text);
        }
        Ok(result)
    }

    fn ensure_current(&self, view: &View) -> Result<(), SelectionError> {
        if self.frame.matches(view) {
            Ok(())
        } else {
            Err(SelectionError::FrameChanged)
        }
    }
}

struct SelectedRow {
    text: String,
    blank: bool,
    soft_wrap: bool,
}

fn position_key(position: CellPosition) -> (usize, usize) {
    (position.row, position.column)
}

fn validate_frame(view: &View) -> Result<(), SelectionError> {
    let rows = view.frame.geometry.rows as usize;
    let cols = view.frame.geometry.cols as usize;
    if rows == 0 || cols == 0 || rows.checked_mul(cols) != Some(view.frame.cells.len()) {
        return Err(SelectionError::InvalidFrameGeometry);
    }
    Ok(())
}

fn validate_position(view: &View, position: CellPosition) -> Result<(), SelectionError> {
    validate_frame(view)?;
    if position.row >= view.frame.geometry.rows as usize
        || position.column >= view.frame.geometry.cols as usize
    {
        return Err(SelectionError::OutOfBounds);
    }
    Ok(())
}

/// Map the trailing half of a valid wide glyph to its leading cell.
///
/// An orphan trailing spacer is left alone, and a leading wrap spacer is never
/// mapped across a row boundary. Both cases remain non-text spacers.
fn glyph_origin(view: &View, position: CellPosition) -> CellPosition {
    if position.column == 0 {
        return position;
    }
    let cols = view.frame.geometry.cols as usize;
    let cell = &view.frame.cells[position.row * cols + position.column];
    let previous = &view.frame.cells[position.row * cols + position.column - 1];
    if cell.flags & WIDE_CHAR_SPACER != 0 && previous.flags & WIDE_CHAR != 0 {
        CellPosition {
            row: position.row,
            column: position.column - 1,
        }
    } else {
        position
    }
}

fn is_spacer(cell: &Cell) -> bool {
    cell.flags & SPACER_FLAGS != 0
}

fn is_default_blank_or_spacer(cell: &Cell) -> bool {
    is_spacer(cell) || cell.text.is_empty() || cell.text == " "
}

fn selected_cell_text(cell: &Cell) -> Option<&str> {
    (!is_spacer(cell)).then_some(cell.text.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Frame, Geometry};

    const WRAP: u16 = WRAPLINE;
    const WIDE: u16 = WIDE_CHAR;
    const WIDE_SPACER: u16 = WIDE_CHAR_SPACER;
    const LEADING_SPACER: u16 = LEADING_WIDE_CHAR_SPACER;

    fn view(rows: &[&[(&str, u16)]]) -> View {
        let cols = rows[0].len();
        assert!(rows.iter().all(|row| row.len() == cols));
        View {
            session: "session-a".into(),
            generation: "generation-a".into(),
            sequence: 7,
            state: "running".into(),
            recovery_path: String::new(),
            pid: Some(42),
            storage_error: None,
            frame: Frame {
                geometry: Geometry {
                    rows: rows.len() as u16,
                    cols: cols as u16,
                },
                cells: rows
                    .iter()
                    .flat_map(|row| row.iter())
                    .map(|(text, flags)| Cell {
                        text: (*text).into(),
                        fg: String::new(),
                        bg: String::new(),
                        flags: *flags,
                        underline: None,
                        hyperlink: None,
                    })
                    .collect(),
                hyperlinks: Vec::new(),
                cursor: (0, 0),
                cursor_style: String::new(),
                modes: 0,
                colors: String::new(),
                palette_overrides: Vec::new(),
                history_lines: 0,
                sync_pending_bytes: 0,
                title: String::new(),
            },
        }
    }

    fn select(view: &View, anchor: CellPosition, focus: CellPosition) -> String {
        CellSelection::new(view, anchor, focus)
            .unwrap()
            .text(view)
            .unwrap()
    }

    #[test]
    fn reverse_drag_extracts_in_reading_order_and_keeps_internal_spaces() {
        let frame = view(&[&[("a", 0), (" ", 0), ("b", 0), ("c", 0)]]);
        assert_eq!(
            select(
                &frame,
                CellPosition { row: 0, column: 3 },
                CellPosition { row: 0, column: 0 },
            ),
            "a bc"
        );
    }

    #[test]
    fn skips_wide_continuation_spacers_and_keeps_combining_text() {
        let frame = view(&[&[
            ("A", 0),
            ("界", WIDE),
            (" ", WIDE_SPACER),
            ("e\u{301}", 0),
            (" ", LEADING_SPACER),
            ("B", 0),
        ]]);
        assert_eq!(
            select(
                &frame,
                CellPosition { row: 0, column: 0 },
                CellPosition { row: 0, column: 5 },
            ),
            "A界e\u{301}B"
        );
        assert_eq!(
            select(
                &frame,
                CellPosition { row: 0, column: 2 },
                CellPosition { row: 0, column: 2 },
            ),
            "界"
        );
    }

    #[test]
    fn selecting_only_wide_trailing_half_selects_and_highlights_the_glyph() {
        let frame = view(&[&[("界", WIDE), (" ", WIDE_SPACER), ("x", 0)]]);
        let trailing = CellPosition { row: 0, column: 1 };
        let leading = CellPosition { row: 0, column: 0 };
        let selection = CellSelection::new(&frame, trailing, trailing).unwrap();

        assert_eq!(selection.anchor(), leading);
        assert_eq!(selection.focus(), leading);
        assert_eq!(selection.text(&frame).unwrap(), "界");
        assert!(selection.contains(&frame, leading));
        assert!(selection.contains(&frame, trailing));
        assert!(!selection.contains(&frame, CellPosition { row: 0, column: 2 }));
    }

    #[test]
    fn reverse_drag_endpoint_inside_wide_glyph_normalizes_to_its_origin() {
        let frame = view(&[&[("a", 0), ("界", WIDE), (" ", WIDE_SPACER), ("z", 0)]]);
        let selection = CellSelection::new(
            &frame,
            CellPosition { row: 0, column: 3 },
            CellPosition { row: 0, column: 2 },
        )
        .unwrap();

        assert_eq!(selection.focus(), CellPosition { row: 0, column: 1 });
        assert_eq!(selection.text(&frame).unwrap(), "界z");
        assert!(selection.contains(&frame, CellPosition { row: 0, column: 2 }));
        assert!(selection.contains(&frame, CellPosition { row: 0, column: 3 }));
        assert!(!selection.contains(&frame, CellPosition { row: 0, column: 0 }));
    }

    #[test]
    fn leading_wide_spacer_is_unselected_wrap_padding() {
        let frame = view(&[
            &[("a", WRAP), (" ", LEADING_SPACER)],
            &[("界", WIDE), (" ", WIDE_SPACER)],
        ]);
        let leading_padding = CellPosition { row: 0, column: 1 };
        let selection = CellSelection::new(&frame, leading_padding, leading_padding).unwrap();

        assert_eq!(selection.text(&frame).unwrap(), "");
        assert!(!selection.contains(&frame, leading_padding));
        assert!(!selection.contains(&frame, CellPosition { row: 1, column: 0 }));
    }

    #[test]
    fn soft_wrap_joins_rows_and_hard_break_adds_newline() {
        let soft = view(&[
            &[("a", 0), ("b", 0), ("c", WRAP)],
            &[("d", 0), ("e", 0), ("f", 0)],
        ]);
        let hard = view(&[
            &[("a", 0), ("b", 0), ("c", 0)],
            &[("d", 0), ("e", 0), ("f", 0)],
        ]);
        let anchor = CellPosition { row: 0, column: 0 };
        let focus = CellPosition { row: 1, column: 2 };
        assert_eq!(select(&soft, anchor, focus), "abcdef");
        assert_eq!(select(&hard, anchor, focus), "abc\ndef");
    }

    #[test]
    fn preserves_internal_empty_line_and_trims_only_trailing_blank_rows() {
        let frame = view(&[
            &[("A", 0), (" ", 0), (" ", 0)],
            &[(" ", 0), (" ", 0), (" ", 0)],
            &[("B", 0), (" ", 0), (" ", 0)],
            &[(" ", 0), (" ", 0), (" ", 0)],
        ]);
        assert_eq!(
            select(
                &frame,
                CellPosition { row: 0, column: 0 },
                CellPosition { row: 3, column: 2 },
            ),
            "A  \n\nB  "
        );
        assert_eq!(
            select(
                &frame,
                CellPosition { row: 3, column: 0 },
                CellPosition { row: 3, column: 2 },
            ),
            ""
        );
    }

    #[test]
    fn intentional_spaces_in_nonblank_rows_are_not_trimmed() {
        let frame = view(&[&[(" ", 0), ("x", 0), (" ", 0)]]);
        assert_eq!(
            select(
                &frame,
                CellPosition { row: 0, column: 0 },
                CellPosition { row: 0, column: 2 },
            ),
            " x "
        );
    }

    #[test]
    fn out_of_range_and_invalid_geometry_are_rejected() {
        let mut frame = view(&[&[("x", 0)]]);
        assert_eq!(
            CellSelection::new(
                &frame,
                CellPosition { row: 1, column: 0 },
                CellPosition { row: 0, column: 0 },
            ),
            Err(SelectionError::OutOfBounds)
        );
        frame.frame.cells.clear();
        assert_eq!(
            CellSelection::new(
                &frame,
                CellPosition { row: 0, column: 0 },
                CellPosition { row: 0, column: 0 },
            ),
            Err(SelectionError::InvalidFrameGeometry)
        );
    }

    #[test]
    fn selection_invalidates_on_sequence_or_generation_change() {
        let frame = view(&[&[("x", 0)]]);
        let mut selection = CellSelection::new(
            &frame,
            CellPosition { row: 0, column: 0 },
            CellPosition { row: 0, column: 0 },
        )
        .unwrap();
        assert_eq!(selection.anchor(), CellPosition { row: 0, column: 0 });
        assert_eq!(selection.focus(), CellPosition { row: 0, column: 0 });
        let mut next = frame.clone();
        next.sequence += 1;
        assert_eq!(selection.text(&next), Err(SelectionError::FrameChanged));
        assert_eq!(
            selection.set_focus(&next, CellPosition { row: 0, column: 0 }),
            Err(SelectionError::FrameChanged)
        );
        next.sequence = frame.sequence;
        next.generation = "generation-b".into();
        assert_eq!(selection.text(&next), Err(SelectionError::FrameChanged));
        next.generation = frame.generation.clone();
        next.session = "session-b".into();
        assert_eq!(selection.text(&next), Err(SelectionError::FrameChanged));
        assert!(selection.is_current(&frame));
    }
}
