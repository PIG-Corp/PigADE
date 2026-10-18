//! Pure intent and correlation state for a single read-only historical grid.
//! Rendering stays in the UI; this module never parses terminal cells.

use crate::protocol;
use uuid::Uuid;

pub(crate) const MAX_GRID_ROWS: u16 = 128;
pub(crate) const MAX_GRID_COLS: u16 = 1024;
pub(crate) const MAX_GRID_CELLS: u32 = 32_768;
const MAX_QUEUED_LINES: i64 = 131_072;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneIdentity {
    pub session: Uuid,
    pub generation: Uuid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Dimensions {
    pub rows: u16,
    pub cols: u16,
}

impl Dimensions {
    pub(crate) fn bounded(rows: u16, cols: u16) -> Option<Self> {
        if rows == 0 || cols == 0 {
            return None;
        }
        let cols = cols.min(MAX_GRID_COLS);
        let rows = rows
            .min(MAX_GRID_ROWS)
            .min((MAX_GRID_CELLS / u32::from(cols)) as u16);
        (rows > 0).then_some(Self { rows, cols })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GridQuery {
    OpenGrid {
        session: Uuid,
        generation: Uuid,
        rows: u16,
        cols: u16,
        back: u64,
    },
    Grid {
        snapshot: Uuid,
        start: u64,
        rows: u16,
        cols: u16,
    },
    GridColumns {
        snapshot: Uuid,
        start: u64,
        rows: u16,
        cols: u16,
        column_start: u16,
    },
    Close {
        snapshot: Uuid,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub id: Uuid,
    pub query: GridQuery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PageMeta {
    pub snapshot: Uuid,
    pub identity: PaneIdentity,
    pub sequence: u64,
    pub total_rows: u64,
    pub start: u64,
    pub rows: u16,
    pub cols: u16,
    pub column_start: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingKind {
    Open {
        identity: PaneIdentity,
        dimensions: Dimensions,
    },
    Grid {
        identity: PaneIdentity,
        snapshot: Uuid,
        dimensions: Dimensions,
        column_start: u16,
    },
    Close(Uuid),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pending {
    id: Uuid,
    kind: PendingKind,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct HistoryViewport {
    desired_identity: Option<PaneIdentity>,
    visible: Option<PageMeta>,
    page: Option<protocol::HistoryGridPage>,
    pending: Option<Pending>,
    close_after_pending: Option<Uuid>,
    desired_back: u64,
    desired_top: bool,
    desired_column_start: u16,
    desired_dimensions: Option<Dimensions>,
    queued_lines: i64,
    needs_query: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Completion {
    AcceptedPage(PageMeta),
    Closed,
    Stale,
    Failed,
    Ignored,
}

impl HistoryViewport {
    pub(crate) fn columns_start(&self) -> u16 {
        self.desired_column_start
    }

    pub(crate) fn reset_columns(&mut self) {
        self.set_column_start(0);
    }

    pub(crate) fn last_column_page(&mut self, cols: u16) {
        let max = self.page.as_ref().map_or(0, |page| {
            page.rows
                .iter()
                .map(|row| row.source_columns)
                .max()
                .unwrap_or(0)
                .saturating_sub(cols)
        });
        self.set_column_start(max.min(MAX_GRID_COLS.saturating_sub(cols)));
    }

    pub(crate) fn pan_columns(&mut self, delta: i16) {
        let Some(dimensions) = self.desired_dimensions else {
            return;
        };
        let max = MAX_GRID_COLS.saturating_sub(dimensions.cols);
        self.set_column_start(
            self.desired_column_start
                .saturating_add_signed(delta)
                .min(max),
        );
    }

    fn set_column_start(&mut self, start: u16) {
        if self.desired_identity.is_some() && self.desired_column_start != start {
            self.desired_column_start = start;
            self.needs_query = true;
            self.error = None;
        }
    }

    pub(crate) fn visible(&self) -> Option<PageMeta> {
        self.visible
    }

    pub(crate) fn displayed_page(&self) -> Option<&protocol::HistoryGridPage> {
        self.page.as_ref()
    }

    pub(crate) fn active_identity(&self) -> Option<PaneIdentity> {
        self.desired_identity
    }

    pub(crate) fn is_live(&self) -> bool {
        self.desired_identity.is_none()
    }

    /// Enter history at the requested distance behind the live bottom.
    pub(crate) fn open(&mut self, identity: PaneIdentity, rows: u16, cols: u16, back: u64) {
        let dimensions = Dimensions::bounded(rows, cols);
        if self.desired_identity != Some(identity) {
            if let Some(page) = self.visible.take() {
                self.close_after_pending = Some(page.snapshot);
            }
            self.page = None;
            self.desired_back = 0;
            self.desired_column_start = 0;
        }
        self.desired_identity = Some(identity);
        self.desired_dimensions = dimensions;
        if let Some(dimensions) = dimensions {
            self.desired_column_start = self
                .desired_column_start
                .min(MAX_GRID_COLS.saturating_sub(dimensions.cols));
        }
        self.desired_back = back.max(1);
        self.desired_top = false;
        self.needs_query = dimensions.is_some();
        self.error = dimensions
            .is_none()
            .then(|| "Размер viewport истории некорректен".into());
    }

    /// Coalesce line input into one bounded desired offset. Positive is up.
    pub(crate) fn scroll_lines(&mut self, delta: i64, rows: u16, cols: u16) {
        let Some(identity) = self.desired_identity else {
            return;
        };
        let Some(dimensions) = Dimensions::bounded(rows, cols) else {
            self.error = Some("Размер viewport истории некорректен".into());
            return;
        };
        self.desired_dimensions = Some(dimensions);
        self.desired_column_start = self
            .desired_column_start
            .min(MAX_GRID_COLS.saturating_sub(dimensions.cols));
        if self.desired_top
            && let Some(page) = self.visible
        {
            self.desired_back = page
                .total_rows
                .saturating_sub(page.start.saturating_add(u64::from(dimensions.rows)));
        }
        let bounded_delta = delta.clamp(-MAX_QUEUED_LINES, MAX_QUEUED_LINES);
        if self.pending.is_some() {
            self.queued_lines = self
                .queued_lines
                .saturating_add(bounded_delta)
                .clamp(-MAX_QUEUED_LINES, MAX_QUEUED_LINES);
        }
        if bounded_delta < 0 {
            let moved = self
                .desired_back
                .saturating_sub(bounded_delta.unsigned_abs());
            if moved == 0 {
                self.end_live();
                return;
            }
            self.desired_back = moved;
        } else if bounded_delta > 0 {
            self.desired_back = self.desired_back.saturating_add(bounded_delta as u64);
        }
        self.desired_top = false;
        self.needs_query = true;
        self.error = None;
        self.desired_identity = Some(identity);
    }

    pub(crate) fn page_step(&mut self, direction: i8, rows: u16, cols: u16) {
        let lines = i64::from(rows.max(1));
        self.scroll_lines(lines * i64::from(direction.signum()), rows, cols);
    }

    pub(crate) fn home(&mut self, rows: u16, cols: u16) {
        if self.desired_identity.is_none() || Dimensions::bounded(rows, cols).is_none() {
            return;
        }
        self.desired_dimensions = Dimensions::bounded(rows, cols);
        if let Some(dimensions) = self.desired_dimensions {
            self.desired_column_start = self
                .desired_column_start
                .min(MAX_GRID_COLS.saturating_sub(dimensions.cols));
        }
        if let (Some(page), Some(dimensions)) = (self.visible, self.desired_dimensions) {
            self.desired_back = page.total_rows.saturating_sub(u64::from(dimensions.rows));
        }
        self.desired_top = true;
        self.needs_query = true;
        self.error = None;
    }

    /// Return to live output. A pending open is allowed to finish only so its
    /// newly-created host snapshot can be explicitly closed.
    pub(crate) fn end_live(&mut self) {
        let visible_snapshot = self.visible.take().map(|page| page.snapshot);
        self.desired_identity = None;
        self.visible = None;
        self.page = None;
        self.desired_back = 0;
        self.desired_column_start = 0;
        self.desired_top = false;
        self.queued_lines = 0;
        self.needs_query = false;
        self.error = None;
        if let Some(Pending {
            kind: PendingKind::Grid { snapshot, .. },
            ..
        }) = self.pending.as_ref()
        {
            self.close_after_pending = Some(*snapshot);
        } else if let Some(snapshot) = visible_snapshot {
            self.close_after_pending = Some(snapshot);
        }
    }

    /// Invalidate a viewport when the pane's generation changes or the host
    /// no longer exposes the exact session-generation pair.
    pub(crate) fn reconcile_generation(&mut self, current: Option<PaneIdentity>) {
        if self.desired_identity.is_some() && current != self.desired_identity {
            self.end_live();
        }
    }

    /// Produce at most one query. Dimension and wheel changes during an
    /// in-flight request are coalesced into the next query after its receipt.
    pub(crate) fn prepare_next(
        &mut self,
        current: Option<PaneIdentity>,
        rows: u16,
        cols: u16,
    ) -> Option<Request> {
        self.reconcile_generation(current);
        if let Some(pending) = &self.pending {
            let _ = pending;
            return None;
        }
        if let Some(snapshot) = self.close_after_pending.take() {
            let request = Request {
                id: Uuid::new_v4(),
                query: GridQuery::Close { snapshot },
            };
            self.pending = Some(Pending {
                id: request.id,
                kind: PendingKind::Close(snapshot),
            });
            return Some(request);
        }
        let identity = self.desired_identity?;
        let dimensions = Dimensions::bounded(rows, cols)?;
        if self.desired_dimensions != Some(dimensions) {
            self.desired_dimensions = Some(dimensions);
            self.desired_column_start = self
                .desired_column_start
                .min(MAX_GRID_COLS.saturating_sub(dimensions.cols));
            self.needs_query = true;
        }
        if !self.needs_query {
            return None;
        }
        let request = if let Some(page) = self.visible {
            let start = if self.desired_top {
                0
            } else {
                page.total_rows
                    .saturating_sub(u64::from(dimensions.rows).saturating_add(self.desired_back))
            };
            Request {
                id: Uuid::new_v4(),
                query: if self.desired_column_start == 0 {
                    GridQuery::Grid {
                        snapshot: page.snapshot,
                        start,
                        rows: dimensions.rows,
                        cols: dimensions.cols,
                    }
                } else {
                    GridQuery::GridColumns {
                        snapshot: page.snapshot,
                        start,
                        rows: dimensions.rows,
                        cols: dimensions.cols,
                        column_start: self.desired_column_start,
                    }
                },
            }
        } else {
            Request {
                id: Uuid::new_v4(),
                query: GridQuery::OpenGrid {
                    session: identity.session,
                    generation: identity.generation,
                    rows: dimensions.rows,
                    cols: dimensions.cols,
                    back: if self.desired_top {
                        u64::MAX
                    } else {
                        self.desired_back
                    },
                },
            }
        };
        let kind = match &request.query {
            GridQuery::OpenGrid { .. } => PendingKind::Open {
                identity,
                dimensions,
            },
            GridQuery::Grid { snapshot, .. } => PendingKind::Grid {
                identity,
                snapshot: *snapshot,
                dimensions,
                column_start: 0,
            },
            GridQuery::GridColumns {
                snapshot,
                column_start,
                ..
            } => PendingKind::Grid {
                identity,
                snapshot: *snapshot,
                dimensions,
                column_start: *column_start,
            },
            GridQuery::Close { .. } => unreachable!(),
        };
        self.pending = Some(Pending {
            id: request.id,
            kind,
        });
        self.needs_query = false;
        Some(request)
    }

    pub(crate) fn complete_page(
        &mut self,
        request_id: Uuid,
        result: Result<protocol::HistoryGridPage, String>,
        current: Option<PaneIdentity>,
    ) -> Completion {
        let Some(pending) = self
            .pending
            .as_ref()
            .filter(|pending| pending.id == request_id)
            .cloned()
        else {
            return Completion::Ignored;
        };
        self.pending = None;
        let had_pending_scroll = self.queued_lines != 0;
        self.queued_lines = 0;
        match result {
            Ok(page) => {
                let page_meta = PageMeta {
                    snapshot: page.snapshot,
                    identity: PaneIdentity {
                        session: page.session,
                        generation: page.generation,
                    },
                    sequence: page.sequence,
                    total_rows: page.total_rows,
                    start: page.start,
                    rows: page.rows_requested,
                    cols: page.cols,
                    column_start: page.column_start,
                };
                let expected = match pending.kind {
                    PendingKind::Open {
                        identity,
                        dimensions,
                    } => Some((identity, None, dimensions, 0)),
                    PendingKind::Grid {
                        identity,
                        snapshot,
                        dimensions,
                        column_start,
                    } => Some((identity, Some(snapshot), dimensions, column_start)),
                    PendingKind::Close(_) => None,
                };
                let valid =
                    expected.is_some_and(|(identity, snapshot, dimensions, column_start)| {
                        page_meta.identity == identity
                            && snapshot.is_none_or(|snapshot| page_meta.snapshot == snapshot)
                            && page_meta.rows == dimensions.rows
                            && page_meta.cols == dimensions.cols
                            && page_meta.column_start == column_start
                            && u32::from(page_meta.rows) * u32::from(page_meta.cols)
                                <= MAX_GRID_CELLS
                    });
                let expected_identity = expected.map(|value| value.0);
                if !valid
                    || current != expected_identity
                    || self.desired_identity != expected_identity
                {
                    self.close_after_pending = Some(page_meta.snapshot);
                    self.visible = None;
                    self.page = None;
                    if self.desired_identity == expected_identity {
                        self.desired_identity = None;
                    }
                    self.needs_query = self.desired_identity.is_some();
                    return Completion::Stale;
                }
                self.visible = Some(page_meta);
                self.page = Some(page);
                self.error = None;
                if !self.desired_top && !had_pending_scroll {
                    self.desired_back = page_meta
                        .total_rows
                        .saturating_sub(page_meta.start.saturating_add(u64::from(page_meta.rows)));
                }
                if let Some(snapshot) = self.close_after_pending.take()
                    && snapshot != page_meta.snapshot
                {
                    self.close_after_pending = Some(snapshot);
                }
                self.needs_query = self.desired_dimensions.is_some_and(|dimensions| {
                    let expected_start = if self.desired_top {
                        0
                    } else {
                        page_meta.total_rows.saturating_sub(
                            u64::from(dimensions.rows).saturating_add(self.desired_back),
                        )
                    };
                    page_meta.start != expected_start
                        || page_meta.rows != dimensions.rows
                        || page_meta.cols != dimensions.cols
                        || page_meta.column_start != self.desired_column_start
                });
                Completion::AcceptedPage(page_meta)
            }
            Err(error) => {
                self.error = Some(error.chars().take(512).collect());
                self.needs_query = false;
                Completion::Failed
            }
        }
    }

    pub(crate) fn complete_close(&mut self, request_id: Uuid, snapshot: Uuid) -> Completion {
        let Some(pending) = self
            .pending
            .as_ref()
            .filter(|pending| pending.id == request_id)
            .cloned()
        else {
            return Completion::Ignored;
        };
        if pending.kind != PendingKind::Close(snapshot) {
            return Completion::Ignored;
        }
        self.pending = None;
        if self.visible.is_some_and(|page| page.snapshot == snapshot) {
            self.visible = None;
            self.page = None;
        }
        Completion::Closed
    }

    pub(crate) fn fail_request(&mut self, request_id: Uuid, message: &str) -> Completion {
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == request_id)
        {
            return Completion::Ignored;
        }
        self.pending = None;
        self.error = Some(message.chars().take(512).collect());
        self.needs_query = false;
        Completion::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(session: u128, generation: u128) -> PaneIdentity {
        PaneIdentity {
            session: Uuid::from_u128(session),
            generation: Uuid::from_u128(generation),
        }
    }

    fn page(snapshot: u128, identity: PaneIdentity, start: u64) -> protocol::HistoryGridPage {
        protocol::HistoryGridPage {
            snapshot: Uuid::from_u128(snapshot),
            session: identity.session,
            generation: identity.generation,
            sequence: 12,
            total_rows: 400,
            start,
            rows_requested: 24,
            rows: vec![protocol::HistoryGridRow {
                row: start,
                source_columns: 200,
                boundary: None,
                cells: vec![],
                palette_overrides: vec![],
            }],
            cols: 80,
            hyperlinks: vec![],
            horizontal_clipped: false,
            column_start: 0,
            warnings: vec![],
        }
    }

    #[test]
    fn late_open_after_return_live_is_closed_without_displaying_history() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 1);
        let request = state.prepare_next(Some(pane), 24, 80).unwrap();
        state.end_live();
        assert_eq!(
            state.complete_page(request.id, Ok(page(3, pane, 375)), Some(pane)),
            Completion::Stale
        );
        assert!(state.visible().is_none());
        let close = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert_eq!(
            close.query,
            GridQuery::Close {
                snapshot: Uuid::from_u128(3)
            }
        );
    }

    #[test]
    fn switching_viewport_while_opening_closes_old_snapshot_then_opens_latest_pane() {
        let first_pane = identity(1, 2);
        let latest_pane = identity(3, 4);
        let mut state = HistoryViewport::default();
        state.open(first_pane, 24, 80, 1);
        let open = state.prepare_next(Some(first_pane), 24, 80).unwrap();
        state.open(latest_pane, 30, 90, 2);
        assert_eq!(
            state.complete_page(open.id, Ok(page(6, first_pane, 375)), Some(latest_pane)),
            Completion::Stale
        );
        assert_eq!(state.active_identity(), Some(latest_pane));
        let close = state.prepare_next(Some(latest_pane), 30, 90).unwrap();
        assert_eq!(
            close.query,
            GridQuery::Close {
                snapshot: Uuid::from_u128(6)
            }
        );
        assert_eq!(
            state.complete_close(close.id, Uuid::from_u128(6)),
            Completion::Closed
        );
        assert!(
            matches!(state.prepare_next(Some(latest_pane), 30, 90).unwrap().query,
            GridQuery::OpenGrid { session, generation, rows:30, cols:90, back:2 }
                if session == latest_pane.session && generation == latest_pane.generation)
        );
    }

    #[test]
    fn returning_live_closes_an_already_visible_snapshot() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 1);
        let open = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(matches!(
            state.complete_page(open.id, Ok(page(7, pane, 375)), Some(pane)),
            Completion::AcceptedPage(_)
        ));
        state.end_live();
        assert!(state.displayed_page().is_none());
        let close = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert_eq!(
            close.query,
            GridQuery::Close {
                snapshot: Uuid::from_u128(7)
            }
        );
    }

    #[test]
    fn generation_change_discards_page_and_dimension_request_is_bounded() {
        let pane = identity(1, 2);
        let changed = identity(1, 9);
        let mut state = HistoryViewport::default();
        state.open(pane, 999, 4096, 10);
        let request = state.prepare_next(Some(pane), 999, 4096).unwrap();
        assert!(matches!(
            &request.query,
            GridQuery::OpenGrid {
                rows: 32,
                cols: 1024,
                ..
            }
        ));
        assert_eq!(
            state.complete_page(request.id, Ok(page(4, pane, 366)), Some(changed)),
            Completion::Stale
        );
        assert!(state.visible().is_none());
    }

    #[test]
    fn wheel_burst_coalesces_to_latest_back_offset_after_one_request() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 1);
        let first = state.prepare_next(Some(pane), 24, 80).unwrap();
        state.scroll_lines(50, 24, 80);
        state.scroll_lines(25, 24, 80);
        assert!(state.prepare_next(Some(pane), 24, 80).is_none());
        state.complete_page(first.id, Ok(page(5, pane, 375)), Some(pane));
        let next = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(
            matches!(&next.query, GridQuery::Grid { snapshot, start:300, rows:24, cols:80 } if *snapshot == Uuid::from_u128(5))
        );
    }

    #[test]
    fn home_then_one_line_down_anchors_at_the_top() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 1);
        let open = state.prepare_next(Some(pane), 24, 80).unwrap();
        state.complete_page(open.id, Ok(page(5, pane, 375)), Some(pane));
        state.home(24, 80);
        let home = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(matches!(home.query, GridQuery::Grid { start: 0, .. }));
        state.complete_page(home.id, Ok(page(5, pane, 0)), Some(pane));
        state.scroll_lines(-1, 24, 80);
        let down = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(matches!(down.query, GridQuery::Grid { start: 1, .. }));
    }

    #[test]
    fn moving_down_after_host_clamps_overscroll_at_history_start() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 2_000_000);
        let open = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(matches!(
            open.query,
            GridQuery::OpenGrid {
                back: 2_000_000,
                ..
            }
        ));
        state.complete_page(open.id, Ok(page(5, pane, 0)), Some(pane));
        state.scroll_lines(-1, 24, 80);
        let down = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(matches!(down.query, GridQuery::Grid { start: 1, .. }));
    }

    #[test]
    fn history_navigation_is_not_capped_at_one_million_rows() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 1);
        let open = state.prepare_next(Some(pane), 24, 80).unwrap();
        let mut large = page(5, pane, 1_999_975);
        large.total_rows = 2_000_000;
        state.complete_page(open.id, Ok(large), Some(pane));
        state.scroll_lines(1, 24, 80);
        let up = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(matches!(
            up.query,
            GridQuery::Grid {
                start: 1_999_974,
                ..
            }
        ));
    }

    #[test]
    fn pending_wheel_delta_is_coalesced_once_without_background_query_loop() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 1);
        let open = state.prepare_next(Some(pane), 24, 80).unwrap();
        state.scroll_lines(5, 24, 80);
        assert!(state.prepare_next(Some(pane), 24, 80).is_none());
        state.complete_page(open.id, Ok(page(5, pane, 375)), Some(pane));
        let next = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(matches!(next.query, GridQuery::Grid { start: 370, .. }));
        state.complete_page(next.id, Ok(page(5, pane, 370)), Some(pane));
        assert!(state.prepare_next(Some(pane), 24, 80).is_none());
    }

    #[test]
    fn explicit_end_clears_view_and_errors_do_not_retry_without_new_input() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 1);
        let request = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert_eq!(
            state.fail_request(request.id, "unavailable"),
            Completion::Failed
        );
        assert!(state.prepare_next(Some(pane), 24, 80).is_none());
        state.scroll_lines(1, 24, 80);
        assert!(state.prepare_next(Some(pane), 24, 80).is_some());
        state.end_live();
        assert!(state.is_live());
        assert!(state.visible().is_none());
    }

    #[test]
    fn horizontal_intent_coalesces_across_open_and_stale_pages() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 1);
        let open = state.prepare_next(Some(pane), 24, 80).unwrap();
        state.pan_columns(8);
        state.pan_columns(8);
        state.complete_page(open.id, Ok(page(5, pane, 375)), Some(pane));
        let next = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(matches!(
            next.query,
            GridQuery::GridColumns {
                column_start: 16,
                ..
            }
        ));
        state.pan_columns(8);
        let mut old = page(5, pane, 375);
        old.column_start = 16;
        assert_eq!(
            state.complete_page(next.id, Ok(old), Some(pane)),
            Completion::AcceptedPage(PageMeta {
                snapshot: Uuid::from_u128(5),
                identity: pane,
                sequence: 12,
                total_rows: 400,
                start: 375,
                rows: 24,
                cols: 80,
                column_start: 16,
            })
        );
        assert_eq!(state.columns_start(), 24);
        let latest = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(matches!(
            latest.query,
            GridQuery::GridColumns {
                column_start: 24,
                ..
            }
        ));
    }

    #[test]
    fn horizontal_offset_is_clamped_on_resize_and_old_zero_query_is_preserved() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 1);
        let open = state.prepare_next(Some(pane), 24, 80).unwrap();
        state.complete_page(open.id, Ok(page(5, pane, 375)), Some(pane));
        assert!(state.prepare_next(Some(pane), 24, 80).is_none());
        assert!(matches!(
            state.prepare_next(Some(pane), 30, 80),
            Some(Request {
                query: GridQuery::Grid { .. },
                ..
            })
        ));
        let resize = state.pending.as_ref().unwrap().id;
        let mut resized = page(5, pane, 370);
        resized.rows_requested = 30;
        assert!(matches!(
            state.complete_page(resize, Ok(resized), Some(pane)),
            Completion::AcceptedPage(_)
        ));
        state.last_column_page(80);
        assert_eq!(state.columns_start(), 120);
        state.pan_columns(500);
        assert!(state.columns_start() <= 944);
    }

    #[test]
    fn vertical_and_horizontal_offsets_are_combined_and_wrong_column_echo_is_rejected() {
        let pane = identity(1, 2);
        let mut state = HistoryViewport::default();
        state.open(pane, 24, 80, 1);
        let open = state.prepare_next(Some(pane), 24, 80).unwrap();
        state.complete_page(open.id, Ok(page(5, pane, 375)), Some(pane));
        state.pan_columns(8);
        state.scroll_lines(4, 24, 80);
        let request = state.prepare_next(Some(pane), 24, 80).unwrap();
        assert!(matches!(
            request.query,
            GridQuery::GridColumns {
                start: 371,
                column_start: 8,
                ..
            }
        ));
        let mut wrong = page(5, pane, 371);
        wrong.column_start = 0;
        assert_eq!(
            state.complete_page(request.id, Ok(wrong), Some(pane)),
            Completion::Stale
        );
        assert!(state.displayed_page().is_none());
    }
}
