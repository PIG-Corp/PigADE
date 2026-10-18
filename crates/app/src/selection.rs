//! Frame-local cell selection and plain-text extraction.
//!
//! A selection is valid only for the exact displayed session, generation,
//! sequence, and frame geometry. Historical selections also bind to one page
//! origin. Extraction copies only cell text; styles, links, and other host
//! metadata are never included.

use crate::protocol::{Cell, HistoryGridPage, View};

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
    rows: u16,
    cols: u16,
}

impl FrameIdentity {
    fn from_view(view: &View) -> Self {
        Self {
            session: view.session.clone(),
            generation: view.generation.clone(),
            sequence: view.sequence,
            rows: view.frame.geometry.rows,
            cols: view.frame.geometry.cols,
        }
    }

    fn matches(&self, view: &View) -> bool {
        self.session == view.session
            && self.generation == view.generation
            && self.sequence == view.sequence
            && self.rows == view.frame.geometry.rows
            && self.cols == view.frame.geometry.cols
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
    unit: SelectionUnit,
    anchor_unit: (CellPosition, CellPosition),
    history: Option<HistoryIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionUnit {
    Character,
    Word,
    Line,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HistoryIdentity {
    snapshot: uuid::Uuid,
    session: uuid::Uuid,
    generation: uuid::Uuid,
    sequence: u64,
    start: u64,
    column_start: u16,
    cols: u16,
    rows_requested: u16,
    row_count: usize,
    total_rows: u64,
    horizontal_clipped: bool,
}

impl HistoryIdentity {
    fn from_page(page: &HistoryGridPage) -> Self {
        Self {
            snapshot: page.snapshot,
            session: page.session,
            generation: page.generation,
            sequence: page.sequence,
            start: page.start,
            column_start: page.column_start,
            cols: page.cols,
            rows_requested: page.rows_requested,
            row_count: page.rows.len(),
            total_rows: page.total_rows,
            horizontal_clipped: page.horizontal_clipped,
        }
    }
    fn matches(&self, page: &HistoryGridPage) -> bool {
        self == &Self::from_page(page)
    }
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
            unit: SelectionUnit::Character,
            anchor_unit: (glyph_origin(view, anchor), glyph_origin(view, anchor)),
            history: None,
        })
    }

    pub fn begin(
        view: &View,
        position: CellPosition,
        unit: SelectionUnit,
        history: Option<&HistoryGridPage>,
    ) -> Result<Self, SelectionError> {
        let mut selection = Self::new(view, position, position)?;
        selection.unit = unit;
        if let Some(page) = history {
            validate_history(view, page)?;
            selection.history = Some(HistoryIdentity::from_page(page));
        }
        let extent = unit_extent(view, position, unit, history);
        selection.anchor_unit = extent;
        selection.anchor = extent.0;
        selection.focus = extent.1;
        Ok(selection)
    }

    pub fn new_with_history(
        view: &View,
        anchor: CellPosition,
        focus: CellPosition,
        page: &HistoryGridPage,
    ) -> Result<Self, SelectionError> {
        let mut selection = Self::new(view, anchor, focus)?;
        validate_history(view, page)?;
        selection.history = Some(HistoryIdentity::from_page(page));
        Ok(selection)
    }

    #[cfg(test)]
    pub fn anchor(&self) -> CellPosition {
        self.anchor
    }

    #[cfg(test)]
    pub fn focus(&self) -> CellPosition {
        self.focus
    }

    /// Move the drag focus by the same unit, within the exact original frame/page.
    pub fn set_focus_in(
        &mut self,
        view: &View,
        focus: CellPosition,
        history: Option<&HistoryGridPage>,
    ) -> Result<(), SelectionError> {
        self.ensure_current(view)?;
        validate_position(view, focus)?;
        if let Some(expected) = &self.history {
            let page = history.ok_or(SelectionError::FrameChanged)?;
            validate_history(view, page)?;
            if !expected.matches(page) {
                return Err(SelectionError::FrameChanged);
            }
        } else if history.is_some() {
            return Err(SelectionError::FrameChanged);
        }
        let focus_extent = unit_extent(view, focus, self.unit, history);
        match self.unit {
            SelectionUnit::Character => self.focus = glyph_origin(view, focus),
            _ => {
                let (a0, a1) = self.anchor_unit;
                if position_key(focus_extent.0) < position_key(a0) {
                    self.anchor = a1;
                    self.focus = focus_extent.0;
                } else if position_key(focus_extent.0) > position_key(a1) {
                    self.anchor = a0;
                    self.focus = focus_extent.1;
                } else {
                    self.anchor = a0;
                    self.focus = a1;
                }
            }
        }
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
        let glyph = glyph_origin(view, position);
        if cell.flags & WIDE_CHAR_SPACER != 0 && glyph == position {
            // An orphan trailing spacer has no leading glyph to select.
            return false;
        }
        let position = glyph;
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
        if self.history.is_some() {
            return Err(SelectionError::FrameChanged);
        }
        self.text_with_spans(view, None, 0, None)
    }

    pub fn text_with_history(
        &self,
        view: &View,
        page: &HistoryGridPage,
    ) -> Result<String, SelectionError> {
        self.ensure_current(view)?;
        validate_history(view, page)?;
        if !self
            .history
            .as_ref()
            .is_some_and(|identity| identity.matches(page))
        {
            return Err(SelectionError::FrameChanged);
        }
        self.text_with_spans(view, None, usize::from(page.column_start), Some(page))
    }

    /// Copy a physical horizontal viewport without joining missing columns
    /// across soft-wrapped source rows. Widths describe complete source rows.
    #[cfg(test)]
    pub fn text_with_row_spans(
        &self,
        view: &View,
        widths: &[u16],
        column_start: u16,
    ) -> Result<String, SelectionError> {
        self.text_with_spans(view, Some(widths), usize::from(column_start), None)
    }

    fn text_with_spans(
        &self,
        view: &View,
        widths: Option<&[u16]>,
        column_start: usize,
        history: Option<&HistoryGridPage>,
    ) -> Result<String, SelectionError> {
        if self.history.is_some() && history.is_none() {
            return Err(SelectionError::FrameChanged);
        }
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
            let source_cols = if let Some(page) = history {
                page.rows.get(row).map_or(0, |source| {
                    if source.boundary.is_some() {
                        cols + column_start
                    } else {
                        usize::from(source.source_columns)
                    }
                })
            } else {
                widths
                    .and_then(|widths| widths.get(row))
                    .copied()
                    .filter(|width| *width > 0)
                    .map_or(cols, usize::from)
            };
            let row_cols = source_cols.saturating_sub(column_start).min(cols);
            let last_col = last_col.min(row_cols.saturating_sub(1));
            let mut last_col = last_col;
            if self.unit == SelectionUnit::Line
                && column_start == 0
                && source_cols <= cols
                && !row_soft_wrap(view, history, row)
            {
                while last_col > first_col && view.frame.cells[row * cols + last_col].text == " " {
                    last_col -= 1;
                }
            }
            let cells = if row_cols > 0 && first_col <= last_col {
                &view.frame.cells[row * cols + first_col..=row * cols + last_col]
            } else {
                &[]
            };
            let blank = cells.iter().all(is_default_blank_or_spacer)
                && self.unit != SelectionUnit::Word
                && !(self.unit == SelectionUnit::Line && row_soft_wrap(view, history, row));
            let text = if blank {
                String::new()
            } else {
                cells.iter().filter_map(selected_cell_text).collect()
            };
            let soft_wrap = if let Some(page) = history {
                row < end.row && row_continues(view, Some(page), row)
            } else {
                column_start == 0
                    && row < end.row
                    && row_cols > 0
                    && first_col <= last_col
                    && source_cols <= cols
                    && view.frame.cells[row * cols + row_cols - 1].flags & WRAPLINE != 0
            };
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

fn validate_history(view: &View, page: &HistoryGridPage) -> Result<(), SelectionError> {
    if page.sequence != view.sequence
        || page.session.to_string() != view.session
        || page.generation.to_string() != view.generation
        || page.cols == 0
        || page.rows_requested != view.frame.geometry.rows
        || page.cols != view.frame.geometry.cols
        || page.rows.len() > usize::from(page.rows_requested)
        || page.rows.iter().enumerate().any(|(index, row)| {
            row.row != page.start + index as u64
                || row.source_columns > 1024
                || row.cells.len()
                    != usize::from(
                        row.source_columns
                            .saturating_sub(page.column_start)
                            .min(page.cols),
                    )
                || (row.boundary.is_some() && !row.cells.is_empty())
        })
    {
        return Err(SelectionError::FrameChanged);
    }
    Ok(())
}

fn unit_extent(
    view: &View,
    position: CellPosition,
    unit: SelectionUnit,
    history: Option<&HistoryGridPage>,
) -> (CellPosition, CellPosition) {
    let p = glyph_origin(view, position);
    if unit == SelectionUnit::Character {
        return (p, p);
    }
    if unit == SelectionUnit::Line {
        let mut first = p.row;
        let mut last = p.row;
        if history.is_none_or(|page| page.column_start == 0) {
            while first > 0 && row_continues(view, history, first - 1) {
                first -= 1;
            }
            while last + 1 < view.frame.geometry.rows as usize && row_continues(view, history, last)
            {
                last += 1;
            }
        }
        return (
            CellPosition {
                row: first,
                column: 0,
            },
            CellPosition {
                row: last,
                column: view.frame.geometry.cols as usize - 1,
            },
        );
    }
    let (lo, hi) = token_extent(view, p, history);
    (lo, hi)
}

fn row_cells<'a>(view: &'a View, _history: Option<&'a HistoryGridPage>, row: usize) -> &'a [Cell] {
    let cols = view.frame.geometry.cols as usize;
    &view.frame.cells[row * cols..(row + 1) * cols]
}
fn source_width(view: &View, history: Option<&HistoryGridPage>, row: usize) -> usize {
    let cols = usize::from(view.frame.geometry.cols);
    if let Some(page) = history {
        page.rows.get(row).map_or(0, |source| {
            if source.boundary.is_some() {
                cols
            } else {
                usize::from(source.source_columns.saturating_sub(page.column_start)).min(cols)
            }
        })
    } else {
        cols
    }
}

fn row_soft_wrap(view: &View, history: Option<&HistoryGridPage>, row: usize) -> bool {
    if let Some(page) = history {
        let Some(source) = page.rows.get(row) else {
            return false;
        };
        page.column_start == 0
            && source.boundary.is_none()
            && source.source_columns > 0
            && source.source_columns <= page.cols
            && view.frame.cells
                [row * usize::from(page.cols) + usize::from(source.source_columns) - 1]
                .flags
                & WRAPLINE
                != 0
    } else {
        let cols = view.frame.geometry.cols as usize;
        view.frame.cells[row * cols + cols - 1].flags & WRAPLINE != 0
    }
}

/// A visible logical continuation cannot cross a saved history boundary or
/// leave the current page, even when the prior source edge carries WRAPLINE.
fn row_continues(view: &View, history: Option<&HistoryGridPage>, row: usize) -> bool {
    row + 1 < usize::from(view.frame.geometry.rows)
        && row_soft_wrap(view, history, row)
        && history.is_none_or(|page| {
            page.rows
                .get(row + 1)
                .is_some_and(|next| next.boundary.is_none())
        })
}

fn previous_token_cell(
    view: &View,
    history: Option<&HistoryGridPage>,
    position: CellPosition,
) -> Option<CellPosition> {
    if position.column > 0 {
        return Some(CellPosition {
            row: position.row,
            column: position.column - 1,
        });
    }
    if position.row == 0 || !row_continues(view, history, position.row - 1) {
        return None;
    }
    let previous_row = position.row - 1;
    let width = source_width(view, history, previous_row);
    width.checked_sub(1).map(|column| CellPosition {
        row: previous_row,
        column,
    })
}

fn next_token_cell(
    view: &View,
    history: Option<&HistoryGridPage>,
    position: CellPosition,
) -> Option<CellPosition> {
    if position.column + 1 < source_width(view, history, position.row) {
        return Some(CellPosition {
            row: position.row,
            column: position.column + 1,
        });
    }
    if position.row + 1 >= usize::from(view.frame.geometry.rows)
        || !row_continues(view, history, position.row)
    {
        return None;
    }
    let row = position.row + 1;
    if history.is_some_and(|page| {
        page.rows
            .get(row)
            .is_none_or(|source| source.boundary.is_some())
    }) {
        return None;
    }
    let width = source_width(view, history, row);
    if width == 0 {
        return None;
    }
    let cells = row_cells(view, history, row);
    if cells[0].flags & LEADING_WIDE_CHAR_SPACER != 0 && width > 1 {
        Some(CellPosition { row, column: 1 })
    } else {
        Some(CellPosition { row, column: 0 })
    }
}

fn token_extent(
    view: &View,
    p: CellPosition,
    history: Option<&HistoryGridPage>,
) -> (CellPosition, CellPosition) {
    let cells = row_cells(view, history, p.row);
    let is_word = |c: &Cell| {
        !is_spacer(c)
            && !c.text.chars().all(char::is_whitespace)
            && !c.text.chars().any(is_delimiter)
    };
    let target = cells.get(p.column);
    if p.column >= source_width(view, history, p.row)
        || target.is_none()
        || is_spacer(target.unwrap())
    {
        return (p, p);
    }
    let whitespace = target.unwrap().text.chars().all(char::is_whitespace);
    if !whitespace && !is_word(target.unwrap()) {
        return (p, p);
    }
    let eligible = |c: &Cell| {
        !is_spacer(c)
            && if whitespace {
                c.text.chars().all(char::is_whitespace)
            } else {
                is_word(c)
            }
    };
    let mut lo = p;
    let mut hi = p;
    while let Some(mut previous) = previous_token_cell(view, history, lo) {
        let previous_cells = row_cells(view, history, previous.row);
        if previous_cells[previous.column].flags & LEADING_WIDE_CHAR_SPACER != 0 {
            let Some(before_wrap) = previous_token_cell(view, history, previous) else {
                break;
            };
            previous = before_wrap;
        }
        let previous_cells = row_cells(view, history, previous.row);
        if previous_cells[previous.column].flags & WIDE_CHAR_SPACER != 0
            && previous.column > 0
            && previous_cells[previous.column - 1].flags & WIDE_CHAR != 0
        {
            previous.column -= 1;
        }
        if !eligible(&row_cells(view, history, previous.row)[previous.column]) {
            break;
        }
        lo = previous;
    }
    while let Some(mut next) = next_token_cell(view, history, hi) {
        let next_cells = row_cells(view, history, next.row);
        if next_cells[next.column].flags & WIDE_CHAR_SPACER != 0
            && next.column > 0
            && next_cells[next.column - 1].flags & WIDE_CHAR != 0
        {
            if let Some(after_spacer) = next_token_cell(view, history, next) {
                next = after_spacer;
            } else {
                break;
            }
        }
        if row_cells(view, history, next.row)[next.column].flags & LEADING_WIDE_CHAR_SPACER != 0 {
            if next.column + 1 != source_width(view, history, next.row)
                || !row_soft_wrap(view, history, next.row)
            {
                break;
            }
            let Some(after_padding) = next_token_cell(view, history, next) else {
                break;
            };
            next = after_padding;
        }
        if !eligible(&row_cells(view, history, next.row)[next.column]) {
            break;
        }
        hi = next;
    }
    lo = glyph_origin(view, lo);
    hi = glyph_origin(view, hi);
    (lo, hi)
}
fn is_delimiter(c: char) -> bool {
    matches!(
        c,
        ',' | ';'
            | ':'
            | '!'
            | '?'
            | '\''
            | '"'
            | '`'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '<'
            | '>'
            | '|'
            | '\\'
    )
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
    use crate::protocol::{Frame, Geometry, HistoryGridRow};

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
            stop_error: None,
            stop_scope: None,
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

    fn select(view: &View, anchor: CellPosition, focus: CellPosition) -> String {
        CellSelection::new(view, anchor, focus)
            .unwrap()
            .text(view)
            .unwrap()
    }

    #[test]
    fn historical_padding_is_excluded_and_source_soft_wraps_are_preserved() {
        let frame = view(&[
            &[("a", 0), ("b", WRAPLINE), (" ", 0), (" ", 0)],
            &[("c", 0), ("d", 0), ("e", 0), (" ", 0)],
        ]);
        let selected = CellSelection::new(
            &frame,
            CellPosition { row: 0, column: 0 },
            CellPosition { row: 1, column: 3 },
        )
        .unwrap();
        assert_eq!(
            selected.text_with_row_spans(&frame, &[2, 3], 0).unwrap(),
            "abcde"
        );
        assert_eq!(selected.text(&frame).unwrap(), "ab  \ncde ");
    }

    #[test]
    fn clipping_a_wider_source_never_claims_a_complete_soft_wrap() {
        let frame = view(&[&[("a", 0), ("b", WRAPLINE)], &[("c", 0), ("d", 0)]]);
        let selected = CellSelection::new(
            &frame,
            CellPosition { row: 0, column: 0 },
            CellPosition { row: 1, column: 1 },
        )
        .unwrap();
        assert_eq!(
            selected.text_with_row_spans(&frame, &[4, 2], 0).unwrap(),
            "ab\ncd"
        );
    }

    #[test]
    fn panned_rows_copy_only_visible_spans_without_false_soft_wrap_joins() {
        let frame = view(&[
            &[("a", 0), ("b", WRAPLINE)],
            &[("c", 0), ("d", WRAPLINE)],
            &[(" ", 0), (" ", 0)],
        ]);
        let selection = CellSelection::new(
            &frame,
            CellPosition { row: 0, column: 0 },
            CellPosition { row: 2, column: 1 },
        )
        .unwrap();
        assert_eq!(
            selection
                .text_with_row_spans(&frame, &[4, 3, 1], 2)
                .unwrap(),
            "ab\nc"
        );
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
    fn orphan_wide_trailing_spacer_is_not_highlighted() {
        let frame = view(&[&[(" ", WIDE_SPACER), ("x", 0)]]);
        let orphan = CellPosition { row: 0, column: 0 };
        let selection = CellSelection::new(&frame, orphan, orphan).unwrap();
        assert!(!selection.contains(&frame, orphan));
        assert_eq!(selection.text(&frame).unwrap(), "");
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
            selection.set_focus_in(&next, CellPosition { row: 0, column: 0 }, None),
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

    #[test]
    fn word_selection_keeps_paths_and_cyrillic_clusters_but_splits_punctuation() {
        let frame = view(&[&[
            ("/", 0),
            ("home", 0),
            ("/", 0),
            ("путь", 0),
            ("_", 0),
            ("файл", 0),
            (".", 0),
            ("txt", 0),
            (",", 0),
            (" ", 0),
            ("\u{301}x", 0),
        ]]);
        let selected = CellSelection::begin(
            &frame,
            CellPosition { row: 0, column: 4 },
            SelectionUnit::Word,
            None,
        )
        .unwrap();
        assert_eq!(selected.text(&frame).unwrap(), "/home/путь_файл.txt");
        let punctuation = CellSelection::begin(
            &frame,
            CellPosition { row: 0, column: 8 },
            SelectionUnit::Word,
            None,
        )
        .unwrap();
        assert_eq!(punctuation.text(&frame).unwrap(), ",");
    }

    #[test]
    fn word_selection_crosses_complete_live_soft_wraps_only() {
        let wrapped = view(&[
            &[("/", 0), ("opt/path", WRAPLINE)],
            &[("file.txt", 0), (" ", 0)],
        ]);
        let selection = CellSelection::begin(
            &wrapped,
            CellPosition { row: 0, column: 1 },
            SelectionUnit::Word,
            None,
        )
        .unwrap();
        assert_eq!(selection.text(&wrapped).unwrap(), "/opt/pathfile.txt");

        let hard = view(&[&[("/", 0), ("opt/path", 0)], &[("file.txt", 0), (" ", 0)]]);
        let selection = CellSelection::begin(
            &hard,
            CellPosition { row: 0, column: 1 },
            SelectionUnit::Word,
            None,
        )
        .unwrap();
        assert_eq!(selection.text(&hard).unwrap(), "/opt/path");
    }

    #[test]
    fn wrapped_wide_glyph_skips_only_the_leading_wrap_padding() {
        let frame = view(&[
            &[("a", 0), (" ", LEADING_WIDE_CHAR_SPACER | WRAPLINE)],
            &[("界", WIDE_CHAR), ("", WIDE_CHAR_SPACER)],
        ]);
        for p in [
            CellPosition { row: 0, column: 0 },
            CellPosition { row: 1, column: 1 },
        ] {
            let selection = CellSelection::begin(&frame, p, SelectionUnit::Word, None).unwrap();
            assert_eq!(selection.text(&frame).unwrap(), "a界");
        }
    }

    #[test]
    fn word_selection_preserves_clicked_whitespace_runs() {
        let frame = view(&[&[("a", 0), (" ", 0), (" ", 0), ("b", 0)]]);
        let selection = CellSelection::begin(
            &frame,
            CellPosition { row: 0, column: 1 },
            SelectionUnit::Word,
            None,
        )
        .unwrap();
        assert_eq!(selection.text(&frame).unwrap(), "  ");
    }

    #[test]
    fn word_drag_extends_by_whole_words_and_soft_wrapped_lines_expand_together() {
        let frame = view(&[
            &[("one", 0), (" ", 0), ("two", WRAPLINE)],
            &[("three", 0), (" ", 0), ("four", 0)],
        ]);
        let mut word = CellSelection::begin(
            &frame,
            CellPosition { row: 0, column: 0 },
            SelectionUnit::Word,
            None,
        )
        .unwrap();
        word.set_focus_in(&frame, CellPosition { row: 1, column: 2 }, None)
            .unwrap();
        assert_eq!(word.text(&frame).unwrap(), "one twothree four");
        let mut reverse = CellSelection::begin(
            &frame,
            CellPosition { row: 1, column: 2 },
            SelectionUnit::Word,
            None,
        )
        .unwrap();
        reverse
            .set_focus_in(&frame, CellPosition { row: 0, column: 0 }, None)
            .unwrap();
        assert_eq!(reverse.text(&frame).unwrap(), "one twothree four");
        let line = CellSelection::begin(
            &frame,
            CellPosition { row: 1, column: 0 },
            SelectionUnit::Line,
            None,
        )
        .unwrap();
        assert_eq!(line.text(&frame).unwrap(), "one twothree four");
        let mut reverse_line = CellSelection::begin(
            &frame,
            CellPosition { row: 1, column: 0 },
            SelectionUnit::Line,
            None,
        )
        .unwrap();
        reverse_line
            .set_focus_in(&frame, CellPosition { row: 0, column: 0 }, None)
            .unwrap();
        assert_eq!(reverse_line.text(&frame).unwrap(), "one twothree four");
    }

    #[test]
    fn character_drag_remains_inclusive_and_does_not_expand_to_a_word() {
        let frame = view(&[&[("hello", 0), (" ", 0), ("world", 0)]]);
        let selection = CellSelection::new(
            &frame,
            CellPosition { row: 0, column: 0 },
            CellPosition { row: 0, column: 1 },
        )
        .unwrap();
        assert_eq!(selection.text(&frame).unwrap(), "hello ");
    }

    #[test]
    fn line_copy_trims_only_complete_hard_edge_padding() {
        let hard = view(&[&[("x", 0), (" ", 0), (" ", 0)]]);
        let line = CellSelection::begin(
            &hard,
            CellPosition { row: 0, column: 0 },
            SelectionUnit::Line,
            None,
        )
        .unwrap();
        assert_eq!(line.text(&hard).unwrap(), "x");
        let soft = view(&[
            &[("x", 0), (" ", 0), (" ", WRAPLINE)],
            &[("y", 0), (" ", 0), (" ", 0)],
        ]);
        let line = CellSelection::begin(
            &soft,
            CellPosition { row: 0, column: 0 },
            SelectionUnit::Line,
            None,
        )
        .unwrap();
        assert_eq!(line.text(&soft).unwrap(), "x  y");
    }

    #[test]
    fn history_selection_is_bound_to_its_page_origin() {
        let mut frame = view(&[&[("a", 0), ("b", WRAPLINE)], &[("c", 0), ("d", 0)]]);
        frame.session = "00000000-0000-0000-0000-000000000001".into();
        frame.generation = "00000000-0000-0000-0000-000000000002".into();
        let page_at = |start| HistoryGridPage {
            snapshot: uuid::Uuid::from_u128(3),
            session: uuid::Uuid::from_u128(1),
            generation: uuid::Uuid::from_u128(2),
            sequence: frame.sequence,
            total_rows: 10,
            start,
            rows_requested: 2,
            rows: frame
                .frame
                .cells
                .chunks(2)
                .enumerate()
                .map(|(offset, cells)| HistoryGridRow {
                    row: start + offset as u64,
                    source_columns: 2,
                    boundary: None,
                    cells: cells.to_vec(),
                    palette_overrides: Vec::new(),
                })
                .collect(),
            hyperlinks: Vec::new(),
            cols: 2,
            column_start: 0,
            horizontal_clipped: false,
            warnings: Vec::new(),
        };
        let page = page_at(4);
        let selection = CellSelection::new_with_history(
            &frame,
            CellPosition { row: 0, column: 0 },
            CellPosition { row: 1, column: 1 },
            &page,
        )
        .unwrap();
        assert_eq!(selection.text_with_history(&frame, &page).unwrap(), "abcd");
        assert_eq!(
            selection.text_with_history(&frame, &page_at(5)),
            Err(SelectionError::FrameChanged)
        );
    }

    #[test]
    fn history_word_crosses_a_short_physical_soft_wrap_but_not_pan_clip_or_hard_breaks() {
        let make_case = |column_start: u16, source_columns: u16, row0_cells: Vec<Cell>| {
            let mut frame = view(&[
                &[("/", 0), ("path", 0), (" ", 0), (" ", 0)],
                &[("name", 0), (" ", 0), ("x", 0), (" ", 0)],
            ]);
            frame.session = "00000000-0000-0000-0000-000000000001".into();
            frame.generation = "00000000-0000-0000-0000-000000000002".into();
            for (column, source_cell) in row0_cells.iter().enumerate() {
                frame.frame.cells[column] = source_cell.clone();
            }
            let mut first_row = row0_cells;
            while first_row.len() < usize::from(source_columns.saturating_sub(column_start).min(4))
            {
                first_row.push(cell(" ", 0));
            }
            let page = HistoryGridPage {
                snapshot: uuid::Uuid::from_u128(3),
                session: uuid::Uuid::from_u128(1),
                generation: uuid::Uuid::from_u128(2),
                sequence: frame.sequence,
                total_rows: 10,
                start: 4,
                rows_requested: 2,
                rows: vec![
                    HistoryGridRow {
                        row: 4,
                        source_columns,
                        boundary: None,
                        cells: first_row,
                        palette_overrides: Vec::new(),
                    },
                    HistoryGridRow {
                        row: 5,
                        source_columns: 3,
                        boundary: None,
                        cells: vec![cell("name", 0), cell(" ", 0), cell("x", 0)]
                            .into_iter()
                            .take(usize::from(3u16.saturating_sub(column_start)))
                            .collect(),
                        palette_overrides: Vec::new(),
                    },
                ],
                hyperlinks: Vec::new(),
                cols: 4,
                column_start,
                horizontal_clipped: source_columns > column_start + 4,
                warnings: Vec::new(),
            };
            (frame, page)
        };

        let (frame, page) = make_case(0, 2, vec![cell("/", 0), cell("path", WRAPLINE)]);
        let selected = CellSelection::begin(
            &frame,
            CellPosition { row: 0, column: 1 },
            SelectionUnit::Word,
            Some(&page),
        )
        .unwrap();
        assert_eq!(
            selected.text_with_history(&frame, &page).unwrap(),
            "/pathname"
        );

        let mut boundary_frame = frame.clone();
        boundary_frame.frame.cells[4] = cell("-", 0);
        let mut boundary_page = page.clone();
        boundary_page.rows[1].source_columns = 0;
        boundary_page.rows[1].boundary = Some("cleared".into());
        boundary_page.rows[1].cells.clear();
        let selected = CellSelection::begin(
            &boundary_frame,
            CellPosition { row: 0, column: 1 },
            SelectionUnit::Word,
            Some(&boundary_page),
        )
        .unwrap();
        assert_eq!(
            selected
                .text_with_history(&boundary_frame, &boundary_page)
                .unwrap(),
            "/path"
        );

        let line = CellSelection::begin(
            &boundary_frame,
            CellPosition { row: 0, column: 1 },
            SelectionUnit::Line,
            Some(&boundary_page),
        )
        .unwrap();
        assert_eq!(
            line.text_with_history(&boundary_frame, &boundary_page)
                .unwrap(),
            "/path"
        );
        let all = CellSelection::new_with_history(
            &boundary_frame,
            CellPosition { row: 0, column: 0 },
            CellPosition { row: 1, column: 0 },
            &boundary_page,
        )
        .unwrap();
        assert_eq!(
            all.text_with_history(&boundary_frame, &boundary_page)
                .unwrap(),
            "/path\n-"
        );

        let (frame, page) = make_case(
            0,
            6,
            vec![
                cell("/", 0),
                cell("path", 0),
                cell(" ", 0),
                cell(" ", WRAPLINE),
            ],
        );
        let selected = CellSelection::begin(
            &frame,
            CellPosition { row: 0, column: 1 },
            SelectionUnit::Word,
            Some(&page),
        )
        .unwrap();
        assert_eq!(selected.text_with_history(&frame, &page).unwrap(), "/path");

        let (frame, page) = make_case(
            1,
            5,
            vec![
                cell("/", 0),
                cell("path", 0),
                cell(" ", 0),
                cell(" ", WRAPLINE),
            ],
        );
        let selected = CellSelection::begin(
            &frame,
            CellPosition { row: 0, column: 1 },
            SelectionUnit::Word,
            Some(&page),
        )
        .unwrap();
        assert_eq!(selected.text_with_history(&frame, &page).unwrap(), "/path");

        let (frame, page) = make_case(0, 2, vec![cell("/", 0), cell("path", 0)]);
        let selected = CellSelection::begin(
            &frame,
            CellPosition { row: 0, column: 1 },
            SelectionUnit::Word,
            Some(&page),
        )
        .unwrap();
        assert_eq!(selected.text_with_history(&frame, &page).unwrap(), "/path");
    }

    #[test]
    fn panned_history_does_not_join_or_copy_hidden_source_columns() {
        let mut frame = view(&[
            &[("b", 0), ("", WIDE_CHAR_SPACER | WRAPLINE)],
            &[("d", 0), ("e", 0)],
        ]);
        frame.session = "00000000-0000-0000-0000-000000000001".into();
        frame.generation = "00000000-0000-0000-0000-000000000002".into();
        let page = HistoryGridPage {
            snapshot: uuid::Uuid::from_u128(3),
            session: uuid::Uuid::from_u128(1),
            generation: uuid::Uuid::from_u128(2),
            sequence: frame.sequence,
            total_rows: 10,
            start: 4,
            rows_requested: 2,
            rows: vec![
                HistoryGridRow {
                    row: 4,
                    source_columns: 4,
                    boundary: None,
                    cells: vec![cell("b", 0), cell("界", WIDE_CHAR | WRAPLINE)],
                    palette_overrides: Vec::new(),
                },
                HistoryGridRow {
                    row: 5,
                    source_columns: 4,
                    boundary: None,
                    cells: vec![cell("d", 0), cell("e", 0)],
                    palette_overrides: Vec::new(),
                },
            ],
            hyperlinks: Vec::new(),
            cols: 2,
            column_start: 1,
            horizontal_clipped: true,
            warnings: Vec::new(),
        };
        let selection = CellSelection::new_with_history(
            &frame,
            CellPosition { row: 0, column: 0 },
            CellPosition { row: 1, column: 1 },
            &page,
        )
        .unwrap();
        assert_eq!(selection.text_with_history(&frame, &page).unwrap(), "b\nde");
    }

    #[test]
    fn logical_line_keeps_entire_blank_soft_wrapped_rows() {
        let frame = view(&[
            &[("a", 0), (" ", WRAPLINE)],
            &[(" ", 0), (" ", WRAPLINE)],
            &[("b", 0), (" ", 0)],
        ]);
        let selected = CellSelection::begin(
            &frame,
            CellPosition { row: 1, column: 0 },
            SelectionUnit::Line,
            None,
        )
        .unwrap();
        assert_eq!(selected.text(&frame).unwrap(), "a   b");
    }

    #[test]
    fn history_boundary_label_remains_selectable_when_page_row_has_no_cells() {
        let mut frame = view(&[&[("-", 0), ("-", 0), ("-", 0), (" ", 0)]]);
        frame.session = "00000000-0000-0000-0000-000000000001".into();
        frame.generation = "00000000-0000-0000-0000-000000000002".into();
        let page = HistoryGridPage {
            snapshot: uuid::Uuid::from_u128(3),
            session: uuid::Uuid::from_u128(1),
            generation: uuid::Uuid::from_u128(2),
            sequence: frame.sequence,
            total_rows: 1,
            start: 0,
            rows_requested: 1,
            rows: vec![HistoryGridRow {
                row: 0,
                source_columns: 0,
                boundary: Some("cleared".into()),
                cells: Vec::new(),
                palette_overrides: Vec::new(),
            }],
            hyperlinks: Vec::new(),
            cols: 4,
            column_start: 0,
            horizontal_clipped: false,
            warnings: Vec::new(),
        };
        let selection = CellSelection::begin(
            &frame,
            CellPosition { row: 0, column: 0 },
            SelectionUnit::Word,
            Some(&page),
        )
        .unwrap();
        assert_eq!(selection.text_with_history(&frame, &page).unwrap(), "---");
    }

    fn cell(text: &str, flags: u16) -> Cell {
        Cell {
            text: text.into(),
            fg: String::new(),
            bg: String::new(),
            flags,
            underline: None,
            hyperlink: None,
        }
    }
}
