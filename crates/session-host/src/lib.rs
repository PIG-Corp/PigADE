//! Linux prototype: authenticated local IPC and durable, per-generation terminal actors.
mod catalog;
#[doc(hidden)]
pub mod checkpoint;
pub mod history;
pub mod history_read;
pub mod input;
pub use catalog::LayoutAction;
mod ipc_server;
mod launch_gate;
pub mod metrics;
pub mod poll;
#[cfg(test)]
mod recovery_tests;
mod replay_cache;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use pigade_storage::{
    EventKind, Journal, JournalHeader, JournalReader, MetadataStore, ProcessIdentity,
};
use pigade_terminal::{CORE_PROFILE, Frame, Geometry, Terminal};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

pub const PROTOCOL: u32 = 2;
const IPC_LIMIT: usize = 8 * 1024 * 1024;
const OUTPUT_CHUNK: usize = 16 * 1024;
// Bound each already-queued output group independently of the PTY read size.
// Ordered control events remain boundaries. A short collection deadline lets
// the PTY reader finish a burst split across several kernel reads.
const OUTPUT_BATCH: usize = 64 * 1024;
const OUTPUT_BATCH_WINDOW: Duration = Duration::from_micros(250);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launch {
    pub session: Uuid,
    pub generation: Uuid,
    pub operation: Uuid,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub geometry: Geometry,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    List,
    HistoryRead {
        query: history_read::Query,
    },
    Poll {
        known: Vec<poll::Known>,
    },
    Catalog,
    CatalogApply {
        batch: pigade_storage::CatalogBatch,
    },
    LayoutEdit {
        expected_revision: u64,
        action: LayoutAction,
    },
    Launch {
        spec: Launch,
    },
    Snapshot {
        session: Uuid,
        generation: Uuid,
    },
    Input {
        session: Uuid,
        generation: Uuid,
        bytes: Vec<u8>,
    },
    Resize {
        session: Uuid,
        generation: Uuid,
        geometry: Geometry,
    },
    Stop {
        session: Uuid,
        generation: Uuid,
    },
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub command: Command,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub session: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub state: String,
    pub recovery_path: String,
    pub pid: Option<u32>,
    pub storage_error: Option<String>,
    pub frame: Frame,
    #[serde(default)]
    pub history: Option<history::Summary>,
    #[serde(default)]
    pub recovery_journal_bytes_read: u64,
    #[serde(default)]
    pub metrics: metrics::SessionMetrics,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub protocol: u32,
    pub views: Vec<View>,
    pub error: Option<String>,
    /// Present only when the complete command was rejected before admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_before_admission: Option<CommandRejection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<pigade_storage::Catalog>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_notice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<Vec<poll::SessionInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_chunk: Option<history::RecordChunk>,
}

/// Stable machine-readable proof that no part of a command was admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandRejection {
    QueueFull,
}

impl std::fmt::Display for CommandRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull => f.write_str("command queue full; command was not admitted"),
        }
    }
}
impl std::error::Error for CommandRejection {}

fn rejected_before_admission(error: &anyhow::Error) -> Option<CommandRejection> {
    error.downcast_ref::<CommandRejection>().copied()
}

struct MessageBuffer(Vec<u8>);
impl Write for MessageBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let length = self
            .0
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("IPC length overflow"))?;
        if length > IPC_LIMIT {
            return Err(std::io::Error::other("IPC payload too large"));
        }
        if length > self.0.capacity() {
            let capacity = self
                .0
                .capacity()
                .saturating_mul(2)
                .max(length)
                .min(IPC_LIMIT);
            self.0
                .try_reserve_exact(capacity - self.0.len())
                .map_err(std::io::Error::other)?;
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn send<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let mut buffer = MessageBuffer(Vec::new());
    serde_json::to_writer(&mut buffer, value)?;
    let bytes = buffer.0;
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}
pub fn receive<T: DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    ensure!(length > 0 && length <= IPC_LIMIT, "invalid IPC length");
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}
pub fn request(socket: &Path, command: Command) -> Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    send(
        &mut stream,
        &Request {
            protocol: PROTOCOL,
            command,
        },
    )?;
    receive(&mut stream)
}
pub(crate) fn peer_pid(stream: &UnixStream) -> Result<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: all pointers reference initialized storage of the advertised size.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    ensure!(result == 0, "peer credentials unavailable");
    // SAFETY: getuid has no preconditions.
    ensure!(
        credentials.uid == unsafe { libc::getuid() },
        "foreign user endpoint"
    );
    Ok(credentials.pid as u32)
}
pub fn identity(pid: u32) -> Result<ProcessIdentity> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let rest = stat.rsplit_once(") ").context("invalid process stat")?.1;
    let ticks = rest
        .split_whitespace()
        .nth(19)
        .context("missing process start time")?
        .parse()?;
    Ok(ProcessIdentity {
        boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_owned(),
        pid,
        start_ticks: ticks,
    })
}
fn private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let meta = fs::symlink_metadata(path)?;
    use std::os::unix::fs::MetadataExt;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::getuid() },
        "unsafe state directory"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

struct Session {
    view: Arc<Mutex<View>>,
    commands: mpsc::SyncSender<Action>,
    input: Option<input::InputWriter>,
    control: mpsc::SyncSender<Control>,
    _history: Option<Arc<history::History>>,
}
#[derive(Clone)]
enum Action {
    Wake,
    Output(Vec<u8>),
    SyncTimeout,
    Resize(Geometry),
    ReadError(String),
    Eof,
}
enum Control {
    Stop,
}
struct ActorConfig {
    geometry: Geometry,
    root: PathBuf,
    control: mpsc::Receiver<Control>,
    history: Arc<history::History>,
}
struct Host {
    root: PathBuf,
    endpoint_root: PathBuf,
    sessions: Vec<Session>,
    metadata: MetadataStore,
    catalog_notice: Option<String>,
    _lock: File,
}

fn restore(path: &Path, fast: bool) -> Result<View> {
    Ok(restore_model(path, fast)?.0)
}

fn validate_replay_payload(
    kind: EventKind,
    payload: &[u8],
) -> std::result::Result<Option<Geometry>, &'static str> {
    match kind {
        EventKind::Resize => serde_json::from_slice(payload)
            .map(Some)
            .map_err(|_| "invalid resize geometry"),
        EventKind::SyncTimeout | EventKind::Exit if !payload.is_empty() => {
            Err("unexpected timer/exit payload")
        }
        _ => Ok(None),
    }
}

fn append_recovery_error(storage_error: &mut Option<String>, message: String) {
    *storage_error = Some(match storage_error.take() {
        Some(previous) => format!("{previous}; {message}"),
        None => message,
    });
}

fn restore_model(
    path: &Path,
    fast: bool,
) -> Result<(View, Terminal, Option<Arc<history::History>>)> {
    let mut reader = JournalReader::open(path)?;
    let header = reader.header().clone();
    ensure!(
        header.profile == CORE_PROFILE,
        "incompatible terminal profile; source preserved"
    );
    let mut terminal = Terminal::new(
        Geometry::new(header.initial_rows, header.initial_cols)?,
        false,
    );
    let mut sequence = 0;
    let mut recovery_path = "full-journal".to_owned();
    let mut recovery_warnings = Vec::new();
    let parent = path.parent().context("journal parent")?;
    let mut history = Some(history::History::replay(parent, &header, None)?);
    let selection = if fast {
        checkpoint::latest(
            path.parent().context("journal parent")?,
            &header,
            fs::metadata(path)?.len(),
        )?
    } else {
        checkpoint::Selection {
            loaded: None,
            warnings: Vec::new(),
        }
    };
    recovery_warnings.extend(selection.warnings);
    let saved = if selection
        .loaded
        .as_ref()
        .is_some_and(|saved| saved.history.is_none())
        && pigade_storage::scan_valid_prefix(path)?
            .corruption
            .is_none()
    {
        // Legacy hot-only checkpoints cannot stand in for a cold-history root.
        // Rebuild from valid raw events; retain the old hot checkpoint only when
        // raw corruption prevents complete reconstruction (explicit warning).
        recovery_path = "full-journal-legacy-checkpoint".into();
        None
    } else {
        selection.loaded
    };
    if let Some(saved) = saved {
        match reader.resume_after_checkpoint(saved.sequence, saved.offset) {
            Ok(tail) => {
                reader = tail;
                terminal = saved.terminal;
                sequence = saved.sequence;
                recovery_path = "checkpoint-tail".into();
                history = if let Some(saved_history) = saved.history {
                    Some(history::History::replay(
                        parent,
                        &header,
                        Some(saved_history),
                    )?)
                } else {
                    recovery_warnings.push("legacy checkpoint has no cold-history root; hot state retained, full raw replay required for complete history".into());
                    None
                };
            }
            Err(error) => {
                eprintln!("checkpoint watermark rejected; full replay: {error:#}");
                reader = JournalReader::open(path)?;
            }
        }
    }
    if let Some(history) = &history {
        terminal.set_history_observer(Some(history.clone()));
    }
    let mut storage_error = (!recovery_warnings.is_empty()).then(|| recovery_warnings.join("; "));
    loop {
        let event = match reader.next_event() {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(error) => {
                append_recovery_error(
                    &mut storage_error,
                    format!("recovered valid prefix only: {error:#}"),
                );
                break;
            }
        };
        // A valid frame CRC does not prove its typed payload is applicable to
        // this terminal profile. Validate before history.begin or any mutation
        // so malformed tails cannot hide an otherwise recoverable generation.
        let resize = match validate_replay_payload(event.kind, &event.payload) {
            Ok(geometry) => geometry,
            Err(reason) => {
                append_recovery_error(
                    &mut storage_error,
                    format!(
                        "recovered valid prefix only: rejected event sequence {}: {reason}; raw tail retained",
                        event.sequence
                    ),
                );
                break;
            }
        };
        if let Some(history) = &history {
            history.begin(event.sequence)?;
        }
        match event.kind {
            EventKind::Output => {
                terminal.output(&event.payload);
            }
            EventKind::Resize => terminal.resize(resize.expect("validated resize geometry")),
            EventKind::SyncTimeout => terminal.sync_timeout(),
            EventKind::Exit => (),
        }
        if let Some(history) = &history {
            history.replay_event()?;
        }
        sequence = event.sequence;
    }
    if let Some(history) = &history {
        history.finish_replay()?;
    }
    let view = View {
        session: header.session_id,
        generation: header.generation,
        sequence,
        state: "history".into(),
        recovery_path,
        pid: None,
        storage_error,
        frame: terminal.frame(),
        history: history.as_ref().map(|h| h.summary()),
        recovery_journal_bytes_read: reader.bytes_read(),
        metrics: metrics::SessionMetrics::default(),
    };
    Ok((view, terminal, history))
}
impl Host {
    fn history_source(&self, query: &history_read::Query) -> Result<Arc<history::History>> {
        query.validate()?;
        for session in &self.sessions {
            let view = session.view.lock().expect("view lock");
            if view.session == query.session && view.generation == query.generation {
                ensure!(
                    view.sequence == query.expected_sequence,
                    "history view sequence changed"
                );
                return session
                    ._history
                    .clone()
                    .context("cold history unavailable for this generation");
            }
        }
        bail!("unknown session generation")
    }
    fn dispatch(&mut self, command: Command) -> Result<Response> {
        if let Command::Poll { known } = command {
            self.reconcile_exits()?;
            let result = poll::poll(self.sessions.iter().map(|s| s.view.as_ref()), &known)?;
            return Ok(Response {
                protocol: PROTOCOL,
                views: result.views,
                error: None,
                rejected_before_admission: None,
                catalog: None,
                catalog_notice: self.catalog_notice.clone(),
                selected: Some(result.selected),
                history_chunk: None,
            });
        }
        let catalog = match command {
            Command::Catalog => catalog::read_metadata(&self.metadata)?,
            Command::CatalogApply { batch } => catalog::apply_metadata(&mut self.metadata, batch)?,
            Command::LayoutEdit {
                expected_revision,
                action,
            } => catalog::layout_edit(&mut self.metadata, expected_revision, action)?,
            terminal => {
                return Ok(Response {
                    protocol: PROTOCOL,
                    views: self.handle(terminal)?,
                    error: None,
                    rejected_before_admission: None,
                    catalog: None,
                    catalog_notice: None,
                    selected: None,
                    history_chunk: None,
                });
            }
        };
        Ok(Response {
            protocol: PROTOCOL,
            views: Vec::new(),
            error: None,
            rejected_before_admission: None,
            catalog: Some(catalog),
            catalog_notice: self.catalog_notice.clone(),
            selected: None,
            history_chunk: None,
        })
    }
    fn open(root: PathBuf) -> Result<Self> {
        private_directory(&root)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(root.join("host.lock"))?;
        // SAFETY: flock borrows a valid descriptor for the duration of the call.
        ensure!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
            "another host owns this state"
        );
        let mut metadata = MetadataStore::open(root.join("metadata.sqlite"))?;
        let catalog_notice = match catalog::migrate_legacy_layout(&mut metadata) {
            Ok(notice) => notice,
            Err(error) => Some(format!(
                "Catalog recovery requires a compatible reader; originals preserved: {error:#}"
            )),
        };
        if let Some(notice) = &catalog_notice {
            eprintln!("{notice}");
        }
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&root)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "journal") {
                match restore_model(&path, true) {
                    Ok((view, _terminal, history)) => {
                        let (tx, _rx) = mpsc::sync_channel(1);
                        let (control, _) = mpsc::sync_channel(1);
                        sessions.push(Session {
                            view: Arc::new(Mutex::new(view)),
                            commands: tx,
                            input: None,
                            control,
                            _history: history,
                        });
                    }
                    Err(error) => eprintln!("history unavailable {}: {error:#}", path.display()),
                }
            }
        }
        // Never spawn on recovery. Every unfinished launch remains locked for investigation.
        for record in metadata.list_launches()? {
            if !matches!(record.state, pigade_storage::LaunchState::Exited) {
                metadata.mark_unknown(
                    record.operation_id,
                    "host restarted: reconcile process identity before any launch",
                )?;
                for session in &sessions {
                    let mut view = session.view.lock().expect("view lock");
                    if view.generation == record.generation {
                        view.state = "launch_outcome_unknown".into();
                    }
                }
            }
        }
        Ok(Self {
            endpoint_root: root.clone(),
            root,
            sessions,
            metadata,
            catalog_notice,
            _lock: lock,
        })
    }
    fn reconcile_exits(&mut self) -> Result<()> {
        // The actor cannot write SQLite. Poll metadata only when an attached
        // generation has actually exited; idle polls do not scan launch rows.
        if !self
            .sessions
            .iter()
            .any(|s| s.input.is_some() && s.view.lock().expect("view lock").state == "exited")
        {
            return Ok(());
        }
        for record in self.metadata.list_launches()? {
            if record.state == pigade_storage::LaunchState::Running
                && self.sessions.iter().any(|s| {
                    let v = s.view.lock().expect("view lock");
                    v.generation == record.generation && v.state == "exited"
                })
            {
                self.metadata.mark_exited(record.operation_id)?;
                for session in &mut self.sessions {
                    if session.view.lock().expect("view lock").generation == record.generation {
                        session.input = None;
                    }
                }
            }
        }
        Ok(())
    }
    fn handle(&mut self, command: Command) -> Result<Vec<View>> {
        self.reconcile_exits()?;
        if let Command::Launch { spec } = command {
            return self.launch(spec);
        }
        if matches!(command, Command::List) {
            return Ok(self
                .sessions
                .iter()
                .map(|s| s.view.lock().expect("view lock").clone())
                .collect());
        }
        let (id, generation) = match &command {
            Command::Snapshot {
                session,
                generation,
            }
            | Command::Input {
                session,
                generation,
                ..
            }
            | Command::Resize {
                session,
                generation,
                ..
            }
            | Command::Stop {
                session,
                generation,
            } => (*session, *generation),
            _ => unreachable!(),
        };
        let session = self
            .sessions
            .iter()
            .find(|s| {
                let v = s.view.lock().expect("view lock");
                v.session == id && v.generation == generation
            })
            .context("unknown session generation")?;
        match command {
            Command::Input { bytes, .. } => {
                ensure!(bytes.len() <= OUTPUT_CHUNK, "input exceeds limit");
                let result = session
                    .input
                    .as_ref()
                    .context("historical terminal is read-only")?
                    .try_write(bytes);
                match result {
                    Ok(()) => (),
                    Err(input::InputWriteError::Full) => {
                        return Err(CommandRejection::QueueFull.into());
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Command::Resize { geometry, .. } => {
                Geometry::new(geometry.rows, geometry.cols)?;
                match session.commands.try_send(Action::Resize(geometry)) {
                    Ok(()) => (),
                    Err(mpsc::TrySendError::Full(_)) => {
                        return Err(CommandRejection::QueueFull.into());
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        return Err(anyhow::anyhow!("session command actor unavailable"));
                    }
                }
            }
            Command::Stop { .. } => {
                session.control.try_send(Control::Stop)?;
                // Wake a clean actor sleeping without a timer. A full output
                // queue already guarantees it will wake and inspect Stop.
                let _ = session.commands.try_send(Action::Wake);
            }
            _ => (),
        }
        Ok(vec![session.view.lock().expect("view lock").clone()])
    }
    fn launch(&mut self, spec: Launch) -> Result<Vec<View>> {
        Geometry::new(spec.geometry.rows, spec.geometry.cols)?;
        ensure!(self.sessions.len() < 32, "prototype limit: 32 generations");
        ensure!(
            spec.executable.is_absolute() && spec.executable.is_file(),
            "absolute executable required"
        );
        ensure!(
            spec.cwd.is_absolute() && spec.cwd.is_dir(),
            "cwd unavailable"
        );
        ensure!(
            spec.args.len() <= 128 && spec.args.iter().map(String::len).sum::<usize>() <= 16384,
            "argv exceeds bounds"
        );
        if let Some(existing) = self.metadata.get_launch(spec.operation)? {
            ensure!(
                existing.session_id == spec.session && existing.generation == spec.generation,
                "operation identity mismatch"
            );
            let view = self.sessions.iter().find_map(|s| {
                let v = s.view.lock().expect("view lock");
                (v.generation == spec.generation).then(|| v.clone())
            });
            return Ok(vec![view.context(
                "existing launch has no attachable frame; outcome must be reconciled",
            )?]);
        }
        let nonce = Uuid::new_v4();
        let begin =
            self.metadata
                .begin_launch(spec.session, spec.generation, spec.operation, nonce)?;
        ensure!(
            begin.created,
            "operation already exists; no duplicate spawn"
        );
        let path = self.root.join(format!("{}.journal", spec.generation));
        let journal = Journal::create(
            &path,
            JournalHeader {
                session_id: spec.session,
                generation: spec.generation,
                profile: CORE_PROFILE.into(),
                initial_rows: spec.geometry.rows,
                initial_cols: spec.geometry.cols,
            },
        )?;
        let history = history::History::live(&self.root, journal.header())?;
        let handshake = launch_gate::LaunchGate::bind(&self.endpoint_root)?;
        let pair = native_pty_system().openpty(pty_size(spec.geometry))?;
        let mut builder = CommandBuilder::new(std::env::current_exe()?);
        builder.arg("__helper");
        builder.arg(&handshake.path);
        builder.arg(nonce.to_string());
        builder.arg(&spec.executable);
        builder.args(&spec.args);
        builder.cwd(&spec.cwd);
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");
        let mut child = pair.slave.spawn_command(builder)?;
        drop(pair.slave);
        let child_pid = child.process_id().context("helper pid unavailable")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut gate = loop {
            match handshake.listener.accept() {
                Ok((stream, _)) => {
                    ensure!(peer_pid(&stream)? == child_pid, "unexpected helper peer");
                    break stream;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        self.metadata
                            .mark_unknown(spec.operation, "helper handshake timeout")?;
                        bail!("helper handshake timeout");
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.into()),
            }
        };
        gate.set_read_timeout(Some(Duration::from_secs(5)))?;
        let hello: String = receive(&mut gate)?;
        ensure!(hello == nonce.to_string(), "wrong launch nonce");
        self.metadata
            .register_helper(spec.operation, identity(child_pid)?)?;
        self.metadata.permit_exec(spec.operation)?;
        send(&mut gate, &nonce.to_string())?;
        drop(gate);
        drop(handshake);
        self.metadata.mark_running(spec.operation)?;
        let view = Arc::new(Mutex::new(View {
            session: spec.session,
            generation: spec.generation,
            sequence: 0,
            state: "running".into(),
            recovery_path: "live".into(),
            pid: Some(child_pid),
            storage_error: None,
            frame: Terminal::new(spec.geometry, false).frame(),
            history: Some(history.summary()),
            recovery_journal_bytes_read: 0,
            metrics: metrics::SessionMetrics::default(),
        }));
        let (tx, rx) = mpsc::sync_channel(8);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = input::InputWriter::new(pair.master.take_writer()?);
        let (control_tx, control_rx) = mpsc::sync_channel(1);
        let reader_tx = tx.clone();
        thread::spawn(move || {
            loop {
                let mut bytes = vec![0; OUTPUT_CHUNK];
                match reader.read(&mut bytes) {
                    Ok(0) => {
                        let _ = reader_tx.send(Action::Eof);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                        let _ = reader_tx.send(Action::Eof);
                        break;
                    }
                    Err(error) => {
                        let _ = reader_tx.send(Action::ReadError(error.to_string()));
                        break;
                    }
                    Ok(n) => {
                        bytes.truncate(n);
                        if reader_tx.send(Action::Output(bytes)).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let actor_view = view.clone();
        let actor_root = self.root.clone();
        let actor_writer = writer.clone();
        let actor_history = history.clone();
        thread::spawn(move || {
            actor(
                rx,
                pair.master,
                actor_writer,
                child,
                journal,
                actor_view,
                ActorConfig {
                    geometry: spec.geometry,
                    root: actor_root,
                    control: control_rx,
                    history: actor_history,
                },
            )
        });
        self.sessions.push(Session {
            view: view.clone(),
            commands: tx,
            input: Some(writer),
            control: control_tx,
            _history: Some(history),
        });
        Ok(vec![view.lock().expect("view lock").clone()])
    }
}
fn pty_size(g: Geometry) -> PtySize {
    PtySize {
        rows: g.rows,
        cols: g.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}
fn coalesce_output(
    action: Action,
    rx: &mpsc::Receiver<Action>,
    deferred: &mut Option<Action>,
) -> Action {
    let Action::Output(mut bytes) = action else {
        return action;
    };
    debug_assert!(deferred.is_none());
    let deadline = Instant::now() + OUTPUT_BATCH_WINDOW;
    while bytes.len() < OUTPUT_BATCH {
        let next = match rx.try_recv() {
            Ok(next) => Some(next),
            Err(mpsc::TryRecvError::Empty) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    None
                } else {
                    rx.recv_timeout(remaining).ok()
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        match next {
            Some(Action::Output(next)) if bytes.len() + next.len() <= OUTPUT_BATCH => {
                bytes.extend_from_slice(&next)
            }
            Some(next) => {
                *deferred = Some(next);
                break;
            }
            None => break,
        }
    }
    Action::Output(bytes)
}

#[cfg(test)]
mod batching_tests {
    use super::*;
    #[test]
    fn backlog_groups_only_four_read_chunks_without_consuming_the_fifth() {
        let (tx, rx) = mpsc::sync_channel(8);
        for byte in 1..=4 {
            tx.send(Action::Output(vec![byte; OUTPUT_CHUNK])).unwrap();
        }
        let mut deferred = None;
        let Action::Output(bytes) =
            coalesce_output(Action::Output(vec![0; OUTPUT_CHUNK]), &rx, &mut deferred)
        else {
            panic!()
        };
        assert_eq!(bytes.len(), OUTPUT_BATCH);
        for (byte, chunk) in bytes.as_chunks::<OUTPUT_CHUNK>().0.iter().enumerate() {
            assert!(chunk.iter().all(|b| *b == byte as u8));
        }
        assert!(deferred.is_none());
        assert!(
            matches!(rx.try_recv().unwrap(), Action::Output(bytes) if bytes == vec![4; OUTPUT_CHUNK])
        );
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn adjacent_output_batches_are_bounded_and_never_cross_resize_or_eof() {
        let (tx, rx) = mpsc::sync_channel(8);
        tx.send(Action::Output(vec![b'b'; 6000])).unwrap();
        tx.send(Action::Resize(Geometry::new(12, 80).unwrap()))
            .unwrap();
        tx.send(Action::Output(vec![b'c'; 6000])).unwrap();
        tx.send(Action::Eof).unwrap();
        let mut deferred = None;
        let Action::Output(bytes) =
            coalesce_output(Action::Output(vec![b'a'; 6000]), &rx, &mut deferred)
        else {
            panic!()
        };
        assert_eq!(bytes, [vec![b'a'; 6000], vec![b'b'; 6000]].concat());
        assert!(matches!(deferred.take(), Some(Action::Resize(_))));
        let Action::Output(bytes) = coalesce_output(rx.try_recv().unwrap(), &rx, &mut deferred)
        else {
            panic!()
        };
        assert_eq!(bytes, vec![b'c'; 6000]);
        assert!(matches!(deferred, Some(Action::Eof)));
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn oversized_next_chunk_is_retained_and_low_volume_is_not_dropped() {
        let (tx, rx) = mpsc::sync_channel(8);
        tx.send(Action::Output(vec![b'b'; OUTPUT_CHUNK])).unwrap();
        let mut deferred = None;
        let Action::Output(bytes) = coalesce_output(
            Action::Output(vec![b'a'; OUTPUT_BATCH - 1]),
            &rx,
            &mut deferred,
        ) else {
            panic!()
        };
        assert_eq!(bytes, vec![b'a'; OUTPUT_BATCH - 1]);
        let Action::Output(next) = deferred.take().unwrap() else {
            panic!()
        };
        assert_eq!(next, vec![b'b'; OUTPUT_CHUNK]);
        assert!(
            matches!(coalesce_output(Action::Output(vec![1]),&rx,&mut deferred),Action::Output(bytes) if bytes==vec![1])
        );
    }
}

/// Bounds dirty recovery tails by data, event count, measured model work and
/// idle wall time. Work is a scheduling heuristic, not a recovery budget claim.
struct CheckpointProgress {
    receipt: Option<pigade_storage::Receipt>,
    bytes: usize,
    events: u32,
    model_work: Duration,
    clock: Instant,
}
impl Default for CheckpointProgress {
    fn default() -> Self {
        Self {
            receipt: None,
            bytes: 0,
            events: 0,
            model_work: Duration::ZERO,
            clock: Instant::now(),
        }
    }
}
impl CheckpointProgress {
    fn observe(&mut self, receipt: pigade_storage::Receipt, payload: usize, work: Duration) {
        self.receipt = Some(receipt);
        self.bytes += 33 + payload;
        self.events += 1;
        self.model_work += work;
    }
    fn publish(
        &mut self,
        force: bool,
        root: &Path,
        header: &JournalHeader,
        terminal: &Terminal,
        history: &history::History,
    ) -> Result<bool> {
        let due = self.events > 0
            && (force
                || self.bytes >= 16 * 1024 * 1024
                || self.events >= 1024
                || self.model_work >= Duration::from_millis(125)
                || self.clock.elapsed() >= Duration::from_secs(5));
        if due {
            checkpoint::publish_with_history(
                root,
                header,
                self.receipt.context("checkpoint lacks durable receipt")?,
                terminal,
                Some(history.checkpoint()?),
            )?;
            self.bytes = 0;
            self.events = 0;
            self.model_work = Duration::ZERO;
            self.clock = Instant::now();
        }
        Ok(due)
    }
}

fn actor(
    rx: mpsc::Receiver<Action>,
    master: Box<dyn MasterPty + Send>,
    writer: input::InputWriter,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    mut journal: Journal,
    view: Arc<Mutex<View>>,
    config: ActorConfig,
) {
    let mut terminal = Terminal::new(config.geometry, true);
    let history = &config.history;
    terminal.set_history_observer(Some(history.clone()));
    let mut deferred = None;
    let mut checkpoints = CheckpointProgress::default();
    let mut metrics = metrics::SessionMetrics::default();
    let mut query_responses: Vec<u8> = Vec::new();
    let mut query_response_offset = 0;
    loop {
        if let Ok(Control::Stop) = config.control.try_recv() {
            let _ = child.kill();
        }
        if !query_responses.is_empty() {
            let end = (query_response_offset + input::MAX_INPUT_BYTES).min(query_responses.len());
            match writer.try_write(query_responses[query_response_offset..end].to_vec()) {
                Ok(()) => {
                    query_response_offset = end;
                    if end == query_responses.len() {
                        query_responses.clear();
                        query_response_offset = 0;
                    } else {
                        continue;
                    }
                }
                Err(input::InputWriteError::Full) => {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                Err(error) => {
                    let mut v = view.lock().expect("view lock");
                    v.storage_error = Some(format!("PTY query response failed: {error}"));
                    v.state = "degraded".into();
                    drop(v);
                    loop {
                        std::hint::black_box((&query_responses, &rx, &master));
                        wait_degraded(&config.control, &mut *child);
                    }
                }
            }
        }
        let mut action = if let Some(action) = deferred.take() {
            action
        } else {
            let received = if terminal.sync_timeout_pending() {
                rx.recv_timeout(Duration::from_millis(10))
            } else if checkpoints.events > 0 {
                rx.recv_timeout(Duration::from_secs(5).saturating_sub(checkpoints.clock.elapsed()))
            } else {
                rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            };
            match received {
                Ok(a) => a,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if terminal.sync_timeout_due() {
                        Action::SyncTimeout
                    } else {
                        // A quiet terminal still checkpoints its last dirty state
                        // after five seconds; no synthetic model event is needed.
                        let checkpoint_start = Instant::now();
                        let checkpoint_result = checkpoints.publish(
                            false,
                            &config.root,
                            journal.header(),
                            &terminal,
                            history,
                        );
                        if matches!(checkpoint_result, Ok(true)) {
                            metrics.idle_checkpoint(checkpoint_start.elapsed());
                            view.lock().expect("view lock").metrics = metrics.clone();
                        }
                        if let Err(error) = checkpoint_result {
                            let mut current = view.lock().expect("view lock");
                            current.storage_error =
                                Some(format!("idle checkpoint failed: {error:#}"));
                            current.state = "degraded".into();
                            drop(current);
                            loop {
                                wait_degraded(&config.control, &mut *child);
                            }
                        }
                        continue;
                    }
                }
                Err(_) => break,
            }
        };
        if matches!(action, Action::Wake) {
            continue;
        }
        if (terminal.sync_timeout_due()
            || matches!(&action, Action::Eof) && terminal.sync_timeout_pending())
            && !matches!(&action, Action::SyncTimeout)
        {
            deferred = Some(action);
            action = Action::SyncTimeout;
        }
        // Collect adjacent output within a 250us scheduling window. This
        // amortizes fsync and frame construction without dropping bytes or
        // crossing resize, EOF or ordered timers. One next action is retained.
        action = coalesce_output(action, &rx, &mut deferred);
        let pending_action = action.clone();
        let result = (|| -> Result<bool> {
            // Closing all slave descriptors is not proof the process exited.
            // Keep priority Stop serviceable instead of blocking in child.wait().
            if matches!(&action, Action::Eof) && child.try_wait()?.is_none() {
                deferred = Some(Action::Eof);
                thread::sleep(Duration::from_millis(2));
                return Ok(true);
            }
            let (kind, payload, resize) = match action {
                Action::Wake => unreachable!("wake is not a terminal event"),
                Action::ReadError(error) => bail!("PTY output read failure: {error}"),
                Action::SyncTimeout => (EventKind::SyncTimeout, Vec::new(), None),
                Action::Output(bytes) => (EventKind::Output, bytes, None),
                Action::Resize(g) => (EventKind::Resize, serde_json::to_vec(&g)?, Some(g)),
                Action::Eof => (EventKind::Exit, Vec::new(), None),
            };
            // One pending chunk plus the bounded reader queue. On failure no model publication.
            if let Some(g) = resize {
                master.resize(pty_size(g))?;
            }
            let journal_start = Instant::now();
            let receipt = journal.append(kind, &payload)?;
            let journal_work = journal_start.elapsed();
            history.begin(receipt.sequence)?;
            let model_start = Instant::now();
            match kind {
                EventKind::Output => terminal.output(&payload),
                EventKind::SyncTimeout => terminal.sync_timeout(),
                EventKind::Resize => {
                    let g = resize.expect("resize geometry");
                    terminal.resize(g);
                }
                EventKind::Exit => (),
            }
            let model_work = model_start.elapsed();
            // Derived cells and index reach durability before either the frame
            // or a checkpoint is allowed to reference their committed root.
            let archive_start = Instant::now();
            history.commit()?;
            let archive_work = archive_start.elapsed();
            checkpoints.observe(receipt, payload.len(), model_work);
            let checkpoint_start = Instant::now();
            let checkpointed = checkpoints.publish(
                kind == EventKind::Exit,
                &config.root,
                journal.header(),
                &terminal,
                history,
            )?;
            let checkpoint_work = checkpoint_start.elapsed();
            let responses = terminal.take_responses();
            if !responses.is_empty() {
                ensure!(
                    responses.len() <= 16 * 1024 * 1024,
                    "terminal query response budget exceeded"
                );
                query_responses = responses;
            }
            let frame_start = Instant::now();
            let frame = terminal.frame();
            let history_summary = history.summary();
            let mut current = view.lock().expect("view lock");
            current.sequence = receipt.sequence;
            current.frame = frame;
            current.history = Some(history_summary);
            metrics.event(metrics::EventCost {
                output_bytes: if kind == EventKind::Output {
                    payload.len()
                } else {
                    0
                },
                journal: journal_work,
                model: model_work,
                archive: archive_work,
                checkpoint: checkpoint_work,
                frame: frame_start.elapsed(),
                checkpointed,
            });
            current.metrics = metrics.clone();
            if kind == EventKind::Exit {
                current.state = "exited".into();
                return Ok(false);
            }
            Ok(true)
        })();
        match result {
            Ok(true) => (),
            Ok(false) => break,
            Err(error) => {
                let mut current = view.lock().expect("view lock");
                current.storage_error = Some(format!("{error:#}"));
                current.state = "degraded".into();
                // Preserve the durable prefix and stop reading. No further publication/commands.
                drop(current);
                loop {
                    std::hint::black_box((&pending_action, &rx, &master));
                    wait_degraded(&config.control, &mut *child);
                }
            }
        }
    }
}

fn wait_degraded(control: &mpsc::Receiver<Control>, child: &mut dyn portable_pty::Child) {
    if let Ok(Control::Stop) = control.recv_timeout(Duration::from_millis(100)) {
        let _ = child.kill();
    }
}

pub fn serve(root: PathBuf) -> Result<()> {
    private_directory(&root)?;
    serve_with_runtime(root.clone(), root)
}

/// Separate short socket paths from durable state paths. Both namespaces are
/// exclusively locked; selecting a runtime directory cannot unlink a different
/// live host's endpoint. Existing callers keep state-local sockets by default.
pub fn serve_with_runtime(root: PathBuf, runtime: PathBuf) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if let Ok(metadata) = fs::symlink_metadata(&runtime) {
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::getuid() }
                && metadata.mode() & 0o777 == 0o700,
            "runtime must be a dedicated private 0700 directory; original permissions preserved"
        );
    }
    private_directory(&runtime)?;
    let runtime_lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(runtime.join("endpoint.lock"))?;
    let metadata = runtime_lock.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::getuid() }
            && metadata.nlink() == 1
            && metadata.mode() & 0o777 == 0o600,
        "unsafe endpoint ownership lock; original preserved"
    );
    // SAFETY: flock borrows a valid descriptor; keep it alive through dispatch.
    ensure!(
        unsafe { libc::flock(runtime_lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another host owns this runtime directory"
    );
    let mut host = Host::open(root.clone())?;
    host.endpoint_root = runtime.clone();
    let jobs = ipc_server::bind(&runtime.join("host.sock"))?;
    let history_reader = history_read::Reader::new()?;
    // Reclamation is optional derived-data maintenance. Never make the first
    // recovered frame wait for old cache removal. Per-cache leases also protect
    // independent verifiers that do not participate in this host's state lock.
    let cleanup_root = root.clone();
    if let Err(error) = thread::Builder::new()
        .name("pigade-replay-cleanup".into())
        .spawn(move || {
            let report = replay_cache::cleanup(&cleanup_root);
            for warning in &report.warnings {
                eprintln!("Private replay-cache cleanup: {warning}");
            }
        })
    {
        eprintln!("Private replay-cache cleanup unavailable: {error}");
    }
    for job in jobs {
        if let Command::HistoryRead { query } = job.request.command {
            match host.history_source(&query) {
                Ok(source) => history_reader.submit(source, query, job.reply),
                Err(error) => {
                    let _ = job.reply.try_send(history_read::response(Err(error), None));
                }
            }
            continue;
        }
        let response = match host.dispatch(job.request.command) {
            Ok(response) => response,
            Err(error) => Response {
                protocol: PROTOCOL,
                views: Vec::new(),
                error: Some(format!("{error:#}")),
                rejected_before_admission: rejected_before_admission(&error),
                catalog: None,
                catalog_notice: host.catalog_notice.clone(),
                selected: None,
                history_chunk: None,
            },
        };
        let _ = job.reply.try_send(response);
    }
    Ok(())
}

/// No CLI can exec until the host durably registers this helper's OS identity.
pub fn helper(socket: &Path, nonce: &str, executable: &Path, args: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let mut stream = UnixStream::connect(socket)?;
    peer_pid(&stream)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    send(&mut stream, &nonce)?;
    let permission: String = receive(&mut stream)?;
    ensure!(permission == nonce, "exec denied");
    drop(stream);
    let error = std::process::Command::new(executable).args(args).exec();
    Err(error.into())
}

pub fn replay_journal(path: &Path) -> Result<View> {
    restore(path, false)
}

/// Fast, generation-bound recovery; full replay remains an independent oracle.
pub fn recover_journal(path: &Path) -> Result<View> {
    restore(path, true)
}

#[derive(Debug, Serialize)]
pub struct RecoveryVerification {
    pub profile: &'static str,
    pub sequence: u64,
    pub recovery_path: String,
    pub full_replay_ms: f64,
    pub checkpoint_tail_ms: f64,
    pub semantic_checkpoint_bytes: usize,
    pub complete_state_equal: bool,
    pub frame_equal: bool,
    pub cold_history_records: u64,
    pub cold_history_equal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_state_equal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_frame_equal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_cold_history_equal: Option<bool>,
    pub archive_comparison_ms: f64,
    pub full_journal_bytes_read: u64,
    pub checkpoint_tail_journal_bytes_read: u64,
    pub cold_archive_format: &'static str,
}

/// Source-preserving oracle for an immutable, finished journal. Recovery creates
/// temporary derived archives; it never rewrites source journals/checkpoints.
/// Compare complete model/parser bytes and every cold record, not screen digests.
pub fn verify_recovery(path: &Path) -> Result<RecoveryVerification> {
    verify_recovery_inner(path, None)
}

/// Compare full replay and checkpoint-tail recovery with the still-live model
/// and durable history source that produced the journal.
pub fn verify_recovery_against_live(
    path: &Path,
    live_terminal: &Terminal,
    live_history: &history::History,
) -> Result<RecoveryVerification> {
    verify_recovery_inner(path, Some((live_terminal, live_history)))
}

fn verify_recovery_inner(
    path: &Path,
    live: Option<(&Terminal, &history::History)>,
) -> Result<RecoveryVerification> {
    let start = Instant::now();
    let (full_view, full, full_history) = restore_model(path, false)?;
    let full_replay_ms = start.elapsed().as_secs_f64() * 1000.;
    let start = Instant::now();
    let (fast_view, fast, fast_history) = restore_model(path, true)?;
    let checkpoint_tail_ms = start.elapsed().as_secs_f64() * 1000.;
    ensure!(
        full_view.storage_error.is_none() && fast_view.storage_error.is_none(),
        "recovery has storage warnings; verification cannot pass"
    );
    ensure!(
        fast_view.recovery_path == "checkpoint-tail",
        "no valid checkpoint: fast recovery not exercised"
    );
    ensure!(
        full_view.sequence == fast_view.sequence,
        "recovery watermark differs"
    );
    let expected = full.checkpoint()?;
    let recovered = fast.checkpoint()?;
    ensure!(
        expected == recovered,
        "complete terminal/parser state differs"
    );
    ensure!(full_view.frame == fast_view.frame, "cell frame differs");
    let full_history = full_history.context("full replay has no cold history")?;
    let fast_history = fast_history.context("checkpoint has no cold history root")?;
    ensure!(
        full_history.state() == fast_history.state(),
        "cold-history continuation state differs"
    );
    let (live_state_equal, live_frame_equal) = if let Some((live_terminal, live_history)) = live {
        let live_state_equal = live_terminal.checkpoint()? == expected;
        ensure!(
            live_state_equal,
            "live terminal/parser state differs from replay"
        );
        let live_frame_equal = live_terminal.frame() == full_view.frame;
        ensure!(live_frame_equal, "live cell frame differs from replay");
        let live_checkpoint = live_history.checkpoint()?;
        ensure!(
            live_checkpoint.root.directory == format!("{}.history", full_view.generation),
            "live cold history belongs to a different generation"
        );
        ensure!(
            live_history.state() == full_history.state(),
            "live cold-history continuation state differs from replay"
        );
        (Some(true), Some(true))
    } else {
        (None, None)
    };
    // CRCs are not an equality proof: compare every canonical encoded record.
    let comparison_start = Instant::now();
    for ordinal in 0..full_history.state().next_entry {
        let canonical = full_history.record(ordinal)?;
        ensure!(
            canonical == fast_history.record(ordinal)?,
            "cold-history record {ordinal} differs"
        );
        if let Some((_, live_history)) = live {
            ensure!(
                canonical == live_history.record(ordinal)?,
                "live cold-history record {ordinal} differs"
            );
        }
    }
    Ok(RecoveryVerification {
        profile: CORE_PROFILE,
        sequence: full_view.sequence,
        recovery_path: fast_view.recovery_path,
        full_replay_ms,
        checkpoint_tail_ms,
        semantic_checkpoint_bytes: expected.len(),
        complete_state_equal: true,
        frame_equal: true,
        cold_history_records: full_history.state().next_entry,
        cold_history_equal: true,
        live_state_equal,
        live_frame_equal,
        live_cold_history_equal: live.map(|_| true),
        archive_comparison_ms: comparison_start.elapsed().as_secs_f64() * 1000.,
        full_journal_bytes_read: full_view.recovery_journal_bytes_read,
        checkpoint_tail_journal_bytes_read: fast_view.recovery_journal_bytes_read,
        cold_archive_format: history::ARCHIVE_FORMAT,
    })
}
