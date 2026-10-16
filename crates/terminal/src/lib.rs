//! The host is the sole ANSI parser. A Frame is a render replica, NOT a checkpoint.
pub mod history;
pub use alacritty_terminal::{
    grid::{HistoryObserver, HistoryRetirement, Row as HistoryRow},
    term::cell::{Cell as HistoryCell, Flags as CellFlags},
    vte::ansi::Rgb as PaletteRgb,
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Osc52};
use alacritty_terminal::vte::ansi::{Processor, Rgb, Timeout};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const CHECKPOINT_BYTE_LIMIT: usize = 128 * 1024 * 1024;
struct CheckpointWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl CheckpointWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
}
impl std::io::Write for CheckpointWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let wanted = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("checkpoint length overflow"))?;
        if wanted > self.limit {
            return Err(std::io::Error::other(
                "checkpoint encoded byte budget exceeded",
            ));
        }
        if wanted > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(wanted)
                .min(self.limit);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(std::io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub const CORE_PROFILE: &str = "alacritty_terminal=0.26.0+pigade-state.2;vte=0.15.0+pigade-state.1;osc=65536-ignore-overflow;unicode-width=0.2.2;font=unfixed-prototype;history=10000;osc52=disabled;kitty=false;schema=2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Geometry {
    pub rows: u16,
    pub cols: u16,
}
impl<'de> Deserialize<'de> for Geometry {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            rows: u16,
            cols: u16,
        }
        let raw = Raw::deserialize(deserializer)?;
        Self::new(raw.rows, raw.cols).map_err(serde::de::Error::custom)
    }
}
impl Geometry {
    pub fn new(rows: u16, cols: u16) -> Result<Self> {
        ensure!(
            (1..=512).contains(&rows) && (2..=1024).contains(&cols),
            "geometry outside prototype bounds"
        );
        ensure!(
            rows as usize * cols as usize <= 32768,
            "prototype cell budget exceeded"
        );
        Ok(Self { rows, cols })
    }
}
impl Dimensions for Geometry {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub text: String,
    pub fg: String,
    pub bg: String,
    pub flags: u16,
    pub underline: Option<String>,
    pub hyperlink: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub geometry: Geometry,
    pub cells: Vec<Cell>,
    pub hyperlinks: Vec<(String, String)>,
    pub cursor: (i32, usize),
    pub cursor_style: String,
    pub modes: u32,
    pub colors: String,
    pub history_lines: usize,
    pub sync_pending_bytes: usize,
    pub title: String,
}
impl Frame {
    pub fn text(&self) -> String {
        self.cells
            .chunks(self.geometry.cols as usize)
            .map(|line| {
                line.iter()
                    .map(|cell| cell.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Default)]
struct Effects {
    events: Vec<Event>,
}
#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Effects>>);
impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        // The parser handles a bounded output chunk before this queue is drained.
        self.0.lock().expect("event lock").events.push(event);
    }
}

/// Expiry is applied ONLY by an ordered, durable SyncTimeout event.
#[derive(Default, Clone, Serialize, Deserialize)]
struct OrderedTimeout {
    armed: bool,
    #[serde(skip)]
    deadline: Option<Instant>,
}
impl Timeout for OrderedTimeout {
    fn set_timeout(&mut self, duration: Duration) {
        self.armed = true;
        self.deadline = Some(Instant::now() + duration);
    }
    fn clear_timeout(&mut self) {
        self.armed = false;
        self.deadline = None;
    }
    fn pending_timeout(&self) -> bool {
        self.armed
    }
}

#[derive(Serialize, Deserialize)]
struct CheckpointV2 {
    schema: u32,
    profile: String,
    geometry: Geometry,
    title: String,
    term: alacritty_terminal::term::checkpoint::TermCheckpointV1,
    processor: alacritty_terminal::vte::ansi::ProcessorCheckpointV1<OrderedTimeout>,
}

pub struct Terminal {
    term: Term<Listener>,
    parser: Processor<OrderedTimeout>,
    listener: Listener,
    geometry: Geometry,
    title: String,
    live: bool,
    responses: Vec<u8>,
}
impl Terminal {
    pub fn new(geometry: Geometry, live: bool) -> Self {
        let listener = Listener::default();
        let config = Config {
            osc52: Osc52::Disabled,
            ..Config::default()
        };
        Self {
            term: Term::new(config, &geometry, listener.clone()),
            parser: Processor::new(),
            listener,
            geometry,
            title: String::new(),
            live,
            responses: Vec::new(),
        }
    }
    /// Runtime-only normal-history observer; this does not enable archive
    /// persistence or change the authoritative hot-state checkpoint schema.
    pub fn set_history_observer(
        &mut self,
        observer: Option<
            Arc<
                dyn alacritty_terminal::grid::HistoryObserver<alacritty_terminal::term::cell::Cell>,
            >,
        >,
    ) {
        self.term.set_history_observer(observer);
    }

    /// Complete semantic state export for the pinned experimental core pair.
    /// Runtime sinks/damage and wall-clock deadlines are deliberately reconstructed.
    pub fn checkpoint(&self) -> Result<Vec<u8>> {
        let snapshot = CheckpointV2 {
            schema: 2,
            profile: CORE_PROFILE.into(),
            geometry: self.geometry,
            title: self.title.clone(),
            term: self.term.export_checkpoint(),
            processor: self.parser.export_checkpoint(),
        };
        snapshot.term.validate()?;
        ensure!(
            snapshot.title.len() <= 65536,
            "title checkpoint budget exceeded"
        );
        Ok(postcard::to_io(&snapshot, CheckpointWriter::new(CHECKPOINT_BYTE_LIMIT))?.bytes)
    }
    pub fn from_checkpoint(bytes: &[u8], live: bool) -> Result<Self> {
        ensure!(
            !live,
            "checkpoint import is historical only; live reattach requires the existing host model"
        );
        ensure!(
            bytes.len() <= CHECKPOINT_BYTE_LIMIT,
            "checkpoint byte budget exceeded"
        );
        let (snapshot, remaining): (CheckpointV2, &[u8]) = postcard::take_from_bytes(bytes)?;
        ensure!(
            remaining.is_empty() && snapshot.schema == 2 && snapshot.profile == CORE_PROFILE,
            "incompatible checkpoint envelope"
        );
        ensure!(
            snapshot.term.grid.lines == snapshot.geometry.rows as usize
                && snapshot.term.grid.columns == snapshot.geometry.cols as usize,
            "checkpoint geometry mismatch"
        );
        ensure!(
            snapshot.term.config.scrolling_history == 10000
                && snapshot.term.config.osc52 == Osc52::Disabled
                && !snapshot.term.config.kitty_keyboard,
            "checkpoint changed terminal profile"
        );
        let listener = Listener::default();
        let term = Term::from_checkpoint(snapshot.term, listener.clone())?;
        let parser = Processor::from_checkpoint(snapshot.processor).map_err(anyhow::Error::msg)?;
        Ok(Self {
            term,
            parser,
            listener,
            geometry: snapshot.geometry,
            title: snapshot.title,
            live,
            responses: Vec::new(),
        })
    }
    pub fn output(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        self.drain_effects();
    }
    pub fn resize(&mut self, geometry: Geometry) {
        self.term.resize(geometry);
        self.geometry = geometry;
        self.drain_effects();
    }
    pub fn sync_timeout_pending(&self) -> bool {
        self.parser.sync_timeout().armed
    }
    pub fn sync_timeout_due(&self) -> bool {
        self.parser
            .sync_timeout()
            .deadline
            .is_some_and(|d| Instant::now() >= d)
    }
    pub fn sync_timeout(&mut self) {
        self.parser.stop_sync(&mut self.term);
        self.drain_effects();
    }
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }
    fn drain_effects(&mut self) {
        let events = std::mem::take(&mut self.listener.0.lock().expect("event lock").events);
        for event in events {
            match event {
                Event::Title(title) => self.title = title,
                Event::ResetTitle => self.title.clear(),
                Event::PtyWrite(text) if self.live => self.responses.extend(text.bytes()),
                Event::TextAreaSizeRequest(format) if self.live => self.responses.extend(
                    format(WindowSize {
                        num_lines: self.geometry.rows,
                        num_cols: self.geometry.cols,
                        cell_width: 0,
                        cell_height: 0,
                    })
                    .bytes(),
                ),
                Event::ColorRequest(index, format) if self.live => {
                    // Return only explicitly known dynamic colors; do not invent a palette.
                    if let Some(color) = self.term.colors()[index] {
                        self.responses.extend(
                            format(Rgb {
                                r: color.r,
                                g: color.g,
                                b: color.b,
                            })
                            .bytes(),
                        );
                    }
                }
                // Clipboard, bell, URL/external actions are never executed by this prototype.
                _ => (),
            }
        }
    }
    pub fn frame(&self) -> Frame {
        let grid = self.term.grid();
        let mut hyperlinks = Vec::new();
        let mut link_index = std::collections::HashMap::new();
        let mut link_contents = std::collections::HashMap::new();
        let mut cells =
            Vec::with_capacity(self.geometry.rows as usize * self.geometry.cols as usize);
        for row in 0..self.geometry.rows {
            for col in 0..self.geometry.cols {
                let cell = &grid[Line(row as i32)][Column(col as usize)];
                let mut text = cell.c.to_string();
                if let Some(extra) = cell.zerowidth() {
                    text.extend(extra);
                }
                cells.push(Cell {
                    text,
                    fg: format!("{:?}", cell.fg),
                    bg: format!("{:?}", cell.bg),
                    flags: cell.flags.bits(),
                    underline: cell.underline_color().map(|c| format!("{c:?}")),
                    hyperlink: cell.hyperlink().map(|h| {
                        let identity = (h.id().as_ptr() as usize, h.uri().as_ptr() as usize);
                        *link_index.entry(identity).or_insert_with(|| {
                            let content = (h.id().to_owned(), h.uri().to_owned());
                            *link_contents.entry(content.clone()).or_insert_with(|| {
                                let index = hyperlinks.len() as u32;
                                hyperlinks.push(content);
                                index
                            })
                        })
                    }),
                });
            }
        }
        Frame {
            geometry: self.geometry,
            cells,
            hyperlinks,
            cursor: (grid.cursor.point.line.0, grid.cursor.point.column.0),
            cursor_style: format!("{:?}", self.term.cursor_style()),
            modes: self.term.mode().bits(),
            colors: format!(
                "{:?}",
                (0..alacritty_terminal::term::color::COUNT)
                    .map(|i| self.term.colors()[i])
                    .collect::<Vec<_>>()
            ),
            history_lines: grid.history_size(),
            sync_pending_bytes: self.parser.sync_bytes_count(),
            title: self.title.clone(),
        }
    }
    /// Diagnostic oracle for the active grid incl. cursor/saved cursor and hot history.
    /// Inactive grid, private Term fields and parser state are NOT a checkpoint.
    pub fn active_grid_diagnostic(&self) -> String {
        format!("{:?}", self.term.grid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn term() -> Terminal {
        Terminal::new(Geometry::new(12, 80).unwrap(), false)
    }
    #[test]
    fn utf8_and_escape_boundaries_do_not_change_cells() {
        let bytes = "Начало e\u{301} 界 👩\u{200d}💻\r\n\x1b[38;2;21;54;89mцвет\x1b[0m\x1b[?2004h\x1b]8;id=fixture;https://example.test\x07link\x1b]8;;\x07".as_bytes();
        let mut continuous = term();
        continuous.output(bytes);
        for size in 1..=bytes.len() {
            let mut split = term();
            for chunk in bytes.chunks(size) {
                split.output(chunk);
            }
            let expected = continuous.frame();
            let actual = split.frame();
            if expected != actual {
                panic!(
                    "chunk={size}; first cell difference={:?}; cursor {:?} vs {:?}",
                    expected
                        .cells
                        .iter()
                        .zip(&actual.cells)
                        .enumerate()
                        .find(|(_, (a, b))| a != b),
                    expected.cursor,
                    actual.cursor
                );
            }
            assert_eq!(
                continuous.active_grid_diagnostic(),
                split.active_grid_diagnostic()
            );
        }
    }
    #[test]
    fn complete_checkpoint_continues_at_every_byte_boundary() {
        let fixtures:&[&[u8]]=&[
            "начало é 界 e\u{301} 👩\u{200d}💻\x1b[31mred\x1b[0m".as_bytes(),
            b"normal\x1b[3;9H\x1b7\x1b[?1049halt\x1b[5;20H\x1b[?1049l\x1b8N",
            b"A\x1b[3b\x1b[3g\x1b[4G\x1bH\r\tTAB\x1b[2;9r\x1b[?6hREGION\x1b[?6l",
            b"\x1b]2;title\x07\x1b[22;0t\x1b]2;other\x07\x1b[23;0t\x1b]8;;https://example.test\x07link\x1b]8;;\x07",
            b"\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[38;2;1;2;3mCOLOR\x1b[?2026hbuffer\x1b[?2026l",
            b"\x1b(0lqk\x1b(B\x1b[>1uKEY\x1b[<u\x1b]4;1;rgb:aa/bb/cc\x07",
        ];
        for (index, bytes) in fixtures.iter().enumerate() {
            for cut in 0..=bytes.len() {
                let mut live = term();
                live.output(&bytes[..cut]);
                let saved = live.checkpoint().unwrap();
                let mut restored = Terminal::from_checkpoint(&saved, false)
                    .unwrap_or_else(|e| panic!("fixture={index} cut={cut}: {e:#}"));
                assert!(
                    saved == restored.checkpoint().unwrap(),
                    "import changed semantic state: fixture={index} cut={cut}"
                );
                for t in [&mut live, &mut restored] {
                    t.output(&bytes[cut..]);
                    t.resize(Geometry::new(9, 43).unwrap());
                    t.output(b"\x1b[?1049hsecond alt\x1b[?1049l\x1b8\tZ\x1b[2b\r\nend");
                    t.sync_timeout();
                }
                assert!(
                    live.checkpoint().unwrap() == restored.checkpoint().unwrap(),
                    "continuation mismatch fixture={index} cut={cut}"
                );
            }
        }
    }
    #[test]
    fn overlong_osc_is_bounded_ignored_whole_and_journal_replayable() {
        let mut t = term();
        t.output(b"\x1b]2;");
        t.output(&vec![b'x'; 1024 * 1024]);
        let snapshot = t.checkpoint().unwrap();
        assert!(snapshot.len() < 200000);
        let mut replay = Terminal::from_checkpoint(&snapshot, false).unwrap();
        for term in [&mut t, &mut replay] {
            term.output(b"\x07after");
            assert_eq!(term.frame().title, "");
            assert!(term.frame().text().starts_with("after"));
        }
        assert!(t.checkpoint().unwrap() == replay.checkpoint().unwrap());
    }
    #[test]
    fn frame_interns_long_osc_links_and_encoding_is_bounded() {
        use std::io::Write;
        let mut t = term();
        let uri = format!("https://example.test/{}", "x".repeat(32768));
        t.output(format!("\x1b]8;;{uri}\x07{}\x1b]8;;\x07", "A".repeat(4200)).as_bytes());
        let frame = t.frame();
        assert_eq!(frame.hyperlinks.len(), 1);
        assert!(serde_json::to_vec(&frame).unwrap().len() < 1024 * 1024);
        let mut writer = CheckpointWriter::new(8);
        writer.write_all(b"12345678").unwrap();
        assert!(writer.write_all(b"9").is_err());
        assert_eq!(writer.bytes.len(), 8);
    }
    #[test]
    fn invalid_geometry_cannot_enter_from_journal_or_ipc() {
        for bytes in [
            r#"{"rows":0,"cols":0}"#,
            r#"{"rows":65535,"cols":65535}"#,
            r#"{"rows":512,"cols":1024}"#,
        ] {
            assert!(serde_json::from_str::<Geometry>(bytes).is_err());
        }
    }
    #[test]
    fn partial_utf8_never_consumes_following_ascii() {
        let bytes = "é 界".as_bytes();
        for cut in 1..bytes.len() {
            let mut t = term();
            t.output(&bytes[..cut]);
            t.output(&bytes[cut..]);
            assert!(t.frame().text().starts_with("é 界"), "cut={cut}");
        }
    }
    #[test]
    fn replay_has_no_external_terminal_responses() {
        let mut replay = term();
        replay.output(b"\x1b[6n\x1b[c\x1b]52;c;aGVsbG8=\x07\x07");
        assert!(replay.take_responses().is_empty());
        let mut live = Terminal::new(Geometry::new(12, 80).unwrap(), true);
        live.output(b"\x1b[6n");
        assert!(!live.take_responses().is_empty());
    }
    #[test]
    fn frame_is_not_checkpoint_partial_parser_witness() {
        let mut pending = term();
        let mut clean = term();
        pending.output(b"\x1b[31");
        assert_eq!(pending.frame(), clean.frame());
        pending.output(b"mX");
        clean.output(b"mX");
        assert_ne!(pending.frame(), clean.frame());
    }
    #[test]
    fn ordered_sync_timeout_replays_without_wallclock() {
        let mut a = term();
        a.output(b"before\x1b[?2026hbuffered");
        assert!(a.frame().sync_pending_bytes > 0);
        let mut b = term();
        b.output(b"before\x1b[?2026hbuffered");
        a.sync_timeout();
        b.sync_timeout();
        assert_eq!(a.frame(), b.frame());
        assert!(a.frame().text().contains("buffered"));
    }
    #[test]
    fn alternate_and_resize_replay_exercises_both_grids() {
        let mut a = term();
        let mut b = term();
        for t in [&mut a, &mut b] {
            t.output(b"normal\x1b[?1049h\x1b[2Jalternate\x1b[4;12Hcursor");
            t.resize(Geometry::new(9, 43).unwrap());
        }
        assert_eq!(a.frame(), b.frame());
        a.output(b"\x1b[?1049l");
        b.output(b"\x1b[?1049l");
        assert_eq!(a.frame(), b.frame());
        assert!(a.frame().text().contains("normal"));
    }
    #[test]
    fn retirement_observer_preserves_hot_state_and_follows_normal_grid() {
        use alacritty_terminal::{
            grid::{HistoryObserver, HistoryRetirement, Row},
            index::Column,
            term::cell::Cell as CoreCell,
        };
        #[derive(Debug, Default)]
        struct Sink {
            rows: Mutex<usize>,
            samples: Mutex<Vec<String>>,
            boundaries: Mutex<usize>,
        }
        impl HistoryObserver<CoreCell> for Sink {
            fn row(&self, row: &Row<CoreCell>, columns: usize, _: HistoryRetirement) {
                assert_eq!(row.len(), columns);
                *self.rows.lock().unwrap() += 1;
                let mut samples = self.samples.lock().unwrap();
                if samples.len() < 3 {
                    samples.push((0..columns).map(|column| row[Column(column)].c).collect());
                }
            }
            fn boundary(&self, _: HistoryRetirement) {
                *self.boundaries.lock().unwrap() += 1;
            }
        }
        let sink = Arc::new(Sink::default());
        let geometry = Geometry::new(4, 40).unwrap();
        let mut observed = Terminal::new(geometry, false);
        let mut reference = Terminal::new(geometry, false);
        observed.set_history_observer(Some(sink.clone()));
        let body = (0..5500)
            .map(|line| format!("line{line:05} Русский 界 {}\r\n", "abcd".repeat(8)))
            .collect::<String>();
        observed.output(body.as_bytes());
        reference.output(body.as_bytes());
        assert!(*sink.rows.lock().unwrap() > 0);
        assert!(sink.samples.lock().unwrap()[0].starts_with("line00000"));
        assert_eq!(
            observed.checkpoint().unwrap(),
            reference.checkpoint().unwrap()
        );
        let rows = *sink.rows.lock().unwrap();
        let alternate = format!("\x1b[?1049h{}", "alternate redraw\r\n".repeat(100));
        observed.output(alternate.as_bytes());
        reference.output(alternate.as_bytes());
        assert_eq!(
            *sink.rows.lock().unwrap(),
            rows,
            "alternate redraw is not normal scrollback"
        );
        let geometry = Geometry::new(4, 20).unwrap();
        observed.resize(geometry);
        reference.resize(geometry);
        assert!(
            *sink.rows.lock().unwrap() > rows,
            "inactive normal width reflow also retires rows"
        );
        assert_eq!(
            observed.checkpoint().unwrap(),
            reference.checkpoint().unwrap()
        );
        for tail in [b"\x1b[?1049l".as_slice(), b"\x1b[3J", b"\x1bc"] {
            observed.output(tail);
            reference.output(tail);
            assert_eq!(
                observed.checkpoint().unwrap(),
                reference.checkpoint().unwrap()
            );
        }
        assert!(*sink.boundaries.lock().unwrap() >= 2);
    }
}
