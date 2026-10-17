//! Reproducible long-history journal/cold-history corpus generator.
//!
//! This drives the same public production Terminal, Journal, History observer,
//! and checkpoint publication APIs as the session actor. Every event is
//! journal-fsynced before model application; history is committed before a
//! checkpoint can refer to it. It does not launch a PTY, UI, provider CLI, or
//! claim an actual 24-hour run: the logical schedule exists only in the JSON
//! manifest because journal events intentionally have no wall-clock fields.
use anyhow::{Context, Result, ensure};
use pigade_session_host::{checkpoint, history::History, verify_recovery_against_live};
use pigade_storage::{EventKind, Journal, JournalHeader, Receipt};
use pigade_terminal::{CORE_PROFILE, Geometry, Terminal};
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const OUTPUT_CHUNK: usize = 64 * 1024;
const CHECKPOINT_EVENTS: u64 = 1024;
const CHECKPOINT_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
const CHECKPOINT_WALL: Duration = Duration::from_secs(5);
const SCENARIO_EVENTS: u64 = 13;
const SYNC_BEGIN_SEQUENCE: u64 = 10;
const SYNC_TIMEOUT_SEQUENCE: u64 = 12;
const SYNC_TIMEOUT_DELAY_NS: u64 = 200_000_000;
const SYNC_BEGIN_TIMESTAMP_NS: u64 = (SYNC_BEGIN_SEQUENCE - 1) * 1_000_000;
const SYNC_TIMEOUT_TIMESTAMP_NS: u64 = SYNC_BEGIN_TIMESTAMP_NS + SYNC_TIMEOUT_DELAY_NS;

#[derive(Clone, Copy)]
enum OutputPhase {
    Body,
    Prelude,
    Tail,
}

struct EndingRecipe {
    prelude: Vec<(EventKind, Vec<u8>)>,
    tail: Vec<(EventKind, Vec<u8>)>,
}

#[derive(Clone, Debug)]
struct Options {
    directory: PathBuf,
    sessions: u32,
    output_bytes: u64,
    resize_events: u64,
    logical_hours: f64,
    seed: u64,
    verify: bool,
}

#[derive(Serialize)]
struct SessionMetrics {
    session_id: Uuid,
    generation: Uuid,
    journal: String,
    journal_bytes: u64,
    logical_timeline: String,
    logical_timeline_bytes: u64,
    logical_timeline_sha256: String,
    logical_timeline_duration_ns: u64,
    requested_output_bytes: u64,
    final_prelude_output_bytes: u64,
    final_tail_output_bytes: u64,
    final_prelude_events: u64,
    final_tail_events: u64,
    resize_events: u64,
    total_events: u64,
    checkpoints: u64,
    checkpoint_durations_ms: Vec<f64>,
    cold_history_records: u64,
    cold_history_bytes: u64,
    final_sequence: u64,
    verification: Option<pigade_session_host::RecoveryVerification>,
}

#[derive(Serialize)]
struct Manifest {
    schema: u32,
    kind: &'static str,
    created_unix_ms: u128,
    completed_unix_ms: u128,
    core_profile: &'static str,
    sessions: u32,
    requested_output_bytes_per_session: u64,
    resize_events_per_session: u64,
    logical_hours_metadata: f64,
    logical_schedule_scope_note: &'static str,
    seed: u64,
    verification_requested: bool,
    claims: [&'static str; 7],
    results: Vec<SessionMetrics>,
}

struct SessionBuilder {
    root: PathBuf,
    journal_path: PathBuf,
    journal: Journal,
    timeline: LogicalTimeline,
    terminal: Terminal,
    history: Arc<History>,
    header: JournalHeader,
    event_count: u64,
    output_bytes: u64,
    body_output_bytes: u64,
    prelude_output_bytes: u64,
    tail_output_bytes: u64,
    output_phase: OutputPhase,
    prelude_events: u64,
    output_at_checkpoint: u64,
    events_at_checkpoint: u64,
    checkpoint_at: Instant,
    checkpoints: u64,
    checkpoint_durations_ms: Vec<f64>,
    resize_events: u64,
    last_receipt: Option<Receipt>,
}

#[derive(Serialize)]
struct TimelineHeader<'a> {
    record: &'static str,
    schema: u32,
    session_id: Uuid,
    generation: Uuid,
    profile: &'a str,
    duration_ns: u64,
    expected_events: u64,
    sync_timeout_delay_ns: u64,
    sync_pairs: Vec<(u64, u64)>,
}

#[derive(Serialize)]
struct TimelineEvent {
    record: &'static str,
    sequence: u64,
    event: &'static str,
    logical_nanoseconds: u64,
}

struct LogicalTimeline {
    path: PathBuf,
    writer: BufWriter<File>,
    duration_ns: u64,
    expected_events: u64,
    sync_pairs: Vec<(u64, u64)>,
    written_events: u64,
    last_timestamp_ns: Option<u64>,
}

fn interpolate(sequence: u64, from_seq: u64, from_ns: u64, to_seq: u64, to_ns: u64) -> u64 {
    let position = (sequence - from_seq) as u128;
    let span = (to_seq - from_seq) as u128;
    let elapsed = (to_ns - from_ns) as u128;
    from_ns + (elapsed * position / span) as u64
}

impl LogicalTimeline {
    fn create(
        path: PathBuf,
        parent: &Path,
        header: &JournalHeader,
        duration_ns: u64,
        expected_events: u64,
        sync_pairs: Vec<(u64, u64)>,
    ) -> Result<Self> {
        ensure!(
            sync_pairs.first() == Some(&(SYNC_BEGIN_SEQUENCE, SYNC_TIMEOUT_SEQUENCE)),
            "timeline must include the standard synchronized-output boundary"
        );
        ensure!(
            sync_pairs.len() <= 2
                && sync_pairs.get(1).is_none_or(|pair| pair.1 == pair.0 + 2)
                && sync_pairs
                    .get(1)
                    .is_none_or(|pair| pair.1 + 1 == expected_events),
            "timeline supports at most one final synchronized-output boundary"
        );
        ensure!(
            expected_events > SYNC_TIMEOUT_SEQUENCE,
            "logical timeline needs events after SyncTimeout"
        );
        ensure!(
            duration_ns >= SYNC_TIMEOUT_TIMESTAMP_NS,
            "logical duration is shorter than the SyncTimeout boundary"
        );
        ensure!(
            sync_pairs.len() == 1 || duration_ns > SYNC_TIMEOUT_TIMESTAMP_NS + 202_000_000,
            "logical duration is too short for the final synchronized-output boundary"
        );
        ensure!(
            duration_ns - SYNC_TIMEOUT_TIMESTAMP_NS >= expected_events - SYNC_TIMEOUT_SEQUENCE,
            "logical duration is too short for strictly increasing event timestamps"
        );
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(
            &mut writer,
            &TimelineHeader {
                record: "header",
                schema: 1,
                session_id: header.session_id,
                generation: header.generation,
                profile: &header.profile,
                duration_ns,
                expected_events,
                sync_timeout_delay_ns: SYNC_TIMEOUT_DELAY_NS,
                sync_pairs: sync_pairs.clone(),
            },
        )?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        File::open(parent)?.sync_all()?;
        Ok(Self {
            path,
            writer,
            duration_ns,
            expected_events,
            sync_pairs,
            written_events: 0,
            last_timestamp_ns: None,
        })
    }

    fn timestamp(&self, sequence: u64) -> Result<u64> {
        ensure!(
            sequence > 0 && sequence <= self.expected_events,
            "timeline sequence outside planned range"
        );
        if sequence < SYNC_TIMEOUT_SEQUENCE {
            return Ok((sequence - 1) * 1_000_000);
        }
        if sequence == SYNC_TIMEOUT_SEQUENCE {
            return Ok(SYNC_TIMEOUT_TIMESTAMP_NS);
        }
        if let Some(&(begin, timeout)) = self.sync_pairs.get(1) {
            let begin_time = self.duration_ns - 202_000_000;
            if sequence < begin {
                return Ok(interpolate(
                    sequence,
                    SYNC_TIMEOUT_SEQUENCE,
                    SYNC_TIMEOUT_TIMESTAMP_NS,
                    begin,
                    begin_time,
                ));
            }
            if sequence == begin {
                return Ok(begin_time);
            }
            if sequence == begin + 1 {
                return Ok(self.duration_ns - 201_000_000);
            }
            if sequence == timeout {
                return Ok(self.duration_ns - 2_000_000);
            }
            if sequence == timeout + 1 {
                return Ok(self.duration_ns);
            }
            if sequence < timeout {
                return Ok(interpolate(
                    sequence,
                    begin + 1,
                    self.duration_ns - 201_000_000,
                    timeout,
                    self.duration_ns - 2_000_000,
                ));
            }
            anyhow::bail!("timeline sequence crosses an ending sync boundary")
        }
        Ok(interpolate(
            sequence,
            SYNC_TIMEOUT_SEQUENCE,
            SYNC_TIMEOUT_TIMESTAMP_NS,
            self.expected_events,
            self.duration_ns,
        ))
    }

    fn record(&mut self, sequence: u64, kind: EventKind) -> Result<()> {
        ensure!(
            sequence == self.written_events + 1,
            "logical timeline event sequence gap"
        );
        let timeout_sequences = self.sync_pairs.iter().any(|pair| pair.1 == sequence);
        let begin_sequences = self.sync_pairs.iter().any(|pair| pair.0 == sequence);
        ensure!(
            timeout_sequences == (kind == EventKind::SyncTimeout),
            "logical SyncTimeout boundaries do not match journal events"
        );
        ensure!(
            !begin_sequences || kind == EventKind::Output,
            "logical synchronized-output begin does not match journal events"
        );
        let timestamp = self.timestamp(sequence)?;
        ensure!(
            self.last_timestamp_ns.is_none_or(|last| timestamp > last),
            "logical timeline timestamps are not strictly increasing"
        );
        serde_json::to_writer(
            &mut self.writer,
            &TimelineEvent {
                record: "event",
                sequence,
                event: event_name(kind),
                logical_nanoseconds: timestamp,
            },
        )?;
        self.writer.write_all(b"\n")?;
        self.last_timestamp_ns = Some(timestamp);
        self.written_events += 1;
        Ok(())
    }

    fn sync(&mut self) -> Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        Ok(())
    }

    fn finish(mut self) -> Result<(String, u64, String, u64)> {
        ensure!(
            self.written_events == self.expected_events,
            "logical timeline ended before planned final sequence"
        );
        ensure!(
            self.last_timestamp_ns == Some(self.duration_ns),
            "logical timeline does not reach its declared duration"
        );
        self.sync()?;
        File::open(self.path.parent().context("timeline parent")?)?.sync_all()?;
        let bytes = fs::metadata(&self.path)?.len();
        let hash = sha256_file(&self.path)?;
        Ok((
            self.path
                .file_name()
                .context("timeline name")?
                .to_string_lossy()
                .into_owned(),
            bytes,
            hash,
            self.duration_ns,
        ))
    }
}

fn event_name(kind: EventKind) -> &'static str {
    match kind {
        EventKind::Output => "output",
        EventKind::Resize => "resize",
        EventKind::SyncTimeout => "sync_timeout",
        EventKind::Exit => "exit",
        EventKind::OutputRejectedResource => "output_rejected_resource",
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let result = Command::new("sha256sum")
        .arg("--")
        .arg(path)
        .output()
        .context("sha256sum is required to bind the logical timeline sidecar")?;
    ensure!(
        result.status.success(),
        "sha256sum failed for logical timeline"
    );
    let output = String::from_utf8(result.stdout)?;
    let hash = output
        .split_whitespace()
        .next()
        .context("sha256sum output missing digest")?;
    ensure!(
        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid sha256sum digest"
    );
    Ok(hash.to_owned())
}

impl SessionBuilder {
    fn new(
        root: &Path,
        seed: u64,
        index: u32,
        logical_duration_ns: u64,
        expected_events: u64,
        sync_pairs: Vec<(u64, u64)>,
    ) -> Result<Self> {
        let session_id = Uuid::from_u128(seed as u128 ^ ((index as u128 + 1) << 96));
        let generation = Uuid::from_u128(
            (seed.rotate_left(17) as u128) ^ ((index as u128 + 1) << 64) ^ 0x5049_4741_4445,
        );
        let geometry = Geometry::new(36, 120)?;
        let header = JournalHeader {
            session_id,
            generation,
            profile: CORE_PROFILE.into(),
            initial_rows: geometry.rows,
            initial_cols: geometry.cols,
        };
        let journal_path = root.join(format!("{}.journal", generation));
        let journal = Journal::create(&journal_path, header.clone())?;
        let timeline = LogicalTimeline::create(
            root.join(format!("{}.timeline.jsonl", generation)),
            root,
            &header,
            logical_duration_ns,
            expected_events,
            sync_pairs,
        )?;
        let history = History::live(root, &header)?;
        let mut terminal = Terminal::new(geometry, false);
        terminal.set_history_observer(Some(history.clone()));
        Ok(Self {
            root: root.into(),
            journal_path,
            journal,
            timeline,
            terminal,
            history,
            header,
            event_count: 0,
            output_bytes: 0,
            body_output_bytes: 0,
            prelude_output_bytes: 0,
            tail_output_bytes: 0,
            output_phase: OutputPhase::Body,
            prelude_events: 0,
            output_at_checkpoint: 0,
            events_at_checkpoint: 0,
            checkpoint_at: Instant::now(),
            checkpoints: 0,
            checkpoint_durations_ms: Vec::new(),
            resize_events: 0,
            last_receipt: None,
        })
    }

    fn event(
        &mut self,
        kind: EventKind,
        payload: &[u8],
        force_checkpoint: bool,
    ) -> Result<Receipt> {
        // This ordering intentionally mirrors the durable generation actor:
        // journal fsync, observer-wrapped model mutation, archive/index fsync,
        // then optional atomic checkpoint publication.
        ensure!(
            kind != EventKind::OutputRejectedResource,
            "corpus cannot apply refused raw output"
        );
        let receipt = self.journal.append(kind, payload)?;
        self.last_receipt = Some(receipt);
        self.history.begin(receipt.sequence)?;
        match kind {
            EventKind::Output => {
                self.terminal.output(payload);
                self.output_bytes = self.output_bytes.saturating_add(payload.len() as u64);
                match self.output_phase {
                    OutputPhase::Body => self.body_output_bytes += payload.len() as u64,
                    OutputPhase::Prelude => self.prelude_output_bytes += payload.len() as u64,
                    OutputPhase::Tail => self.tail_output_bytes += payload.len() as u64,
                }
            }
            EventKind::Resize => {
                let geometry: Geometry = serde_json::from_slice(payload)?;
                self.terminal.resize(geometry);
                self.resize_events += 1;
            }
            EventKind::SyncTimeout => self.terminal.sync_timeout(),
            EventKind::Exit => (),
            EventKind::OutputRejectedResource => unreachable!("corpus guard rejects refusals"),
        }
        self.history.commit()?;
        self.timeline.record(receipt.sequence, kind)?;
        self.event_count += 1;
        if matches!(self.output_phase, OutputPhase::Prelude) {
            self.prelude_events += 1;
        }
        let due = force_checkpoint
            || self.event_count.saturating_sub(self.events_at_checkpoint) >= CHECKPOINT_EVENTS
            || self.output_bytes.saturating_sub(self.output_at_checkpoint)
                >= CHECKPOINT_OUTPUT_BYTES
            || self.checkpoint_at.elapsed() >= CHECKPOINT_WALL;
        if due {
            self.publish_checkpoint(receipt)?;
        }
        Ok(receipt)
    }

    fn publish_checkpoint(&mut self, receipt: Receipt) -> Result<()> {
        let started = Instant::now();
        self.timeline.sync()?;
        checkpoint::publish_with_history(
            &self.root,
            &self.header,
            receipt,
            &self.terminal,
            Some(self.history.checkpoint()?),
        )?;
        self.checkpoint_durations_ms
            .push(started.elapsed().as_secs_f64() * 1000.0);
        self.checkpoints += 1;
        self.events_at_checkpoint = self.event_count;
        self.output_at_checkpoint = self.output_bytes;
        self.checkpoint_at = Instant::now();
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let geometry = Geometry::new(rows, cols)?;
        self.event(EventKind::Resize, &serde_json::to_vec(&geometry)?, false)?;
        Ok(())
    }

    fn output(&mut self, bytes: &[u8], force_checkpoint: bool) -> Result<()> {
        self.event(EventKind::Output, bytes, force_checkpoint)?;
        Ok(())
    }

    fn finish(
        mut self,
        requested_output: u64,
        requested_resizes: u64,
        recipe: EndingRecipe,
        verify: bool,
    ) -> Result<SessionMetrics> {
        ensure!(
            self.body_output_bytes == requested_output,
            "output byte accounting mismatch"
        );
        ensure!(
            self.resize_events == requested_resizes,
            "resize event accounting mismatch"
        );
        ensure!(
            !recipe.tail.is_empty(),
            "ending recipe must include a tail event"
        );
        self.output_phase = OutputPhase::Tail;
        let mut last = self.last_receipt.context("corpus has no durable event")?;
        let final_tail_events = recipe.tail.len() as u64;
        for (kind, payload) in recipe.tail {
            if kind == EventKind::Output {
                self.tail_output_bytes += payload.len() as u64;
            }
            ensure!(
                kind != EventKind::OutputRejectedResource,
                "ending recipe cannot apply refused raw output"
            );
            last = self.journal.append(kind, &payload)?;
            self.history.begin(last.sequence)?;
            match kind {
                EventKind::Output => self.terminal.output(&payload),
                EventKind::Resize => anyhow::bail!("ending recipe cannot resize"),
                EventKind::SyncTimeout => self.terminal.sync_timeout(),
                EventKind::Exit => anyhow::bail!("ending recipe cannot exit"),
                EventKind::OutputRejectedResource => {
                    unreachable!("ending recipe guard rejects refusals")
                }
            }
            self.history.commit()?;
            self.timeline.record(last.sequence, kind)?;
            self.event_count += 1;
        }
        let (
            logical_timeline,
            logical_timeline_bytes,
            logical_timeline_sha256,
            logical_timeline_duration_ns,
        ) = self.timeline.finish()?;
        let final_prelude_events = self.prelude_events;
        drop(self.journal);
        let journal_bytes = fs::metadata(&self.journal_path)?.len();
        let history = self.history.summary();
        let cold_history_bytes = directory_bytes_with_extensions(
            &self
                .root
                .join(format!("{}.history", self.header.generation)),
        )?;
        let result = if verify {
            Some(verify_recovery_against_live(
                &self.journal_path,
                &self.terminal,
                &self.history,
            )?)
        } else {
            None
        };
        Ok(SessionMetrics {
            session_id: self.header.session_id,
            generation: self.header.generation,
            journal: self
                .journal_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            journal_bytes,
            logical_timeline,
            logical_timeline_bytes,
            logical_timeline_sha256,
            logical_timeline_duration_ns,
            requested_output_bytes: self.body_output_bytes,
            final_prelude_output_bytes: self.prelude_output_bytes,
            final_tail_output_bytes: self.tail_output_bytes,
            final_prelude_events,
            final_tail_events,
            resize_events: self.resize_events,
            total_events: self.event_count,
            checkpoints: self.checkpoints,
            checkpoint_durations_ms: self.checkpoint_durations_ms,
            cold_history_records: history.records,
            cold_history_bytes,
            final_sequence: last.sequence,
            verification: result,
        })
    }
}

fn now_ms() -> Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}

fn directory_bytes_with_extensions(root: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            total = total.saturating_add(directory_bytes_with_extensions(&entry.path())?);
        } else if kind.is_file()
            && (entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "archive")
                || entry.file_name() == "rows.index")
        {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

fn unique_root(parent: &Path) -> Result<PathBuf> {
    fs::create_dir_all(parent)?;
    for _ in 0..32 {
        let root = parent.join(format!("long-history-{}", Uuid::new_v4()));
        match fs::DirBuilder::new().mode(0o700).create(&root) {
            Ok(()) => return Ok(root),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    anyhow::bail!("cannot allocate unique corpus directory")
}

fn scenario_events() -> [(EventKind, &'static [u8], bool); SCENARIO_EVENTS as usize] {
    [
        (EventKind::Output, b"\x1b[?1049h", false),
        (
            EventKind::Output,
            "alternate 界 e\u{301}\r\n".as_bytes(),
            true,
        ),
        (EventKind::Output, b"\x1b[?25", true),
        (EventKind::Output, b"l", true),
        (
            EventKind::Output,
            b"\x1b]8;;https://example.invalid/corpus",
            false,
        ),
        (EventKind::Output, b"\x1b", true),
        (EventKind::Output, b"\\linked text\x1b]8;;\x1b\\\r\n", true),
        (EventKind::Output, &[0xe7, 0x95], true),
        (EventKind::Output, &[0x8c], true),
        (EventKind::Output, b"\x1b[?2026h", true),
        (EventKind::Output, b"synchronized pending output", false),
        (EventKind::SyncTimeout, &[], true),
        (EventKind::Output, b"\x1b[0m\x1b[?1049l\r\n", true),
    ]
}

fn scenario_output_bytes() -> u64 {
    scenario_events()
        .iter()
        .filter(|(kind, _, _)| *kind == EventKind::Output)
        .map(|(_, payload, _)| payload.len() as u64)
        .sum()
}

fn emit_scenario(session: &mut SessionBuilder) -> Result<u64> {
    let mut bytes = 0u64;
    for (kind, payload, force_checkpoint) in scenario_events() {
        session.event(kind, payload, force_checkpoint)?;
        if kind == EventKind::Output {
            bytes += payload.len() as u64;
        }
    }
    Ok(bytes)
}

fn ending_recipe(index: u32, body_final_sequence: u64) -> Result<EndingRecipe> {
    let output = |bytes: &[u8]| (EventKind::Output, bytes.to_vec());
    let recipe = match index % 4 {
        0 => EndingRecipe {
            prelude: vec![output(&[0xe7, 0x95])],
            tail: vec![(
                EventKind::Output,
                [vec![0x8c], b"\r\ncorpus-tail\r\n".to_vec()].concat(),
            )],
        },
        1 => EndingRecipe {
            prelude: vec![output(b"\x1b[?1049h"), output(b"\x1b[31")],
            tail: vec![output("mALT final 界\r\ncorpus-tail\r\n".as_bytes())],
        },
        2 => EndingRecipe {
            prelude: vec![
                output(b"\x1b]8;;https://example.invalid/final"),
                output(b"\x1b"),
            ],
            tail: vec![output(
                b"\\linked \xe7\x95\x8c\x1b]8;;\x1b\\\r\ncorpus-tail\r\n",
            )],
        },
        _ => EndingRecipe {
            prelude: vec![
                output(b"\x1b[?2026h"),
                output(b"ending synchronized pending output"),
            ],
            tail: vec![
                (EventKind::SyncTimeout, Vec::new()),
                output(b"\x1b[0m\r\ncorpus-tail\r\n"),
            ],
        },
    };
    let event_count = recipe.prelude.len() as u64 + recipe.tail.len() as u64;
    let final_sequence = body_final_sequence
        .checked_add(event_count)
        .context("ending sequence overflow")?;
    ensure!(
        final_sequence > body_final_sequence + 1,
        "ending is too short"
    );
    Ok(recipe)
}

fn text_chunk(length: usize, seed: u64, ordinal: u64) -> Vec<u8> {
    let mut line =
        "history-corpus seed=0000000000000000 row=0000000000000000 unicode=✓-界-e\u{301}\r\n"
            .as_bytes()
            .to_vec();
    let seed_text = format!("{seed:016x}");
    let ordinal_text = format!("{ordinal:016x}");
    line[20..36].copy_from_slice(seed_text.as_bytes());
    line[41..57].copy_from_slice(ordinal_text.as_bytes());
    let mut output = Vec::with_capacity(length);
    while output.len() + line.len() <= length {
        output.extend_from_slice(&line);
    }
    // Padding keeps exact byte accounting without cutting a Unicode scalar.
    output.resize(length, b' ');
    output
}

fn logical_duration_ns(hours: f64) -> Result<u64> {
    let duration = hours * 3_600_000_000_000.0;
    ensure!(
        duration.is_finite()
            && duration >= SYNC_TIMEOUT_TIMESTAMP_NS as f64
            && duration < u64::MAX as f64,
        "logical hours cannot be represented as nanoseconds or do not cover SyncTimeout"
    );
    Ok(duration.round() as u64)
}

fn fractional_output_target(total: u64, interval: u64, interval_count: u64) -> u64 {
    ((total as u128 * interval as u128) / interval_count as u128) as u64
}

fn base_output_event_count(output_bytes: u64, resize_events: u64) -> Result<u64> {
    let interval_count = resize_events
        .checked_add(1)
        .context("resize interval count overflow")?;
    let mut written = 0u64;
    let mut events = 0u64;
    for interval in 1..=interval_count {
        let target = fractional_output_target(output_bytes, interval, interval_count);
        let bytes = target - written;
        events = events
            .checked_add(bytes / OUTPUT_CHUNK as u64)
            .and_then(|count| {
                count.checked_add(u64::from(!bytes.is_multiple_of(OUTPUT_CHUNK as u64)))
            })
            .context("output event count overflow")?;
        written = target;
    }
    ensure!(
        written == output_bytes,
        "output event plan did not cover requested bytes"
    );
    Ok(events)
}

fn parse_args() -> Result<Options> {
    let mut options = Options {
        directory: PathBuf::from("."),
        sessions: 4,
        output_bytes: 1024 * 1024 * 1024,
        resize_events: 100_000,
        logical_hours: 24.0,
        seed: 1,
        verify: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = |args: &mut std::iter::Skip<std::env::Args>, name: &str| -> Result<String> {
            args.next()
                .with_context(|| format!("missing value for {name}"))
        };
        match arg.as_str() {
            "--directory" => options.directory = value(&mut args, &arg)?.into(),
            "--sessions" => options.sessions = value(&mut args, &arg)?.parse()?,
            "--output-bytes" => options.output_bytes = value(&mut args, &arg)?.parse()?,
            "--resize-events" => options.resize_events = value(&mut args, &arg)?.parse()?,
            "--logical-hours" => options.logical_hours = value(&mut args, &arg)?.parse()?,
            "--seed" => options.seed = value(&mut args, &arg)?.parse()?,
            "--verify" => options.verify = true,
            "--help" | "-h" => {
                println!(
                    "history-corpus [--directory DIR] [--sessions N] [--output-bytes N] [--resize-events N] [--logical-hours H] [--seed N] [--verify]"
                );
                std::process::exit(0);
            }
            _ => anyhow::bail!("unknown argument {arg}"),
        }
    }
    ensure!(
        options.sessions > 0 && options.sessions <= 32,
        "sessions must be in 1..=32"
    );
    ensure!(
        options.resize_events <= 1_000_000,
        "resize events must be at most 1000000"
    );
    ensure!(
        options.logical_hours.is_finite() && options.logical_hours > 0.0,
        "logical hours must be positive and finite"
    );
    ensure!(
        options.output_bytes > 4096,
        "output byte budget is too small for boundary corpus"
    );
    Ok(options)
}

fn main() -> Result<()> {
    let options = parse_args()?;
    let parent = options.directory.canonicalize().or_else(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            fs::create_dir_all(&options.directory)?;
            options.directory.canonicalize()
        } else {
            Err(e)
        }
    })?;
    let root = unique_root(&parent)?;
    let started = now_ms()?;
    let logical_duration_ns = logical_duration_ns(options.logical_hours)?;
    let scenario_bytes = scenario_output_bytes();
    ensure!(
        scenario_bytes < options.output_bytes,
        "scenario exceeds output byte budget"
    );
    let base_output_bytes = options.output_bytes - scenario_bytes;
    let output_events = base_output_event_count(base_output_bytes, options.resize_events)?;
    let body_events = SCENARIO_EVENTS
        .checked_add(output_events)
        .and_then(|events| events.checked_add(options.resize_events))
        .context("planned corpus event count overflow")?;
    let mut results = Vec::new();
    for index in 0..options.sessions {
        let recipe = ending_recipe(index, body_events)?;
        let expected_events = body_events
            .checked_add(recipe.prelude.len() as u64)
            .and_then(|events| events.checked_add(recipe.tail.len() as u64))
            .context("planned ending event count overflow")?;
        let mut sync_pairs = vec![(SYNC_BEGIN_SEQUENCE, SYNC_TIMEOUT_SEQUENCE)];
        if index % 4 == 3 {
            let ending_begin = body_events + 1;
            sync_pairs.push((ending_begin, ending_begin + 2));
        }
        let mut session = SessionBuilder::new(
            &root,
            options.seed,
            index,
            logical_duration_ns,
            expected_events,
            sync_pairs,
        )?;
        let scenario_bytes = emit_scenario(&mut session)?;
        ensure!(
            scenario_bytes < options.output_bytes,
            "scenario exceeds output byte budget"
        );
        let remaining = options.output_bytes - scenario_bytes;
        let mut output_written = 0u64;
        let mut resize_index = 0u64;
        while output_written < remaining || resize_index < options.resize_events {
            let target =
                fractional_output_target(remaining, resize_index + 1, options.resize_events + 1);
            while output_written < target {
                let size = (target - output_written).min(OUTPUT_CHUNK as u64) as usize;
                let chunk = text_chunk(size, options.seed, output_written / OUTPUT_CHUNK as u64);
                session.output(&chunk, false)?;
                output_written += size as u64;
            }
            if resize_index < options.resize_events {
                // Every event changes rows; every 5,000th also changes width to
                // cover bounded reflow without making all 100,000 events reflow.
                let rows = if resize_index.is_multiple_of(2) {
                    35
                } else {
                    36
                };
                let width_epoch = resize_index / 5_000;
                let cols = if width_epoch.is_multiple_of(2) {
                    120
                } else {
                    121
                };
                session.resize(rows, cols)?;
                resize_index += 1;
            }
        }
        ensure!(
            session.body_output_bytes == options.output_bytes,
            "body output byte accounting mismatch before final boundary"
        );
        session.output_phase = OutputPhase::Prelude;
        ensure!(
            !recipe.prelude.is_empty(),
            "ending prelude must establish partial state"
        );
        let prelude_count = recipe.prelude.len();
        for (event_index, (kind, payload)) in recipe.prelude.iter().enumerate() {
            session.event(*kind, payload, event_index + 1 == prelude_count)?;
        }
        let metrics = session.finish(
            options.output_bytes,
            options.resize_events,
            recipe,
            options.verify,
        )?;
        results.push(metrics);
    }
    let manifest = Manifest {
        schema: 1,
        kind: "production-model-journal-cold-history-corpus",
        created_unix_ms: started,
        completed_unix_ms: now_ms()?,
        core_profile: CORE_PROFILE,
        sessions: options.sessions,
        requested_output_bytes_per_session: options.output_bytes,
        resize_events_per_session: options.resize_events,
        logical_hours_metadata: options.logical_hours,
        logical_schedule_scope_note: "Each versioned per-session JSONL sidecar maps every journal sequence to deterministic logical nanoseconds across this nominal duration, including final partial-state boundaries. Every synchronized-output timeout is placed exactly 200 ms after its begin. Logical time is accelerated metadata, not elapsed wall time or a real-time soak.",
        seed: options.seed,
        verification_requested: options.verify,
        claims: [
            "actual production Terminal model",
            "actual durable Journal event append and fsync",
            "actual History observer and derived archive/index commits",
            "actual atomic checkpoint publication",
            "actual geometry-changing resize events",
            "no PTY, UI, GPU, provider CLI, authentication, VM reboot, or real-time soak",
            "logical timeline hours do not encode elapsed wall time",
        ],
        results,
    };
    let manifest_path = root.join("manifest.json");
    let mut manifest_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&manifest_path)?;
    serde_json::to_writer_pretty(&mut manifest_file, &manifest)?;
    manifest_file.write_all(b"\n")?;
    manifest_file.sync_all()?;
    File::open(&root)?.sync_all()?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"corpus_directory":root,"manifest":manifest_path})
        )?
    );
    Ok(())
}
