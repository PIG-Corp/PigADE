//! Parser-derived resource census inputs for future whole-event admission.
//!
//! This reports parser callback counts and selected payload sizes. It is not a
//! heap measurement or a memory budget. Census uses the pinned VTE processor
//! on a clone, so it follows the same ANSI, UTF-8, REP and synchronized-update
//! behavior as normal processing without cloning or mutating a terminal grid.
//! Grid driver counters describe requested callbacks, not effective work after
//! terminal geometry/mode clamps. They do not include host resize/reflow or
//! prove a reservation bound.

use unicode_width::UnicodeWidthChar;
use vte::ansi::{Handler, Hyperlink, Processor, Timeout};

use crate::grid::{Dimensions, Grid, Row};
use crate::term::Term;
use crate::term::cell::{Cell, CellExtra, Hyperlink as TermHyperlink};
use std::collections::HashSet;
use std::mem::size_of;

/// Saturating callback and payload counts observed while advancing one cloned
/// processor. Query counters report parser callbacks, not response sizes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceCensus {
    /// Total `Handler::input` callbacks, including REP repetitions.
    pub input_chars: u64,
    /// Input callbacks whose pinned Unicode width is zero.
    pub zero_width_chars: u64,
    /// Number of title-set callbacks, including reset-title callbacks.
    pub title_set_callbacks: u64,
    /// UTF-8 bytes delivered through title callbacks.
    pub title_bytes: u64,
    /// Largest UTF-8 title payload delivered by one set-title callback.
    pub max_title_bytes: u64,
    /// Number of title-stack pop callbacks.
    pub title_pops: u64,
    /// Number of hyperlink callbacks, including close-link callbacks.
    pub hyperlink_updates: u64,
    /// UTF-8 bytes delivered in hyperlink identifiers.
    pub hyperlink_id_bytes: u64,
    /// Largest UTF-8 identifier delivered by one hyperlink callback.
    pub max_hyperlink_id_bytes: u64,
    /// UTF-8 bytes delivered in hyperlink URIs.
    pub hyperlink_uri_bytes: u64,
    /// Largest UTF-8 URI delivered by one hyperlink callback.
    pub max_hyperlink_uri_bytes: u64,
    /// Title-stack push callbacks.
    pub title_pushes: u64,
    /// Device-attribute query callbacks.
    pub identify_terminal_queries: u64,
    /// Device-status query callbacks.
    pub device_status_queries: u64,
    /// DECRPM query callbacks.
    pub report_mode_queries: u64,
    /// DECRPM private-mode query callbacks.
    pub report_private_mode_queries: u64,
    /// Dynamic-color query callbacks.
    pub dynamic_color_queries: u64,
    /// UTF-8 bytes in dynamic-color callback prefix and terminator strings.
    pub dynamic_color_query_bytes: u64,
    /// Text-area size query callbacks (pixels).
    pub text_area_size_pixels_queries: u64,
    /// Text-area size query callbacks (characters).
    pub text_area_size_chars_queries: u64,
    /// Keyboard-mode query callbacks.
    pub report_keyboard_mode_queries: u64,
    /// Modify-other-keys query callbacks.
    pub report_modify_other_keys_queries: u64,
    /// OSC 52 clipboard-load query callbacks.
    pub clipboard_load_queries: u64,
    /// UTF-8 bytes in OSC 52 clipboard-load terminators.
    pub clipboard_load_terminator_bytes: u64,
    /// Cursor position-setting callbacks (goto, line, or column).
    pub cursor_position_callbacks: u64,
    /// Cursor movement callbacks (including tab movement and restore).
    pub cursor_movement_callbacks: u64,
    /// Sum of row arguments supplied to vertical cursor movement callbacks.
    pub cursor_requested_rows: u64,
    /// Sum of column arguments supplied to horizontal cursor movement callbacks.
    pub cursor_requested_columns: u64,
    /// Linefeed callbacks, which may scroll and retain history rows.
    pub linefeed_callbacks: u64,
    /// Newline callbacks, which invoke linefeed in the terminal model.
    pub newline_callbacks: u64,
    /// Reverse-index callbacks, which may scroll at the top margin.
    pub reverse_index_callbacks: u64,
    /// Scroll-up callback count and sum of requested lines.
    pub scroll_up_callbacks: u64,
    pub scroll_up_requested_lines: u64,
    /// Scroll-down callback count and sum of requested lines.
    pub scroll_down_callbacks: u64,
    pub scroll_down_requested_lines: u64,
    pub insert_blank_line_callbacks: u64,
    pub insert_blank_lines_requested: u64,
    pub delete_line_callbacks: u64,
    pub delete_lines_requested: u64,
    pub insert_blank_char_callbacks: u64,
    pub insert_blank_chars_requested: u64,
    pub delete_char_callbacks: u64,
    pub delete_chars_requested: u64,
    pub erase_char_callbacks: u64,
    pub erase_chars_requested: u64,
    /// Whole/partial line and screen clearing callbacks.
    pub clear_line_callbacks: u64,
    pub clear_screen_callbacks: u64,
    /// Reset callbacks may reset both grids and several retained stacks.
    pub reset_state_callbacks: u64,
    /// Alternate-screen swap-mode callbacks (the parser census cannot know
    /// whether the model's current mode makes a callback an actual swap).
    pub alternate_screen_mode_callbacks: u64,
    /// DECCOLM callbacks may resize/reset grid storage.
    pub column_mode_callbacks: u64,
    /// DECALN callbacks clear the active viewport.
    pub decaln_callbacks: u64,
    /// SGR attribute callbacks may update cursor-template Arc-backed data.
    pub terminal_attribute_callbacks: u64,
    /// Keyboard-mode push/pop callback counts and requested pop count.
    pub keyboard_mode_pushes: u64,
    pub keyboard_mode_pop_callbacks: u64,
    pub keyboard_modes_requested_pop: u64,
    /// Bell callbacks; the terminal queues a transient listener event per callback.
    pub bell_callbacks: u64,
    /// Cursor-style callbacks; the terminal queues a transient listener event per callback.
    pub cursor_style_callbacks: u64,
    /// Private-mode callback totals, including modes ignored by the terminal.
    pub private_mode_set_callbacks: u64,
    pub private_mode_unset_callbacks: u64,
    pub private_mode_named_callbacks: u64,
    pub private_mode_unknown_callbacks: u64,
    /// OSC 52 store callbacks and encoded source payload bytes.
    /// The runtime host may disable this effect; these are parser callback counts only.
    pub clipboard_store_callbacks: u64,
    pub clipboard_store_source_bytes: u64,
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

    fn update_max_bytes(counter: &mut u64, saturated: &mut bool, amount: usize) {
        match u64::try_from(amount) {
            Ok(amount) => *counter = (*counter).max(amount),
            Err(_) => {
                *counter = u64::MAX;
                *saturated = true;
            }
        }
    }

    fn increment(counter: &mut u64, saturated: &mut bool) {
        Self::add_counter(counter, saturated, 1);
    }

    fn record(counter: &mut u64, requested: &mut u64, saturated: &mut bool, amount: usize) {
        Self::increment(counter, saturated);
        Self::add_bytes(requested, saturated, amount);
    }

    fn count_private_mode_kind(mode: crate::vte::ansi::PrivateMode, census: &mut Self) {
        match mode {
            crate::vte::ansi::PrivateMode::Named(_) => {
                Self::increment(
                    &mut census.private_mode_named_callbacks,
                    &mut census.saturated,
                );
            }
            crate::vte::ansi::PrivateMode::Unknown(_) => {
                Self::increment(
                    &mut census.private_mode_unknown_callbacks,
                    &mut census.saturated,
                );
            }
        }
    }
}

impl Handler for ResourceCensus {
    fn bell(&mut self) {
        Self::increment(&mut self.bell_callbacks, &mut self.saturated);
    }

    fn set_cursor_style(&mut self, _: Option<crate::vte::ansi::CursorStyle>) {
        Self::increment(&mut self.cursor_style_callbacks, &mut self.saturated);
    }

    fn input(&mut self, c: char) {
        self.input_repeated(c, 1);
    }

    fn input_repeated(&mut self, c: char, count: u16) {
        ResourceCensus::input_repeated(self, c, count);
    }

    fn goto(&mut self, line: i32, col: usize) {
        Self::increment(&mut self.cursor_position_callbacks, &mut self.saturated);
        Self::add_bytes(
            &mut self.cursor_requested_rows,
            &mut self.saturated,
            line.unsigned_abs() as usize,
        );
        Self::add_bytes(&mut self.cursor_requested_columns, &mut self.saturated, col);
    }

    fn goto_line(&mut self, line: i32) {
        Self::increment(&mut self.cursor_position_callbacks, &mut self.saturated);
        Self::add_bytes(
            &mut self.cursor_requested_rows,
            &mut self.saturated,
            line.unsigned_abs() as usize,
        );
    }

    fn goto_col(&mut self, col: usize) {
        Self::increment(&mut self.cursor_position_callbacks, &mut self.saturated);
        Self::add_bytes(&mut self.cursor_requested_columns, &mut self.saturated, col);
    }

    fn move_up(&mut self, rows: usize) {
        Self::record(
            &mut self.cursor_movement_callbacks,
            &mut self.cursor_requested_rows,
            &mut self.saturated,
            rows,
        );
    }

    fn move_down(&mut self, rows: usize) {
        Self::record(
            &mut self.cursor_movement_callbacks,
            &mut self.cursor_requested_rows,
            &mut self.saturated,
            rows,
        );
    }

    fn move_up_and_cr(&mut self, rows: usize) {
        Self::record(
            &mut self.cursor_movement_callbacks,
            &mut self.cursor_requested_rows,
            &mut self.saturated,
            rows,
        );
    }

    fn move_down_and_cr(&mut self, rows: usize) {
        Self::record(
            &mut self.cursor_movement_callbacks,
            &mut self.cursor_requested_rows,
            &mut self.saturated,
            rows,
        );
    }

    fn move_forward(&mut self, cols: usize) {
        Self::record(
            &mut self.cursor_movement_callbacks,
            &mut self.cursor_requested_columns,
            &mut self.saturated,
            cols,
        );
    }

    fn move_backward(&mut self, cols: usize) {
        Self::record(
            &mut self.cursor_movement_callbacks,
            &mut self.cursor_requested_columns,
            &mut self.saturated,
            cols,
        );
    }

    fn put_tab(&mut self, count: u16) {
        Self::record(
            &mut self.cursor_movement_callbacks,
            &mut self.cursor_requested_columns,
            &mut self.saturated,
            usize::from(count),
        );
    }

    fn move_backward_tabs(&mut self, count: u16) {
        Self::record(
            &mut self.cursor_movement_callbacks,
            &mut self.cursor_requested_columns,
            &mut self.saturated,
            usize::from(count),
        );
    }

    fn move_forward_tabs(&mut self, count: u16) {
        Self::record(
            &mut self.cursor_movement_callbacks,
            &mut self.cursor_requested_columns,
            &mut self.saturated,
            usize::from(count),
        );
    }

    fn backspace(&mut self) {
        Self::record(
            &mut self.cursor_movement_callbacks,
            &mut self.cursor_requested_columns,
            &mut self.saturated,
            1,
        );
    }

    fn restore_cursor_position(&mut self) {
        Self::increment(&mut self.cursor_movement_callbacks, &mut self.saturated);
    }

    fn carriage_return(&mut self) {
        Self::increment(&mut self.cursor_movement_callbacks, &mut self.saturated);
    }

    fn linefeed(&mut self) {
        Self::increment(&mut self.linefeed_callbacks, &mut self.saturated);
    }

    fn newline(&mut self) {
        Self::increment(&mut self.newline_callbacks, &mut self.saturated);
    }

    fn reverse_index(&mut self) {
        Self::increment(&mut self.reverse_index_callbacks, &mut self.saturated);
    }

    fn scroll_up(&mut self, lines: usize) {
        Self::record(
            &mut self.scroll_up_callbacks,
            &mut self.scroll_up_requested_lines,
            &mut self.saturated,
            lines,
        );
    }

    fn scroll_down(&mut self, lines: usize) {
        Self::record(
            &mut self.scroll_down_callbacks,
            &mut self.scroll_down_requested_lines,
            &mut self.saturated,
            lines,
        );
    }

    fn insert_blank_lines(&mut self, lines: usize) {
        Self::record(
            &mut self.insert_blank_line_callbacks,
            &mut self.insert_blank_lines_requested,
            &mut self.saturated,
            lines,
        );
    }

    fn delete_lines(&mut self, lines: usize) {
        Self::record(
            &mut self.delete_line_callbacks,
            &mut self.delete_lines_requested,
            &mut self.saturated,
            lines,
        );
    }

    fn insert_blank(&mut self, chars: usize) {
        Self::record(
            &mut self.insert_blank_char_callbacks,
            &mut self.insert_blank_chars_requested,
            &mut self.saturated,
            chars,
        );
    }

    fn delete_chars(&mut self, chars: usize) {
        Self::record(
            &mut self.delete_char_callbacks,
            &mut self.delete_chars_requested,
            &mut self.saturated,
            chars,
        );
    }

    fn erase_chars(&mut self, chars: usize) {
        Self::record(
            &mut self.erase_char_callbacks,
            &mut self.erase_chars_requested,
            &mut self.saturated,
            chars,
        );
    }

    fn clear_line(&mut self, _: crate::vte::ansi::LineClearMode) {
        Self::increment(&mut self.clear_line_callbacks, &mut self.saturated);
    }

    fn clear_screen(&mut self, _: crate::vte::ansi::ClearMode) {
        Self::increment(&mut self.clear_screen_callbacks, &mut self.saturated);
    }

    fn reset_state(&mut self) {
        Self::increment(&mut self.reset_state_callbacks, &mut self.saturated);
    }

    fn decaln(&mut self) {
        Self::increment(&mut self.decaln_callbacks, &mut self.saturated);
    }

    fn terminal_attribute(&mut self, _: crate::vte::ansi::Attr) {
        Self::increment(&mut self.terminal_attribute_callbacks, &mut self.saturated);
    }

    fn push_keyboard_mode(&mut self, _: crate::vte::ansi::KeyboardModes) {
        Self::increment(&mut self.keyboard_mode_pushes, &mut self.saturated);
    }

    fn pop_keyboard_modes(&mut self, count: u16) {
        Self::record(
            &mut self.keyboard_mode_pop_callbacks,
            &mut self.keyboard_modes_requested_pop,
            &mut self.saturated,
            usize::from(count),
        );
    }

    fn set_private_mode(&mut self, mode: crate::vte::ansi::PrivateMode) {
        Self::increment(&mut self.private_mode_set_callbacks, &mut self.saturated);
        Self::count_private_mode_kind(mode, self);
        if matches!(
            mode,
            crate::vte::ansi::PrivateMode::Named(
                crate::vte::ansi::NamedPrivateMode::SwapScreenAndSetRestoreCursor
            )
        ) {
            Self::increment(
                &mut self.alternate_screen_mode_callbacks,
                &mut self.saturated,
            );
        }
        if matches!(
            mode,
            crate::vte::ansi::PrivateMode::Named(crate::vte::ansi::NamedPrivateMode::ColumnMode)
        ) {
            Self::increment(&mut self.column_mode_callbacks, &mut self.saturated);
        }
    }

    fn unset_private_mode(&mut self, mode: crate::vte::ansi::PrivateMode) {
        Self::increment(&mut self.private_mode_unset_callbacks, &mut self.saturated);
        Self::count_private_mode_kind(mode, self);
        if matches!(
            mode,
            crate::vte::ansi::PrivateMode::Named(
                crate::vte::ansi::NamedPrivateMode::SwapScreenAndSetRestoreCursor
            )
        ) {
            Self::increment(
                &mut self.alternate_screen_mode_callbacks,
                &mut self.saturated,
            );
        }
        if matches!(
            mode,
            crate::vte::ansi::PrivateMode::Named(crate::vte::ansi::NamedPrivateMode::ColumnMode)
        ) {
            Self::increment(&mut self.column_mode_callbacks, &mut self.saturated);
        }
    }

    fn clipboard_store(&mut self, _: u8, encoded: &[u8]) {
        Self::increment(&mut self.clipboard_store_callbacks, &mut self.saturated);
        Self::add_bytes(
            &mut self.clipboard_store_source_bytes,
            &mut self.saturated,
            encoded.len(),
        );
    }

    fn set_title(&mut self, title: Option<String>) {
        Self::increment(&mut self.title_set_callbacks, &mut self.saturated);
        if let Some(title) = title {
            Self::add_bytes(&mut self.title_bytes, &mut self.saturated, title.len());
            Self::update_max_bytes(&mut self.max_title_bytes, &mut self.saturated, title.len());
        }
    }

    fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        Self::increment(&mut self.hyperlink_updates, &mut self.saturated);
        if let Some(hyperlink) = hyperlink {
            if let Some(id) = hyperlink.id {
                Self::add_bytes(&mut self.hyperlink_id_bytes, &mut self.saturated, id.len());
                Self::update_max_bytes(
                    &mut self.max_hyperlink_id_bytes,
                    &mut self.saturated,
                    id.len(),
                );
            }
            Self::add_bytes(
                &mut self.hyperlink_uri_bytes,
                &mut self.saturated,
                hyperlink.uri.len(),
            );
            Self::update_max_bytes(
                &mut self.max_hyperlink_uri_bytes,
                &mut self.saturated,
                hyperlink.uri.len(),
            );
        }
    }

    fn push_title(&mut self) {
        Self::increment(&mut self.title_pushes, &mut self.saturated);
    }

    fn pop_title(&mut self) {
        Self::increment(&mut self.title_pops, &mut self.saturated);
    }

    fn identify_terminal(&mut self, _: Option<char>) {
        Self::increment(&mut self.identify_terminal_queries, &mut self.saturated);
    }

    fn device_status(&mut self, _: usize) {
        Self::increment(&mut self.device_status_queries, &mut self.saturated);
    }

    fn report_mode(&mut self, _: crate::vte::ansi::Mode) {
        Self::increment(&mut self.report_mode_queries, &mut self.saturated);
    }

    fn report_private_mode(&mut self, _: crate::vte::ansi::PrivateMode) {
        Self::increment(&mut self.report_private_mode_queries, &mut self.saturated);
    }

    fn dynamic_color_sequence(&mut self, prefix: String, _: usize, terminator: &str) {
        Self::increment(&mut self.dynamic_color_queries, &mut self.saturated);
        Self::add_bytes(
            &mut self.dynamic_color_query_bytes,
            &mut self.saturated,
            prefix.len(),
        );
        Self::add_bytes(
            &mut self.dynamic_color_query_bytes,
            &mut self.saturated,
            terminator.len(),
        );
    }

    fn text_area_size_pixels(&mut self) {
        Self::increment(&mut self.text_area_size_pixels_queries, &mut self.saturated);
    }

    fn text_area_size_chars(&mut self) {
        Self::increment(&mut self.text_area_size_chars_queries, &mut self.saturated);
    }

    fn report_keyboard_mode(&mut self) {
        Self::increment(&mut self.report_keyboard_mode_queries, &mut self.saturated);
    }

    fn report_modify_other_keys(&mut self) {
        Self::increment(
            &mut self.report_modify_other_keys_queries,
            &mut self.saturated,
        );
    }

    fn clipboard_load(&mut self, _: u8, terminator: &str) {
        Self::increment(&mut self.clipboard_load_queries, &mut self.saturated);
        Self::add_bytes(
            &mut self.clipboard_load_terminator_bytes,
            &mut self.saturated,
            terminator.len(),
        );
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
    /// Census shape for the currently active grid (primary or alternate).
    pub active_grid: GridAllocationCensus,
    /// Census shape for the currently inactive/alternate grid.
    pub inactive_grid: GridAllocationCensus,
    pub cell_visits: usize,
    pub unique_cell_extras: usize,
    /// Number of visited cells/cursor roots referencing a CellExtra Arc.
    pub cell_extra_references: usize,
    pub unique_hyperlinks: usize,
    pub row_slots_capacity: usize,
    pub cell_slots_capacity: usize,
    /// Zero-width Vec capacity counted once per distinct CellExtra allocation.
    pub zero_width_chars_capacity: usize,
    /// Sum of zero-width Vec capacities per reference; shared Arcs count again.
    pub reference_weighted_zero_width_capacity: usize,
    /// Largest zero-width Vec capacity among referenced CellExtra allocations.
    pub max_single_extra_zero_width_capacity: usize,
    /// Unique logical zero-width characters, counted once per CellExtra Arc.
    pub unique_zero_width_chars_len: usize,
    /// Logical zero-width characters summed for every cell/cursor reference.
    pub reference_weighted_zero_width_chars_len: usize,
    /// Largest logical zero-width Vec length among referenced CellExtra Arcs.
    pub max_single_extra_zero_width_chars_len: usize,
    pub hyperlink_id_bytes_capacity: usize,
    pub hyperlink_uri_bytes_capacity: usize,
    /// Unique logical UTF-8 bytes in hyperlink identifiers.
    pub hyperlink_id_bytes_len: usize,
    /// Unique logical UTF-8 bytes in hyperlink URIs.
    pub hyperlink_uri_bytes_len: usize,
    /// Logical byte length of the current title, if present.
    pub current_title_len_bytes: usize,
    /// Backing String capacity of the current title, if present.
    pub current_title_capacity_bytes: usize,
    /// Number of title stack entries, including empty entries.
    pub title_stack_count: usize,
    /// Logical UTF-8 bytes across present title stack entries.
    pub title_stack_len_bytes: usize,
    /// Backing String capacities across present title stack entries.
    pub title_stack_strings_capacity_bytes: usize,
    /// Largest logical title length across current and stacked titles.
    pub max_title_len_bytes: usize,
    /// Largest String capacity across current and stacked titles.
    pub max_title_capacity_bytes: usize,
    pub title_stack_slots_capacity: usize,
    pub tab_stop_capacity: usize,
    pub keyboard_mode_stack_capacity: usize,
    pub inactive_keyboard_mode_stack_capacity: usize,
    pub damage_line_capacity: usize,
    pub estimated_persistent_owned_bytes: usize,
    pub incomplete: bool,
}

/// Geometry and retained row-storage shape for one terminal grid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GridAllocationCensus {
    /// Logical viewport height in rows.
    pub screen_lines: usize,
    /// Logical width in columns.
    pub columns: usize,
    /// Configured scrollback history limit.
    pub history_limit: usize,
    /// Initialized raw rows, including inactive resize-cache rows.
    pub initialized_rows: usize,
    /// Outer row-vector capacity.
    pub outer_row_capacity: usize,
    /// Sum of backing cell-vector capacities for initialized raw rows.
    pub cell_capacity: usize,
    /// Sum of missing columns where initialized row length is below logical width.
    pub short_row_column_gap: usize,
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
            &mut self.result.hyperlink_id_bytes_len,
            link.id().len(),
            &mut self.result.incomplete,
        );
        Self::add(
            &mut self.result.hyperlink_uri_bytes_len,
            link.uri().len(),
            &mut self.result.incomplete,
        );
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
        let zero_width_len = cell.zerowidth().map_or(0, <[char]>::len);
        Self::add(
            &mut self.result.cell_extra_references,
            1,
            &mut self.result.incomplete,
        );
        Self::add(
            &mut self.result.reference_weighted_zero_width_capacity,
            zero_width_capacity,
            &mut self.result.incomplete,
        );
        Self::add(
            &mut self.result.reference_weighted_zero_width_chars_len,
            zero_width_len,
            &mut self.result.incomplete,
        );
        self.result.max_single_extra_zero_width_capacity = self
            .result
            .max_single_extra_zero_width_capacity
            .max(zero_width_capacity);
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
            Self::add(
                &mut self.result.unique_zero_width_chars_len,
                zero_width_len,
                &mut self.result.incomplete,
            );
            self.result.max_single_extra_zero_width_chars_len = self
                .result
                .max_single_extra_zero_width_chars_len
                .max(zero_width_len);
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

    fn grid(&mut self, grid: &Grid<Cell>) -> GridAllocationCensus {
        let (rows, row_capacity) = grid.allocation_rows();
        let mut grid_result = GridAllocationCensus {
            screen_lines: grid.screen_lines(),
            columns: grid.columns(),
            history_limit: grid.checkpoint_max_scroll_limit(),
            initialized_rows: rows.len(),
            outer_row_capacity: row_capacity,
            ..GridAllocationCensus::default()
        };
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
                return grid_result;
            }
            let capacity = row.allocation_capacity();
            Self::add(
                &mut grid_result.cell_capacity,
                capacity,
                &mut self.result.incomplete,
            );
            Self::add(
                &mut grid_result.short_row_column_gap,
                grid_result.columns.saturating_sub(row.initialized().len()),
                &mut self.result.incomplete,
            );
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
                    return grid_result;
                }
            }
        }
        self.cell(&grid.cursor.template);
        if !self.result.incomplete {
            self.cell(&grid.saved_cursor.template);
        }
        grid_result
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
    builder.result.active_grid = builder.grid(&term.grid);
    if !builder.result.incomplete {
        builder.result.inactive_grid = builder.grid(&term.inactive_grid);
    }
    if !builder.result.incomplete {
        if let Some(title) = &term.title {
            builder.result.current_title_len_bytes = title.len();
            builder.result.current_title_capacity_bytes = title.capacity();
            builder.result.max_title_len_bytes = title.len();
            builder.result.max_title_capacity_bytes = title.capacity();
            builder.bytes(title.capacity());
        }
        builder.result.title_stack_count = term.title_stack.len();
        builder.result.title_stack_slots_capacity = term.title_stack.capacity();
        let title_stack_bytes =
            builder.product(term.title_stack.capacity(), size_of::<Option<String>>());
        builder.bytes(title_stack_bytes);
        for title in term.title_stack.iter().flatten() {
            CensusBuilder::add(
                &mut builder.result.title_stack_len_bytes,
                title.len(),
                &mut builder.result.incomplete,
            );
            CensusBuilder::add(
                &mut builder.result.title_stack_strings_capacity_bytes,
                title.capacity(),
                &mut builder.result.incomplete,
            );
            builder.result.max_title_len_bytes =
                builder.result.max_title_len_bytes.max(title.len());
            builder.result.max_title_capacity_bytes = builder
                .result
                .max_title_capacity_bytes
                .max(title.capacity());
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
    use std::sync::Arc;
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
        assert_eq!(census.title_set_callbacks, 1);
        assert_eq!(census.title_bytes, 5);
        assert_eq!(census.max_title_bytes, 5);
        assert_eq!(census.hyperlink_updates, 1);
        assert_eq!(census.hyperlink_id_bytes, 2);
        assert_eq!(census.max_hyperlink_id_bytes, 2);
        assert_eq!(census.hyperlink_uri_bytes, "https://e.test".len() as u64);
        assert_eq!(
            census.max_hyperlink_uri_bytes,
            "https://e.test".len() as u64
        );
        assert_eq!(census.title_pushes, 1);
    }

    #[test]
    fn census_counts_title_push_amplification_and_partial_osc_continuation() {
        let mut processor = Processor::<TestTimeout>::new();
        let mut ignore = ResourceCensus::default();
        processor.advance(&mut ignore, b"\x1b]2;title-prefix");
        let before = state(&processor);

        let suffix = format!("{}\x07{}", "x".repeat(4096), "\x1b[22t".repeat(100));
        let census = census_output(&processor, suffix.as_bytes());
        assert_eq!(census.title_set_callbacks, 1);
        assert_eq!(census.title_bytes, ("title-prefix".len() + 4096) as u64);
        assert_eq!(census.max_title_bytes, ("title-prefix".len() + 4096) as u64);
        assert_eq!(census.title_pushes, 100);
        assert_eq!(census.title_pops, 0);
        assert_eq!(state(&processor), before);

        let pop_processor = Processor::<TestTimeout>::new();
        let pop_census = census_output(&pop_processor, b"\x1b[23t\x1b[23t");
        assert_eq!(pop_census.title_pops, 2);
        assert_eq!(
            state(&pop_processor),
            state(&Processor::<TestTimeout>::new())
        );
    }

    #[test]
    fn sync_timeout_census_counts_buffered_osc_without_mutating_processor() {
        let mut processor = Processor::<TestTimeout>::new();
        let mut ignore = ResourceCensus::default();
        processor.advance(&mut ignore, b"\x1b[?2026h\x1b]2;buffered\x07");
        assert!(processor.sync_timeout().pending_timeout());
        let before = state(&processor);

        let census = census_sync_timeout(&processor);
        assert_eq!(census.title_set_callbacks, 1);
        assert_eq!(census.title_bytes, "buffered".len() as u64);
        assert_eq!(census.max_title_bytes, "buffered".len() as u64);
        assert_eq!(state(&processor), before);
    }

    #[test]
    fn census_counts_repeated_long_osc8_updates_and_maximum_payloads() {
        let mut bytes = Vec::new();
        let uri = "u".repeat(4096);
        for index in 0..8 {
            bytes.extend_from_slice(format!("\x1b]8;id={index};{uri}\x07").as_bytes());
        }
        assert!(bytes.len() < 0x1_0000);

        let processor = Processor::<TestTimeout>::new();
        let before = state(&processor);
        let census = census_output(&processor, &bytes);
        assert_eq!(census.hyperlink_updates, 8);
        assert_eq!(census.hyperlink_id_bytes, 8); // Each numeric id is one digit.
        assert_eq!(census.max_hyperlink_id_bytes, 1);
        assert_eq!(census.hyperlink_uri_bytes, 8 * 4096);
        assert_eq!(census.max_hyperlink_uri_bytes, 4096);
        assert_eq!(state(&processor), before);
    }

    #[test]
    fn census_counts_all_term_query_callbacks_without_answering_them() {
        let query_block = concat!(
            "\x1b[c\x1b[>c",               // Primary and secondary device attributes.
            "\x1b[5n\x1b[6n",              // Device status.
            "\x1b[4$p\x1b[?25$p",          // Public and private mode reports.
            "\x1b[14t\x1b[18t",            // Pixel and character dimensions.
            "\x1b[?u\x1b[?4m",             // Keyboard and modify-other-keys reports.
            "\x1b]4;1;?\x07\x1b]10;?\x07", // Dynamic colors.
            "\x1b]52;c;?\x07",             // Clipboard load query.
        );
        let bytes = query_block.repeat(256);
        let processor = Processor::<TestTimeout>::new();
        let before = state(&processor);
        let census = census_output(&processor, bytes.as_bytes());
        assert_eq!(census.identify_terminal_queries, 2 * 256);
        assert_eq!(census.device_status_queries, 2 * 256);
        assert_eq!(census.report_mode_queries, 256);
        assert_eq!(census.report_private_mode_queries, 256);
        assert_eq!(census.text_area_size_pixels_queries, 256);
        assert_eq!(census.text_area_size_chars_queries, 256);
        assert_eq!(census.report_keyboard_mode_queries, 256);
        assert_eq!(census.report_modify_other_keys_queries, 256);
        assert_eq!(census.dynamic_color_queries, 2 * 256);
        assert!(census.dynamic_color_query_bytes > 0);
        assert_eq!(census.clipboard_load_queries, 256);
        assert_eq!(state(&processor), before);
    }

    #[test]
    fn census_counts_event_producing_callbacks_and_all_private_mode_kinds() {
        let processor = Processor::<TestTimeout>::new();
        let bytes =
            b"\x07\x07\x1b[1 q\x1b[2 q\x1b[?1000h\x1b[?9999h\x1b[?9999l\x1b]52;c;aGVsbG8=\x07";
        let before = state(&processor);
        let census = census_output(&processor, bytes);
        assert_eq!(census.bell_callbacks, 2);
        assert_eq!(census.cursor_style_callbacks, 2);
        assert_eq!(census.private_mode_set_callbacks, 2);
        assert_eq!(census.private_mode_unset_callbacks, 1);
        assert_eq!(census.private_mode_named_callbacks, 1);
        assert_eq!(census.private_mode_unknown_callbacks, 2);
        assert_eq!(census.alternate_screen_mode_callbacks, 0);
        assert_eq!(census.clipboard_store_callbacks, 1);
        assert_eq!(census.clipboard_store_source_bytes, 8);
        assert_eq!(state(&processor), before);
    }

    #[test]
    fn sync_timeout_census_counts_deferred_event_producing_callbacks() {
        let mut processor = Processor::<TestTimeout>::new();
        let mut ignore = ResourceCensus::default();
        processor.advance(&mut ignore, b"\x1b[?2026h");
        processor.advance(&mut ignore, b"\x07\x1b[1 q\x1b[?1000h\x1b[?9999l");
        assert!(processor.sync_timeout().pending_timeout());
        let before = state(&processor);

        let census = census_sync_timeout(&processor);
        assert_eq!(census.bell_callbacks, 1);
        assert_eq!(census.cursor_style_callbacks, 1);
        assert_eq!(census.private_mode_set_callbacks, 1);
        // The ordered sync timeout also reports the parser's SyncUpdate mode
        // exit, in addition to the buffered unknown mode callback.
        assert_eq!(census.private_mode_unset_callbacks, 2);
        assert_eq!(census.private_mode_named_callbacks, 2);
        assert_eq!(census.private_mode_unknown_callbacks, 1);
        assert_eq!(state(&processor), before);
    }

    #[test]
    fn census_counts_grid_drivers_across_partial_csi_continuation() {
        let mut processor = Processor::<TestTimeout>::new();
        let mut ignore = ResourceCensus::default();
        processor.advance(&mut ignore, b"\x1b[2;");
        let before = state(&processor);

        let census = census_output(
            &processor,
            b"4H\x1b[3A\x1b[2B\x1b[5C\x1b[4D\x1b[2S\x1b[3T\x1b[4L\x1b[5M\x1b[6@\x1b[7P\x1b[8X\x1b[K\x1b[2J",
        );
        assert_eq!(census.cursor_position_callbacks, 1);
        assert_eq!(census.cursor_movement_callbacks, 4);
        assert_eq!(census.cursor_requested_rows, 1 + 3 + 2);
        assert_eq!(census.cursor_requested_columns, 3 + 5 + 4);
        assert_eq!(census.scroll_up_callbacks, 1);
        assert_eq!(census.scroll_up_requested_lines, 2);
        assert_eq!(census.scroll_down_callbacks, 1);
        assert_eq!(census.scroll_down_requested_lines, 3);
        assert_eq!(census.insert_blank_line_callbacks, 1);
        assert_eq!(census.insert_blank_lines_requested, 4);
        assert_eq!(census.delete_line_callbacks, 1);
        assert_eq!(census.delete_lines_requested, 5);
        assert_eq!(census.insert_blank_char_callbacks, 1);
        assert_eq!(census.insert_blank_chars_requested, 6);
        assert_eq!(census.delete_char_callbacks, 1);
        assert_eq!(census.delete_chars_requested, 7);
        assert_eq!(census.erase_char_callbacks, 1);
        assert_eq!(census.erase_chars_requested, 8);
        assert_eq!(census.clear_line_callbacks, 1);
        assert_eq!(census.clear_screen_callbacks, 1);
        assert_eq!(state(&processor), before);
    }

    #[test]
    fn census_counts_rep_scroll_clear_reset_and_mode_grid_drivers() {
        let processor = Processor::<TestTimeout>::new();
        let census = census_output(
            &processor,
            b"A\x1b[65535b\x1b[999S\x1b[999T\x1b[2K\x1b[2J\x1bc\x1bM\x1bE\x1b#8\x1b[?1049h\x1b[?1049l\x1b[?3h\x1b[?3l\x1b[>3u\x1b[<2u",
        );
        assert_eq!(census.input_chars, 65_536);
        assert_eq!(census.scroll_up_callbacks, 1);
        assert_eq!(census.scroll_up_requested_lines, 999);
        assert_eq!(census.scroll_down_callbacks, 1); // CSI T; reverse index is separate.
        assert_eq!(census.scroll_down_requested_lines, 999);
        assert_eq!(census.clear_line_callbacks, 1);
        assert_eq!(census.clear_screen_callbacks, 1);
        assert_eq!(census.reset_state_callbacks, 1);
        assert_eq!(census.reverse_index_callbacks, 1);
        // This pinned parser implements ESC E as linefeed + carriage return;
        // the Handler::newline callback is not dispatched by its grammar.
        assert_eq!(census.linefeed_callbacks, 1);
        assert_eq!(census.newline_callbacks, 0);
        assert_eq!(census.cursor_movement_callbacks, 1); // ESC E's carriage return.
        assert_eq!(census.decaln_callbacks, 1);
        assert_eq!(census.alternate_screen_mode_callbacks, 2);
        assert_eq!(census.column_mode_callbacks, 2);
        assert_eq!(census.keyboard_mode_pushes, 1);
        assert_eq!(census.keyboard_mode_pop_callbacks, 1);
        assert_eq!(census.keyboard_modes_requested_pop, 2);
    }

    #[test]
    fn sync_timeout_census_counts_buffered_grid_drivers_without_mutation() {
        let mut processor = Processor::<TestTimeout>::new();
        let mut ignore = ResourceCensus::default();
        processor.advance(&mut ignore, b"\x1b[?2026h\x1b[4S\x1b[3L\x1b[2J");
        assert!(processor.sync_timeout().pending_timeout());
        let before = state(&processor);

        let census = census_sync_timeout(&processor);
        assert_eq!(census.scroll_up_callbacks, 1);
        assert_eq!(census.scroll_up_requested_lines, 4);
        assert_eq!(census.insert_blank_line_callbacks, 1);
        assert_eq!(census.insert_blank_lines_requested, 3);
        assert_eq!(census.clear_screen_callbacks, 1);
        assert_eq!(state(&processor), before);
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

        let mut requested = ResourceCensus {
            scroll_up_requested_lines: u64::MAX - 1,
            ..ResourceCensus::default()
        };
        requested.scroll_up(2);
        assert_eq!(requested.scroll_up_requested_lines, u64::MAX);
        assert!(requested.saturated);
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

        let checkpoint = checkpoint_bytes(&term);
        let result = census(&term);
        assert_eq!(checkpoint_bytes(&term), checkpoint);
        assert!(!result.incomplete);
        assert_eq!(result.unique_cell_extras, baseline.unique_cell_extras + 2);
        assert_eq!(result.unique_hyperlinks, baseline.unique_hyperlinks + 2);
        assert_eq!(
            result.unique_zero_width_chars_len,
            baseline.unique_zero_width_chars_len + 2
        );
        assert_eq!(
            result.reference_weighted_zero_width_chars_len,
            baseline.reference_weighted_zero_width_chars_len + 4
        );
        assert_eq!(result.max_single_extra_zero_width_chars_len, 1);
        assert_eq!(
            result.hyperlink_id_bytes_len,
            baseline.hyperlink_id_bytes_len + 2 * "shared".len()
        );
        assert_eq!(
            result.hyperlink_uri_bytes_len,
            baseline.hyperlink_uri_bytes_len + 2 * "https://x".len()
        );
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
        // A shortened initialized row contributes the gap to current columns.
        let _removed = term.grid[Line(0)].shrink(2);
        let result = census(&term);
        assert!(!result.incomplete);
        assert!(result.row_slots_capacity >= 4);
        assert!(
            result.cell_slots_capacity >= 32,
            "must include cached rows and both screens"
        );
        assert!(result.unique_cell_extras >= 2);
        assert!(result.title_stack_slots_capacity >= 2);
        assert_eq!(result.active_grid.screen_lines, 2);
        assert_eq!(result.inactive_grid.screen_lines, 4);
        assert_eq!(result.active_grid.columns, 4);
        assert_eq!(result.active_grid.initialized_rows, 4);
        assert!(result.active_grid.outer_row_capacity >= result.active_grid.initialized_rows);
        assert_eq!(result.active_grid.short_row_column_gap, 2);
        assert_eq!(result.current_title_len_bytes, "window".len());
        assert_eq!(
            result.current_title_capacity_bytes,
            term.title.as_ref().unwrap().capacity()
        );
        assert_eq!(result.title_stack_count, 2);
        assert_eq!(result.title_stack_len_bytes, "older".len());
        assert_eq!(
            result.title_stack_strings_capacity_bytes,
            term.title_stack[0].as_ref().unwrap().capacity()
        );
        assert_eq!(result.max_title_len_bytes, "window".len());
        assert!(result.max_title_capacity_bytes >= result.max_title_len_bytes);
        assert!(result.estimated_persistent_owned_bytes > 0);
    }

    #[test]
    fn allocation_census_title_metrics_preserve_checkpoint_bytes() {
        let mut term = terminal();
        term.title = Some(String::from("current title"));
        term.title_stack = vec![Some(String::from("prior title")), None];
        let checkpoint = checkpoint_bytes(&term);
        let result = census(&term);
        assert_eq!(result.current_title_len_bytes, "current title".len());
        assert_eq!(result.title_stack_count, 2);
        assert_eq!(result.title_stack_len_bytes, "prior title".len());
        assert_eq!(checkpoint_bytes(&term), checkpoint);
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
        let checkpoint = checkpoint_bytes(&term);
        let result = census(&term);
        assert_eq!(checkpoint_bytes(&term), checkpoint);
        assert_eq!(result.unique_cell_extras, baseline.unique_cell_extras + 4);
        assert_eq!(
            result.unique_zero_width_chars_len,
            baseline.unique_zero_width_chars_len + 4
        );
        assert_eq!(
            result.reference_weighted_zero_width_chars_len,
            baseline.reference_weighted_zero_width_chars_len + 4
        );
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

    #[test]
    fn imported_shared_marks_can_cow_into_large_new_allocations() {
        // This caller-constructed state is deliberately adversarial; it is not
        // asserted to be reachable through ordinary ANSI from a fresh Term.
        let mut original = terminal();
        let baseline = census(&original);
        let mut shared = Cell::default();
        for _ in 0..4096 {
            shared.push_zerowidth('\u{301}');
        }
        for column in 0..4 {
            original.grid[Point::new(Line(0), Column(column))] = shared.clone();
        }
        drop(shared);

        let encoded = postcard::to_allocvec(&original.export_checkpoint()).unwrap();
        let imported = postcard::from_bytes(&encoded).unwrap();
        let mut restored = Term::from_checkpoint(imported, VoidListener).unwrap();
        for column in 1..4 {
            let original_first = original.grid[Point::new(Line(0), Column(0))]
                .extra
                .as_ref()
                .unwrap();
            let original_next = original.grid[Point::new(Line(0), Column(column))]
                .extra
                .as_ref()
                .unwrap();
            assert!(Arc::ptr_eq(original_first, original_next));
            let first = restored.grid[Point::new(Line(0), Column(0))]
                .extra
                .as_ref()
                .unwrap();
            let next = restored.grid[Point::new(Line(0), Column(column))]
                .extra
                .as_ref()
                .unwrap();
            assert!(Arc::ptr_eq(first, next));
        }

        let before = census(&restored);
        assert_eq!(
            before.cell_extra_references,
            baseline.cell_extra_references + 4
        );
        assert_eq!(before.unique_cell_extras, baseline.unique_cell_extras + 1);
        assert_eq!(
            before.reference_weighted_zero_width_capacity,
            baseline.reference_weighted_zero_width_capacity
                + 4 * before.max_single_extra_zero_width_capacity
        );
        assert!(before.max_single_extra_zero_width_capacity >= 4096);

        // Position the VTE cursor so each zero-width character targets a
        // different cell which initially shares the imported Arc allocation.
        let input = "\x1b[1;2H\u{302}\x1b[1;3H\u{303}".as_bytes();
        let mut original_parser = Processor::<TestTimeout>::new();
        let mut restored_parser = Processor::<TestTimeout>::new();
        original_parser.advance(&mut original, input);
        restored_parser.advance(&mut restored, input);

        let after = census(&restored);
        assert_eq!(after.unique_cell_extras, before.unique_cell_extras + 2);
        assert!(
            after.zero_width_chars_capacity
                >= before.zero_width_chars_capacity
                    + 2 * before.max_single_extra_zero_width_capacity,
            "two marks caused two large CellExtra Vec copies"
        );
        assert!(after.zero_width_chars_capacity - before.zero_width_chars_capacity > 4);
        assert!(
            after.reference_weighted_zero_width_capacity
                > before.reference_weighted_zero_width_capacity
        );
        assert!(
            after.max_single_extra_zero_width_capacity
                >= before.max_single_extra_zero_width_capacity
        );
        let original_after = census(&original);
        assert_eq!(after.unique_cell_extras, original_after.unique_cell_extras);
        assert_eq!(
            after.cell_extra_references,
            original_after.cell_extra_references
        );
        assert_eq!(
            after.reference_weighted_zero_width_capacity,
            original_after.reference_weighted_zero_width_capacity
        );
        assert_eq!(
            after.zero_width_chars_capacity,
            original_after.zero_width_chars_capacity
        );
        assert_eq!(
            after.max_single_extra_zero_width_capacity,
            original_after.max_single_extra_zero_width_capacity
        );
        assert_eq!(
            postcard::to_allocvec(&original.export_checkpoint()).unwrap(),
            postcard::to_allocvec(&restored.export_checkpoint()).unwrap(),
            "continued imported model must match source semantics"
        );
    }
}
