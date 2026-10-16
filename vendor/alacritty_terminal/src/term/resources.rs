//! Parser-derived resource census for bounded output admission.
//!
//! This reports parser callback counts and selected payload sizes. It is not a
//! heap measurement or a memory budget. Census uses the pinned VTE processor
//! on a clone, so it follows the same ANSI, UTF-8, REP and synchronized-update
//! behavior as normal processing without cloning or mutating a terminal grid.

use unicode_width::UnicodeWidthChar;
use vte::ansi::{Handler, Hyperlink, Processor, Timeout};

use crate::grid::{Grid, Row};
use crate::term::Term;
use crate::term::cell::{Cell, CellExtra, Hyperlink as TermHyperlink};
use std::collections::HashSet;
use std::mem::size_of;

/// Saturating counts observed while advancing one cloned processor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceCensus {
    /// Total `Handler::input` callbacks, including REP repetitions.
    pub input_chars: u64,
    /// Input callbacks whose pinned Unicode width is zero.
    pub zero_width_chars: u64,
    /// UTF-8 bytes delivered through title callbacks.
    pub title_bytes: u64,
    /// UTF-8 bytes delivered in hyperlink identifiers.
    pub hyperlink_id_bytes: u64,
    /// UTF-8 bytes delivered in hyperlink URIs.
    pub hyperlink_uri_bytes: u64,
    /// Title-stack push callbacks.
    pub title_pushes: u64,
    /// True if any counter saturated at `u64::MAX`.
    pub saturated: bool,
}

impl ResourceCensus {
    fn add_counter(counter: &mut u64, saturated: &mut bool, amount: u64) {
        match counter.checked_add(amount) {
            Some(value) => *counter = value,
            None => {
                *counter = u64::MAX;
                *saturated = true;
            }
        }
    }

    fn input_repeated(&mut self, c: char, count: u16) {
        let count = u64::from(count);
        Self::add_counter(&mut self.input_chars, &mut self.saturated, count);
        if c.width() == Some(0) {
            Self::add_counter(&mut self.zero_width_chars, &mut self.saturated, count);
        }
    }

    fn add_bytes(counter: &mut u64, saturated: &mut bool, amount: usize) {
        match u64::try_from(amount) {
            Ok(amount) => Self::add_counter(counter, saturated, amount),
            Err(_) => {
                *counter = u64::MAX;
                *saturated = true;
            }
        }
    }
}

impl Handler for ResourceCensus {
    fn input(&mut self, c: char) {
        self.input_repeated(c, 1);
    }

    fn input_repeated(&mut self, c: char, count: u16) {
        ResourceCensus::input_repeated(self, c, count);
    }

    fn set_title(&mut self, title: Option<String>) {
        if let Some(title) = title {
            Self::add_bytes(&mut self.title_bytes, &mut self.saturated, title.len());
        }
    }

    fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        if let Some(hyperlink) = hyperlink {
            if let Some(id) = hyperlink.id {
                Self::add_bytes(&mut self.hyperlink_id_bytes, &mut self.saturated, id.len());
            }
            Self::add_bytes(
                &mut self.hyperlink_uri_bytes,
                &mut self.saturated,
                hyperlink.uri.len(),
            );
        }
    }

    fn push_title(&mut self) {
        Self::add_counter(&mut self.title_pushes, &mut self.saturated, 1);
    }
}

/// Census callbacks produced by `bytes` from a cloned processor state.
///
/// Cloning can allocate for already-buffered parser state (the synchronized
/// buffer is capped at 2 MiB and OSC capture at 64 KiB). This is not a zero-
/// allocation dry run; it only avoids duplicating the terminal model.
pub fn census_output<T: Timeout + Clone>(processor: &Processor<T>, bytes: &[u8]) -> ResourceCensus {
    let mut processor = processor.clone();
    let mut census = ResourceCensus::default();
    processor.advance(&mut census, bytes);
    census
}

/// Census callbacks flushed when a synchronized update times out.
///
/// The original processor, including its pending buffer and timeout, is left
/// unchanged.
pub fn census_sync_timeout<T: Timeout + Clone>(processor: &Processor<T>) -> ResourceCensus {
    let mut processor = processor.clone();
    let mut census = ResourceCensus::default();
    processor.stop_sync(&mut census);
    census
}

/// Work limits for a persistent terminal allocation census.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CensusLimits {
    /// Maximum initialized cell values inspected, including cursor templates.
    pub max_cell_visits: usize,
    /// Maximum distinct `CellExtra` and hyperlink Arc allocations tracked.
    pub max_unique_allocations: usize,
}

/// Capacity-based estimate of persistent memory owned by a terminal.
///
/// This payload-capacity estimate is not heap/RSS and is not an upper bound on
/// terminal or process memory. It covers the storage documented by the fields
/// below, but excludes allocator metadata, alignment, fragmentation, generic
/// event listeners, history observers, parser state, other model fields, and
/// transient allocations. It includes Vec capacities, inline Term size, and a
/// two-`usize` Arc header estimate for each deduplicated Arc payload; it makes
/// no claim about physical allocator overhead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AllocationCensus {
    pub cell_visits: usize,
    pub unique_cell_extras: usize,
    pub unique_hyperlinks: usize,
    pub row_slots_capacity: usize,
    pub cell_slots_capacity: usize,
    pub zero_width_chars_capacity: usize,
    pub hyperlink_id_bytes_capacity: usize,
    pub hyperlink_uri_bytes_capacity: usize,
    pub title_stack_slots_capacity: usize,
    pub tab_stop_capacity: usize,
    pub keyboard_mode_stack_capacity: usize,
    pub inactive_keyboard_mode_stack_capacity: usize,
    pub damage_line_capacity: usize,
    pub estimated_persistent_owned_bytes: usize,
    pub incomplete: bool,
}

struct CensusBuilder {
    result: AllocationCensus,
    limits: CensusLimits,
    extras: HashSet<usize>,
    hyperlinks: HashSet<usize>,
}

impl CensusBuilder {
    fn add(target: &mut usize, value: usize, incomplete: &mut bool) {
        if let Some(sum) = target.checked_add(value) {
            *target = sum;
        } else {
            *target = usize::MAX;
            *incomplete = true;
        }
    }

    fn bytes(&mut self, value: usize) {
        Self::add(
            &mut self.result.estimated_persistent_owned_bytes,
            value,
            &mut self.result.incomplete,
        );
    }

    fn product(&mut self, left: usize, right: usize) -> usize {
        match left.checked_mul(right) {
            Some(value) => value,
            None => {
                self.result.incomplete = true;
                usize::MAX
            }
        }
    }

    fn identity(
        result: &mut AllocationCensus,
        limits: CensusLimits,
        set: &mut HashSet<usize>,
        identity: usize,
    ) -> bool {
        if set.contains(&identity) {
            return false;
        }
        let Some(unique) = result
            .unique_cell_extras
            .checked_add(result.unique_hyperlinks)
        else {
            result.incomplete = true;
            return false;
        };
        if unique >= limits.max_unique_allocations || set.try_reserve(1).is_err() {
            result.incomplete = true;
            return false;
        }
        set.insert(identity)
    }

    fn hyperlink(&mut self, link: &TermHyperlink) {
        if !Self::identity(
            &mut self.result,
            self.limits,
            &mut self.hyperlinks,
            link.allocation_identity() as usize,
        ) {
            return;
        }
        Self::add(
            &mut self.result.unique_hyperlinks,
            1,
            &mut self.result.incomplete,
        );
        let (id, uri) = link.allocation_capacities();
        Self::add(
            &mut self.result.hyperlink_id_bytes_capacity,
            id,
            &mut self.result.incomplete,
        );
        Self::add(
            &mut self.result.hyperlink_uri_bytes_capacity,
            uri,
            &mut self.result.incomplete,
        );
        let arc_header = self.product(size_of::<usize>(), 2);
        self.bytes(arc_header);
        // HyperlinkInner is two String handles; their capacities account for payload.
        self.bytes(size_of::<String>() * 2);
        self.bytes(id);
        self.bytes(uri);
    }

    fn cell(&mut self, cell: &Cell) {
        if self.result.cell_visits >= self.limits.max_cell_visits {
            self.result.incomplete = true;
            return;
        }
        self.result.cell_visits += 1;
        let Some(extra) = cell.extra.as_deref() else {
            return;
        };
        let (zero_width_capacity, hyperlink) = extra.allocation_parts();
        if Self::identity(
            &mut self.result,
            self.limits,
            &mut self.extras,
            extra.allocation_identity() as usize,
        ) {
            Self::add(
                &mut self.result.unique_cell_extras,
                1,
                &mut self.result.incomplete,
            );
            Self::add(
                &mut self.result.zero_width_chars_capacity,
                zero_width_capacity,
                &mut self.result.incomplete,
            );
            let arc_header = self.product(size_of::<usize>(), 2);
            self.bytes(arc_header);
            self.bytes(size_of::<CellExtra>());
            let mark_bytes = self.product(zero_width_capacity, size_of::<char>());
            self.bytes(mark_bytes);
        }
        if let Some(link) = hyperlink {
            self.hyperlink(link);
        }
    }

    fn grid(&mut self, grid: &Grid<Cell>) {
        let (rows, row_capacity) = grid.allocation_rows();
        Self::add(
            &mut self.result.row_slots_capacity,
            row_capacity,
            &mut self.result.incomplete,
        );
        let row_bytes = self.product(row_capacity, size_of::<Row<Cell>>());
        self.bytes(row_bytes);
        for row in rows {
            if self.result.cell_visits >= self.limits.max_cell_visits {
                self.result.incomplete = true;
                return;
            }
            let capacity = row.allocation_capacity();
            Self::add(
                &mut self.result.cell_slots_capacity,
                capacity,
                &mut self.result.incomplete,
            );
            let cell_bytes = self.product(capacity, size_of::<Cell>());
            self.bytes(cell_bytes);
            for cell in row.initialized() {
                self.cell(cell);
                if self.result.incomplete {
                    return;
                }
            }
        }
        self.cell(&grid.cursor.template);
        if !self.result.incomplete {
            self.cell(&grid.saved_cursor.template);
        }
    }
}

/// Census selected persistent allocation capacities without cloning or mutating `term`.
///
/// Traversal stops as soon as either caller-provided work bound is reached.
/// `incomplete` means this census did not finish its selected-storage scan;
/// a complete result still excludes the storage listed in [`AllocationCensus`].
pub fn census_terminal_allocations<T>(term: &Term<T>, limits: CensusLimits) -> AllocationCensus {
    let mut builder = CensusBuilder {
        result: AllocationCensus::default(),
        limits,
        extras: HashSet::new(),
        hyperlinks: HashSet::new(),
    };
    builder.bytes(size_of::<Term<T>>());
    builder.grid(&term.grid);
    if !builder.result.incomplete {
        builder.grid(&term.inactive_grid);
    }
    if !builder.result.incomplete {
        if let Some(title) = &term.title {
            builder.bytes(title.capacity());
        }
        builder.result.title_stack_slots_capacity = term.title_stack.capacity();
        let title_stack_bytes =
            builder.product(term.title_stack.capacity(), size_of::<Option<String>>());
        builder.bytes(title_stack_bytes);
        for title in term.title_stack.iter().flatten() {
            builder.bytes(title.capacity());
        }
        builder.bytes(term.config.semantic_escape_chars.capacity());
        builder.result.tab_stop_capacity = term.tabs.tabs.capacity();
        builder.bytes(term.tabs.tabs.capacity()); // Vec<bool> stores one byte per value.
        builder.result.keyboard_mode_stack_capacity = term.keyboard_mode_stack.capacity();
        let keyboard_stack_bytes = builder.product(
            term.keyboard_mode_stack.capacity(),
            size_of::<crate::vte::ansi::KeyboardModes>(),
        );
        builder.bytes(keyboard_stack_bytes);
        builder.result.inactive_keyboard_mode_stack_capacity =
            term.inactive_keyboard_mode_stack.capacity();
        let inactive_keyboard_stack_bytes = builder.product(
            term.inactive_keyboard_mode_stack.capacity(),
            size_of::<crate::vte::ansi::KeyboardModes>(),
        );
        builder.bytes(inactive_keyboard_stack_bytes);
        builder.result.damage_line_capacity = term.damage.lines.capacity();
        let damage_line_bytes = builder.product(
            term.damage.lines.capacity(),
            size_of::<crate::term::LineDamageBounds>(),
        );
        builder.bytes(damage_line_bytes);
    }
    builder.result
}

#[cfg(all(test, feature = "serde"))]
mod tests {
    use super::*;
    use crate::event::VoidListener;
    use crate::index::{Column, Line, Point};
    use crate::term::{Config, Dimensions};
    use serde::{Deserialize, Serialize};
    use std::time::Duration;
    use vte::ansi::ProcessorCheckpointV1;

    struct Size(usize, usize);

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    fn terminal() -> Term<VoidListener> {
        Term::new(Config::default(), &Size(4, 4), VoidListener)
    }

    fn checkpoint_bytes(term: &Term<VoidListener>) -> Vec<u8> {
        serde_json::to_vec(&term.export_checkpoint()).unwrap()
    }

    fn census(term: &Term<VoidListener>) -> AllocationCensus {
        census_terminal_allocations(
            term,
            CensusLimits {
                max_cell_visits: 1000,
                max_unique_allocations: 100,
            },
        )
    }

    #[derive(Clone, Default, Serialize, Deserialize)]
    struct TestTimeout(bool);

    impl Timeout for TestTimeout {
        fn set_timeout(&mut self, _: Duration) {
            self.0 = true;
        }

        fn clear_timeout(&mut self) {
            self.0 = false;
        }

        fn pending_timeout(&self) -> bool {
            self.0
        }
    }

    fn state(processor: &Processor<TestTimeout>) -> Vec<u8> {
        let checkpoint: ProcessorCheckpointV1<TestTimeout> = processor.export_checkpoint();
        postcard::to_allocvec(&checkpoint).unwrap()
    }

    #[test]
    fn census_handles_partial_utf8_without_mutating_original_processor() {
        let mut processor = Processor::<TestTimeout>::new();
        let mut ignore = ResourceCensus::default();
        processor.advance(&mut ignore, &[0xc3]);
        let before = state(&processor);

        let census = census_output(&processor, &[0xa9, b'A']);
        assert_eq!(census.input_chars, 2);
        assert_eq!(census.zero_width_chars, 0);
        assert_eq!(state(&processor), before);
    }

    #[test]
    fn census_aggregates_maximum_rep_count() {
        let mut processor = Processor::<TestTimeout>::new();
        let mut ignore = ResourceCensus::default();
        processor.advance(&mut ignore, b"A");
        let before = state(&processor);

        let census = census_output(&processor, b"\x1b[65535b");
        assert_eq!(census.input_chars, 65_535);
        assert_eq!(census.zero_width_chars, 0);
        assert_eq!(state(&processor), before);
    }

    #[test]
    fn sync_timeout_census_counts_buffered_content_without_mutating_it() {
        let mut processor = Processor::<TestTimeout>::new();
        let mut ignore = ResourceCensus::default();
        processor.advance(&mut ignore, "\x1b[?2026hA\u{301}".as_bytes());
        assert!(processor.sync_timeout().pending_timeout());
        let before = state(&processor);

        let census = census_sync_timeout(&processor);
        assert_eq!(census.input_chars, 2);
        assert_eq!(census.zero_width_chars, 1);
        assert_eq!(state(&processor), before);
    }

    #[test]
    fn census_reports_title_hyperlink_and_title_stack_payloads() {
        let processor = Processor::<TestTimeout>::new();
        let census = census_output(
            &processor,
            b"\x1b]2;hello\x07\x1b]8;id=xy;https://e.test\x07\x1b[22t",
        );
        assert_eq!(census.title_bytes, 5);
        assert_eq!(census.hyperlink_id_bytes, 2);
        assert_eq!(census.hyperlink_uri_bytes, "https://e.test".len() as u64);
        assert_eq!(census.title_pushes, 1);
    }

    #[test]
    fn census_counters_saturate_instead_of_wrapping() {
        let mut census = ResourceCensus {
            input_chars: u64::MAX - 1,
            ..ResourceCensus::default()
        };
        census.input_repeated('A', 2);
        assert_eq!(census.input_chars, u64::MAX);
        assert!(census.saturated);
    }

    #[test]
    fn allocation_census_deduplicates_shared_extras_and_hyperlink_arcs() {
        let mut term = terminal();
        let baseline = census(&term);
        let mut cell = Cell::default();
        cell.push_zerowidth('\u{301}');
        cell.set_hyperlink(Some(TermHyperlink::new(Some("shared"), "https://x".into())));
        term.grid[Point::new(Line(0), Column(0))] = cell.clone();
        term.grid[Point::new(Line(0), Column(1))] = cell.clone();
        term.inactive_grid[Point::new(Line(0), Column(0))] = cell;
        let mut distinct = Cell::default();
        distinct.push_zerowidth('\u{301}');
        distinct.set_hyperlink(Some(TermHyperlink::new(Some("shared"), "https://x".into())));
        term.inactive_grid[Point::new(Line(0), Column(1))] = distinct;

        let result = census(&term);
        assert!(!result.incomplete);
        assert_eq!(result.unique_cell_extras, baseline.unique_cell_extras + 2);
        assert_eq!(result.unique_hyperlinks, baseline.unique_hyperlinks + 2);
        assert!(result.zero_width_chars_capacity >= baseline.zero_width_chars_capacity + 2);
        assert_eq!(
            result.hyperlink_id_bytes_capacity,
            baseline.hyperlink_id_bytes_capacity + 2 * "shared".len()
        );
        assert_eq!(
            result.hyperlink_uri_bytes_capacity,
            baseline.hyperlink_uri_bytes_capacity + 2 * "https://x".len()
        );
    }

    #[test]
    fn allocation_census_counts_cow_extras_inactive_cache_and_titles() {
        let mut term = terminal();
        let mut cell = Cell::default();
        cell.push_zerowidth('\u{301}');
        term.grid[Point::new(Line(0), Column(0))] = cell.clone();
        term.grid[Point::new(Line(0), Column(1))] = cell;
        // Force copy-on-write so two distinct extra allocations survive.
        term.grid[Point::new(Line(0), Column(1))].push_zerowidth('\u{302}');
        // Shrinking retains the extra raw rows as the grid's inactive cache.
        term.grid.resize(false, 2, 4);
        term.title = Some(String::from("window"));
        term.title_stack = vec![Some(String::from("older")), None];

        let result = census(&term);
        assert!(!result.incomplete);
        assert!(result.row_slots_capacity >= 4);
        assert!(
            result.cell_slots_capacity >= 32,
            "must include cached rows and both screens"
        );
        assert!(result.unique_cell_extras >= 2);
        assert!(result.title_stack_slots_capacity >= 2);
        assert!(result.estimated_persistent_owned_bytes > 0);
    }

    #[test]
    fn allocation_census_fails_closed_at_work_limit_without_mutating_checkpoint() {
        let mut term = terminal();
        let mut cell = Cell::default();
        cell.push_zerowidth('\u{301}');
        term.grid[Point::new(Line(0), Column(0))] = cell;
        let before = checkpoint_bytes(&term);

        let result = census_terminal_allocations(
            &term,
            CensusLimits {
                max_cell_visits: 1,
                max_unique_allocations: 10,
            },
        );
        assert!(result.incomplete);
        assert_eq!(result.cell_visits, 1);
        assert_eq!(checkpoint_bytes(&term), before);
    }

    #[test]
    fn allocation_census_includes_all_four_cursor_templates() {
        let mut term = terminal();
        let baseline = census(&term);
        for (grid, mark) in [
            (&mut term.grid, '\u{301}'),
            (&mut term.inactive_grid, '\u{302}'),
        ] {
            let mut current = Cell::default();
            current.push_zerowidth(mark);
            grid.cursor.template = current;
            let mut saved = Cell::default();
            saved.push_zerowidth(char::from_u32(mark as u32 + 1).unwrap());
            grid.saved_cursor.template = saved;
        }
        let result = census(&term);
        assert_eq!(result.unique_cell_extras, baseline.unique_cell_extras + 4);
        assert!(result.zero_width_chars_capacity >= baseline.zero_width_chars_capacity + 4);
    }

    #[test]
    fn allocation_census_accounts_for_terminal_metadata_vec_capacities() {
        let mut term = terminal();
        let baseline = census(&term);
        term.tabs.tabs = Vec::with_capacity(128);
        term.keyboard_mode_stack = Vec::with_capacity(129);
        term.inactive_keyboard_mode_stack = Vec::with_capacity(130);
        term.damage.lines = Vec::with_capacity(131);

        let result = census(&term);
        assert_eq!(result.tab_stop_capacity, term.tabs.tabs.capacity());
        assert_eq!(
            result.keyboard_mode_stack_capacity,
            term.keyboard_mode_stack.capacity()
        );
        assert_eq!(
            result.inactive_keyboard_mode_stack_capacity,
            term.inactive_keyboard_mode_stack.capacity()
        );
        assert_eq!(result.damage_line_capacity, term.damage.lines.capacity());

        let expected_delta = (result.tab_stop_capacity - baseline.tab_stop_capacity)
            + (result.keyboard_mode_stack_capacity - baseline.keyboard_mode_stack_capacity)
                * size_of::<crate::vte::ansi::KeyboardModes>()
            + (result.inactive_keyboard_mode_stack_capacity
                - baseline.inactive_keyboard_mode_stack_capacity)
                * size_of::<crate::vte::ansi::KeyboardModes>()
            + (result.damage_line_capacity - baseline.damage_line_capacity)
                * size_of::<crate::term::LineDamageBounds>();
        assert_eq!(
            result.estimated_persistent_owned_bytes - baseline.estimated_persistent_owned_bytes,
            expected_delta
        );
    }
}
