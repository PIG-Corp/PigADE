//! Bounded, immutable text paging and literal search over one restored history snapshot.
//!
//! All projection comes from the host terminal model. This worker never parses
//! ANSI and never publishes or mutates a live terminal.

use crate::{CommandRejection, PROTOCOL, Response, history, restore_history_view};
use anyhow::{Context, Result, ensure};
use pigade_terminal::{Terminal, history_text::TextChunk};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use uuid::Uuid;

const QUEUE_CAPACITY: usize = 4;
const SNAPSHOT_LIMIT: usize = 2;
const SNAPSHOT_IDLE: Duration = Duration::from_secs(120);
const PAGE_ROW_LIMIT: usize = 128;
const PAGE_BYTE_LIMIT: usize = 64 * 1024;
const TEXT_CHUNK_LIMIT: usize = 32 * 1024;
const SEARCH_ROW_LIMIT: usize = 128;
const SEARCH_BYTE_LIMIT: usize = 256 * 1024;
const SEARCH_TEXT_LIMIT: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    pub row: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Query {
    Open {
        session: Uuid,
        generation: Uuid,
    },
    Page {
        snapshot: Uuid,
        start: Cursor,
    },
    Search {
        snapshot: Uuid,
        text: String,
        cursor: Option<Cursor>,
    },
    Close {
        snapshot: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reply {
    Page { page: Page },
    Search { result: SearchResult },
    Closed { snapshot: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub snapshot: Uuid,
    pub session: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub total_rows: u64,
    pub start: Cursor,
    pub rows: Vec<TextRow>,
    pub next: Option<Cursor>,
    pub previous: Option<Cursor>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextRow {
    pub row: u64,
    pub chunk: TextChunk,
    pub boundary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub snapshot: Uuid,
    pub session: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub found: Option<Cursor>,
    pub next: Option<Cursor>,
    pub scanned_bytes: u64,
}

struct Snapshot {
    id: Uuid,
    session: Uuid,
    generation: Uuid,
    sequence: u64,
    terminal: Terminal,
    history: Arc<history::History>,
    warnings: Vec<String>,
    total_rows: u64,
    last_used: Instant,
    decoded: Option<(
        u64,
        history::RecordMetadata,
        Option<pigade_terminal::history::DecodedRow>,
    )>,
    search: Option<SearchState>,
}

struct SearchState {
    text: String,
    next: Option<Cursor>,
    window: String,
    origins: VecDeque<(usize, Cursor)>,
}

impl SearchState {
    fn new(text: String) -> Self {
        Self {
            text,
            next: Some(Cursor { row: 0, offset: 0 }),
            window: String::new(),
            origins: VecDeque::new(),
        }
    }

    fn hard_break(&mut self) {
        self.window.clear();
        self.origins.clear();
    }

    fn push(&mut self, ch: char, origin: Cursor, after: Cursor) -> Option<(Cursor, Cursor)> {
        // Keep at most query.len() - 1 bytes so a match can cross a chunk or
        // soft-wrap boundary without retaining an unbounded history prefix.
        let keep = self.text.len().saturating_sub(1);
        while self.window.len() > keep {
            let removed = self
                .window
                .chars()
                .next()
                .expect("nonempty window")
                .len_utf8();
            self.window.drain(..removed);
            while self.origins.front().is_some_and(|(at, _)| *at < removed) {
                self.origins.pop_front();
            }
            for (at, _) in &mut self.origins {
                *at -= removed;
            }
        }
        let at = self.window.len();
        self.window.push(ch);
        self.origins.push_back((at, origin));
        if self.window.ends_with(&self.text) {
            let start_at = self.window.len().checked_sub(self.text.len())?;
            let start = self.origins.iter().find(|(at, _)| *at == start_at)?.1;
            return Some((start, after));
        }
        None
    }
}

enum Task {
    Query {
        query: Query,
        source: Option<(PathBuf, u64)>,
        reply: mpsc::SyncSender<Response>,
    },
}

/// Single ordered worker: snapshot restore, paging, search, and eviction all
/// share one bounded queue and one LRU state owner.
pub(crate) struct Reader {
    sender: mpsc::SyncSender<Task>,
}

impl Reader {
    pub(crate) fn new() -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Task>(QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("pigade-history-view".into())
            .spawn(move || {
                let mut snapshots = VecDeque::<Snapshot>::new();
                loop {
                    match receiver.recv_timeout(SNAPSHOT_IDLE) {
                        Ok(Task::Query {
                            query,
                            source,
                            reply,
                        }) => {
                            let response = process(&mut snapshots, query, source);
                            let _ = reply.try_send(response);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            snapshots
                                .retain(|snapshot| snapshot.last_used.elapsed() < SNAPSHOT_IDLE);
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Self { sender })
    }

    pub(crate) fn submit(
        &self,
        query: Query,
        source: Option<(PathBuf, u64)>,
        reply: mpsc::SyncSender<Response>,
    ) {
        match self.sender.try_send(Task::Query {
            query,
            source,
            reply,
        }) {
            Ok(()) => (),
            Err(mpsc::TrySendError::Full(Task::Query { reply, .. })) => {
                let _ = reply.try_send(response(
                    None,
                    Some(CommandRejection::QueueFull),
                    Some(CommandRejection::QueueFull.to_string()),
                ));
            }
            Err(mpsc::TrySendError::Disconnected(Task::Query { reply, .. })) => {
                let _ = reply.try_send(response(
                    None,
                    None,
                    Some("history view worker unavailable".into()),
                ));
            }
        }
    }
}

fn response(
    history_view: Option<Reply>,
    rejection: Option<CommandRejection>,
    error: Option<String>,
) -> Response {
    Response {
        protocol: PROTOCOL,
        views: Vec::new(),
        error,
        rejected_before_admission: rejection,
        catalog: None,
        selection: None,
        context: None,
        launch_preview: None,
        launch_phase: None,
        catalog_notice: None,
        recovery: None,
        selected: None,
        history_chunk: None,
        change_token: None,
        generations: None,
        generation_selection: None,
        history_view,
        providers: None,
        template_run: None,
        template_runs: None,
        panel_directory: None,
        workspace_git_snapshot: None,
    }
}

fn process(
    snapshots: &mut VecDeque<Snapshot>,
    query: Query,
    source: Option<(PathBuf, u64)>,
) -> Response {
    snapshots.retain(|snapshot| snapshot.last_used.elapsed() < SNAPSHOT_IDLE);
    match query {
        Query::Open {
            session,
            generation,
        } => {
            let Some((path, sequence)) = source else {
                return error("open requires a validated snapshot source");
            };
            if snapshots.len() == SNAPSHOT_LIMIT {
                snapshots.pop_front();
            }
            match open_snapshot(&path, session, generation, sequence) {
                Ok(snapshot) => {
                    let start = Cursor {
                        row: snapshot.total_rows.saturating_sub(PAGE_ROW_LIMIT as u64),
                        offset: 0,
                    };
                    snapshots.push_back(snapshot);
                    match page(snapshots.back_mut().expect("just inserted"), start) {
                        Ok(page) => response(Some(Reply::Page { page }), None, None),
                        Err(e) => {
                            snapshots.pop_back();
                            error(&format!("history page failed: {e:#}"))
                        }
                    }
                }
                Err(e) => error(&format!("history restore failed: {e:#}")),
            }
        }
        Query::Page { snapshot, start } => {
            if source.is_some() {
                return error("page cannot supply a new source");
            }
            match take_snapshot(snapshots, snapshot) {
                Ok(mut s) => {
                    let result = page(&mut s, start);
                    snapshots.push_back(s);
                    result
                        .map(|page| response(Some(Reply::Page { page }), None, None))
                        .unwrap_or_else(|e| error(&format!("history page failed: {e:#}")))
                }
                Err(e) => error(&e),
            }
        }
        Query::Search {
            snapshot,
            text,
            cursor,
        } => {
            if source.is_some() {
                return error("search cannot supply a new source");
            }
            match take_snapshot(snapshots, snapshot) {
                Ok(mut s) => {
                    let result = search(&mut s, text, cursor);
                    snapshots.push_back(s);
                    result
                        .map(|result| response(Some(Reply::Search { result }), None, None))
                        .unwrap_or_else(|e| error(&format!("history search failed: {e:#}")))
                }
                Err(e) => error(&e),
            }
        }
        Query::Close { snapshot } => {
            if source.is_some() {
                return error("close cannot supply a new source");
            }
            let Some(index) = snapshots.iter().position(|s| s.id == snapshot) else {
                return error("history view snapshot expired or closed");
            };
            snapshots.remove(index);
            response(Some(Reply::Closed { snapshot }), None, None)
        }
    }
}

fn error(message: &str) -> Response {
    response(None, None, Some(message.to_owned()))
}

fn take_snapshot(snapshots: &mut VecDeque<Snapshot>, id: Uuid) -> Result<Snapshot, String> {
    let Some(index) = snapshots.iter().position(|snapshot| snapshot.id == id) else {
        return Err("history view snapshot expired or closed".into());
    };
    let mut snapshot = snapshots.remove(index).expect("index found");
    snapshot.last_used = Instant::now();
    Ok(snapshot)
}

fn open_snapshot(path: &Path, session: Uuid, generation: Uuid, sequence: u64) -> Result<Snapshot> {
    let (terminal, history, warnings) = restore_history_view(path, session, generation, sequence)?;
    let cold = history.summary().records;
    let hot = u64::try_from(terminal.normal_text_row_count()).context("hot row count overflow")?;
    let total_rows = cold
        .checked_add(hot)
        .context("history row count overflow")?;
    Ok(Snapshot {
        id: Uuid::new_v4(),
        session,
        generation,
        sequence,
        terminal,
        history,
        warnings,
        total_rows,
        last_used: Instant::now(),
        decoded: None,
        search: None,
    })
}

fn cold_record(
    snapshot: &mut Snapshot,
    ordinal: u64,
) -> Result<(
    &history::RecordMetadata,
    Option<&pigade_terminal::history::DecodedRow>,
)> {
    if !snapshot
        .decoded
        .as_ref()
        .is_some_and(|(cached, _, _)| *cached == ordinal)
    {
        let bytes = snapshot.history.record(ordinal)?;
        let decoded = history::decode_record(&bytes)?;
        snapshot.decoded = Some((ordinal, decoded.0, decoded.1));
    }
    let (_, metadata, row) = snapshot.decoded.as_ref().expect("decoded row cached");
    Ok((metadata, row.as_ref()))
}

fn text_chunk(
    snapshot: &mut Snapshot,
    row: u64,
    offset: u64,
    max_bytes: usize,
) -> Result<(TextChunk, Option<String>)> {
    let cold_rows = snapshot.history.summary().records;
    if row < cold_rows {
        let (metadata, decoded) = cold_record(snapshot, row)?;
        match decoded {
            Some(decoded) => Ok((
                pigade_terminal::history_text::decoded_chunk(decoded, offset, max_bytes)?,
                None,
            )),
            None => {
                ensure!(offset == 0, "boundary cursor offset must be zero");
                Ok((
                    TextChunk {
                        text: String::new(),
                        total_bytes: 0,
                        offset: 0,
                        next_offset: None,
                        wrap: false,
                    },
                    Some(metadata.reason.clone()),
                ))
            }
        }
    } else {
        let hot_index = usize::try_from(row - cold_rows).context("hot row index overflow")?;
        Ok((
            snapshot
                .terminal
                .normal_text_chunk(hot_index, offset, max_bytes)?,
            None,
        ))
    }
}

fn cursor_after(row: u64, chunk: &TextChunk) -> Result<Cursor> {
    if let Some(offset) = chunk.next_offset {
        return Ok(Cursor { row, offset });
    }
    Ok(Cursor {
        row: row.checked_add(1).context("history cursor row overflow")?,
        offset: 0,
    })
}

fn chunk_allowance(remaining: usize, per_chunk_limit: usize) -> Option<usize> {
    (remaining >= 4).then_some(remaining.min(per_chunk_limit))
}

fn page(snapshot: &mut Snapshot, start: Cursor) -> Result<Page> {
    ensure!(
        start.row <= snapshot.total_rows,
        "page cursor row outside snapshot"
    );
    ensure!(
        start.row < snapshot.total_rows || start.offset == 0,
        "end-of-history cursor offset must be zero"
    );
    let mut cursor = start;
    let mut rows = Vec::new();
    let mut bytes = 0usize;
    while cursor.row < snapshot.total_rows && rows.len() < PAGE_ROW_LIMIT && bytes < PAGE_BYTE_LIMIT
    {
        let Some(allowance) = chunk_allowance(PAGE_BYTE_LIMIT - bytes, TEXT_CHUNK_LIMIT) else {
            break;
        };
        // A UTF-8 scalar is at most four bytes. Leave the cursor untouched if
        // this page has too little room for the next complete scalar.
        let (chunk, boundary) = text_chunk(snapshot, cursor.row, cursor.offset, allowance)?;
        ensure!(
            chunk.text.len() <= allowance,
            "history chunk exceeded page budget"
        );
        bytes = bytes
            .checked_add(chunk.text.len())
            .context("page byte count overflow")?;
        let row = cursor.row;
        cursor = cursor_after(row, &chunk)?;
        rows.push(TextRow {
            row,
            chunk,
            boundary,
        });
    }
    let next = (cursor.row < snapshot.total_rows).then_some(cursor);
    let previous = (start.row > 0 || start.offset > 0).then_some(Cursor {
        row: start.row.saturating_sub(PAGE_ROW_LIMIT as u64),
        offset: 0,
    });
    Ok(Page {
        snapshot: snapshot.id,
        session: snapshot.session,
        generation: snapshot.generation,
        sequence: snapshot.sequence,
        total_rows: snapshot.total_rows,
        start,
        rows,
        next,
        previous,
        warnings: snapshot.warnings.clone(),
    })
}

fn search(snapshot: &mut Snapshot, text: String, cursor: Option<Cursor>) -> Result<SearchResult> {
    validate_search_text(&text)?;
    if let Some(cursor) = cursor {
        let state = snapshot
            .search
            .as_ref()
            .context("stale history search cursor")?;
        validate_search_cursor(state, &text, cursor)?;
    } else {
        snapshot.search = Some(SearchState::new(text.clone()));
    }
    let mut state = snapshot.search.take().context("search state unavailable")?;
    let mut cursor = cursor.unwrap_or(Cursor { row: 0, offset: 0 });
    ensure!(
        cursor.row <= snapshot.total_rows,
        "search cursor row outside snapshot"
    );
    ensure!(
        cursor.row < snapshot.total_rows || cursor.offset == 0,
        "end-of-history search cursor offset must be zero"
    );
    let mut scanned_bytes = 0u64;
    let mut scanned_rows = 0usize;
    let mut found = None;
    while cursor.row < snapshot.total_rows
        && scanned_rows < SEARCH_ROW_LIMIT
        && scanned_bytes < SEARCH_BYTE_LIMIT as u64
    {
        let row_index = cursor.row;
        let Some(allowance) =
            chunk_allowance(SEARCH_BYTE_LIMIT - scanned_bytes as usize, TEXT_CHUNK_LIMIT)
        else {
            break;
        };
        // Keep the continuation at the first unscanned byte when fewer than
        // four bytes remain, so a multibyte scalar is never partially read.
        let (chunk, boundary) = text_chunk(snapshot, row_index, cursor.offset, allowance)?;
        scanned_rows += 1;
        if boundary.is_some() {
            state.hard_break();
        }
        for ch in chunk.text.chars() {
            scanned_bytes = scanned_bytes
                .checked_add(ch.len_utf8() as u64)
                .context("search byte count overflow")?;
            let origin = Cursor {
                row: row_index,
                offset: cursor.offset,
            };
            let after = Cursor {
                row: row_index,
                offset: cursor
                    .offset
                    .checked_add(ch.len_utf8() as u64)
                    .context("search offset overflow")?,
            };
            if let Some((start, end)) = state.push(ch, origin, after) {
                found = Some(start);
                state.next = Some(end);
                state.hard_break();
                break;
            }
            cursor.offset = after.offset;
        }
        if found.is_some() {
            break;
        }
        if let Some(offset) = chunk.next_offset {
            cursor = Cursor {
                row: row_index,
                offset,
            };
        } else {
            if !chunk.wrap {
                state.hard_break();
            }
            cursor = Cursor {
                row: row_index.checked_add(1).context("search row overflow")?,
                offset: 0,
            };
        }
        state.next = (cursor.row < snapshot.total_rows).then_some(cursor);
    }
    let next = state.next;
    let result = SearchResult {
        snapshot: snapshot.id,
        session: snapshot.session,
        generation: snapshot.generation,
        sequence: snapshot.sequence,
        found,
        next,
        scanned_bytes,
    };
    snapshot.search = Some(state);
    Ok(result)
}

fn validate_search_text(text: &str) -> Result<()> {
    ensure!(
        !text.is_empty() && text.len() <= SEARCH_TEXT_LIMIT,
        "search text must contain 1 to 512 UTF-8 bytes"
    );
    Ok(())
}

fn validate_search_cursor(state: &SearchState, text: &str, cursor: Cursor) -> Result<()> {
    ensure!(
        state.text == text && state.next == Some(cursor),
        "stale history search cursor"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_search_state_matches_across_utf8_chunk_boundary() {
        let mut state = SearchState::new("界x".into());
        assert_eq!(
            state.push(
                '界',
                Cursor { row: 2, offset: 0 },
                Cursor { row: 2, offset: 3 }
            ),
            None
        );
        assert_eq!(
            state.push(
                'x',
                Cursor { row: 2, offset: 3 },
                Cursor { row: 2, offset: 4 }
            ),
            Some((Cursor { row: 2, offset: 0 }, Cursor { row: 2, offset: 4 }))
        );
    }

    #[test]
    fn search_state_retains_soft_wrap_suffix_and_hard_break_clears_it() {
        let mut state = SearchState::new("ab".into());
        state.push(
            'a',
            Cursor { row: 0, offset: 0 },
            Cursor { row: 0, offset: 1 },
        );
        state.hard_break();
        assert_eq!(
            state.push(
                'b',
                Cursor { row: 1, offset: 0 },
                Cursor { row: 1, offset: 1 }
            ),
            None
        );
        state.push(
            'a',
            Cursor { row: 2, offset: 0 },
            Cursor { row: 2, offset: 1 },
        );
        assert_eq!(
            state.push(
                'b',
                Cursor { row: 3, offset: 0 },
                Cursor { row: 3, offset: 1 }
            ),
            Some((Cursor { row: 2, offset: 0 }, Cursor { row: 3, offset: 1 }))
        );
    }

    #[test]
    fn search_query_validation_and_stale_cursor_are_rejected() {
        assert!(validate_search_text("").is_err());
        assert!(validate_search_text(&"x".repeat(SEARCH_TEXT_LIMIT + 1)).is_err());
        let state = SearchState::new("needle".into());
        assert_eq!(state.next, Some(Cursor { row: 0, offset: 0 }));
        assert!(validate_search_cursor(&state, "needle", Cursor { row: 1, offset: 0 }).is_err());
        assert!(validate_search_cursor(&state, "different", Cursor { row: 0, offset: 0 }).is_err());
    }

    #[test]
    fn byte_budgets_leave_room_for_a_complete_utf8_scalar() {
        assert_eq!(chunk_allowance(0, TEXT_CHUNK_LIMIT), None);
        assert_eq!(chunk_allowance(1, TEXT_CHUNK_LIMIT), None);
        assert_eq!(chunk_allowance(2, TEXT_CHUNK_LIMIT), None);
        assert_eq!(chunk_allowance(3, TEXT_CHUNK_LIMIT), None);
        assert_eq!(chunk_allowance(4, TEXT_CHUNK_LIMIT), Some(4));
        assert_eq!(
            chunk_allowance(usize::MAX, TEXT_CHUNK_LIMIT),
            Some(TEXT_CHUNK_LIMIT)
        );
    }

    #[test]
    fn page_projects_cold_rows_boundary_and_hot_rows_without_materializing_history() -> Result<()> {
        use pigade_storage::JournalHeader;
        use pigade_terminal::{
            CORE_PROFILE, Geometry, HistoryObserver, HistoryRetirement, Terminal,
        };

        let directory = tempfile::tempdir()?;
        let session = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let header = JournalHeader {
            session_id: session,
            generation,
            profile: CORE_PROFILE.into(),
            initial_rows: 2,
            initial_cols: 8,
        };
        let history = history::History::live(directory.path(), &header)?;
        let mut terminal = Terminal::new(Geometry::new(2, 8)?, false);
        terminal.set_history_observer(Some(history.clone()));
        history.begin(1)?;
        terminal.output(b"COLD_TEXT_MARKER\r\n");
        let mut output = Vec::with_capacity(80 * 1024);
        for _ in 0..10_050 {
            output.extend_from_slice(b"filler\r\n");
        }
        output.extend_from_slice(b"hot text row\r\nlast hot row");
        terminal.output(&output);
        history.commit()?;
        history.begin(2)?;
        HistoryObserver::boundary(history.as_ref(), HistoryRetirement::Clear);
        history.commit()?;

        let cold = history.summary().records;
        let hot = terminal.normal_text_row_count() as u64;
        assert!(cold > 0 && hot > 0);
        let mut snapshot = Snapshot {
            id: Uuid::new_v4(),
            session,
            generation,
            sequence: 2,
            terminal,
            history,
            warnings: Vec::new(),
            total_rows: cold + hot,
            last_used: Instant::now(),
            decoded: None,
            search: None,
        };
        let page = page(&mut snapshot, Cursor { row: 0, offset: 0 })?;
        assert_eq!(page.total_rows, cold + hot);
        assert!(
            page.rows
                .iter()
                .any(|row| row.boundary.as_deref() == Some("clear"))
        );
        assert!(
            page.rows
                .iter()
                .any(|row| row.row >= cold && row.chunk.total_bytes > 0)
        );
        assert!(
            page.rows
                .iter()
                .any(|row| row.row < cold && row.chunk.total_bytes > 0)
        );
        Ok(())
    }
}
