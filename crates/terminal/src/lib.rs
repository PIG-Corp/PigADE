//! The host is the sole ANSI parser. A Frame is a render replica, NOT a checkpoint.
pub mod history;
pub use alacritty_terminal::term::resources::{
    AllocationCensus, CensusLimits, GridAllocationCensus, ResourceCensus,
};
pub use alacritty_terminal::vte::ansi::ProcessorBufferUsage;
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
use alacritty_terminal::term::{Config, Osc52, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, Processor, Rgb, Timeout};
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
    /// Typed active cursor presentation. `Hidden` reflects terminal cursor
    /// visibility modes while preserving the style's blinking setting.
    #[serde(default)]
    pub cursor_presentation: Option<CursorPresentation>,
    pub modes: u32,
    pub colors: String,
    /// Explicitly configured palette entries, sorted by terminal color index.
    /// Indices 0..=255 are the ANSI/xterm indexed palette; higher indices are
    /// special terminal colors such as foreground (256), background (257),
    /// cursor, and the pinned terminal's other named color slots. The vector
    /// contains only colors explicitly set by the terminal model.
    #[serde(default)]
    pub palette_overrides: Vec<(u16, [u8; 3])>,
    pub history_lines: usize,
    pub sync_pending_bytes: usize,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorPresentationShape {
    Block,
    Underline,
    Beam,
    HollowBlock,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPresentation {
    pub shape: CursorPresentationShape,
    pub blinking: bool,
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
    // Title updates have no per-update external effect. Only their last value
    // is published at the end of this ordered terminal event.
    title: Option<Option<String>>,
    events: Vec<Event>,
}
#[derive(Clone)]
struct Listener(Arc<Mutex<Effects>>, bool);
impl Listener {
    fn new(live: bool) -> Self {
        Self(Arc::default(), live)
    }
}
impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(title) => {
                self.0.lock().expect("event lock").title = Some(Some(title));
            }
            Event::ResetTitle => {
                self.0.lock().expect("event lock").title = Some(None);
            }
            Event::PtyWrite(_) | Event::ColorRequest(_, _) | Event::TextAreaSizeRequest(_)
                if self.1 =>
            {
                // Retain every live response in order; admission must account
                // for this queue rather than discard responses at a size cap.
                self.0.lock().expect("event lock").events.push(event);
            }
            // These effects were already ignored on drain. Historical response
            // closures and ignored UI/clipboard/bell events need no retention.
            _ => (),
        }
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

// Schema2 uses this exact prefix in Postcard. Borrow it before decoding any
// owned grids, pools or parser buffers; no duplicate terminal parser is involved.
type CheckpointHeader<'a> = (u32, &'a str, Geometry, &'a str);

#[derive(Serialize)]
struct CheckpointExport<'a> {
    schema: u32,
    profile: &'static str,
    geometry: Geometry,
    title: &'a str,
    term: alacritty_terminal::term::checkpoint::TermCheckpointRef<'a, Listener>,
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
    /// Diagnostic capacity census of the underlying Term, including retained
    /// row caches. This excludes this wrapper, parser, sinks and all transients;
    /// it is neither a heap limit nor an RSS measurement. Never run per frame.
    pub fn term_allocation_census(&self, limits: CensusLimits) -> AllocationCensus {
        alacritty_terminal::term::resources::census_terminal_allocations(&self.term, limits)
    }

    /// Count parser callbacks for a candidate output event without advancing
    /// this model, its ordered timer, response queue or history observer. The
    /// cloned parser may allocate for its bounded existing OSC/sync buffers.
    /// Counts are requested work, not effective grid work or a memory bound.
    pub fn output_resource_census(&self, bytes: &[u8]) -> ResourceCensus {
        alacritty_terminal::term::resources::census_output(&self.parser, bytes)
    }

    /// Count callbacks released by the next ordered synchronized-output timeout
    /// without consuming the pending timeout or publishing buffered cells.
    pub fn sync_timeout_resource_census(&self) -> ResourceCensus {
        alacritty_terminal::term::resources::census_sync_timeout(&self.parser)
    }

    /// Numeric parser backing storage, separate from the Term census. Inline
    /// bytes already include the parser and timeout handles; allocator overhead
    /// and any dynamic data owned by a generic timeout are not measured.
    pub fn parser_buffer_usage(&self) -> ProcessorBufferUsage {
        self.parser.buffer_usage()
    }

    pub fn new(geometry: Geometry, live: bool) -> Self {
        let listener = Listener::new(live);
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
        ensure!(
            self.title.len() <= 65536,
            "title checkpoint budget exceeded"
        );
        let snapshot = CheckpointExport {
            schema: 2,
            profile: CORE_PROFILE,
            geometry: self.geometry,
            title: &self.title,
            term: alacritty_terminal::term::checkpoint::TermCheckpointRef::new(&self.term)?,
            processor: self.parser.export_checkpoint(),
        };
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
        let ((schema, profile, _, title), _): (CheckpointHeader<'_>, _) =
            postcard::take_from_bytes(bytes)?;
        ensure!(schema == 2, "incompatible checkpoint schema");
        ensure!(profile == CORE_PROFILE, "incompatible checkpoint profile");
        ensure!(title.len() <= 65_536, "title checkpoint budget exceeded");
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
        let listener = Listener::new(live);
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
        let Effects { title, events } =
            std::mem::take(&mut *self.listener.0.lock().expect("event lock"));
        if let Some(title) = title {
            self.title = title.unwrap_or_default();
        }
        for event in events {
            match event {
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
        let cursor_style = self.term.cursor_style();
        let cursor_visible = self.term.mode().contains(TermMode::SHOW_CURSOR)
            || self.term.mode().contains(TermMode::VI);
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
                            // Hyperlink keys retain a shallow Arc handle and
                            // compare contents. Only the wire pool owns copies
                            // of the ID/URI payloads.
                            *link_contents.entry(h).or_insert_with_key(|link| {
                                let index = hyperlinks.len() as u32;
                                hyperlinks.push((link.id().to_owned(), link.uri().to_owned()));
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
            cursor_presentation: Some(CursorPresentation {
                shape: if cursor_visible {
                    match cursor_style.shape {
                        CursorShape::Block => CursorPresentationShape::Block,
                        CursorShape::Underline => CursorPresentationShape::Underline,
                        CursorShape::Beam => CursorPresentationShape::Beam,
                        CursorShape::HollowBlock => CursorPresentationShape::HollowBlock,
                        CursorShape::Hidden => CursorPresentationShape::Hidden,
                    }
                } else {
                    CursorPresentationShape::Hidden
                },
                blinking: cursor_style.blinking,
            }),
            modes: self.term.mode().bits(),
            colors: format!(
                "{:?}",
                (0..alacritty_terminal::term::color::COUNT)
                    .map(|i| self.term.colors()[i])
                    .collect::<Vec<_>>()
            ),
            palette_overrides: (0..alacritty_terminal::term::color::COUNT)
                .filter_map(|index| {
                    self.term.colors()[index]
                        .map(|color| (index as u16, [color.r, color.g, color.b]))
                })
                .collect(),
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
    fn owned_checkpoint_oracle(terminal: &Terminal) -> Vec<u8> {
        let snapshot = CheckpointV2 {
            schema: 2,
            profile: CORE_PROFILE.into(),
            geometry: terminal.geometry,
            title: terminal.title.clone(),
            term: terminal.term.export_checkpoint(),
            processor: terminal.parser.export_checkpoint(),
        };
        snapshot.term.validate().unwrap();
        postcard::to_io(&snapshot, CheckpointWriter::new(CHECKPOINT_BYTE_LIMIT))
            .unwrap()
            .bytes
    }
    #[test]
    fn checkpoint_envelope_rejects_bad_headers_before_decoding_term() {
        // These are deliberately truncated envelopes: no Term/parser follows.
        // A header-specific error proves rejection precedes those allocations.
        let geometry = Geometry::new(12, 80).unwrap();
        for (schema, profile, title, expected) in [
            (
                3,
                CORE_PROFILE,
                String::new(),
                "incompatible checkpoint schema",
            ),
            (
                2,
                "wrong-profile",
                String::new(),
                "incompatible checkpoint profile",
            ),
            (
                2,
                CORE_PROFILE,
                "Ж".repeat(32_769),
                "title checkpoint budget exceeded",
            ),
        ] {
            let bytes = postcard::to_allocvec(&(schema as u32, profile, geometry, title)).unwrap();
            let error = Terminal::from_checkpoint(&bytes, false).err().unwrap();
            assert!(error.to_string().contains(expected), "{error:#}");
        }
    }

    #[test]
    fn checkpoint_title_import_matches_export_byte_limit() {
        let original = term();
        let saved = original.checkpoint().unwrap();
        let mut snapshot: CheckpointV2 = postcard::from_bytes(&saved).unwrap();
        snapshot.title = "Ж".repeat(32_768);
        let boundary = postcard::to_allocvec(&snapshot).unwrap();
        let restored = Terminal::from_checkpoint(&boundary, false).unwrap();
        assert_eq!(restored.frame().title.len(), 65_536);
        assert_eq!(restored.checkpoint().unwrap(), boundary);

        snapshot.title.push('x');
        let excessive = postcard::to_allocvec(&snapshot).unwrap();
        let error = Terminal::from_checkpoint(&excessive, false).err().unwrap();
        assert!(
            error
                .to_string()
                .contains("title checkpoint budget exceeded")
        );
        assert_eq!(original.checkpoint().unwrap(), saved);
    }

    #[test]
    fn parser_buffer_accounting_preserves_checkpoint_bytes_and_continuation() {
        let fixtures: &[(&[u8], &[u8])] = &[
            (b"prefix\x1b]2;partial", b" title\x07Z"),
            (b"prefix\x1b[?2026hbuffered\xc3", b"\xa9\x1b[?2026lZ"),
            (
                b"prefix\x1b[?2026h\x1b]8;id=link;https://example",
                b".test\x1b\\L\x1b[?2026lZ",
            ),
        ];
        for &(prefix, tail) in fixtures {
            let mut original = term();
            assert_eq!(original.parser_buffer_usage().sync_buffer_capacity, 0);
            original.output(prefix);
            let saved = original.checkpoint().unwrap();
            let usage = original.parser_buffer_usage();
            assert!(usage.inline_size_bytes >= usage.parser.inline_size_bytes);
            assert!(usage.sync_buffer_len <= usage.sync_buffer_capacity);
            assert_eq!(original.checkpoint().unwrap(), saved);
            assert_eq!(saved, owned_checkpoint_oracle(&original));

            let mut restored = Terminal::from_checkpoint(&saved, false).unwrap();
            assert_eq!(
                restored.parser_buffer_usage().sync_buffer_len,
                usage.sync_buffer_len
            );
            assert_eq!(
                restored.parser_buffer_usage().parser.pending_osc_len,
                usage.parser.pending_osc_len
            );
            assert_eq!(restored.checkpoint().unwrap(), saved);
            original.output(tail);
            restored.output(tail);
            assert_eq!(
                original.checkpoint().unwrap(),
                restored.checkpoint().unwrap()
            );
        }
    }

    #[test]
    fn frame_deduplicates_equal_links_from_distinct_model_allocations() {
        let mut terminal = term();
        terminal.output(
            b"\x1b]8;id=same;https://example.test/shared\x07A\x1b]8;;\x07\x1b]8;id=same;https://example.test/shared\x07B\x1b]8;;\x07",
        );
        let census = terminal.term_allocation_census(CensusLimits {
            max_cell_visits: 100_000,
            max_unique_allocations: 100,
        });
        assert!(!census.incomplete);
        assert_eq!(
            census.unique_hyperlinks, 2,
            "fixture lost distinct allocations"
        );
        let before = terminal.checkpoint().unwrap();
        let frame = terminal.frame();
        assert_eq!(
            frame.hyperlinks,
            vec![("same".into(), "https://example.test/shared".into())]
        );
        assert_eq!(frame.cells[0].hyperlink, Some(0));
        assert_eq!(frame.cells[1].hyperlink, Some(0));
        assert_eq!(terminal.checkpoint().unwrap(), before);
    }

    #[test]
    fn dynamic_palette_overrides_follow_set_reset_and_checkpoint_replay() {
        let mut live = term();
        live.output(
            b"\x1b]4;1;rgb:11/22/33;16;rgb:aa/bb/cc\x07\x1b]10;rgb:01/02/03\x07\x1b]11;rgb:04/05/06\x07",
        );
        let expected = vec![
            (1, [0x11, 0x22, 0x33]),
            (16, [0xaa, 0xbb, 0xcc]),
            (256, [0x01, 0x02, 0x03]),
            (257, [0x04, 0x05, 0x06]),
        ];
        assert_eq!(live.frame().palette_overrides, expected);
        assert!(
            live.frame()
                .palette_overrides
                .windows(2)
                .all(|pair| pair[0].0 < pair[1].0)
        );

        let saved = live.checkpoint().unwrap();
        let mut replay = Terminal::from_checkpoint(&saved, false).unwrap();
        assert_eq!(replay.frame().palette_overrides, expected);
        let reset_tail = b"\x1b]104;1\x07\x1b]110\x07\x1b]111\x07";
        live.output(reset_tail);
        replay.output(reset_tail);
        assert_eq!(
            live.frame().palette_overrides,
            vec![(16, [0xaa, 0xbb, 0xcc])]
        );
        assert_eq!(live.frame(), replay.frame());
        assert_eq!(live.checkpoint().unwrap(), replay.checkpoint().unwrap());

        live.output(b"\x1b]104\x07");
        replay.output(b"\x1b]104\x07");
        assert!(live.frame().palette_overrides.is_empty());
        assert_eq!(live.frame(), replay.frame());
        assert_eq!(live.checkpoint().unwrap(), replay.checkpoint().unwrap());
    }

    #[test]
    fn frame_without_typed_palette_field_deserializes_as_no_overrides() {
        let frame = term().frame();
        let mut value = serde_json::to_value(&frame).unwrap();
        value.as_object_mut().unwrap().remove("palette_overrides");
        value.as_object_mut().unwrap().remove("cursor_presentation");
        let decoded: Frame = serde_json::from_value(value).unwrap();
        assert!(decoded.palette_overrides.is_empty());
        assert_eq!(decoded.cursor_presentation, None);
        assert_eq!(decoded.geometry, frame.geometry);
        assert_eq!(decoded.cells, frame.cells);
        assert_eq!(decoded.colors, frame.colors);
    }

    #[test]
    fn cursor_presentation_tracks_decsusr_visibility_and_checkpoint_continuation() {
        let shapes = [
            (1, CursorPresentationShape::Block, true),
            (2, CursorPresentationShape::Block, false),
            (3, CursorPresentationShape::Underline, true),
            (4, CursorPresentationShape::Underline, false),
            (5, CursorPresentationShape::Beam, true),
            (6, CursorPresentationShape::Beam, false),
        ];
        let mut live = term();
        for (code, shape, blinking) in shapes {
            live.output(format!("\x1b[{code} q").as_bytes());
            assert_eq!(
                live.frame().cursor_presentation,
                Some(CursorPresentation { shape, blinking }),
                "DECSCUSR {code}"
            );
        }

        live.output(b"\x1b[5 q\x1b[?25l");
        assert_eq!(
            live.frame().cursor_presentation,
            Some(CursorPresentation {
                shape: CursorPresentationShape::Hidden,
                blinking: true
            })
        );
        live.output(b"\x1b[?25h");
        assert_eq!(
            live.frame().cursor_presentation,
            Some(CursorPresentation {
                shape: CursorPresentationShape::Beam,
                blinking: true
            })
        );

        let mut partial = term();
        partial.output(b"\x1b[3 ");
        let saved = partial.checkpoint().unwrap();
        let mut restored = Terminal::from_checkpoint(&saved, false).unwrap();
        partial.output(b"q");
        restored.output(b"q");
        let expected = Some(CursorPresentation {
            shape: CursorPresentationShape::Underline,
            blinking: true,
        });
        assert_eq!(partial.frame().cursor_presentation, expected);
        assert_eq!(partial.frame(), restored.frame());
        assert_eq!(
            partial.checkpoint().unwrap(),
            restored.checkpoint().unwrap()
        );
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
                assert_eq!(
                    saved,
                    owned_checkpoint_oracle(&live),
                    "checkpoint wire changed: fixture={index} cut={cut}"
                );
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

#[cfg(test)]
mod listener_retention_tests {
    use super::*;

    fn mixed_effects() -> &'static [u8] {
        b"\x1b]0;alpha\x07\x1b[5nAB\x1b[6n\x1b[22;0t\x1b]0;beta\x07\x1b[18t\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07\x1b[14t\x1b[23;0t\x1b[2 q\x1b[?1000h\x07\x1b[?1000l\x1b[6n"
    }

    #[test]
    fn mixed_listener_effects_keep_exact_live_response_fifo_and_historical_title() -> Result<()> {
        let geometry = Geometry::new(8, 40)?;
        let mut live = Terminal::new(geometry, true);
        live.output(mixed_effects());
        assert_eq!(
            live.take_responses(),
            b"\x1b[0n\x1b[1;3R\x1b[8;8;40t\x1b]4;1;rgb:1212/3434/5656\x07\x1b[4;0;0t\x1b[1;3R"
        );
        assert_eq!(live.frame().title, "alpha");
        let mut historical = Terminal::new(geometry, false);
        historical.output(mixed_effects());
        assert!(historical.take_responses().is_empty());
        assert_eq!(historical.checkpoint()?, live.checkpoint()?);
        assert_eq!(historical.frame(), live.frame());
        Ok(())
    }

    #[test]
    fn listener_title_and_response_semantics_survive_event_cuts_and_sync_checkpoint() -> Result<()>
    {
        let geometry = Geometry::new(8, 40)?;
        let mut whole = Terminal::new(geometry, true);
        whole.output(mixed_effects());
        let expected_responses = whole.take_responses();
        for cut in 0..=mixed_effects().len() {
            let mut split = Terminal::new(geometry, true);
            split.output(&mixed_effects()[..cut]);
            split.output(&mixed_effects()[cut..]);
            assert_eq!(split.take_responses(), expected_responses, "cut={cut}");
            assert_eq!(split.checkpoint()?, whole.checkpoint()?, "cut={cut}");
        }

        let mut pending = Terminal::new(geometry, true);
        pending.output(b"\x1b[?2026h");
        pending.output(mixed_effects());
        assert!(pending.take_responses().is_empty());
        let mut restored = Terminal::from_checkpoint(&pending.checkpoint()?, false)?;
        pending.sync_timeout();
        restored.sync_timeout();
        assert_eq!(pending.take_responses(), expected_responses);
        assert!(restored.take_responses().is_empty());
        assert_eq!(pending.checkpoint()?, restored.checkpoint()?);
        assert_eq!(pending.frame(), whole.frame());
        Ok(())
    }

    #[test]
    fn parser_listener_does_not_retain_ignored_storms_or_historical_responses() -> Result<()> {
        let geometry = Geometry::new(8, 40)?;
        let ignored = b"\x07\x1b[2 q\x1b[?1000h\x1b[?1000l".repeat(4096);
        let queries = b"\x1b[6n\x1b[18t\x1b[14t".repeat(128);
        let mut historical = Terminal::new(geometry, false);
        historical.parser.advance(&mut historical.term, &ignored);
        historical.parser.advance(&mut historical.term, &queries);
        {
            let effects = historical.listener.0.lock().unwrap();
            assert_eq!(effects.events.len(), 0);
            assert_eq!(effects.events.capacity(), 0);
            assert!(effects.title.is_none());
        }
        historical.drain_effects();
        assert!(historical.take_responses().is_empty());

        let mut live = Terminal::new(geometry, true);
        live.parser.advance(&mut live.term, &ignored);
        assert!(live.listener.0.lock().unwrap().events.is_empty());
        live.parser.advance(&mut live.term, &queries);
        assert_eq!(live.listener.0.lock().unwrap().events.len(), 384);
        live.drain_effects();
        assert_eq!(
            live.take_responses(),
            b"\x1b[1;1R\x1b[8;8;40t\x1b[4;0;0t".repeat(128),
        );
        assert_eq!(live.checkpoint()?, historical.checkpoint()?);
        Ok(())
    }

    #[test]
    fn parser_listener_retains_one_last_title_and_preserves_reset_pop_order() -> Result<()> {
        let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
        let titles = b"\x1b]0;first\x07\x1b]0;second\x07".repeat(4096);
        terminal.parser.advance(&mut terminal.term, &titles);
        {
            let effects = terminal.listener.0.lock().unwrap();
            assert!(effects.events.is_empty());
            assert_eq!(effects.events.capacity(), 0);
            assert_eq!(effects.title.as_ref().unwrap().as_deref(), Some("second"));
        }
        assert!(
            terminal.title.is_empty(),
            "title publishes only on ordered drain"
        );
        terminal.drain_effects();
        assert_eq!(terminal.frame().title, "second");

        // Pop of the initial empty title emits ResetTitle. A following set must
        // replace it, while the reverse order must retain the reset instead.
        for (body, expected) in [
            (
                b"\x1b[22;0t\x1b]0;temporary\x07\x1b[23;0t\x1b]0;final\x07".as_slice(),
                "final",
            ),
            (b"\x1b[22;0t\x1b]0;temporary\x07\x1b[23;0t".as_slice(), ""),
        ] {
            let mut fresh = Terminal::new(Geometry::new(8, 40)?, false);
            fresh.output(body);
            assert_eq!(fresh.frame().title, expected);
            let restored = Terminal::from_checkpoint(&fresh.checkpoint()?, false)?;
            assert_eq!(restored.checkpoint()?, fresh.checkpoint()?);
            assert_eq!(restored.frame(), fresh.frame());
        }
        Ok(())
    }
}

#[cfg(test)]
mod resource_preflight_tests {
    use super::*;

    #[test]
    fn output_census_preserves_partial_parser_model_and_pending_live_responses() -> Result<()> {
        let mut terminal = Terminal::new(Geometry::new(8, 40)?, true);
        terminal.output(b"prefix\x1b[6n\xe7\x95");
        assert!(
            !terminal.responses.is_empty(),
            "fixture must contain a live query response"
        );
        let checkpoint = terminal.checkpoint()?;
        let frame = terminal.frame();
        let responses = terminal.responses.clone();
        let payload = b"\x8c\x1b[5b\x1b[3S\x1b[2J\x1b]0;candidate-title\x07\x1b[6n";
        let census = terminal.output_resource_census(payload);
        assert_eq!(
            census.input_chars, 6,
            "split CJK scalar and REP must follow cloned parser state"
        );
        assert_eq!(census.scroll_up_requested_lines, 3);
        assert_eq!(census.clear_screen_callbacks, 1);
        assert_eq!(census.title_set_callbacks, 1);
        assert_eq!(census.device_status_queries, 1);
        assert!(!census.saturated);
        assert_eq!(terminal.checkpoint()?, checkpoint);
        assert_eq!(terminal.frame(), frame);
        assert_eq!(terminal.responses, responses);
        assert!(terminal.listener.0.lock().unwrap().events.is_empty());
        // Actual continuation still produces exactly the historical model from
        // the untouched checkpoint; preflight must not consume the UTF8 prefix.
        let mut expected = Terminal::from_checkpoint(&checkpoint, false)?;
        expected.output(payload);
        terminal.output(payload);
        assert_eq!(terminal.checkpoint()?, expected.checkpoint()?);
        assert_eq!(terminal.frame(), expected.frame());
        Ok(())
    }

    #[test]
    fn timeout_census_does_not_consume_pending_sync_or_publish_buffered_work() -> Result<()> {
        let mut terminal = Terminal::new(Geometry::new(8, 40)?, false);
        terminal.output(b"\x1b[?2026habc\x1b[2S\x1b[4L");
        assert!(terminal.sync_timeout_pending());
        let checkpoint = terminal.checkpoint()?;
        let frame = terminal.frame();
        let census = terminal.sync_timeout_resource_census();
        assert_eq!(census.input_chars, 3);
        assert_eq!(census.scroll_up_requested_lines, 2);
        assert_eq!(census.insert_blank_lines_requested, 4);
        assert_eq!(terminal.checkpoint()?, checkpoint);
        assert_eq!(terminal.frame(), frame);
        assert!(terminal.sync_timeout_pending());
        assert!(terminal.responses.is_empty());
        let mut expected = Terminal::from_checkpoint(&checkpoint, false)?;
        expected.sync_timeout();
        terminal.sync_timeout();
        assert_eq!(terminal.checkpoint()?, expected.checkpoint()?);
        assert_eq!(terminal.frame(), expected.frame());
        assert!(!terminal.sync_timeout_pending());
        Ok(())
    }
}
