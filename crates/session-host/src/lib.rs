//! Linux prototype: authenticated local IPC and durable, per-generation terminal actors.
mod adapters;
mod catalog;
pub mod changes;
#[doc(hidden)]
pub mod checkpoint;
pub mod generations;
pub mod history;
pub mod history_read;
pub mod history_view;
pub mod input;
mod opencode_resume;
mod panel_directory;
mod panel_launch;
mod prepared;
mod projects;
mod resume_probe;
mod template_run;
mod workspace_git;
pub use catalog::LayoutAction;
pub use panel_directory::{PanelDirectory, PanelEntry};
mod ipc_server;
pub mod journal_policy;
mod launch_gate;
pub mod metrics;
pub mod poll;
pub mod recovery_diagnostics;
#[cfg(test)]
mod recovery_tests;
mod replay_cache;
mod request_decode;
pub use recovery_diagnostics::{RecoveryDiagnostics, RecoveryIssue};
use std::collections::VecDeque;
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

pub use adapters::Capabilities as AdapterCapabilities;
pub use prepared::{LaunchPreview, PrepareRequest, ResumeSource};
pub use template_run::{
    TemplateRunRequest, TemplateRunSlotView, TemplateRunSource, TemplateRunView,
};
pub use workspace_git::{
    GitDiffSide, GitIndexAction, GitIndexPhase, GitIndexTicket, WorkspaceGitDiff,
    WorkspaceGitEntry, WorkspaceGitIndexResult, WorkspaceGitSnapshot,
};
pub const PROTOCOL: u32 = 2;
const IPC_LIMIT: usize = 8 * 1024 * 1024;
const OUTPUT_CHUNK: usize = 16 * 1024;
// Bound each already-queued output group independently of the PTY read size.
// Ordered control events remain boundaries. A short collection deadline lets
// the PTY reader finish a burst split across several kernel reads.
const OUTPUT_BATCH: usize = 64 * 1024;
const OUTPUT_BATCH_WINDOW: Duration = Duration::from_micros(250);
const MAX_PENDING_QUERY_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const QUERY_RESPONSE_RETRY: Duration = Duration::from_millis(2);
// A bounded burst can match response production without starving PTY output.
const QUERY_RESPONSE_BURST: usize = 8;

fn enqueue_query_responses(
    queue: &mut VecDeque<Vec<u8>>,
    queued_bytes: &mut usize,
    responses: &[u8],
    max_pending_bytes: usize,
) -> Result<()> {
    let new_total = queued_bytes
        .checked_add(responses.len())
        .context("terminal query response byte count overflow")?;
    ensure!(
        new_total <= max_pending_bytes,
        "pending terminal query response budget exceeded"
    );
    if responses.is_empty() {
        return Ok(());
    }

    let mut remaining = responses;
    if let Some(tail) = queue.back_mut() {
        let available = input::MAX_INPUT_BYTES - tail.len();
        let copied = available.min(remaining.len());
        tail.extend_from_slice(&remaining[..copied]);
        remaining = &remaining[copied..];
    }
    for chunk in remaining.chunks(input::MAX_INPUT_BYTES) {
        queue.push_back(chunk.to_vec());
    }
    *queued_bytes = new_total;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launch {
    pub session: Uuid,
    pub generation: Uuid,
    pub operation: Uuid,
    pub executable: PathBuf,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub geometry: Geometry,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    List,
    DetectProviders,
    HistoryRead {
        query: history_read::Query,
    },
    HistoryView {
        query: history_view::Query,
    },
    Poll {
        known: Vec<poll::Known>,
    },
    PollVisible {
        sessions: Vec<Uuid>,
        known: Vec<poll::Known>,
    },
    WaitForChange {
        after: changes::Token,
        timeout_ms: u16,
    },
    Catalog,
    Generations {
        session: Uuid,
        before: Option<u64>,
    },
    SelectGeneration {
        session: Uuid,
        generation: Option<Uuid>,
    },
    PollPanels {
        panels: Vec<poll::PanelTarget>,
        known: Vec<poll::Known>,
    },
    GetContext {
        scope_id: Uuid,
    },
    SaveContext {
        document: pigade_storage::ContextDocument,
        expected_revision: u64,
    },
    PrepareLaunch {
        request: prepared::PrepareRequest,
    },
    PrepareProfileLaunch {
        tab: Uuid,
        profile: Uuid,
        expected_revision: u64,
        side: pigade_layout::Direction,
        target: Option<Uuid>,
    },
    StartPrepared {
        plan_id: Uuid,
    },
    PrepareResume {
        session: Uuid,
    },
    PrepareTemplateRun {
        request: TemplateRunRequest,
    },
    GetTemplateRun {
        run: Uuid,
    },
    AdvanceTemplateRun {
        run: Uuid,
        expected_revision: u64,
    },
    CancelTemplateRun {
        run: Uuid,
        expected_revision: u64,
    },
    ListTemplateRuns {
        before: Option<u64>,
    },
    RenamePane {
        session: Uuid,
        name: String,
        expected_revision: Option<u64>,
    },
    SearchPanels {
        query: String,
        after: Option<Uuid>,
    },
    ActivatePane {
        tab: Uuid,
        session: Uuid,
        expected_layout_revision: u64,
    },
    GetSessionContext {
        session: Uuid,
        generation: Uuid,
    },
    BindConversation {
        session: Uuid,
        generation: Uuid,
        conversation_id: String,
        expected_revision: u64,
    },
    LaunchInTab {
        tab: Uuid,
        provider: String,
        session: Uuid,
        generation: Uuid,
        operation: Uuid,
        side: pigade_layout::Direction,
        target: Option<Uuid>,
    },
    AddProject {
        path: PathBuf,
    },
    CreateProject {
        parent: PathBuf,
        name: String,
    },
    CreateWorkspace {
        project: Uuid,
        task: String,
        worktree: bool,
    },
    WorkspaceGitSnapshot {
        workspace: Uuid,
        expected_revision: u64,
    },
    WorkspaceGitDiff {
        workspace: Uuid,
        expected_revision: u64,
        path: String,
        side: GitDiffSide,
    },
    WorkspaceGitIndex {
        workspace: Uuid,
        expected_revision: u64,
        ticket: Uuid,
        inspect: bool,
    },
    WorkspaceGitIndexAcknowledge {
        workspace: Uuid,
        expected_revision: u64,
        ticket: Uuid,
    },
    CatalogApply {
        batch: Box<pigade_storage::CatalogBatch>,
    },
    SetCanvasOffset {
        tab: Uuid,
        offset: pigade_storage::CanvasOffset,
    },
    CloseTab {
        tab: Uuid,
        expected_revision: u64,
        expected_layout_revision: u64,
        expected_selection_revision: u64,
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
        #[serde(deserialize_with = "input::deserialize_bytes")]
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
    /// Live pipeline sample; absent for recovered generations and older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_diagnostic: Option<metrics::ActorDiagnostic>,
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
    pub selection: Option<pigade_storage::Selection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<pigade_storage::ContextDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_preview: Option<prepared::LaunchPreview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_notice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<Vec<poll::SessionInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_chunk: Option<history::RecordChunk>,
    /// Current host publication epoch; absent for older peers and non-poll replies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_token: Option<changes::Token>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generations: Option<generations::GenerationList>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_selection: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_view: Option<history_view::Reply>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<ProviderInfo>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_run: Option<TemplateRunView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_runs: Option<pigade_storage::TemplateRunPage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_directory: Option<PanelDirectory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_git_snapshot: Option<WorkspaceGitSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_git_diff: Option<WorkspaceGitDiff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_git_index: Option<WorkspaceGitIndexResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderInfo {
    pub provider: String,
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    pub available: bool,
    pub reason: Option<String>,
}

/// Stable machine-readable proof that no part of a command was admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandRejection {
    QueueFull,
    LaunchPreflight,
}

impl std::fmt::Display for CommandRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull => f.write_str("command queue full; command was not admitted"),
            Self::LaunchPreflight => f.write_str("launch rejected before process creation"),
        }
    }
}
impl std::error::Error for CommandRejection {}

fn rejected_before_admission(error: &anyhow::Error) -> Option<CommandRejection> {
    error
        .downcast_ref::<CommandRejection>()
        .copied()
        .or_else(|| {
            panel_launch::rejected_before_spawn(error).then_some(CommandRejection::LaunchPreflight)
        })
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

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut buffer = MessageBuffer(Vec::new());
    serde_json::to_writer(&mut buffer, value)?;
    Ok(buffer.0)
}

fn send_encoded(stream: &mut UnixStream, bytes: &[u8]) -> Result<()> {
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)?;
    Ok(())
}

pub fn send<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    send_encoded(stream, &encode(value)?)
}
pub fn receive<T: DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    let bytes = receive_bytes(stream)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn receive_bytes(stream: &mut UnixStream) -> Result<Vec<u8>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    ensure!(length > 0 && length <= IPC_LIMIT, "invalid IPC length");
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Decode a bounded IPC request after checking Input arrays before Serde buffers enum content.
pub(crate) fn receive_request(stream: &mut UnixStream) -> Result<Request> {
    let bytes = receive_bytes(stream)?;
    request_decode::validate(&bytes)?;
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
    actor_probe: Option<Arc<metrics::ActorProbe>>,
    commands: mpsc::SyncSender<Action>,
    input: Option<input::InputWriter>,
    control: mpsc::SyncSender<Control>,
    _history: Option<Arc<history::History>>,
}

fn snapshot_view(session: &Session) -> View {
    // Release the published-view lock before sampling the actor's independent
    // probe. A blocked actor stage never holds either lock while doing work.
    let mut view = session.view.lock().expect("view lock").clone();
    view.actor_diagnostic = session
        .actor_probe
        .as_ref()
        .map(|probe| probe.try_snapshot());
    view
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
    probe: Arc<metrics::ActorProbe>,
    changes: changes::ChangeHub,
}
struct Host {
    root: PathBuf,
    endpoint_root: PathBuf,
    sessions: Vec<Session>,
    generation_heads: std::collections::BTreeMap<Uuid, Uuid>,
    metadata: MetadataStore,
    catalog_notice: Option<String>,
    recovery: RecoveryDiagnostics,
    resume_verified: Option<resume_probe::Permit>,
    changes: changes::ChangeHub,
    _lock: File,
}

struct ProviderTask {
    reply: mpsc::SyncSender<Response>,
}

struct ProviderDetector {
    sender: mpsc::SyncSender<ProviderTask>,
}

impl ProviderDetector {
    fn new() -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<ProviderTask>(4);
        let receiver = Arc::new(Mutex::new(receiver));
        let worker_receiver = receiver.clone();
        thread::Builder::new()
            .name("pigade-provider-detect".into())
            .spawn(move || {
                loop {
                    let task = worker_receiver
                        .lock()
                        .expect("provider detector queue")
                        .recv();
                    let Ok(task) = task else { break };
                    let providers = detect_providers();
                    let _ = task
                        .reply
                        .try_send(provider_response(providers, None, None));
                }
            })?;
        Ok(Self { sender })
    }

    fn submit(&self, reply: mpsc::SyncSender<Response>) {
        match self.sender.try_send(ProviderTask { reply }) {
            Ok(()) => (),
            Err(mpsc::TrySendError::Full(task)) => {
                let _ = task.reply.try_send(provider_response(
                    Vec::new(),
                    Some("provider detection queue is full".into()),
                    Some(CommandRejection::QueueFull),
                ));
            }
            Err(mpsc::TrySendError::Disconnected(task)) => {
                let _ = task.reply.try_send(provider_response(
                    Vec::new(),
                    Some("provider detection worker unavailable".into()),
                    None,
                ));
            }
        }
    }
}

fn detect_providers() -> Vec<ProviderInfo> {
    let providers = ["shell", "claude", "codex", "opencode"];
    let (sender, receiver) = mpsc::channel();
    for provider in providers {
        let sender = sender.clone();
        let provider = provider.to_owned();
        let _ = thread::Builder::new()
            .name(format!("pigade-detect-{provider}"))
            .spawn(move || {
                let info = match panel_launch::resolve_provider(&provider) {
                    Ok(executable) => {
                        let inspected = adapters::inspect(&provider, &executable);
                        let (version, reason) = match inspected {
                            Ok(info) => (
                                (info.cli_version != "unknown" && provider != "shell")
                                    .then_some(info.cli_version),
                                info.unsupported_reason,
                            ),
                            Err(error) => {
                                (None, Some(format!("version inspection failed: {error:#}")))
                            }
                        };
                        ProviderInfo {
                            provider,
                            executable: Some(executable),
                            version,
                            available: true,
                            reason,
                        }
                    }
                    Err(error) => ProviderInfo {
                        provider,
                        executable: None,
                        version: None,
                        available: false,
                        reason: Some(format!("{error:#}")),
                    },
                };
                let _ = sender.send(info);
            });
    }
    drop(sender);
    let mut results = Vec::new();
    for _ in providers {
        match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(info) => results.push(info),
            Err(_) => break,
        }
    }
    results.sort_by_key(|info| {
        ["shell", "claude", "codex", "opencode"]
            .iter()
            .position(|provider| *provider == info.provider)
            .unwrap_or(usize::MAX)
    });
    results
}

fn provider_response(
    providers: Vec<ProviderInfo>,
    error: Option<String>,
    rejection: Option<CommandRejection>,
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
        history_view: None,
        providers: Some(providers),
        template_run: None,
        template_runs: None,
        panel_directory: None,
        workspace_git_snapshot: None,
        workspace_git_diff: None,
        workspace_git_index: None,
    }
}

fn restore(path: &Path, fast: bool) -> Result<View> {
    Ok(restore_model(path, fast)?.0)
}

fn validate_replay_payload(
    policy: journal_policy::Policy,
    kind: EventKind,
    payload: &[u8],
) -> std::result::Result<Option<Geometry>, &'static str> {
    match kind {
        EventKind::OutputRejectedResource => Err(match policy {
            journal_policy::Policy::Legacy => {
                "resource refusal event is not permitted by legacy journal profile"
            }
            journal_policy::Policy::ResourceRefusalV1 => {
                "terminal resource budget refused this output; raw bytes retained"
            }
        }),
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
    restore_model_through(path, fast, None)
}

/// Freeze only the durable prefix that was published for this exact generation.
/// A history request never reparses an arbitrarily large raw journal on behalf
/// of the UI. Existing recovery has a separate, unrestricted replay contract.
pub(crate) fn restore_history_view(
    path: &Path,
    session: Uuid,
    generation: Uuid,
    sequence: u64,
) -> Result<(Terminal, Arc<history::History>, Vec<String>)> {
    let reader = JournalReader::open(path)?;
    ensure!(
        reader.header().session_id == session && reader.header().generation == generation,
        "history snapshot source identity mismatch"
    );
    drop(reader);
    let (view, terminal, history) = restore_model_through(path, true, Some(sequence))?;
    ensure!(
        view.session == session && view.generation == generation && view.sequence == sequence,
        "history snapshot did not reach its published source watermark"
    );
    let warnings = view.storage_error.into_iter().collect();
    Ok((
        terminal,
        history.context("complete normal history unavailable")?,
        warnings,
    ))
}

fn restore_model_through(
    path: &Path,
    fast: bool,
    target: Option<u64>,
) -> Result<(View, Terminal, Option<Arc<history::History>>)> {
    const HISTORY_REPLAY_BYTES: u64 = 16 * 1024 * 1024;
    let mut reader = JournalReader::open(path)?;
    let header = reader.header().clone();
    let policy = journal_policy::Policy::from_profile(&header.profile)
        .context("incompatible terminal profile or journal policy; source preserved")?;
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
        checkpoint::latest_through(
            path.parent().context("journal parent")?,
            &header,
            fs::metadata(path)?.len(),
            target.unwrap_or(u64::MAX),
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
        && (target.is_some()
            || pigade_storage::scan_valid_prefix(path)?
                .corruption
                .is_none())
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
    let replay_start_bytes = reader.bytes_read();
    loop {
        if target.is_some_and(|target| sequence == target) {
            break;
        }
        ensure!(
            target.is_none()
                || reader.bytes_read().saturating_sub(replay_start_bytes) < HISTORY_REPLAY_BYTES,
            "history snapshot exceeds bounded replay tail; a newer complete checkpoint is required"
        );
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
        ensure!(
            target.is_none()
                || reader.bytes_read().saturating_sub(replay_start_bytes) <= HISTORY_REPLAY_BYTES,
            "history snapshot exceeds bounded replay tail; a newer complete checkpoint is required"
        );
        // A valid frame CRC does not prove its typed payload is applicable to
        // this terminal profile. Validate before history.begin or any mutation
        // so malformed tails cannot hide an otherwise recoverable generation.
        let resize = match validate_replay_payload(policy, event.kind, &event.payload) {
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
            EventKind::OutputRejectedResource => {
                unreachable!("refusal is rejected before history/model mutation")
            }
        }
        if let Some(history) = &history {
            history.replay_event()?;
        }
        sequence = event.sequence;
    }
    ensure!(
        target.is_none_or(|target| sequence == target),
        "history snapshot source watermark unavailable: requested {}, recovered {sequence}",
        target.unwrap_or_default()
    );
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
        actor_diagnostic: None,
    };
    Ok((view, terminal, history))
}
impl Host {
    fn history_view_source(&self, query: &history_view::Query) -> Result<Option<(PathBuf, u64)>> {
        let history_view::Query::Open {
            session,
            generation,
        } = query
        else {
            return Ok(None);
        };
        for candidate in &self.sessions {
            let view = candidate.view.lock().expect("view lock");
            if view.session == *session && view.generation == *generation {
                return Ok(Some((
                    self.root.join(format!("{generation}.journal")),
                    view.sequence,
                )));
            }
        }
        bail!("unknown session generation")
    }
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
        if matches!(&command, Command::WaitForChange { .. }) {
            bail!("wait_for_change is only available through the IPC notification service");
        }
        if matches!(
            &command,
            Command::Poll { .. } | Command::PollVisible { .. } | Command::PollPanels { .. }
        ) {
            self.reconcile_exits()?;
            let result = match command {
                Command::Poll { known } => poll::poll(
                    self.sessions.iter().map(|s| s.view.as_ref()),
                    &known,
                    &self.changes,
                )?,
                Command::PollVisible { sessions, known } => poll::poll_visible(
                    self.sessions.iter().map(|s| s.view.as_ref()),
                    &known,
                    &self.changes,
                    &sessions,
                )?,
                Command::PollPanels { panels, known } => poll::poll_panels(
                    self.sessions.iter().map(|s| s.view.as_ref()),
                    &known,
                    &self.changes,
                    &panels,
                    &self.generation_heads,
                )?,
                _ => unreachable!(),
            };
            return Ok(Response {
                protocol: PROTOCOL,
                views: result.views,
                error: None,
                rejected_before_admission: None,
                catalog: None,
                selection: None,
                context: None,
                launch_preview: None,
                launch_phase: None,
                catalog_notice: self.catalog_notice.clone(),
                recovery: self.recovery.response(),
                selected: Some(result.selected),
                history_chunk: None,
                change_token: result.change_token,
                generations: None,
                generation_selection: None,
                history_view: None,
                providers: None,
                template_run: None,
                template_runs: None,
                panel_directory: None,
                workspace_git_snapshot: None,
                workspace_git_diff: None,
                workspace_git_index: None,
            });
        }
        if matches!(
            &command,
            Command::GetContext { .. } | Command::SaveContext { .. }
        ) {
            let scope = match &command {
                Command::GetContext { scope_id } => *scope_id,
                Command::SaveContext { document, .. } => document.scope_id,
                _ => unreachable!(),
            };
            let catalog = self.metadata.catalog()?;
            ensure!(
                catalog.workspaces.iter().any(|w| w.id == scope)
                    || catalog.projects.iter().any(|p| p.id == scope),
                "unknown context owner"
            );
            let context = match command {
                Command::GetContext { scope_id } => self.metadata.context(scope_id)?,
                Command::SaveContext {
                    document,
                    expected_revision,
                } => self.metadata.save_context(document, expected_revision)?,
                _ => unreachable!(),
            };
            return Ok(Response {
                protocol: PROTOCOL,
                views: Vec::new(),
                error: None,
                rejected_before_admission: None,
                catalog: None,
                selection: None,
                context: Some(context),
                launch_preview: None,
                launch_phase: None,
                catalog_notice: None,
                recovery: None,
                selected: None,
                history_chunk: None,
                change_token: None,
                generations: None,
                generation_selection: None,
                history_view: None,
                providers: None,
                template_run: None,
                template_runs: None,
                panel_directory: None,
                workspace_git_snapshot: None,
                workspace_git_diff: None,
                workspace_git_index: None,
            });
        }
        if let Command::SetCanvasOffset { tab, offset } = &command {
            let selection = catalog::save_canvas_offset(&mut self.metadata, *tab, *offset)?;
            return Ok(Response {
                protocol: PROTOCOL,
                views: Vec::new(),
                error: None,
                rejected_before_admission: None,
                catalog: None,
                selection: Some(selection),
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
                history_view: None,
                providers: None,
                template_run: None,
                template_runs: None,
                panel_directory: None,
                workspace_git_snapshot: None,
                workspace_git_diff: None,
                workspace_git_index: None,
            });
        }
        match &command {
            Command::RenamePane {
                session,
                name,
                expected_revision,
            } => {
                let saved = catalog::rename_pane(
                    &mut self.metadata,
                    *session,
                    name.clone(),
                    *expected_revision,
                )?;
                return Ok(Response {
                    protocol: PROTOCOL,
                    views: Vec::new(),
                    error: None,
                    rejected_before_admission: None,
                    catalog: Some(saved),
                    selection: None,
                    context: None,
                    launch_preview: None,
                    launch_phase: None,
                    catalog_notice: None,
                    recovery: self.recovery.response(),
                    selected: None,
                    history_chunk: None,
                    change_token: None,
                    generations: None,
                    generation_selection: None,
                    history_view: None,
                    providers: None,
                    template_run: None,
                    template_runs: None,
                    panel_directory: None,
                    workspace_git_snapshot: None,
                    workspace_git_diff: None,
                    workspace_git_index: None,
                });
            }
            Command::SearchPanels { query, after } => {
                self.reconcile_exits()?;
                let directory = panel_directory::search(self, query.clone(), *after)?;
                return Ok(Response {
                    protocol: PROTOCOL,
                    views: Vec::new(),
                    error: None,
                    rejected_before_admission: None,
                    catalog: None,
                    selection: None,
                    context: None,
                    launch_preview: None,
                    launch_phase: None,
                    catalog_notice: None,
                    recovery: self.recovery.response(),
                    selected: None,
                    history_chunk: None,
                    change_token: None,
                    generations: None,
                    generation_selection: None,
                    history_view: None,
                    providers: None,
                    template_run: None,
                    template_runs: None,
                    panel_directory: Some(directory),
                    workspace_git_snapshot: None,
                    workspace_git_diff: None,
                    workspace_git_index: None,
                });
            }
            Command::ActivatePane {
                tab,
                session,
                expected_layout_revision,
            } => {
                let saved = catalog::activate_pane(
                    &mut self.metadata,
                    *tab,
                    *session,
                    *expected_layout_revision,
                )?;
                return Ok(Response {
                    protocol: PROTOCOL,
                    views: Vec::new(),
                    error: None,
                    rejected_before_admission: None,
                    catalog: Some(saved),
                    selection: None,
                    context: None,
                    launch_preview: None,
                    launch_phase: None,
                    catalog_notice: None,
                    recovery: self.recovery.response(),
                    selected: None,
                    history_chunk: None,
                    change_token: None,
                    generations: None,
                    generation_selection: None,
                    history_view: None,
                    providers: None,
                    template_run: None,
                    template_runs: None,
                    panel_directory: None,
                    workspace_git_snapshot: None,
                    workspace_git_diff: None,
                    workspace_git_index: None,
                });
            }
            Command::Generations { session, before } => {
                let generations = generations::list(self, *session, *before)?;
                return Ok(Response {
                    protocol: PROTOCOL,
                    views: Vec::new(),
                    error: None,
                    rejected_before_admission: None,
                    catalog: None,
                    selection: None,
                    context: None,
                    launch_preview: None,
                    launch_phase: None,
                    catalog_notice: None,
                    recovery: self.recovery.response(),
                    selected: None,
                    history_chunk: None,
                    change_token: None,
                    generations: Some(generations),
                    generation_selection: Some(true),
                    history_view: None,
                    providers: None,
                    template_run: None,
                    template_runs: None,
                    panel_directory: None,
                    workspace_git_snapshot: None,
                    workspace_git_diff: None,
                    workspace_git_index: None,
                });
            }
            Command::SelectGeneration {
                session,
                generation,
            } => {
                let selection = generations::select(self, *session, *generation)?;
                notify_actor_change(&self.changes, "generation selection");
                return Ok(Response {
                    protocol: PROTOCOL,
                    views: Vec::new(),
                    error: None,
                    rejected_before_admission: None,
                    catalog: None,
                    selection: Some(selection),
                    context: None,
                    launch_preview: None,
                    launch_phase: None,
                    catalog_notice: None,
                    recovery: self.recovery.response(),
                    selected: None,
                    history_chunk: None,
                    change_token: None,
                    generations: None,
                    generation_selection: Some(true),
                    history_view: None,
                    providers: None,
                    template_run: None,
                    template_runs: None,
                    panel_directory: None,
                    workspace_git_snapshot: None,
                    workspace_git_diff: None,
                    workspace_git_index: None,
                });
            }
            Command::PrepareLaunch { request } => {
                let preview = prepared::prepare(self, request.clone())?;
                return Ok(self.launch_response(None, preview, None));
            }
            Command::PrepareProfileLaunch {
                tab,
                profile,
                expected_revision,
                side,
                target,
            } => {
                let preview = prepared::prepare_profile(
                    self,
                    *tab,
                    *profile,
                    *expected_revision,
                    *side,
                    *target,
                )?;
                return Ok(self.launch_response(None, preview, None));
            }
            Command::PrepareTemplateRun { request } => {
                let run = template_run::prepare(self, request.clone())?;
                return Ok(self.template_run_response(Some(run), None, None));
            }
            Command::GetTemplateRun { run } => {
                let view = template_run::get(self, *run)?;
                return Ok(self.template_run_response(Some(view), None, None));
            }
            Command::AdvanceTemplateRun {
                run,
                expected_revision,
            } => {
                let view = template_run::advance(self, *run, *expected_revision)?;
                let catalog = catalog::read_metadata(&self.metadata)?;
                return Ok(self.template_run_response(Some(view), None, Some(catalog)));
            }
            Command::CancelTemplateRun {
                run,
                expected_revision,
            } => {
                let view = template_run::cancel(self, *run, *expected_revision)?;
                return Ok(self.template_run_response(Some(view), None, None));
            }
            Command::ListTemplateRuns { before } => {
                let runs = template_run::list(self, *before)?;
                return Ok(self.template_run_response(None, Some(runs), None));
            }
            Command::PrepareResume { session } => {
                let preview = prepared::prepare_resume(self, *session)?;
                return Ok(self.launch_response(None, preview, None));
            }
            Command::StartPrepared { plan_id } => {
                let (catalog, preview) = prepared::start(self, *plan_id)?;
                let phase = self
                    .metadata
                    .prepared_launch(*plan_id)?
                    .map(|(_, phase)| phase);
                return Ok(self.launch_response(Some(catalog), preview, phase));
            }
            Command::GetSessionContext {
                session,
                generation,
            } => {
                let (preview, phase) = prepared::session_context(self, *session, *generation)?;
                return Ok(self.launch_response(None, preview, Some(phase)));
            }
            Command::BindConversation {
                session,
                generation,
                conversation_id,
                expected_revision,
            } => {
                let (preview, phase) = prepared::bind_conversation(
                    self,
                    *session,
                    *generation,
                    conversation_id,
                    *expected_revision,
                )?;
                return Ok(self.launch_response(None, preview, Some(phase)));
            }
            _ => (),
        }
        if matches!(&command, Command::DetectProviders) {
            bail!("provider detection is only available through the provider worker");
        }
        let catalog = match command {
            Command::DetectProviders => unreachable!("provider request routed to worker"),
            Command::WorkspaceGitSnapshot { .. }
            | Command::WorkspaceGitDiff { .. }
            | Command::WorkspaceGitIndex { .. }
            | Command::WorkspaceGitIndexAcknowledge { .. } => {
                unreachable!("workspace Git status request routed to worker")
            }
            Command::Catalog => catalog::read_metadata(&self.metadata)?,
            Command::LaunchInTab {
                tab,
                provider,
                session,
                generation,
                operation,
                side,
                target,
            } => panel_launch::launch_in_tab(
                self, tab, provider, session, generation, operation, side, target,
            )?,
            Command::AddProject { path } => projects::add_project(&mut self.metadata, path)?,
            Command::CreateProject { parent, name } => {
                projects::create_project(&mut self.metadata, parent, name)?
            }
            Command::CreateWorkspace {
                project,
                task,
                worktree,
            } => {
                projects::create_workspace(&mut self.metadata, &self.root, project, task, worktree)?
            }
            Command::CatalogApply { batch } => catalog::apply_metadata(&mut self.metadata, *batch)?,
            Command::CloseTab {
                tab,
                expected_revision,
                expected_layout_revision,
                expected_selection_revision,
            } => catalog::close_tab(
                &mut self.metadata,
                tab,
                expected_revision,
                expected_layout_revision,
                expected_selection_revision,
            )?,
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
                    selection: None,
                    context: None,
                    launch_preview: None,
                    launch_phase: None,
                    catalog_notice: None,
                    recovery: self.recovery.response(),
                    selected: None,
                    history_chunk: None,
                    change_token: None,
                    generations: None,
                    generation_selection: None,
                    history_view: None,
                    providers: None,
                    template_run: None,
                    template_runs: None,
                    panel_directory: None,
                    workspace_git_snapshot: None,
                    workspace_git_diff: None,
                    workspace_git_index: None,
                });
            }
        };
        Ok(Response {
            protocol: PROTOCOL,
            views: Vec::new(),
            error: None,
            rejected_before_admission: None,
            catalog: Some(catalog),
            selection: None,
            context: None,
            launch_preview: None,
            launch_phase: None,
            catalog_notice: self.catalog_notice.clone(),
            recovery: self.recovery.response(),
            selected: None,
            history_chunk: None,
            change_token: None,
            generations: None,
            generation_selection: Some(true),
            history_view: None,
            providers: None,
            template_run: None,
            template_runs: None,
            panel_directory: None,
            workspace_git_snapshot: None,
            workspace_git_diff: None,
            workspace_git_index: None,
        })
    }
    fn launch_response(
        &self,
        catalog: Option<pigade_storage::Catalog>,
        preview: prepared::LaunchPreview,
        phase: Option<String>,
    ) -> Response {
        Response {
            protocol: PROTOCOL,
            views: Vec::new(),
            error: None,
            rejected_before_admission: None,
            catalog,
            selection: None,
            context: None,
            launch_preview: Some(preview),
            launch_phase: phase,
            catalog_notice: None,
            recovery: self.recovery.response(),
            selected: None,
            history_chunk: None,
            change_token: None,
            generations: None,
            generation_selection: None,
            history_view: None,
            providers: None,
            template_run: None,
            template_runs: None,
            panel_directory: None,
            workspace_git_snapshot: None,
            workspace_git_diff: None,
            workspace_git_index: None,
        }
    }
    fn template_run_response(
        &self,
        template_run: Option<TemplateRunView>,
        template_runs: Option<pigade_storage::TemplateRunPage>,
        catalog: Option<pigade_storage::Catalog>,
    ) -> Response {
        Response {
            protocol: PROTOCOL,
            views: Vec::new(),
            error: None,
            rejected_before_admission: None,
            catalog,
            selection: None,
            context: None,
            launch_preview: None,
            launch_phase: None,
            catalog_notice: None,
            recovery: self.recovery.response(),
            selected: None,
            history_chunk: None,
            change_token: None,
            generations: None,
            generation_selection: None,
            history_view: None,
            providers: None,
            template_run,
            template_runs,
            panel_directory: None,
            workspace_git_snapshot: None,
            workspace_git_diff: None,
            workspace_git_index: None,
        }
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
        let mut recovery = RecoveryDiagnostics::default();
        for entry in fs::read_dir(&root)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "journal") {
                match restore_model(&path, true) {
                    Ok((view, _terminal, history)) => {
                        let (tx, _rx) = mpsc::sync_channel(1);
                        let (control, _) = mpsc::sync_channel(1);
                        sessions.push(Session {
                            view: Arc::new(Mutex::new(view)),
                            actor_probe: None,
                            commands: tx,
                            input: None,
                            control,
                            _history: history,
                        });
                    }
                    Err(error) => {
                        recovery.record(&path, &error);
                        eprintln!("history unavailable {}: {error:#}", path.display());
                    }
                }
            }
        }
        let mut ordered_heads = std::collections::BTreeMap::new();
        for record in metadata.list_launches()? {
            let head = ordered_heads
                .entry(record.session_id)
                .or_insert((0, record.generation));
            if record.ordinal > head.0 {
                *head = (record.ordinal, record.generation);
            }
        }
        let generation_heads = ordered_heads
            .into_iter()
            .map(|(session, (_, generation))| (session, generation))
            .collect();
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
            generation_heads,
            metadata,
            catalog_notice,
            recovery,
            resume_verified: None,
            changes: changes::ChangeHub::new(),
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
            prepared::guard_direct_launch(self, spec.operation, spec.session, spec.generation)?;
            return self.launch(spec);
        }
        if matches!(command, Command::List) {
            return Ok(self.sessions.iter().map(snapshot_view).collect());
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
            Command::WaitForChange { .. } | Command::Poll { .. } => {
                unreachable!("poll and change waits are handled before terminal command dispatch")
            }
            _ => (),
        }
        Ok(vec![snapshot_view(session)])
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
            spec.args.len() <= 128
                && spec
                    .args
                    .iter()
                    .all(|arg| !arg.contains('\0') && arg.len() <= 96 * 1024)
                && spec.args.iter().map(String::len).sum::<usize>() <= 128 * 1024,
            "argv exceeds bounds"
        );
        ensure!(
            spec.env.len() <= 1
                && spec
                    .env
                    .iter()
                    .all(|(name, value)| name == "OPENCODE_CONFIG_CONTENT"
                        && !value.contains('\0')
                        && value.len() <= 64 * 1024),
            "unsupported or oversized session environment"
        );
        if let Some(existing) = self.metadata.get_launch(spec.operation)? {
            ensure!(
                existing.session_id == spec.session && existing.generation == spec.generation,
                "operation identity mismatch"
            );
            let view = self.sessions.iter().find_map(|s| {
                let matches = s.view.lock().expect("view lock").generation == spec.generation;
                matches.then(|| snapshot_view(s))
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
        self.generation_heads.insert(spec.session, spec.generation);
        notify_actor_change(&self.changes, "generation intent");
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
        for (name, value) in &spec.env {
            builder.env(name, value);
        }
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
            actor_diagnostic: None,
        }));
        let actor_probe = Arc::new(metrics::ActorProbe::default());
        actor_probe.enter(metrics::ActorStage::Waiting, None);
        let (tx, rx) = mpsc::sync_channel(8);
        let mut reader = pair.master.try_clone_reader()?;
        let writer = input::InputWriter::new(pair.master.take_writer()?);
        let (control_tx, control_rx) = mpsc::sync_channel(1);
        let reader_tx = tx.clone();
        let reader_probe = actor_probe.clone();
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
                        reader_probe.reader_read(n);
                        if reader_tx.send(Action::Output(bytes)).is_err() {
                            break;
                        }
                        reader_probe.reader_enqueued(n);
                    }
                }
            }
        });
        let actor_view = view.clone();
        let actor_root = self.root.clone();
        let actor_writer = writer.clone();
        let actor_history = history.clone();
        let actor_probe_for_thread = actor_probe.clone();
        let actor_changes = self.changes.clone();
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
                    probe: actor_probe_for_thread,
                    changes: actor_changes,
                },
            )
        });
        self.sessions.push(Session {
            view: view.clone(),
            actor_probe: Some(actor_probe),
            commands: tx,
            input: Some(writer),
            control: control_tx,
            _history: Some(history),
        });
        notify_actor_change(&self.changes, "session launch");
        Ok(vec![snapshot_view(
            self.sessions.last().expect("new session"),
        )])
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

#[cfg(test)]
mod query_response_queue_tests {
    use super::*;

    #[test]
    fn tiny_replies_coalesce_into_bounded_fifo_chunks() {
        let mut queue = VecDeque::new();
        let mut queued_bytes = 0;
        let mut expected = Vec::new();
        for _ in 0..10_000 {
            let response = b"\x1b[1;1R";
            expected.extend_from_slice(response);
            enqueue_query_responses(
                &mut queue,
                &mut queued_bytes,
                response,
                MAX_PENDING_QUERY_RESPONSE_BYTES,
            )
            .unwrap();
        }

        assert_eq!(queued_bytes, expected.len());
        assert_eq!(queue.iter().map(Vec::len).sum::<usize>(), expected.len());
        assert!(
            queue
                .iter()
                .all(|chunk| chunk.len() <= input::MAX_INPUT_BYTES)
        );
        assert_eq!(queue.len(), expected.len().div_ceil(input::MAX_INPUT_BYTES));
        let actual = queue.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn response_budget_rejection_keeps_pending_fifo_unchanged() {
        let mut queue = VecDeque::from([b"already queued".to_vec()]);
        let mut queued_bytes = 14;
        let before_queue = queue.clone();
        let before_bytes = queued_bytes;
        let over_budget = [0u8; 1];

        assert!(enqueue_query_responses(&mut queue, &mut queued_bytes, &over_budget, 14).is_err());
        assert_eq!(queue, before_queue);
        assert_eq!(queued_bytes, before_bytes);
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
    pending: Option<checkpoint::Pending>,
    retry_busy: bool,
    completed: u64,
}
impl Default for CheckpointProgress {
    fn default() -> Self {
        Self {
            receipt: None,
            bytes: 0,
            events: 0,
            model_work: Duration::ZERO,
            clock: Instant::now(),
            pending: None,
            retry_busy: false,
            completed: 0,
        }
    }
}
impl CheckpointProgress {
    fn observe(&mut self, receipt: pigade_storage::Receipt, payload: usize, work: Duration) {
        self.receipt = Some(receipt);
        self.bytes = self.bytes.saturating_add(33).saturating_add(payload);
        self.events = self.events.saturating_add(1);
        self.model_work = self.model_work.saturating_add(work);
    }
    // Completion remains observable on an otherwise quiet terminal. A busy
    // global slot retries on a bounded timer instead of a zero-timeout spin.
    fn receive_timeout(&self) -> Option<Duration> {
        if self.pending.is_some() || self.retry_busy {
            Some(Duration::from_millis(10))
        } else if self.events > 0 {
            Some(Duration::from_secs(5).saturating_sub(self.clock.elapsed()))
        } else {
            None
        }
    }

    fn complete(&mut self, wait: bool) -> Result<()> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if wait {
            pending.wait()?;
        } else {
            match pending.poll() {
                checkpoint::Completion::Pending => {
                    self.pending = Some(pending);
                    return Ok(());
                }
                checkpoint::Completion::Succeeded => (),
                checkpoint::Completion::Failed(message) => {
                    bail!("checkpoint worker failed: {message}")
                }
            }
        }
        self.completed = self.completed.saturating_add(1);
        Ok(())
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
        self.complete(force)?;
        if !due || self.pending.is_some() {
            return Ok(false);
        }
        let receipt = self.receipt.context("checkpoint lacks durable receipt")?;
        if force {
            // Exit retains deterministic final checkpoint-tail recovery. The
            // synchronous API waits on the same global capture/persist permit.
            checkpoint::publish_with_history(
                root,
                header,
                receipt,
                terminal,
                Some(history.checkpoint()?),
            )?;
            self.completed = self.completed.saturating_add(1);
        } else {
            match checkpoint::schedule(
                root,
                header,
                receipt,
                terminal,
                Some(history.checkpoint()?),
            )? {
                Some(pending) => self.pending = Some(pending),
                None => {
                    self.retry_busy = true;
                    return Ok(false);
                }
            }
        }
        self.bytes = 0;
        self.events = 0;
        self.model_work = Duration::ZERO;
        self.clock = Instant::now();
        self.retry_busy = false;
        Ok(due)
    }
}

#[cfg(test)]
mod checkpoint_progress_tests {
    use super::*;

    #[test]
    fn deferred_due_checkpoint_has_bounded_nonzero_idle_wait() {
        let mut progress = CheckpointProgress {
            clock: Instant::now() - Duration::from_secs(60),
            retry_busy: true,
            ..Default::default()
        };
        let receipt = pigade_storage::Receipt {
            kind: EventKind::Output,
            sequence: 1,
            end_offset: 100,
            checksum: 0,
        };
        progress.observe(receipt, usize::MAX, Duration::MAX);
        progress.observe(receipt, 10, Duration::from_secs(1));
        let timeout = progress
            .receive_timeout()
            .expect("dirty deferred work needs retry");
        assert!(timeout > Duration::ZERO && timeout <= Duration::from_millis(100));
        assert_eq!(progress.bytes, usize::MAX);
        assert_eq!(progress.model_work, Duration::MAX);
        assert!(progress.events > 0, "busy work must stay dirty");
    }

    #[test]
    fn worker_failure_is_observable_with_no_new_terminal_events() -> Result<()> {
        let root = tempfile::tempdir()?;
        let terminal = Terminal::new(Geometry::new(4, 20)?, false);
        let header = JournalHeader {
            session_id: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            profile: CORE_PROFILE.into(),
            initial_rows: 4,
            initial_cols: 20,
        };
        let receipt = pigade_storage::Receipt {
            kind: EventKind::Output,
            sequence: 0,
            end_offset: 0,
            checksum: 0,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        let pending = loop {
            if let Some(pending) = checkpoint::schedule(
                &root.path().join("missing"),
                &header,
                receipt,
                &terminal,
                None,
            )? {
                break pending;
            }
            ensure!(
                Instant::now() < deadline,
                "checkpoint worker did not release permit"
            );
            thread::sleep(Duration::from_millis(1));
        };
        let mut progress = CheckpointProgress {
            pending: Some(pending),
            ..Default::default()
        };
        assert_eq!(
            progress.events, 0,
            "no subsequent PTY event drives completion"
        );
        loop {
            let timeout = progress
                .receive_timeout()
                .context("pending result must remain serviceable")?;
            match progress.complete(false) {
                Err(error) => {
                    assert!(error.to_string().contains("checkpoint worker failed"));
                    assert_eq!(progress.completed, 0);
                    return Ok(());
                }
                Ok(()) => {
                    ensure!(Instant::now() < deadline, "idle failure was never observed");
                    thread::sleep(timeout);
                }
            }
        }
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
    let probe = &config.probe;
    let mut terminal = Terminal::new(config.geometry, true);
    let history = &config.history;
    terminal.set_history_observer(Some(history.clone()));
    let mut deferred = None;
    let mut checkpoints = CheckpointProgress::default();
    let mut metrics = metrics::SessionMetrics::default();
    // Fixed-size response chunks cap both owned bytes and queue-node count.
    // Query replies are ordered, but a full input writer must never prevent
    // this actor from draining PTY output (which may unblock the child reader).
    let mut query_responses: VecDeque<Vec<u8>> = VecDeque::new();
    let mut query_response_bytes = 0usize;
    loop {
        probe.enter(metrics::ActorStage::Waiting, None);
        if let Ok(Control::Stop) = config.control.try_recv() {
            let _ = child.kill();
        }
        if let Err(error) = checkpoints.complete(false) {
            probe.enter(metrics::ActorStage::Degraded, None);
            let mut current = view.lock().expect("view lock");
            current.storage_error = Some(format!("checkpoint completion failed: {error:#}"));
            current.state = "degraded".into();
            drop(current);
            notify_actor_change(&config.changes, "checkpoint completion failure");
            loop {
                wait_degraded(&config.control, &mut *child);
            }
        }
        if metrics.checkpoints != checkpoints.completed {
            metrics.checkpoints = checkpoints.completed;
            view.lock().expect("view lock").metrics = metrics.clone();
        }
        if !query_responses.is_empty() {
            probe.enter(metrics::ActorStage::QueryResponses, None);
        }
        for _ in 0..QUERY_RESPONSE_BURST {
            let Some(response) = query_responses.front() else {
                break;
            };
            match writer.try_write(response.clone()) {
                Ok(()) => {
                    let completed = query_responses.pop_front().expect("front response");
                    query_response_bytes -= completed.len();
                }
                // Continue servicing the ordered action queue. Retrying on a
                // short receive timeout below still makes progress when the
                // child reads input while producing no output.
                Err(input::InputWriteError::Full) => break,
                Err(error) => {
                    probe.enter(metrics::ActorStage::Degraded, None);
                    let mut v = view.lock().expect("view lock");
                    v.storage_error = Some(format!("PTY query response failed: {error}"));
                    v.state = "degraded".into();
                    drop(v);
                    notify_actor_change(&config.changes, "query response failure");
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
            } else if !query_responses.is_empty() {
                rx.recv_timeout(QUERY_RESPONSE_RETRY)
            } else if let Some(timeout) = checkpoints.receive_timeout() {
                rx.recv_timeout(timeout)
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
                        probe.enter(metrics::ActorStage::Checkpoint, None);
                        let checkpoint_result = checkpoints.publish(
                            false,
                            &config.root,
                            journal.header(),
                            &terminal,
                            history,
                        );
                        if matches!(checkpoint_result, Ok(true)) {
                            metrics.idle_checkpoint(checkpoint_start.elapsed());
                            metrics.checkpoints = checkpoints.completed;
                            view.lock().expect("view lock").metrics = metrics.clone();
                        }
                        if let Err(error) = checkpoint_result {
                            probe.enter(metrics::ActorStage::Degraded, None);
                            let mut current = view.lock().expect("view lock");
                            current.storage_error =
                                Some(format!("idle checkpoint failed: {error:#}"));
                            current.state = "degraded".into();
                            drop(current);
                            notify_actor_change(&config.changes, "idle checkpoint failure");
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
        let candidate_sequence = journal.durable_sequence().saturating_add(1);
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
                probe.enter(metrics::ActorStage::PtyResize, Some(candidate_sequence));
                master.resize(pty_size(g))?;
            }
            let journal_start = Instant::now();
            probe.enter(metrics::ActorStage::JournalAppend, Some(candidate_sequence));
            let receipt = journal.append(kind, &payload)?;
            let journal_work = journal_start.elapsed();
            probe.enter(metrics::ActorStage::HistoryBegin, Some(receipt.sequence));
            history.begin(receipt.sequence)?;
            let model_start = Instant::now();
            probe.enter(metrics::ActorStage::ModelApply, Some(receipt.sequence));
            match kind {
                EventKind::Output => terminal.output(&payload),
                EventKind::SyncTimeout => terminal.sync_timeout(),
                EventKind::Resize => {
                    let g = resize.expect("resize geometry");
                    terminal.resize(g);
                }
                EventKind::Exit => (),
                EventKind::OutputRejectedResource => unreachable!(
                    "live action cannot create resource refusal before admission integration"
                ),
            }
            let model_work = model_start.elapsed();
            // Check the combined pending-response bound before committing
            // derived history/checkpoint state or publishing this model event.
            // The raw event is already durable and remains replayable on error.
            let responses = terminal.take_responses();
            if !responses.is_empty() {
                ensure!(
                    responses.len() <= MAX_PENDING_QUERY_RESPONSE_BYTES,
                    "terminal query response budget exceeded"
                );
                let pending = query_response_bytes
                    .checked_add(responses.len())
                    .context("terminal query response byte count overflow")?;
                ensure!(
                    pending <= MAX_PENDING_QUERY_RESPONSE_BYTES,
                    "pending terminal query response budget exceeded"
                );
            }
            // Derived cells and index reach durability before either the frame
            // or a checkpoint is allowed to reference their committed root.
            let archive_start = Instant::now();
            probe.enter(metrics::ActorStage::HistoryCommit, Some(receipt.sequence));
            history.commit()?;
            let archive_work = archive_start.elapsed();
            checkpoints.observe(receipt, payload.len(), model_work);
            let checkpoint_start = Instant::now();
            probe.enter(metrics::ActorStage::Checkpoint, Some(receipt.sequence));
            checkpoints.publish(
                kind == EventKind::Exit,
                &config.root,
                journal.header(),
                &terminal,
                history,
            )?;
            let checkpoint_work = checkpoint_start.elapsed();
            if !responses.is_empty() {
                enqueue_query_responses(
                    &mut query_responses,
                    &mut query_response_bytes,
                    &responses,
                    MAX_PENDING_QUERY_RESPONSE_BYTES,
                )?;
            }
            let frame_start = Instant::now();
            probe.enter(metrics::ActorStage::FrameBuild, Some(receipt.sequence));
            let frame = terminal.frame();
            let history_summary = history.summary();
            probe.enter(metrics::ActorStage::Publish, Some(receipt.sequence));
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
            });
            metrics.checkpoints = checkpoints.completed;
            current.metrics = metrics.clone();
            if kind == EventKind::Exit {
                current.state = "exited".into();
                return Ok(false);
            }
            Ok(true)
        })();
        match result {
            Ok(true) => notify_actor_change(&config.changes, "terminal publication"),
            Ok(false) => {
                notify_actor_change(&config.changes, "terminal exit publication");
                probe.enter(metrics::ActorStage::Exited, Some(candidate_sequence));
                break;
            }
            Err(error) => {
                probe.enter(metrics::ActorStage::Degraded, Some(candidate_sequence));
                let mut current = view.lock().expect("view lock");
                current.storage_error = Some(format!("{error:#}"));
                current.state = "degraded".into();
                // Preserve the durable prefix and stop reading. No further publication/commands.
                drop(current);
                notify_actor_change(&config.changes, "terminal actor degraded");
                loop {
                    std::hint::black_box((&pending_action, &rx, &master));
                    wait_degraded(&config.control, &mut *child);
                }
            }
        }
    }
}

fn notify_actor_change(changes: &changes::ChangeHub, reason: &str) {
    if let Err(error) = changes.notify() {
        // Wait requests also fail closed; clients can fall back to Poll. Keep
        // the wake-path failure visible instead of treating it as success.
        changes.report_failure_once(|| {
            eprintln!("host change notification failed ({reason}): {error:#}");
        });
    }
}

#[cfg(test)]
mod change_notification_tests {
    use super::*;

    #[test]
    fn unavailable_change_hub_does_not_turn_committed_publication_into_failure() {
        let changes = changes::ChangeHub::new();
        changes.close_for_test();

        // Notifications are an optimization. A failed hub is reported once,
        // but it must not propagate as a failed terminal/launch operation.
        notify_actor_change(&changes, "test publication");
        notify_actor_change(&changes, "test publication retry");
        assert!(changes.token().is_err());
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
    let (jobs, internal_jobs) = ipc_server::bind(&runtime.join("host.sock"), host.changes.clone())?;
    let resume_reader = resume_probe::Reader::new(internal_jobs)?;
    let history_reader = history_read::Reader::new()?;
    let history_view_reader = history_view::Reader::new()?;
    let workspace_git_reader = workspace_git::Reader::new(root.clone())?;
    let provider_detector = ProviderDetector::new()?;
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
    for mut job in jobs {
        let mut resume_error = None;
        if let Some(completion) = job.resume_probe.take() {
            match resume_probe::accept(&mut host, &job.request.command, completion) {
                Ok(permit) => host.resume_verified = Some(permit),
                Err(error) => {
                    resume_error = Some(resume_probe::rejection(&host, &job.request.command, error))
                }
            }
        } else {
            match resume_probe::target(&mut host, &job.request.command) {
                Ok(Some(target)) => {
                    resume_reader.submit(job, target);
                    continue;
                }
                Ok(None) => (),
                Err(error) => {
                    resume_error = Some(resume_probe::rejection(&host, &job.request.command, error))
                }
            }
        }
        if let Command::DetectProviders = job.request.command {
            provider_detector.submit(job.reply);
            continue;
        }
        if matches!(&job.request.command, Command::CatalogApply { .. })
            && workspace_git_reader.index_busy()
        {
            let _ = job.reply.try_send(workspace_git::response(
                None,
                Some("Git index operation is in progress; metadata edit was not applied".into()),
            ));
            continue;
        }
        let index_command = match &job.request.command {
            Command::WorkspaceGitIndex {
                workspace,
                expected_revision,
                ticket,
                inspect,
            } => Some((workspace, expected_revision, ticket, *inspect, false)),
            Command::WorkspaceGitIndexAcknowledge {
                workspace,
                expected_revision,
                ticket,
            } => Some((workspace, expected_revision, ticket, true, true)),
            _ => None,
        };
        if let Some((workspace, expected_revision, ticket, inspect, acknowledge)) = index_command {
            let target = host.metadata.catalog().and_then(|catalog| {
                // Receipt inspection is read-only and remains possible after a
                // metadata rename/archive; it never revives a write lease.
                if inspect {
                    let current = catalog
                        .workspaces
                        .iter()
                        .find(|value| value.id == *workspace)
                        .ok_or_else(|| anyhow::anyhow!("workspace does not exist"))?;
                    let project = catalog
                        .projects
                        .iter()
                        .find(|value| value.id == current.project_id)
                        .ok_or_else(|| anyhow::anyhow!("workspace project does not exist"))?;
                    Ok(workspace_git::Target {
                        workspace_id: *workspace,
                        workspace_revision: *expected_revision,
                        project_id: current.project_id,
                        cwd: current.cwd.clone(),
                        project_root: project.canonical_path.clone(),
                        git_common_dir: project.git_common_dir.clone(),
                    })
                } else {
                    workspace_git::target(&catalog, *workspace, *expected_revision)
                }
            });
            match target {
                Ok(target) => {
                    if acknowledge {
                        workspace_git_reader.submit_index_ack(target, *ticket, job.reply);
                    } else {
                        workspace_git_reader.submit_index(target, *ticket, inspect, job.reply);
                    }
                }
                Err(error) => {
                    let _ = job.reply.try_send(workspace_git::response(
                        None,
                        Some(format!("workspace Git index unavailable: {error:#}")),
                    ));
                }
            }
            continue;
        }
        if let Command::WorkspaceGitDiff {
            workspace,
            expected_revision,
            path,
            side,
        } = &job.request.command
        {
            let target = host.metadata.catalog().and_then(|catalog| {
                workspace_git::target(&catalog, *workspace, *expected_revision)
            });
            match target {
                Ok(target) => {
                    workspace_git_reader.submit_diff(target, path.clone(), *side, job.reply)
                }
                Err(error) => {
                    let _ = job.reply.try_send(workspace_git::response(
                        None,
                        Some(format!("workspace Git diff unavailable: {error:#}")),
                    ));
                }
            }
            continue;
        }
        if let Command::WorkspaceGitSnapshot {
            workspace,
            expected_revision,
        } = job.request.command
        {
            let target = host
                .metadata
                .catalog()
                .and_then(|catalog| workspace_git::target(&catalog, workspace, expected_revision));
            match target {
                Ok(target) => workspace_git_reader.submit(target, job.reply),
                Err(error) => {
                    let _ = job.reply.try_send(workspace_git::response(
                        None,
                        Some(format!("workspace Git status unavailable: {error:#}")),
                    ));
                }
            }
            continue;
        }
        if let Command::HistoryView { query } = job.request.command {
            match host.history_view_source(&query) {
                Ok(source) => history_view_reader.submit(query, source, job.reply),
                Err(error) => {
                    let _ = job.reply.try_send(history_read::response(Err(error), None));
                }
            }
            continue;
        }
        if let Command::HistoryRead { query } = job.request.command {
            match host.history_source(&query) {
                Ok(source) => history_reader.submit(source, query, job.reply),
                Err(error) => {
                    let _ = job.reply.try_send(history_read::response(Err(error), None));
                }
            }
            continue;
        }
        let result = if let Some(error) = resume_error {
            Err(error)
        } else {
            host.dispatch(job.request.command)
        };
        host.resume_verified = None;
        let response = match result {
            Ok(response) => response,
            Err(error) => Response {
                protocol: PROTOCOL,
                views: Vec::new(),
                error: Some(format!("{error:#}")),
                rejected_before_admission: rejected_before_admission(&error),
                catalog: None,
                selection: None,
                context: None,
                launch_preview: None,
                launch_phase: None,
                catalog_notice: host.catalog_notice.clone(),
                recovery: host.recovery.response(),
                selected: None,
                history_chunk: None,
                change_token: None,
                generations: None,
                generation_selection: None,
                history_view: None,
                providers: None,
                template_run: None,
                template_runs: None,
                panel_directory: None,
                workspace_git_snapshot: None,
                workspace_git_diff: None,
                workspace_git_index: None,
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

#[cfg(test)]
mod actor_diagnostic_tests {
    use super::*;

    fn test_view() -> View {
        let geometry = Geometry::new(4, 12).unwrap();
        View {
            session: Uuid::new_v4(),
            generation: Uuid::new_v4(),
            sequence: 0,
            state: "running".into(),
            recovery_path: "live".into(),
            pid: Some(1),
            storage_error: None,
            frame: Terminal::new(geometry, false).frame(),
            history: None,
            recovery_journal_bytes_read: 0,
            metrics: metrics::SessionMetrics::default(),
            actor_diagnostic: None,
        }
    }

    #[test]
    fn blocked_stage_is_visible_while_published_view_remains_unchanged() {
        let probe = Arc::new(metrics::ActorProbe::default());
        let view = Arc::new(Mutex::new(test_view()));
        let (commands, _rx) = mpsc::sync_channel(1);
        let (control, _control_rx) = mpsc::sync_channel(1);
        let session = Session {
            view,
            actor_probe: Some(probe.clone()),
            commands,
            input: None,
            control,
            _history: None,
        };
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let actor_probe = probe.clone();
        let blocked = thread::spawn(move || {
            actor_probe.enter(metrics::ActorStage::JournalAppend, Some(1));
            entered_tx.send(()).unwrap();
            // Simulates a blocking journal/fsync call after the actor has
            // released the probe lock; the real snapshot path must stay live.
            release_rx.recv().unwrap();
            actor_probe.enter(metrics::ActorStage::ModelApply, Some(1));
        });
        entered_rx.recv().unwrap();

        let snapshot = snapshot_view(&session);
        assert_eq!(snapshot.sequence, 0, "no frame publication occurred");
        let diagnostic = snapshot.actor_diagnostic.unwrap();
        assert!(diagnostic.sample_available);
        assert!(matches!(
            diagnostic.stage,
            Some(metrics::ActorStage::JournalAppend)
        ));
        assert_eq!(diagnostic.candidate_sequence, Some(1));
        assert!(diagnostic.entered_monotonic_ns.is_some());

        release_tx.send(()).unwrap();
        blocked.join().unwrap();
    }

    #[test]
    fn older_view_json_without_diagnostic_decodes() {
        let mut value = serde_json::to_value(test_view()).unwrap();
        value.as_object_mut().unwrap().remove("actor_diagnostic");
        let decoded: View = serde_json::from_value(value).unwrap();
        assert!(decoded.actor_diagnostic.is_none());
    }
}

#[cfg(test)]
mod resource_refusal_tests;
