//! Borrowed serializer for the semantic terminal checkpoint schema.
//!
//! The DTO exporter remains useful to callers which need an owned value. This
//! view streams rows/cells and borrows large strings/combining sequences when
//! serializing, while retaining only identity maps for shared cell extras and
//! equal-content hyperlinks.
use std::{collections::HashMap, sync::Arc};

use serde::{
    Serialize,
    ser::{SerializeSeq, SerializeStruct, Serializer},
};

use crate::{
    grid::{Dimensions, Grid},
    term::{
        Config, Term, TermMode,
        cell::{Cell, CellExtra, Hyperlink},
        checkpoint::{CHECKPOINT_CORE, CheckpointError, CursorStyleV1, MAX_COMBINING_CHARS},
        color,
    },
    vte::ansi::{CharsetIndex, KeyboardModes},
};

use crate::grid::checkpoint::CharsetV1;

const MAX_GRID_CELLS: usize = 16_777_216;
const MAX_TERM_GRID_CELLS: usize = 16_777_216 + 512 * 4096;
const MAX_GRID_COLUMNS: usize = 4096;
const MAX_GRID_LINES: usize = 512;
const MAX_SCROLLBACK: usize = 10_000;
const MAX_TEXT_BYTES: usize = 65_536;

/// A validated, borrowed view of the existing `TermCheckpointV1` wire shape.
///
/// Construction performs the same semantic bounds checks as
/// `TermCheckpointV1::validate`. Serialization does not construct owned row,
/// cell, extra, hyperlink, title, or combining-character DTOs.
pub struct TermCheckpointRef<'a, T> {
    term: &'a Term<T>,
    pools: BorrowedPools<'a>,
    active_max_scroll_limit: usize,
    inactive_max_scroll_limit: usize,
}

impl<'a, T> TermCheckpointRef<'a, T> {
    pub fn new(term: &'a Term<T>) -> Result<Self, CheckpointError> {
        let active_max_scroll_limit = term.grid.checkpoint_max_scroll_limit();
        let inactive_max_scroll_limit = term.inactive_grid.checkpoint_max_scroll_limit();
        validate_grid(&term.grid, active_max_scroll_limit)?;
        validate_grid(&term.inactive_grid, inactive_max_scroll_limit)?;
        if term
            .grid
            .total_lines()
            .checked_mul(term.grid.columns())
            .and_then(|active| {
                term.inactive_grid
                    .total_lines()
                    .checked_mul(term.inactive_grid.columns())
                    .and_then(|inactive| active.checked_add(inactive))
            })
            .is_none_or(|cells| cells > MAX_TERM_GRID_CELLS)
        {
            return Err(CheckpointError(
                "terminal checkpoint exceeds cell safety limit",
            ));
        }
        validate_term(term)?;

        let mut pools = BorrowedPools::default();
        register_grid(&mut pools, &term.grid)?;
        register_grid(&mut pools, &term.inactive_grid)?;

        Ok(Self {
            term,
            pools,
            active_max_scroll_limit,
            inactive_max_scroll_limit,
        })
    }
}

impl<T> Serialize for TermCheckpointRef<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let term = self.term;
        let mut state = serializer.serialize_struct("TermCheckpointV1", 21)?;
        state.serialize_field("schema", &2u32)?;
        state.serialize_field("core", CHECKPOINT_CORE)?;
        state.serialize_field("is_focused", &term.is_focused)?;
        state.serialize_field("vi_mode_cursor", &term.vi_mode_cursor.point)?;
        state.serialize_field("selection", &term.selection)?;
        state.serialize_field(
            "grid",
            &GridRef::new(&term.grid, self.active_max_scroll_limit, &self.pools),
        )?;
        state.serialize_field(
            "inactive_grid",
            &GridRef::new(
                &term.inactive_grid,
                self.inactive_max_scroll_limit,
                &self.pools,
            ),
        )?;
        state.serialize_field("extra_pool", &ExtraPoolRef(&self.pools))?;
        state.serialize_field("hyperlink_pool", &HyperlinkPoolRef(&self.pools))?;
        state.serialize_field("active_charset", &(term.active_charset as u8))?;
        state.serialize_field("tabs", &term.tabs.tabs)?;
        state.serialize_field("mode", &term.mode.bits())?;
        state.serialize_field(
            "scroll_region",
            &(term.scroll_region.start.0, term.scroll_region.end.0),
        )?;
        state.serialize_field("colors", &ColorsRef(&term.colors))?;
        state.serialize_field("cursor_style", &term.cursor_style.map(CursorStyleV1::from))?;
        state.serialize_field("title", &term.title.as_deref())?;
        state.serialize_field("title_stack", &TitleStackRef(&term.title_stack))?;
        state.serialize_field(
            "keyboard_mode_stack",
            &KeyboardStackRef(&term.keyboard_mode_stack),
        )?;
        state.serialize_field(
            "inactive_keyboard_mode_stack",
            &KeyboardStackRef(&term.inactive_keyboard_mode_stack),
        )?;
        state.serialize_field("config", &ConfigRef(&term.config))?;
        state.serialize_field("next_hyperlink_id", &term.next_hyperlink_id)?;
        state.end()
    }
}

#[derive(Default)]
struct BorrowedPools<'a> {
    extra_ids: HashMap<*const CellExtra, u32>,
    link_ids: HashMap<*const (), u32>,
    link_values: HashMap<&'a Hyperlink, u32>,
    extras: Vec<&'a CellExtra>,
    hyperlinks: Vec<&'a Hyperlink>,
    combining_chars: usize,
}

impl<'a> BorrowedPools<'a> {
    fn register_cell(&mut self, cell: &'a Cell) -> Result<(), CheckpointError> {
        let Some(extra) = cell.extra.as_deref() else {
            return Ok(());
        };
        let pointer = extra as *const CellExtra;
        if self.extra_ids.contains_key(&pointer) {
            return Ok(());
        }

        let (zerowidth, _, hyperlink) = extra.checkpoint_borrowed_parts();
        self.combining_chars = self
            .combining_chars
            .checked_add(zerowidth.len())
            .ok_or(CheckpointError("combining budget overflow"))?;
        if self.combining_chars > MAX_COMBINING_CHARS {
            return Err(CheckpointError(
                "terminal text/combining decode budget exceeded",
            ));
        }
        if let Some(link) = hyperlink {
            self.register_hyperlink(link)?;
        }
        let index = pool_index(self.extras.len())?;
        debug_assert_eq!(index as usize, self.extras.len());
        self.extras.push(extra);
        self.extra_ids.insert(pointer, index);
        Ok(())
    }

    fn register_hyperlink(&mut self, link: &'a Hyperlink) -> Result<u32, CheckpointError> {
        if link.id().len() > MAX_TEXT_BYTES || link.uri().len() > MAX_TEXT_BYTES {
            return Err(CheckpointError(
                "terminal text/combining decode budget exceeded",
            ));
        }
        let pointer = link.checkpoint_identity();
        if let Some(index) = self.link_ids.get(&pointer) {
            return Ok(*index);
        }
        if let Some(index) = self.link_values.get(&link) {
            self.link_ids.insert(pointer, *index);
            return Ok(*index);
        }
        let index = pool_index(self.hyperlinks.len())?;
        self.hyperlinks.push(link);
        self.link_ids.insert(pointer, index);
        self.link_values.insert(link, index);
        Ok(index)
    }

    fn extra_index(&self, cell: &Cell) -> Option<u32> {
        cell.extra
            .as_ref()
            .and_then(|extra| self.extra_ids.get(&Arc::as_ptr(extra)).copied())
    }

    fn hyperlink_index(&self, link: &Hyperlink) -> u32 {
        *self
            .link_ids
            .get(&link.checkpoint_identity())
            .expect("all borrowed pool links are registered before serialization")
    }
}

fn pool_index(len: usize) -> Result<u32, CheckpointError> {
    u32::try_from(len).map_err(|_| CheckpointError("checkpoint shared pool exceeds index limit"))
}

fn register_grid<'a>(
    pools: &mut BorrowedPools<'a>,
    grid: &'a Grid<Cell>,
) -> Result<(), CheckpointError> {
    for row in grid.checkpoint_rows() {
        for cell in row.initialized().iter().take(grid.columns()) {
            pools.register_cell(cell)?;
        }
    }
    pools.register_cell(&grid.cursor.template)?;
    pools.register_cell(&grid.saved_cursor.template)?;
    Ok(())
}

fn validate_grid(grid: &Grid<Cell>, max_scroll_limit: usize) -> Result<(), CheckpointError> {
    let columns = grid.columns();
    let lines = grid.screen_lines();
    if !(2..=MAX_GRID_COLUMNS).contains(&columns) || lines == 0 || lines > MAX_GRID_LINES {
        return Err(CheckpointError(
            "grid dimensions outside checkpoint safety limits",
        ));
    }
    let total_lines = grid.total_lines();
    if max_scroll_limit > MAX_SCROLLBACK
        || total_lines < lines
        || total_lines - lines > max_scroll_limit
        || grid.display_offset() > total_lines - lines
    {
        return Err(CheckpointError("invalid grid history bounds"));
    }
    if total_lines
        .checked_mul(columns)
        .is_none_or(|cells| cells > MAX_GRID_CELLS)
    {
        return Err(CheckpointError("grid checkpoint exceeds cell safety limit"));
    }
    for row in grid.checkpoint_rows() {
        if row.len() < columns || row.occupied() > columns {
            return Err(CheckpointError("invalid grid row shape"));
        }
    }
    validate_cursor(&grid.cursor, lines, columns)?;
    validate_cursor(&grid.saved_cursor, lines, columns)?;
    Ok(())
}

fn validate_cursor(
    cursor: &crate::grid::Cursor<Cell>,
    lines: usize,
    columns: usize,
) -> Result<(), CheckpointError> {
    if cursor.point.line.0 < 0
        || cursor.point.line.0 as usize >= lines
        || cursor.point.column.0 >= columns
    {
        return Err(CheckpointError("cursor outside viewport"));
    }
    Ok(())
}

fn validate_term<T>(term: &Term<T>) -> Result<(), CheckpointError> {
    if term
        .title
        .as_ref()
        .is_some_and(|title| title.len() > MAX_TEXT_BYTES)
        || term
            .title_stack
            .iter()
            .flatten()
            .any(|title| title.len() > MAX_TEXT_BYTES)
    {
        return Err(CheckpointError(
            "terminal text/combining decode budget exceeded",
        ));
    }
    let alternate_active = term.mode.contains(TermMode::ALT_SCREEN);
    let (normal, alternate) = if alternate_active {
        (&term.inactive_grid, &term.grid)
    } else {
        (&term.grid, &term.inactive_grid)
    };
    if normal.checkpoint_max_scroll_limit() != term.config.scrolling_history
        || alternate.checkpoint_max_scroll_limit() != 0
        || alternate.total_lines() != alternate.screen_lines()
    {
        return Err(CheckpointError(
            "terminal normal/alternate history profile mismatch",
        ));
    }
    if term.grid.columns() != term.inactive_grid.columns()
        || term.grid.screen_lines() != term.inactive_grid.screen_lines()
    {
        return Err(CheckpointError("terminal grids disagree on dimensions"));
    }
    if term.active_charset as u8 > 3 || term.tabs.tabs.len() != term.grid.columns() {
        return Err(CheckpointError("invalid charset or tabstop shape"));
    }
    if term.scroll_region.start.0 < 0
        || term.scroll_region.end.0 <= term.scroll_region.start.0
        || term.scroll_region.end.0 as usize > term.grid.screen_lines()
    {
        return Err(CheckpointError("invalid terminal scroll region"));
    }
    if term.mode.contains(TermMode::VI) {
        let oldest_line = -((term.grid.total_lines() - term.grid.screen_lines()) as i32);
        if term.vi_mode_cursor.point.line.0 < oldest_line
            || term.vi_mode_cursor.point.line.0 >= term.grid.screen_lines() as i32
            || term.vi_mode_cursor.point.column.0 >= term.grid.columns()
        {
            return Err(CheckpointError("vi cursor outside available grid"));
        }
    }
    if term.title_stack.len() > crate::term::TITLE_STACK_MAX_DEPTH
        || term.keyboard_mode_stack.len() > crate::term::KEYBOARD_MODE_STACK_MAX_DEPTH
        || term.inactive_keyboard_mode_stack.len() > crate::term::KEYBOARD_MODE_STACK_MAX_DEPTH
    {
        return Err(CheckpointError("terminal stack exceeds safety limit"));
    }
    if term
        .keyboard_mode_stack
        .iter()
        .chain(&term.inactive_keyboard_mode_stack)
        .any(|mode| KeyboardModes::from_bits(mode.bits()).is_none())
    {
        return Err(CheckpointError("invalid keyboard mode bits"));
    }
    if term.config.scrolling_history > MAX_SCROLLBACK
        || term.config.semantic_escape_chars.len() > MAX_TEXT_BYTES
        || term.next_hyperlink_id == u64::MAX
    {
        return Err(CheckpointError(
            "terminal configuration outside checkpoint safety limits",
        ));
    }
    Ok(())
}

struct GridRef<'a> {
    grid: &'a Grid<Cell>,
    max_scroll_limit: usize,
    pools: &'a BorrowedPools<'a>,
}

impl<'a> GridRef<'a> {
    fn new(grid: &'a Grid<Cell>, max_scroll_limit: usize, pools: &'a BorrowedPools<'a>) -> Self {
        Self {
            grid,
            max_scroll_limit,
            pools,
        }
    }
}

impl Serialize for GridRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("GridCheckpointV1", 7)?;
        state.serialize_field("rows", &RowsRef(self.grid, self.pools))?;
        state.serialize_field("columns", &self.grid.columns())?;
        state.serialize_field("lines", &self.grid.screen_lines())?;
        state.serialize_field("display_offset", &self.grid.display_offset())?;
        state.serialize_field("max_scroll_limit", &self.max_scroll_limit)?;
        state.serialize_field("cursor", &CursorRef(&self.grid.cursor, self.pools))?;
        state.serialize_field(
            "saved_cursor",
            &CursorRef(&self.grid.saved_cursor, self.pools),
        )?;
        state.end()
    }
}

struct RowsRef<'a>(&'a Grid<Cell>, &'a BorrowedPools<'a>);
impl Serialize for RowsRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let grid = self.0;
        let count = grid.total_lines();
        let mut sequence = serializer.serialize_seq(Some(count))?;
        for row in grid.checkpoint_rows() {
            sequence.serialize_element(&RowRef(row, grid.columns(), self.1))?;
        }
        sequence.end()
    }
}

struct RowRef<'a>(&'a crate::grid::Row<Cell>, usize, &'a BorrowedPools<'a>);
impl Serialize for RowRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("RowCheckpointV2", 2)?;
        state.serialize_field("cells", &CellsRef(self.0, self.1, self.2))?;
        state.serialize_field("occupied", &self.0.occupied())?;
        state.end()
    }
}

struct CellsRef<'a>(&'a crate::grid::Row<Cell>, usize, &'a BorrowedPools<'a>);
impl Serialize for CellsRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.1))?;
        for cell in self.0.initialized().iter().take(self.1) {
            sequence.serialize_element(&CellRef(cell, self.2))?;
        }
        sequence.end()
    }
}

struct CellRef<'a>(&'a Cell, &'a BorrowedPools<'a>);
impl Serialize for CellRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("CellCheckpointV2", 5)?;
        state.serialize_field("c", &self.0.c)?;
        state.serialize_field("fg", &self.0.fg)?;
        state.serialize_field("bg", &self.0.bg)?;
        state.serialize_field("flags", &self.0.flags)?;
        state.serialize_field("extra", &self.1.extra_index(self.0))?;
        state.end()
    }
}

struct CursorRef<'a>(&'a crate::grid::Cursor<Cell>, &'a BorrowedPools<'a>);
impl Serialize for CursorRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let cursor = self.0;
        let charsets = [
            CharsetIndex::G0,
            CharsetIndex::G1,
            CharsetIndex::G2,
            CharsetIndex::G3,
        ]
        .map(|index| CharsetV1::from(cursor.charsets[index]));
        let mut state = serializer.serialize_struct("CursorCheckpointV1", 4)?;
        state.serialize_field("point", &cursor.point)?;
        state.serialize_field("template", &CellRef(&cursor.template, self.1))?;
        state.serialize_field("charsets", &charsets)?;
        state.serialize_field("input_needs_wrap", &cursor.input_needs_wrap)?;
        state.end()
    }
}

struct ExtraPoolRef<'a>(&'a BorrowedPools<'a>);
impl Serialize for ExtraPoolRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.extras.len()))?;
        for extra in &self.0.extras {
            sequence.serialize_element(&ExtraRef(extra, self.0))?;
        }
        sequence.end()
    }
}

struct ExtraRef<'a>(&'a CellExtra, &'a BorrowedPools<'a>);
impl Serialize for ExtraRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (zerowidth, underline_color, hyperlink) = self.0.checkpoint_borrowed_parts();
        let hyperlink = hyperlink.map(|link| self.1.hyperlink_index(link));
        let mut state = serializer.serialize_struct("ExtraCheckpointV2", 3)?;
        state.serialize_field("zerowidth", zerowidth)?;
        state.serialize_field("underline_color", &underline_color)?;
        state.serialize_field("hyperlink", &hyperlink)?;
        state.end()
    }
}

struct HyperlinkPoolRef<'a>(&'a BorrowedPools<'a>);
impl Serialize for HyperlinkPoolRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.hyperlinks.len()))?;
        for link in &self.0.hyperlinks {
            sequence.serialize_element(&HyperlinkRef(link))?;
        }
        sequence.end()
    }
}

struct HyperlinkRef<'a>(&'a Hyperlink);
impl Serialize for HyperlinkRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("HyperlinkCheckpointV2", 2)?;
        state.serialize_field("id", self.0.id())?;
        state.serialize_field("uri", self.0.uri())?;
        state.end()
    }
}

struct ColorsRef<'a>(&'a color::Colors);
impl Serialize for ColorsRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(color::COUNT))?;
        for index in 0..color::COUNT {
            sequence.serialize_element(&self.0[index].map(|rgb| [rgb.r, rgb.g, rgb.b]))?;
        }
        sequence.end()
    }
}

struct TitleStackRef<'a>(&'a [Option<String>]);
impl Serialize for TitleStackRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for title in self.0 {
            sequence.serialize_element(&title.as_deref())?;
        }
        sequence.end()
    }
}

struct KeyboardStackRef<'a>(&'a [KeyboardModes]);
impl Serialize for KeyboardStackRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for mode in self.0 {
            sequence.serialize_element(&mode.bits())?;
        }
        sequence.end()
    }
}

struct ConfigRef<'a>(&'a Config);
impl Serialize for ConfigRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let config = self.0;
        let default_cursor_style = CursorStyleV1::from(config.default_cursor_style);
        let vi_mode_cursor_style = config.vi_mode_cursor_style.map(CursorStyleV1::from);
        let mut state = serializer.serialize_struct("ConfigCheckpointV1", 6)?;
        state.serialize_field("scrolling_history", &config.scrolling_history)?;
        state.serialize_field("default_cursor_style", &default_cursor_style)?;
        state.serialize_field("vi_mode_cursor_style", &vi_mode_cursor_style)?;
        state.serialize_field(
            "semantic_escape_chars",
            config.semantic_escape_chars.as_str(),
        )?;
        state.serialize_field("kitty_keyboard", &config.kitty_keyboard)?;
        state.serialize_field("osc52", &config.osc52)?;
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        event::{Event, EventListener},
        index::{Column, Line},
        selection::{Selection, SelectionType},
        term::test::TermSize,
        vte::ansi::{Processor, StdSyncHandler},
    };
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Listener(Arc<Mutex<usize>>);
    impl EventListener for Listener {
        fn send_event(&self, _: Event) {
            *self.0.lock().unwrap() += 1;
        }
    }

    fn terminal() -> Term<Listener> {
        Term::new(
            Config::default(),
            &TermSize::new(40, 12),
            Listener::default(),
        )
    }

    fn feed(term: &mut Term<Listener>, bytes: &[u8]) {
        Processor::<StdSyncHandler>::new().advance(term, bytes);
    }

    fn assert_borrowed_bytes_match(term: &Term<Listener>) {
        let before = postcard::to_allocvec(&term.export_checkpoint()).unwrap();
        let reference = TermCheckpointRef::new(term).unwrap();
        let borrowed = postcard::to_allocvec(&reference).unwrap();
        assert_eq!(borrowed, before);
        let exported = postcard::to_allocvec(&term.export_checkpoint()).unwrap();
        assert_eq!(
            exported, before,
            "borrowed serialization mutated source state"
        );
    }

    fn assert_validation_agrees(term: &Term<Listener>) {
        let owned = term.export_checkpoint().validate().map(|_| ());
        let borrowed = TermCheckpointRef::new(term).map(|_| ());
        assert_eq!(borrowed, owned);
    }

    #[test]
    fn borrowed_serializer_matches_owned_schema_across_terminal_state_shapes() {
        let mut term = terminal();
        assert_borrowed_bytes_match(&term);
        for i in 0..50 {
            feed(
                &mut term,
                format!("row {i} Русский e\u{301} 界\r\n").as_bytes(),
            );
        }
        feed(
            &mut term,
            b"\x1b]0;title\x07\x1b[22;0t\x1b]0;top\x07\x1b[>3u\x1b[38;2;12;34;56m\x1b[?1049halt\x1b[2Jalternate",
        );
        term.selection = Some(Selection::new(
            SelectionType::Block,
            crate::index::Point::new(Line(0), Column(0)),
            crate::index::Side::Left,
        ));
        assert_borrowed_bytes_match(&term);
        term.scroll_display(crate::grid::Scroll::Delta(4));
        assert_borrowed_bytes_match(&term);
        term.resize(TermSize::new(27, 9));
        assert_borrowed_bytes_match(&term);
        feed(
            &mut term,
            b"\x1b]8;id;https://example.test/shared\x07A\x1b[1;1HB\x1b]8;id;https://example.test/shared\x07C\x1b]8;;\x07",
        );
        assert_borrowed_bytes_match(&term);
    }

    #[test]
    fn borrowed_constructor_rejects_the_same_invalid_terminal_states() {
        let mut term = terminal();
        assert_validation_agrees(&term);
        term.grid
            .update_history(term.config.scrolling_history.saturating_sub(1));
        assert_validation_agrees(&term);

        let mut term = terminal();
        term.next_hyperlink_id = u64::MAX;
        assert_validation_agrees(&term);

        let mut term = terminal();
        term.title = Some("x".repeat(MAX_TEXT_BYTES + 1));
        assert_validation_agrees(&term);

        let mut term = terminal();
        term.grid.cursor.point.line = Line(term.grid.screen_lines() as i32);
        assert_validation_agrees(&term);
    }

    #[test]
    fn borrowed_pool_order_matches_each_grids_rows_and_cursors() {
        let mut term = terminal();
        term.grid[Line(0)][Column(0)].set_hyperlink(Some(Hyperlink::new(
            Some("active-row-zero"),
            "https://example.test/active-row-zero".into(),
        )));
        term.grid[Line(1)][Column(0)].set_hyperlink(Some(Hyperlink::new(
            Some("active-row-one"),
            "https://example.test/active-row-one".into(),
        )));
        term.grid.cursor.template.set_hyperlink(Some(Hyperlink::new(
            Some("active-cursor"),
            "https://example.test/active-cursor".into(),
        )));
        term.grid
            .saved_cursor
            .template
            .set_hyperlink(Some(Hyperlink::new(
                Some("active-saved"),
                "https://example.test/active-saved".into(),
            )));
        term.inactive_grid[Line(0)][Column(0)].set_hyperlink(Some(Hyperlink::new(
            Some("inactive-row"),
            "https://example.test/inactive-row".into(),
        )));
        term.inactive_grid
            .cursor
            .template
            .set_hyperlink(Some(Hyperlink::new(
                Some("inactive-cursor"),
                "https://example.test/inactive-cursor".into(),
            )));
        assert_borrowed_bytes_match(&term);
    }

    #[test]
    fn borrowed_rows_ignore_inactive_grown_cell_capacity() {
        let mut term = terminal();
        let columns = term.grid.columns();
        term.grid[Line(2)].grow(columns + 4);
        assert_borrowed_bytes_match(&term);
    }

    #[test]
    fn borrowed_serializer_matches_owned_schema_for_upstream_reference_corpus() {
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct RefConfig {
            history_size: u32,
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ref");
        let mut paths: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect();
        paths.sort();
        let mut tested = 0;
        for path in paths {
            let recording = std::fs::read(path.join("alacritty.recording")).unwrap();
            let size: TermSize =
                serde_json::from_slice(&std::fs::read(path.join("size.json")).unwrap()).unwrap();
            let config: RefConfig =
                serde_json::from_slice(&std::fs::read(path.join("config.json")).unwrap()).unwrap();
            let config = Config {
                scrolling_history: config.history_size as usize,
                ..Config::default()
            };
            for cut in [
                recording.len() / 3,
                recording.len() * 2 / 3,
                recording.len(),
            ] {
                let mut term = Term::new(config.clone(), &size, Listener::default());
                feed(&mut term, &recording[..cut]);
                assert_borrowed_bytes_match(&term);
            }
            tested += 1;
        }
        assert_eq!(
            tested, 45,
            "upstream corpus changed: update pinned evidence"
        );
    }
}
