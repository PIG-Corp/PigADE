//! Host protocol 2 display-only DTOs. No terminal parser/core dependency.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::Path,
    time::Duration,
};

pub const LIMIT: usize = 8 * 1024 * 1024;
pub const MAX_RECOVERY_ISSUES: usize = 16;
pub const MAX_RECOVERY_TEXT_BYTES: usize = 1024;

pub fn generation_label(value: &str) -> String {
    let Ok(id) = uuid::Uuid::parse_str(value) else {
        return "неизвестное поколение".into();
    };
    let id = id.simple().to_string();
    format!("{}…{}", &id[..8], &id[28..])
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Geometry {
    pub rows: u16,
    pub cols: u16,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Cell {
    pub text: String,
    pub fg: String,
    pub bg: String,
    pub flags: u16,
    pub underline: Option<String>,
    pub hyperlink: Option<u32>,
}
const PALETTE_COLORS: usize = 269;

fn validate_palette(overrides: &[(u16, [u8; 3])]) -> Result<(), String> {
    if overrides.len() > PALETTE_COLORS
        || overrides
            .iter()
            .any(|(index, _)| usize::from(*index) >= PALETTE_COLORS)
        || overrides.windows(2).any(|pair| pair[0].0 >= pair[1].0)
    {
        return Err("invalid terminal palette overrides".into());
    }
    Ok(())
}

fn deserialize_palette_overrides<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<(u16, [u8; 3])>, D::Error> {
    struct PaletteVisitor;
    impl<'de> serde::de::Visitor<'de> for PaletteVisitor {
        type Value = Vec<(u16, [u8; 3])>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("at most269 sorted unique terminal RGB overrides")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            if seq.size_hint().is_some_and(|n| n > PALETTE_COLORS) {
                return Err(serde::de::Error::custom("too many terminal colors"));
            }
            let mut entries: Vec<(u16, [u8; 3])> = Vec::new();
            while let Some(entry) = seq.next_element::<(u16, [u8; 3])>()? {
                if entries.len() == PALETTE_COLORS
                    || usize::from(entry.0) >= PALETTE_COLORS
                    || entries.last().is_some_and(|last| last.0 >= entry.0)
                {
                    return Err(serde::de::Error::custom(
                        "invalid terminal palette overrides",
                    ));
                }
                entries.push(entry);
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_seq(PaletteVisitor)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CursorPresentationShape {
    Block,
    Underline,
    Beam,
    HollowBlock,
    Hidden,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub struct CursorPresentation {
    pub shape: CursorPresentationShape,
    pub blinking: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Frame {
    pub geometry: Geometry,
    pub cells: Vec<Cell>,
    pub hyperlinks: Vec<(String, String)>,
    pub cursor: (i32, usize),
    pub cursor_style: String,
    #[serde(default)]
    pub cursor_presentation: Option<CursorPresentation>,
    pub modes: u32,
    pub colors: String,
    #[serde(default, deserialize_with = "deserialize_palette_overrides")]
    pub palette_overrides: Vec<(u16, [u8; 3])>,
    pub history_lines: usize,
    pub sync_pending_bytes: usize,
    pub title: String,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct View {
    pub session: String,
    pub generation: String,
    pub sequence: u64,
    pub state: String,
    pub recovery_path: String,
    pub pid: Option<u32>,
    pub storage_error: Option<String>,
    pub frame: Frame,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Known {
    pub session: String,
    pub generation: String,
    pub sequence: u64,
    pub state: String,
    pub pid: Option<u32>,
    pub storage_error: Option<String>,
}
impl View {
    pub fn known(&self) -> Known {
        Known {
            session: self.session.clone(),
            generation: self.generation.clone(),
            sequence: self.sequence,
            state: self.state.clone(),
            pid: self.pid,
            storage_error: self.storage_error.clone(),
        }
    }
}
/// Opaque notification epoch; independent of terminal generation/sequence.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChangeToken {
    pub host_instance: String,
    pub revision: u64,
}
impl ChangeToken {
    fn validate(&self) -> Result<(), String> {
        let bytes = self.host_instance.as_bytes();
        if bytes.len() != 36
            || bytes.iter().enumerate().any(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    *byte != b'-'
                } else {
                    !byte.is_ascii_hexdigit()
                }
            })
        {
            return Err("Invalid host notification epoch".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Response {
    protocol: u32,
    views: Vec<View>,
    error: Option<String>,
    #[serde(default)]
    rejected_before_admission: Option<String>,
    #[serde(default)]
    selected: Option<Vec<Known>>,
    #[serde(default)]
    recovery: Option<RecoveryDiagnostics>,
    #[serde(default)]
    change_token: Option<ChangeToken>,
    #[serde(default)]
    activity: Option<Value>,
    #[serde(default)]
    activity_error: Option<String>,
    #[serde(default)]
    catalog: Option<pigade_storage::Catalog>,
    #[serde(default)]
    selection: Option<pigade_storage::Selection>,
    #[serde(default)]
    context: Option<pigade_storage::ContextDocument>,
    #[serde(default)]
    launch_preview: Option<LaunchPreview>,
    #[serde(default)]
    launch_phase: Option<String>,
    #[serde(default)]
    generations: Option<GenerationList>,
    #[serde(default)]
    generation_selection: Option<bool>,
    #[serde(default)]
    history_view: Option<HistoryViewReply>,
    #[serde(default)]
    providers: Option<Vec<ProviderInfo>>,
    #[serde(default)]
    template_run: Option<TemplateRunView>,
    #[serde(default)]
    template_runs: Option<pigade_storage::TemplateRunPage>,
    #[serde(default)]
    panel_directory: Option<PanelDirectory>,
    #[serde(default)]
    workspace_git_snapshot: Option<WorkspaceGitSnapshot>,
    #[serde(default)]
    workspace_git_diff: Option<WorkspaceGitDiff>,
    #[serde(default)]
    workspace_git_index: Option<WorkspaceGitIndexResult>,
    #[serde(default)]
    workspace_git_commit_plan: Option<WorkspaceGitCommitPlan>,
    #[serde(default)]
    workspace_git_commit: Option<WorkspaceGitCommitResult>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitSnapshot {
    pub workspace_id: uuid::Uuid,
    pub workspace_revision: u64,
    pub project_id: uuid::Uuid,
    pub cwd: std::path::PathBuf,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub detached: bool,
    pub entries: Vec<WorkspaceGitEntry>,
    pub truncated: bool,
    #[serde(default)]
    pub index_operation: Option<WorkspaceGitIndexResult>,
    #[serde(default)]
    pub index_operation_unavailable_reason: Option<String>,
    #[serde(default)]
    pub commit_schema: Option<u32>,
    #[serde(default)]
    pub commit_operation: Option<WorkspaceGitCommitResult>,
    #[serde(default)]
    pub commit_unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitEntry {
    pub path: String,
    pub original_path: Option<String>,
    pub index_status: char,
    pub worktree_status: char,
    pub untracked: bool,
    pub unmerged: bool,
}

impl WorkspaceGitSnapshot {
    fn validate(&self, target: Option<(uuid::Uuid, u64)>) -> Result<(), String> {
        let status = |value: char| ".MADRCUT?!".contains(value);
        if target != Some((self.workspace_id, self.workspace_revision))
            || self.workspace_id.is_nil()
            || self.project_id.is_nil()
            || !self.cwd.is_absolute()
            || self.entries.len() > 128
            || self.branch.as_ref().is_some_and(|value| value.len() > 4096)
            || self.head.as_ref().is_some_and(|value| {
                value.len() > 128 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || self
                .index_operation_unavailable_reason
                .as_ref()
                .is_some_and(|reason| reason.len() > 1024)
            || self.commit_schema.is_some_and(|schema| schema != 1)
            || self
                .commit_unavailable_reason
                .as_ref()
                .is_some_and(|reason| reason.len() > 1024)
            || self.commit_operation.as_ref().is_some_and(|operation| {
                operation.validate().is_err()
                    || operation.workspace_id != self.workspace_id
                    || operation.project_id != self.project_id
                    || operation.cwd != self.cwd
            })
            || self.entries.iter().any(|entry| {
                entry.path.is_empty()
                    || entry.path.len() > 32768
                    || entry
                        .original_path
                        .as_ref()
                        .is_some_and(|path| path.len() > 32768)
                    || !status(entry.index_status)
                    || !status(entry.worktree_status)
            })
            || self
                .index_operation
                .as_ref()
                .is_some_and(|operation| operation.validate_pending_snapshot(self).is_err())
        {
            return Err("Ответ Git не соответствует выбранному воркспейсу".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitCommitEntry {
    pub path: String,
    pub status: char,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceGitCommitPlan {
    pub ticket: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    pub workspace_revision: u64,
    pub project_id: uuid::Uuid,
    pub cwd: std::path::PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub files: Vec<GitCommitEntry>,
    pub signing_requested: bool,
}

impl WorkspaceGitCommitPlan {
    fn validate(&self, target: Option<(uuid::Uuid, u64)>) -> Result<(), String> {
        if self.ticket.is_nil()
            || target != Some((self.workspace_id, self.workspace_revision))
            || self.workspace_id.is_nil()
            || self.project_id.is_nil()
            || !self.cwd.is_absolute()
            || self.files.is_empty()
            || self.files.len() > 512
            || self.files.iter().any(|file| {
                file.path.is_empty()
                    || file.path.len() > 32768
                    || !matches!(file.status, 'A' | 'M' | 'D' | 'T')
            })
            || self.files.iter().map(|file| file.path.len()).sum::<usize>() > LIMIT
            || self
                .files
                .windows(2)
                .any(|pair| pair[0].path == pair[1].path)
            || self
                .head
                .as_ref()
                .is_some_and(|head| !valid_commit_oid(head))
            || self
                .branch
                .as_ref()
                .is_some_and(|branch| branch.len() > 4096)
        {
            return Err("План коммита Git не соответствует воркспейсу".into());
        }
        Ok(())
    }
}

fn valid_commit_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitCommitPhase {
    Running,
    Applied,
    Rejected,
    Failed,
    Unknown,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceGitCommitResult {
    pub ticket: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    pub workspace_revision: u64,
    pub project_id: uuid::Uuid,
    pub cwd: std::path::PathBuf,
    pub phase: GitCommitPhase,
    pub commit: Option<String>,
    pub acknowledged: bool,
    pub message: Option<String>,
}

impl WorkspaceGitCommitResult {
    fn validate(&self) -> Result<(), String> {
        if self.ticket.is_nil()
            || self.workspace_id.is_nil()
            || self.project_id.is_nil()
            || !self.cwd.is_absolute()
            || self
                .commit
                .as_ref()
                .is_some_and(|oid| !valid_commit_oid(oid))
            || self
                .message
                .as_ref()
                .is_some_and(|message| message.len() > 1024)
            || (self.phase == GitCommitPhase::Applied) != self.commit.is_some()
        {
            return Err("Квитанция коммита Git некорректна".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitDiffSide {
    Index,
    Worktree,
    Untracked,
    Conflict,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitDiff {
    pub workspace_id: uuid::Uuid,
    pub workspace_revision: u64,
    pub project_id: uuid::Uuid,
    pub cwd: std::path::PathBuf,
    pub path: String,
    pub side: GitDiffSide,
    pub text: String,
    pub truncated: bool,
    pub binary: bool,
    #[serde(default)]
    pub index_ticket: Option<GitIndexTicket>,
    #[serde(default)]
    pub index_unavailable_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitIndexAction {
    Stage,
    Unstage,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitIndexPhase {
    Applied,
    Rejected,
    Unknown,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GitIndexTicket {
    pub id: uuid::Uuid,
    pub action: GitIndexAction,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceGitIndexResult {
    pub ticket: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    pub workspace_revision: u64,
    pub project_id: uuid::Uuid,
    pub cwd: std::path::PathBuf,
    pub path: String,
    pub side: GitDiffSide,
    pub action: GitIndexAction,
    pub phase: GitIndexPhase,
    pub message: Option<String>,
    #[serde(default)]
    pub acknowledged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitDiffTarget {
    workspace_id: uuid::Uuid,
    workspace_revision: u64,
    path: String,
    side: GitDiffSide,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceGitIndexTarget {
    workspace_id: uuid::Uuid,
    workspace_revision: u64,
    ticket: uuid::Uuid,
    acknowledge: bool,
}

fn workspace_git_index_target(command: &Value) -> Result<Option<WorkspaceGitIndexTarget>, String> {
    let command_name = command.get("command").and_then(Value::as_str);
    let acknowledge = command_name == Some("workspace_git_index_acknowledge");
    if !acknowledge && command_name != Some("workspace_git_index") {
        return Ok(None);
    }
    let workspace_id = command
        .get("workspace")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<uuid::Uuid>().ok())
        .filter(|id| !id.is_nil())
        .ok_or_else(|| "Не указан воркспейс изменения индекса Git".to_owned())?;
    let workspace_revision = command
        .get("expected_revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Не указана ревизия воркспейса изменения индекса Git".to_owned())?;
    let ticket = command
        .get("ticket")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<uuid::Uuid>().ok())
        .filter(|id| !id.is_nil())
        .ok_or_else(|| "Не указан билет изменения индекса Git".to_owned())?;
    if !acknowledge {
        command
            .get("inspect")
            .and_then(Value::as_bool)
            .ok_or_else(|| "Не указан режим проверки изменения индекса Git".to_owned())?;
    }
    Ok(Some(WorkspaceGitIndexTarget {
        workspace_id,
        workspace_revision,
        ticket,
        acknowledge,
    }))
}

fn index_side_matches(side: GitDiffSide, action: GitIndexAction) -> bool {
    matches!(
        (side, action),
        (GitDiffSide::Index, GitIndexAction::Unstage)
            | (
                GitDiffSide::Worktree | GitDiffSide::Untracked,
                GitIndexAction::Stage
            )
    )
}

fn validate_git_diff_path(path: &str) -> Result<(), String> {
    let bytes = path.as_bytes();
    let has_drive_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if path.is_empty()
        || path.len() > 4096
        || path.starts_with('-')
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains('\\')
        || has_drive_prefix
        || std::path::Path::new(path).is_absolute()
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err("Недопустимый относительный путь файла Git".into());
    }
    Ok(())
}

fn workspace_git_diff_target(command: &Value) -> Result<Option<WorkspaceGitDiffTarget>, String> {
    if command.get("command").and_then(Value::as_str) != Some("workspace_git_diff") {
        return Ok(None);
    }
    let workspace_id = command
        .get("workspace")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<uuid::Uuid>().ok())
        .filter(|id| !id.is_nil())
        .ok_or_else(|| "Не указан воркспейс Git diff".to_owned())?;
    let workspace_revision = command
        .get("expected_revision")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Не указана ревизия воркспейса Git diff".to_owned())?;
    let path = command
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "Не указан путь файла Git diff".to_owned())?
        .to_owned();
    validate_git_diff_path(&path)?;
    let side = command
        .get("side")
        .cloned()
        .ok_or_else(|| "Не указана сторона Git diff".to_owned())
        .and_then(|value| {
            serde_json::from_value::<GitDiffSide>(value)
                .map_err(|_| "Недопустимая сторона Git diff".to_owned())
        })?;
    Ok(Some(WorkspaceGitDiffTarget {
        workspace_id,
        workspace_revision,
        path,
        side,
    }))
}

impl WorkspaceGitDiff {
    fn validate(&self, target: Option<&WorkspaceGitDiffTarget>) -> Result<(), String> {
        if target.is_none_or(|target| {
            self.workspace_id != target.workspace_id
                || self.workspace_revision != target.workspace_revision
                || self.path != target.path
                || self.side != target.side
        }) || self.workspace_id.is_nil()
            || self.project_id.is_nil()
            || !self.cwd.is_absolute()
            || validate_git_diff_path(&self.path).is_err()
            || self.text.len() > 128 * 1024
            || self.text.lines().count() > 2000
            || self.binary && !self.text.is_empty()
            || self
                .index_unavailable_reason
                .as_ref()
                .is_some_and(|reason| reason.len() > 1024)
            || self.index_ticket.is_some() && self.index_unavailable_reason.is_some()
            || self.index_ticket.is_some_and(|ticket| {
                ticket.id.is_nil() || !index_side_matches(self.side, ticket.action)
            })
        {
            return Err("Ответ Git diff не соответствует выбранному файлу".into());
        }
        Ok(())
    }
}

impl WorkspaceGitIndexResult {
    fn validate(&self, target: Option<&WorkspaceGitIndexTarget>) -> Result<(), String> {
        if target.is_none_or(|target| {
            self.ticket != target.ticket
                || self.workspace_id != target.workspace_id
                || self.workspace_revision != target.workspace_revision
        }) || self.workspace_id.is_nil()
            || self.project_id.is_nil()
            || !self.cwd.is_absolute()
            || validate_git_diff_path(&self.path).is_err()
            || !index_side_matches(self.side, self.action)
            || self
                .message
                .as_ref()
                .is_some_and(|message| message.len() > 1024)
            || target.is_some_and(|target| {
                target.acknowledge
                    && !self.acknowledged
                    && !(matches!(
                        self.phase,
                        GitIndexPhase::Unavailable | GitIndexPhase::Rejected
                    ) && self
                        .message
                        .as_ref()
                        .is_some_and(|message| !message.is_empty()))
            })
        {
            return Err("Ответ изменения индекса Git не соответствует выбранному билету".into());
        }
        Ok(())
    }

    fn validate_pending_snapshot(&self, snapshot: &WorkspaceGitSnapshot) -> Result<(), String> {
        if self.workspace_id != snapshot.workspace_id
            || self.project_id != snapshot.project_id
            || self.cwd != snapshot.cwd
            || self.workspace_revision > snapshot.workspace_revision
            || self.phase != GitIndexPhase::Unknown
            || self.acknowledged
        {
            return Err("Недопустимая незавершённая операция индекса Git".into());
        }
        self.validate(Some(&WorkspaceGitIndexTarget {
            workspace_id: self.workspace_id,
            workspace_revision: self.workspace_revision,
            ticket: self.ticket,
            acknowledge: false,
        }))
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct PanelDirectory {
    pub query: String,
    pub entries: Vec<PanelEntry>,
    pub next_after: Option<uuid::Uuid>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct PanelEntry {
    pub session: uuid::Uuid,
    pub tab_id: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    pub project_id: uuid::Uuid,
    pub generation: Option<uuid::Uuid>,
    pub title: String,
    pub provider: Option<String>,
    pub role: String,
    pub deliver_context: bool,
    pub state: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TemplateRunView {
    pub id: uuid::Uuid,
    pub tab_id: uuid::Uuid,
    pub workspace_id: uuid::Uuid,
    pub name: String,
    pub revision: u64,
    pub status: String,
    pub can_advance: bool,
    pub error: Option<String>,
    pub slots: Vec<TemplateRunSlotView>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TemplateRunSlotView {
    pub preview: LaunchPreview,
    pub phase: String,
    pub placed: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderInfo {
    pub provider: String,
    pub executable: Option<std::path::PathBuf>,
    pub version: Option<String>,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct HistoryCursor {
    pub row: u64,
    pub offset: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HistoryViewQuery {
    OpenGrid {
        session: uuid::Uuid,
        generation: uuid::Uuid,
        rows: u16,
        cols: u16,
        back: u64,
    },
    Grid {
        snapshot: uuid::Uuid,
        start: u64,
        rows: u16,
        cols: u16,
    },
    Open {
        session: uuid::Uuid,
        generation: uuid::Uuid,
    },
    Page {
        snapshot: uuid::Uuid,
        start: HistoryCursor,
    },
    Search {
        snapshot: uuid::Uuid,
        text: String,
        cursor: Option<HistoryCursor>,
    },
    Close {
        snapshot: uuid::Uuid,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct HistoryTextChunk {
    pub text: String,
    pub total_bytes: u64,
    pub offset: u64,
    pub next_offset: Option<u64>,
    pub wrap: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct HistoryTextRow {
    pub row: u64,
    pub chunk: HistoryTextChunk,
    pub boundary: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct HistoryPage {
    pub snapshot: uuid::Uuid,
    pub session: uuid::Uuid,
    pub generation: uuid::Uuid,
    pub sequence: u64,
    pub total_rows: u64,
    pub start: HistoryCursor,
    pub rows: Vec<HistoryTextRow>,
    pub next: Option<HistoryCursor>,
    pub previous: Option<HistoryCursor>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct HistorySearchResult {
    pub snapshot: uuid::Uuid,
    pub session: uuid::Uuid,
    pub generation: uuid::Uuid,
    pub sequence: u64,
    pub found: Option<HistoryCursor>,
    pub next: Option<HistoryCursor>,
    pub scanned_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct HistoryGridRow {
    pub row: u64,
    pub source_columns: u16,
    pub boundary: Option<String>,
    pub cells: Vec<Cell>,
    #[serde(default, deserialize_with = "deserialize_palette_overrides")]
    pub palette_overrides: Vec<(u16, [u8; 3])>,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct HistoryGridPage {
    pub snapshot: uuid::Uuid,
    pub session: uuid::Uuid,
    pub generation: uuid::Uuid,
    pub sequence: u64,
    pub total_rows: u64,
    pub start: u64,
    pub rows_requested: u16,
    pub rows: Vec<HistoryGridRow>,
    pub hyperlinks: Vec<(String, String)>,
    pub cols: u16,
    pub horizontal_clipped: bool,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryViewReply {
    Grid { page: HistoryGridPage },
    Page { page: HistoryPage },
    Search { result: HistorySearchResult },
    Closed { snapshot: uuid::Uuid },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PanelTarget {
    pub session: uuid::Uuid,
    pub generation: Option<uuid::Uuid>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GenerationInfo {
    pub generation: uuid::Uuid,
    pub ordinal: u64,
    pub state: String,
    pub has_frame: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GenerationList {
    pub session: uuid::Uuid,
    pub head: Option<uuid::Uuid>,
    pub pinned: Option<uuid::Uuid>,
    pub entries: Vec<GenerationInfo>,
    pub next_before: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AdapterCapabilities {
    pub startup_context: bool,
    pub context_receipt: bool,
    pub native_resume: bool,
    pub conversation_id: bool,
    #[serde(default)]
    pub model_selection: bool,
    #[serde(default)]
    pub reasoning_effort: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LaunchPreview {
    pub plan_id: uuid::Uuid,
    pub tab_id: uuid::Uuid,
    pub side: pigade_layout::Direction,
    pub target: Option<uuid::Uuid>,
    pub session: uuid::Uuid,
    pub generation: uuid::Uuid,
    pub provider: String,
    pub cli_version: String,
    pub adapter_version: u32,
    pub delivery_method: String,
    pub context: pigade_storage::EffectiveContext,
    pub capabilities: AdapterCapabilities,
    pub include_memory: bool,
    #[serde(default)]
    pub memory: Option<pigade_storage::MemoryReview>,
    pub role_override: String,
    pub agent_instructions: String,
    pub deliver_context: bool,
    pub can_launch: bool,
    pub unavailable_reason: Option<String>,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub executable: Option<std::path::PathBuf>,
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
    #[serde(default)]
    pub conversation: Option<pigade_storage::ConversationBinding>,
    #[serde(default)]
    pub resume: Option<ResumeSource>,
    #[serde(default)]
    pub profile: Option<ProfileSource>,
    #[serde(default)]
    pub engine: pigade_storage::EngineOptions,
    #[serde(default)]
    pub custom_command: Option<pigade_storage::CustomCommand>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProfileSource {
    pub id: uuid::Uuid,
    pub name: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResumeSource {
    pub binding: pigade_storage::ConversationBinding,
    pub previous_head: uuid::Uuid,
}

pub struct ProductReply {
    pub ui_request: Option<uuid::Uuid>,
    pub providers_local: bool,
    pub providers: Option<Vec<ProviderInfo>>,
    pub providers_request: bool,
    pub generation_selection: Option<bool>,
    pub generation_selected: bool,
    pub generations: Option<GenerationList>,
    pub context_saved: bool,
    pub launch_preview: Option<LaunchPreview>,
    pub launch_phase: Option<String>,
    pub sessions: Option<Vec<View>>,
    pub process_view: Option<View>,
    pub launch_confirmed: bool,
    pub catalog: Option<pigade_storage::Catalog>,
    pub selection: Option<pigade_storage::Selection>,
    pub context: Option<pigade_storage::ContextDocument>,
    pub history_view: Option<HistoryViewReply>,
    pub history_view_request: bool,
    pub template_run: Option<TemplateRunView>,
    pub template_runs: Option<pigade_storage::TemplateRunPage>,
    pub panel_directory: Option<PanelDirectory>,
    pub workspace_git_snapshot: Option<WorkspaceGitSnapshot>,
    pub workspace_git_diff: Option<WorkspaceGitDiff>,
    pub workspace_git_index: Option<WorkspaceGitIndexResult>,
    pub workspace_git_commit_plan: Option<WorkspaceGitCommitPlan>,
    pub workspace_git_commit: Option<WorkspaceGitCommitResult>,
}

pub struct ProductError {
    pub ui_request: Option<uuid::Uuid>,
    pub providers_request: bool,
    pub message: String,
    pub rejected_before_launch: bool,
}

pub fn product_request(socket: &Path, mut command: Value) -> Result<ProductReply, ProductError> {
    let ui_request = command
        .as_object_mut()
        .and_then(|object| object.remove("_ui_request"))
        .and_then(|value| {
            value
                .as_str()
                .and_then(|value| value.parse::<uuid::Uuid>().ok())
        });
    let providers_request =
        command.get("command").and_then(Value::as_str) == Some("detect_providers");
    match product_request_inner(socket, command) {
        Ok(mut reply) => {
            reply.ui_request = ui_request;
            reply.providers_request = providers_request;
            Ok(reply)
        }
        Err(mut error) => {
            error.ui_request = ui_request;
            error.providers_request = providers_request;
            Err(error)
        }
    }
}

fn product_request_inner(socket: &Path, command: Value) -> Result<ProductReply, ProductError> {
    let command_name = command.get("command").and_then(Value::as_str);
    let git_diff_target = workspace_git_diff_target(&command).map_err(|message| ProductError {
        ui_request: None,
        providers_request: false,
        message,
        rejected_before_launch: false,
    })?;
    let git_index_target =
        workspace_git_index_target(&command).map_err(|message| ProductError {
            ui_request: None,
            providers_request: false,
            message,
            rejected_before_launch: false,
        })?;
    let git_commit_request = matches!(
        command_name,
        Some(
            "workspace_git_commit_prepare"
                | "workspace_git_commit"
                | "workspace_git_commit_acknowledge"
        )
    );
    let git_commit_prepare_request = command_name == Some("workspace_git_commit_prepare");
    let git_commit_target = git_commit_request
        .then(|| {
            Some((
                command
                    .get("workspace")?
                    .as_str()?
                    .parse::<uuid::Uuid>()
                    .ok()?,
                command.get("expected_revision")?.as_u64()?,
            ))
        })
        .flatten();
    let git_request = command_name == Some("workspace_git_snapshot");
    let git_target = git_request
        .then(|| {
            Some((
                command
                    .get("workspace")?
                    .as_str()?
                    .parse::<uuid::Uuid>()
                    .ok()?,
                command.get("expected_revision")?.as_u64()?,
            ))
        })
        .flatten();
    let discovery_request = command_name == Some("detect_providers");
    let launch_confirmed = matches!(command_name, Some("launch_in_tab" | "start_prepared"));
    let list_sessions = command_name == Some("list");
    let git_diff_request = command_name == Some("workspace_git_diff");
    let git_index_request = matches!(
        command_name,
        Some("workspace_git_index" | "workspace_git_index_acknowledge")
    );
    let context_saved = command_name == Some("save_context");
    let generation_selected = command_name == Some("select_generation");
    let history_view_request = command_name == Some("history_view");
    let generation_owner = (command_name == Some("generations"))
        .then(|| {
            command
                .get("session")
                .and_then(Value::as_str)
                .and_then(|id| id.parse::<uuid::Uuid>().ok())
        })
        .flatten();
    let process_target = matches!(command_name, Some("stop" | "snapshot")).then(|| {
        (
            command
                .get("session")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<uuid::Uuid>().ok()),
            command
                .get("generation")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<uuid::Uuid>().ok()),
        )
    });
    let mut response = request_response_with_timeout(socket, command, Duration::from_secs(15))
        .map_err(|message| ProductError {
            ui_request: None,
            providers_request: false,
            message,
            rejected_before_launch: false,
        })?;
    let providers_local = discovery_request
        && response.error.as_deref() == Some("invalid request framing or payload");
    if providers_local {
        response.providers = Some(crate::provider_discovery::detect().map_err(|message| ProductError {
            ui_request: None, providers_request: true,
            message: format!("Работающий host не поддерживает проверку CLI; локальная проверка не выполнена: {message}"),
            rejected_before_launch: false,
        })?);
        response.error = None;
    }
    if let Some(message) = response.error {
        return Err(ProductError {
            ui_request: None,
            providers_request: false,
            message,
            rejected_before_launch: launch_confirmed
                && matches!(
                    response.rejected_before_admission.as_deref(),
                    Some("launch_preflight" | "queue_full" | "host_shutting_down")
                ),
        });
    }
    if let Some(page) = &response.generations {
        let mut unique = std::collections::HashSet::new();
        if generation_owner != Some(page.session)
            || page.entries.len() > 32
            || page
                .entries
                .iter()
                .any(|entry| entry.generation.is_nil() || !unique.insert(entry.generation))
        {
            return Err(ProductError {
                ui_request: None,
                providers_request: false,
                message: "Недопустимый список поколений панели".into(),
                rejected_before_launch: false,
            });
        }
    }
    if let Some(providers) = &response.providers {
        let mut unique = std::collections::HashSet::new();
        if providers.len() > 4
            || providers.iter().any(|info| {
                !matches!(
                    info.provider.as_str(),
                    "shell" | "claude" | "codex" | "opencode"
                ) || !unique.insert(info.provider.as_str())
                    || info.available
                        && info
                            .executable
                            .as_ref()
                            .is_none_or(|path| !path.is_absolute())
                    || info
                        .version
                        .as_ref()
                        .is_some_and(|value| value.len() > 65536)
                    || info
                        .reason
                        .as_ref()
                        .is_some_and(|value| value.len() > 65536)
            })
        {
            return Err(ProductError {
                ui_request: None,
                providers_request: false,
                message: "Некорректный список CLI от session host".into(),
                rejected_before_launch: false,
            });
        }
    }
    if git_request || response.workspace_git_snapshot.is_some() {
        response.workspace_git_snapshot.as_ref()
            .ok_or_else(|| "Эта версия session host не вернула Git-статус. Обновите приложение при следующем запуске.".to_owned())
            .and_then(|snapshot| snapshot.validate(git_target))
            .map_err(|message| ProductError { ui_request:None, providers_request:false, message, rejected_before_launch:false })?;
    }
    if git_diff_request || response.workspace_git_diff.is_some() {
        response
            .workspace_git_diff
            .as_ref()
            .ok_or_else(|| {
                "Эта версия session host не вернула Git diff. Обновите приложение при следующем запуске."
                    .to_owned()
            })
            .and_then(|diff| diff.validate(git_diff_target.as_ref()))
            .map_err(|message| ProductError {
                ui_request: None,
                providers_request: false,
                message,
                rejected_before_launch: false,
            })?;
    }
    if git_index_request || response.workspace_git_index.is_some() {
        response
            .workspace_git_index
            .as_ref()
            .ok_or_else(|| "Session host не вернул квитанцию изменения индекса Git".to_owned())
            .and_then(|result| result.validate(git_index_target.as_ref()))
            .map_err(|message| ProductError {
                ui_request: None,
                providers_request: false,
                message,
                rejected_before_launch: false,
            })?;
    }
    if git_commit_prepare_request || response.workspace_git_commit_plan.is_some() {
        response
            .workspace_git_commit_plan
            .as_ref()
            .ok_or_else(|| "Session host не вернул план коммита Git".to_owned())
            .and_then(|plan| plan.validate(git_commit_target))
            .map_err(|message| ProductError {
                ui_request: None,
                providers_request: false,
                message,
                rejected_before_launch: false,
            })?;
    }
    if (git_commit_request && !git_commit_prepare_request)
        || response.workspace_git_commit.is_some()
    {
        response
            .workspace_git_commit
            .as_ref()
            .ok_or_else(|| "Session host не вернул квитанцию коммита Git".to_owned())
            .and_then(|result| result.validate())
            .and_then(|_| {
                let result = response.workspace_git_commit.as_ref().unwrap();
                if git_commit_target == Some((result.workspace_id, result.workspace_revision)) {
                    Ok(())
                } else {
                    Err("Квитанция коммита относится к другому воркспейсу".into())
                }
            })
            .map_err(|message| ProductError {
                ui_request: None,
                providers_request: false,
                message,
                rejected_before_launch: false,
            })?;
    }
    let process_view = if let Some((session, generation)) = process_target {
        if response.views.len() != 1
            || session.is_none()
            || generation.is_none()
            || response.views[0].session.parse::<uuid::Uuid>().ok() != session
            || response.views[0].generation.parse::<uuid::Uuid>().ok() != generation
        {
            return Err(ProductError {
                ui_request: None,
                providers_request: false,
                message: "Ответ остановки/проверки не соответствует выбранному поколению".into(),
                rejected_before_launch: false,
            });
        }
        Some(response.views.remove(0))
    } else {
        None
    };
    Ok(ProductReply {
        ui_request: None,
        providers_local,
        providers: response.providers,
        providers_request: false,
        generation_selection: response.generation_selection,
        generation_selected,
        generations: response.generations,
        context_saved,
        launch_preview: response.launch_preview,
        launch_phase: response.launch_phase,
        sessions: list_sessions.then_some(response.views),
        process_view,
        launch_confirmed,
        catalog: response.catalog,
        selection: response.selection,
        context: response.context,
        history_view: response.history_view,
        history_view_request,
        template_run: response.template_run,
        template_runs: response.template_runs,
        panel_directory: response.panel_directory,
        workspace_git_snapshot: response.workspace_git_snapshot,
        workspace_git_diff: response.workspace_git_diff,
        workspace_git_index: response.workspace_git_index,
        workspace_git_commit_plan: response.workspace_git_commit_plan,
        workspace_git_commit: response.workspace_git_commit,
    })
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RecoveryDiagnostics {
    pub issues: Vec<RecoveryIssue>,
    pub omitted: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct RecoveryIssue {
    pub journal: String,
    pub message: String,
}

fn validate_recovery(recovery: Option<&RecoveryDiagnostics>) -> Result<(), String> {
    let Some(recovery) = recovery else {
        return Ok(());
    };
    if recovery.issues.len() > MAX_RECOVERY_ISSUES {
        return Err("too many recovery diagnostics".into());
    }
    for issue in &recovery.issues {
        if issue.journal.is_empty()
            || issue.journal.len() > MAX_RECOVERY_TEXT_BYTES
            || issue.journal == "."
            || issue.journal == ".."
            || issue.journal.contains('/')
            || issue.journal.contains('\\')
            || issue.journal.chars().any(char::is_control)
        {
            return Err("invalid recovery journal basename".into());
        }
        if issue.message.len() > MAX_RECOVERY_TEXT_BYTES {
            return Err("recovery diagnostic message exceeds limit".into());
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandFailure {
    NotAdmitted { code: String, message: String },
    OutcomeUnknown(String),
}

impl std::fmt::Display for CommandFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAdmitted { message, .. } | Self::OutcomeUnknown(message) => {
                f.write_str(message)
            }
        }
    }
}

pub fn request(socket: &Path, command: Value) -> Result<Vec<View>, CommandFailure> {
    let response = request_response(socket, command).map_err(CommandFailure::OutcomeUnknown)?;
    command_result(response)
}

fn command_result(response: Response) -> Result<Vec<View>, CommandFailure> {
    if let Some(message) = response.error {
        return if matches!(
            response.rejected_before_admission.as_deref(),
            Some("queue_full" | "host_shutting_down")
        ) {
            Err(CommandFailure::NotAdmitted {
                code: response
                    .rejected_before_admission
                    .expect("known rejection code"),
                message,
            })
        } else {
            Err(CommandFailure::OutcomeUnknown(message))
        };
    }
    Ok(response.views)
}

fn request_response(socket: &Path, command: Value) -> Result<Response, String> {
    request_response_with_timeout(socket, command, Duration::from_secs(2))
}

fn request_response_with_timeout(
    socket: &Path,
    command: Value,
    timeout: Duration,
) -> Result<Response, String> {
    // Host and UI run as the same user. Reject group/world-accessible endpoints.
    let metadata = socket.metadata().map_err(|e| e.to_string())?;
    let own_uid = std::fs::metadata("/proc/self")
        .map_err(|e| e.to_string())?
        .uid();
    if metadata.uid() != own_uid || metadata.mode() & 0o077 != 0 {
        return Err("Host endpoint must be private (0700/0600)".into());
    }
    let mut stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    let payload =
        serde_json::to_vec(&json!({"protocol":2,"command":command})).map_err(|e| e.to_string())?;
    if payload.len() > LIMIT {
        return Err("request exceeds limit".into());
    }
    stream
        .write_all(&(payload.len() as u32).to_le_bytes())
        .and_then(|_| stream.write_all(&payload))
        .map_err(|e| e.to_string())?;
    let mut length = [0; 4];
    stream.read_exact(&mut length).map_err(|e| e.to_string())?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > LIMIT {
        return Err("invalid response length".into());
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).map_err(|e| e.to_string())?;
    let response: Response = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    if response.protocol != 2 {
        return Err("incompatible host protocol".into());
    }
    validate_recovery(response.recovery.as_ref())?;
    if response.error.is_some() {
        return Ok(response);
    }
    for view in &response.views {
        validate_palette(&view.frame.palette_overrides)?;
        let geometry = &view.frame.geometry;
        if geometry.rows == 0
            || geometry.cols == 0
            || geometry.rows as usize * geometry.cols as usize > 32768
            || view.frame.cells.len() != geometry.rows as usize * geometry.cols as usize
        {
            return Err("invalid cell frame geometry".into());
        }
        if view.frame.cells.iter().any(|c| {
            c.hyperlink
                .is_some_and(|i| i as usize >= view.frame.hyperlinks.len())
        }) {
            return Err("invalid hyperlink pool index".into());
        }
    }
    Ok(response)
}

pub struct Poll {
    pub selected: Vec<Known>,
    pub views: Vec<View>,
    pub recovery: Option<RecoveryDiagnostics>,
    pub activity: Option<crate::activity::Snapshot>,
    pub activity_error: Option<String>,
}

fn poll_response(reply: Response) -> Result<Poll, String> {
    if let Some(error) = reply.error {
        return Err(error);
    }
    validate_recovery(reply.recovery.as_ref())?;
    let activity = reply
        .activity
        .map(|value| -> Result<crate::activity::Snapshot, String> {
            let activity: crate::activity::Snapshot = serde_json::from_value(value)
                .map_err(|e| format!("Invalid activity metadata: {e}"))?;
            activity.validate()?;
            let token = reply
                .change_token
                .as_ref()
                .ok_or("Activity without host token")?;
            if token.host_instance.parse::<uuid::Uuid>().ok() != Some(activity.host_instance)
                || token.revision != activity.revision
            {
                return Err("Activity host token mismatch".into());
            }
            Ok(activity)
        });
    let (mut activity, mut activity_error) = match activity {
        None => (None, None),
        Some(Ok(snapshot)) => (Some(snapshot), None),
        Some(Err(error)) => (None, Some(error)),
    };
    if let Some(error) = reply.activity_error {
        activity = None;
        activity_error = Some(error.chars().take(256).collect());
    }
    Ok(Poll {
        selected: reply
            .selected
            .ok_or("Host does not support conditional cell polling")?,
        views: reply.views,
        recovery: reply.recovery,
        activity,
        activity_error,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum PollError {
    Busy,
    Failed(String),
}

fn decode_poll(reply: Response) -> Result<(Poll, Option<ChangeToken>), PollError> {
    if reply.error.is_some()
        && matches!(
            reply.rejected_before_admission.as_deref(),
            Some("queue_full" | "host_shutting_down")
        )
    {
        return Err(PollError::Busy);
    }
    let token = reply.change_token.clone();
    if let Some(token) = &token {
        token.validate().map_err(PollError::Failed)?;
    }
    Ok((poll_response(reply).map_err(PollError::Failed)?, token))
}

pub fn poll(socket: &Path, known: &[Known]) -> Result<(Poll, Option<ChangeToken>), PollError> {
    let reply = request_response(socket, json!({"command":"poll", "known":known}))
        .map_err(PollError::Failed)?;
    decode_poll(reply)
}

pub fn poll_visible(
    socket: &Path,
    known: &[Known],
    sessions: &[uuid::Uuid],
) -> Result<(Poll, Option<ChangeToken>), PollError> {
    let reply = request_response(
        socket,
        json!({"command":"poll_visible", "sessions":sessions, "known":known}),
    )
    .map_err(PollError::Failed)?;
    decode_panel_poll(
        reply,
        &sessions
            .iter()
            .map(|session| PanelTarget {
                session: *session,
                generation: None,
            })
            .collect::<Vec<_>>(),
    )
}

pub fn poll_panels(
    socket: &Path,
    known: &[Known],
    panels: &[PanelTarget],
) -> Result<(Poll, Option<ChangeToken>), PollError> {
    let reply = request_response(
        socket,
        json!({"command":"poll_panels","panels":panels,"known":known}),
    )
    .map_err(PollError::Failed)?;
    decode_panel_poll(reply, panels)
}

fn decode_panel_poll(
    reply: Response,
    panels: &[PanelTarget],
) -> Result<(Poll, Option<ChangeToken>), PollError> {
    let result = decode_poll(reply)?;
    let mut unique = std::collections::HashSet::new();
    for selected in &result.0.selected {
        if !unique.insert(&selected.session)
            || !panels.iter().any(|target| {
                target.session.to_string() == selected.session
                    && target
                        .generation
                        .is_none_or(|generation| generation.to_string() == selected.generation)
            })
        {
            return Err(PollError::Failed(
                "Host вернул другое или несколько поколений одной панели; кадр не применён".into(),
            ));
        }
    }
    Ok(result)
}

/// This read-only wait has no input/launch admission or retry semantics. An old
/// host omits the Poll token, so callers retain bounded legacy polling instead.
pub fn wait_for_change(socket: &Path, after: &ChangeToken) -> Result<ChangeToken, String> {
    after.validate()?;
    let reply = request_response(
        socket,
        json!({
            "command":"wait_for_change", "after":after, "timeout_ms":1000,
        }),
    )?;
    if let Some(error) = reply.error {
        return Err(error);
    }
    if !reply.views.is_empty() || reply.selected.is_some() || reply.recovery.is_some() {
        return Err("Unexpected terminal data in change wait reply".into());
    }
    let token = reply.change_token.ok_or("Host omitted change wait token")?;
    token.validate()?;
    Ok(token)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PollChanges {
    pub views: bool,
    pub recovery: bool,
}

/// Validate the whole replica update before moving any cached frame. Unchanged
/// cells stay in the cache; removals and new generations follow the host list.
pub fn merge_poll(
    current: &mut Vec<View>,
    recovery: &mut Option<RecoveryDiagnostics>,
    reply: Poll,
) -> Result<PollChanges, String> {
    if reply.selected.len() > 32 || reply.views.len() > 32 {
        return Err("Too many selected terminal frames".into());
    }
    validate_recovery(reply.recovery.as_ref())?;
    for view in &reply.views {
        validate_palette(&view.frame.palette_overrides)?;
    }
    let key = |known: &Known| (known.session.clone(), known.generation.clone());
    let mut selected = std::collections::HashSet::new();
    for info in &reply.selected {
        if !selected.insert(key(info)) {
            return Err("Duplicate selected terminal generation".into());
        }
        let changed = reply
            .views
            .iter()
            .filter(|v| key(&v.known()) == key(info))
            .collect::<Vec<_>>();
        if changed.len() > 1 {
            return Err("Duplicate changed terminal generation".into());
        }
        let previous = current.iter().find(|v| key(&v.known()) == key(info));
        let view = changed
            .first()
            .copied()
            .or(previous)
            .ok_or("Missing terminal replica")?;
        if view.known() != *info {
            return Err("Terminal metadata/frame watermark mismatch".into());
        }
        if previous.is_some_and(|old| old.sequence > view.sequence) {
            return Err("Host sequence regression".into());
        }
    }
    if reply
        .views
        .iter()
        .any(|v| !selected.contains(&key(&v.known())))
    {
        return Err("Unselected terminal frame".into());
    }
    let views_changed = !reply.views.is_empty()
        || current.len() != reply.selected.len()
        || current
            .iter()
            .zip(&reply.selected)
            .any(|(v, info)| v.known() != *info);
    let recovery_changed = recovery.as_ref() != reply.recovery.as_ref();
    let changes = PollChanges {
        views: views_changed,
        recovery: recovery_changed,
    };
    if changes.views {
        let mut old = std::mem::take(current);
        let mut updates = reply.views;
        for info in reply.selected {
            if let Some(index) = updates.iter().position(|v| key(&v.known()) == key(&info)) {
                current.push(updates.swap_remove(index));
            } else {
                let index = old
                    .iter()
                    .position(|v| key(&v.known()) == key(&info))
                    .expect("validated cached replica");
                current.push(old.swap_remove(index));
            }
        }
    }
    if changes.recovery {
        *recovery = reply.recovery;
    }
    Ok(changes)
}

pub fn command(view: &View, name: &str) -> Value {
    json!({"command":name,"session":view.session,"generation":view.generation})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_reply_requires_the_requested_workspace_and_revision() {
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        for variant in 0..4 {
            let workspace = uuid::Uuid::from_u128(301);
            let request_id = uuid::Uuid::new_v4();
            let path =
                std::env::temp_dir().join(format!("pigade-git-{}.sock", uuid::Uuid::new_v4()));
            let listener = UnixListener::bind(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut length = [0; 4];
                stream.read_exact(&mut length).unwrap();
                let mut payload = vec![0; u32::from_le_bytes(length) as usize];
                stream.read_exact(&mut payload).unwrap();
                let command: Value = serde_json::from_slice(&payload).unwrap();
                assert_eq!(command["command"]["command"], "workspace_git_snapshot");
                assert_eq!(command["command"]["workspace"], workspace.to_string());
                assert!(command["command"].get("_ui_request").is_none());
                let snapshot = json!({"workspace_id":if variant==1 {uuid::Uuid::new_v4()} else {workspace},
                    "workspace_revision":if variant==2 {5} else {4},"project_id":uuid::Uuid::from_u128(302),
                    "cwd":"/worktree","branch":"main","head":null,"detached":false,"entries":[],"truncated":true});
                let response = json!({"protocol":2,"views":[],"error":null,"workspace_git_snapshot":if variant==3 {Value::Null} else {snapshot}});
                let bytes = serde_json::to_vec(&response).unwrap();
                stream
                    .write_all(&(bytes.len() as u32).to_le_bytes())
                    .unwrap();
                stream.write_all(&bytes).unwrap();
            });
            let result = product_request(
                &path,
                json!({"command":"workspace_git_snapshot","workspace":workspace,"expected_revision":4,"_ui_request":request_id}),
            );
            server.join().unwrap();
            std::fs::remove_file(path).unwrap();
            if variant == 0 {
                let reply = result.unwrap_or_else(|error| panic!("{}", error.message));
                assert_eq!(reply.ui_request, Some(request_id));
                assert!(reply.workspace_git_snapshot.unwrap().truncated);
                assert!(
                    reply.catalog.is_none() && reply.sessions.is_none() && !reply.launch_confirmed
                );
            } else {
                let error = match result {
                    Err(error) => error,
                    Ok(_) => panic!("accepted mismatching Git response"),
                };
                assert_eq!(error.ui_request, Some(request_id));
                assert!(!error.rejected_before_launch);
            }
        }
    }

    #[test]
    fn git_diff_reply_is_bound_to_the_exact_request_and_bounded() {
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        for variant in 0..12 {
            let workspace = uuid::Uuid::from_u128(401);
            let request_id = uuid::Uuid::new_v4();
            let path =
                std::env::temp_dir().join(format!("pigade-git-diff-{}.sock", uuid::Uuid::new_v4()));
            let listener = UnixListener::bind(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut length = [0; 4];
                stream.read_exact(&mut length).unwrap();
                let mut payload = vec![0; u32::from_le_bytes(length) as usize];
                stream.read_exact(&mut payload).unwrap();
                let command: Value = serde_json::from_slice(&payload).unwrap();
                assert_eq!(command["command"]["command"], "workspace_git_diff");
                assert_eq!(command["command"]["workspace"], workspace.to_string());
                assert_eq!(command["command"]["expected_revision"], 7);
                assert_eq!(command["command"]["path"], "src/main.rs");
                assert_eq!(command["command"]["side"], "worktree");
                assert!(command["command"].get("_ui_request").is_none());
                let mut diff = json!({
                    "workspace_id": workspace,
                    "workspace_revision": 7,
                    "project_id": uuid::Uuid::from_u128(402),
                    "cwd": "/worktree",
                    "path": "src/main.rs",
                    "side": "worktree",
                    "text": "",
                    "truncated": false,
                    "binary": false
                });
                match variant {
                    1 => diff["workspace_id"] = json!(uuid::Uuid::from_u128(499)),
                    2 => diff["workspace_revision"] = json!(8),
                    3 => diff["path"] = json!("src/lib.rs"),
                    4 => diff["side"] = json!("index"),
                    5 => diff["project_id"] = json!(uuid::Uuid::nil()),
                    6 => diff["cwd"] = json!("relative/worktree"),
                    7 => diff["text"] = json!("x".repeat(128 * 1024 + 1)),
                    8 => diff["text"] = json!("x\n".repeat(2001)),
                    9 => {
                        diff["binary"] = json!(true);
                        diff["text"] = json!("binary diff must not carry text");
                    }
                    10 => diff["side"] = json!("unrecognized"),
                    11 => diff = Value::Null,
                    _ => {}
                }
                let response = json!({
                    "protocol": 2,
                    "views": [],
                    "error": null,
                    "workspace_git_diff": diff
                });
                let bytes = serde_json::to_vec(&response).unwrap();
                stream
                    .write_all(&(bytes.len() as u32).to_le_bytes())
                    .unwrap();
                stream.write_all(&bytes).unwrap();
            });
            let result = product_request(
                &path,
                json!({"command":"workspace_git_diff","workspace":workspace,
                    "expected_revision":7,"path":"src/main.rs","side":"worktree",
                    "_ui_request":request_id}),
            );
            server.join().unwrap();
            std::fs::remove_file(path).unwrap();
            if variant == 0 {
                let reply = result.unwrap_or_else(|error| panic!("{}", error.message));
                assert_eq!(reply.ui_request, Some(request_id));
                let diff = reply.workspace_git_diff.unwrap();
                assert_eq!(diff.side, GitDiffSide::Worktree);
                assert_eq!(diff.path, "src/main.rs");
                assert!(!diff.binary);
                assert!(diff.text.is_empty());
            } else {
                let error = match result {
                    Err(error) => error,
                    Ok(_) => panic!("accepted malformed or mismatched Git diff"),
                };
                assert_eq!(error.ui_request, Some(request_id));
            }
        }
    }

    #[test]
    fn git_diff_request_rejects_unsafe_path_before_socket_access() {
        let request_id = uuid::Uuid::new_v4();
        let result = product_request(
            Path::new("/no/such/pigade.sock"),
            json!({"command":"workspace_git_diff","workspace":uuid::Uuid::from_u128(401),
                "expected_revision":7,"path":"../outside","side":"worktree",
                "_ui_request":request_id}),
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("accepted unsafe Git path"),
        };
        assert_eq!(error.ui_request, Some(request_id));
    }

    #[test]
    fn git_diff_path_validator_rejects_ambiguous_or_unsafe_paths() {
        for path in [
            "",
            "-option",
            "/absolute/file",
            "\\\\server\\share\\file",
            "C:\\work\\file",
            "src/../outside",
            "src/./main.rs",
            "src//main.rs",
            "src/line\nfeed",
        ] {
            assert!(validate_git_diff_path(path).is_err(), "accepted {path:?}");
        }
        assert!(validate_git_diff_path(&"a".repeat(4097)).is_err());
        assert!(validate_git_diff_path("src/main.rs").is_ok());
    }

    #[test]
    fn git_index_receipt_is_bound_to_ticket_workspace_and_action() {
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        for variant in 0..9 {
            let workspace = uuid::Uuid::from_u128(501);
            let ticket = uuid::Uuid::from_u128(502);
            let request_id = uuid::Uuid::new_v4();
            let path = std::env::temp_dir()
                .join(format!("pigade-git-index-{}.sock", uuid::Uuid::new_v4()));
            let listener = UnixListener::bind(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut length = [0; 4];
                stream.read_exact(&mut length).unwrap();
                let mut payload = vec![0; u32::from_le_bytes(length) as usize];
                stream.read_exact(&mut payload).unwrap();
                let command: Value = serde_json::from_slice(&payload).unwrap();
                assert_eq!(command["command"]["command"], "workspace_git_index");
                assert_eq!(command["command"]["workspace"], workspace.to_string());
                assert_eq!(command["command"]["expected_revision"], 6);
                assert_eq!(command["command"]["ticket"], ticket.to_string());
                assert_eq!(command["command"]["inspect"], true);
                assert!(command["command"].get("_ui_request").is_none());
                let mut result = json!({
                    "ticket": ticket,
                    "workspace_id": workspace,
                    "workspace_revision": 6,
                    "project_id": uuid::Uuid::from_u128(503),
                    "cwd": "/worktree",
                    "path": "src/main.rs",
                    "side": "worktree",
                    "action": "stage",
                    "phase": "applied",
                    "message": null
                });
                match variant {
                    1 => result["ticket"] = json!(uuid::Uuid::from_u128(504)),
                    2 => result["workspace_id"] = json!(uuid::Uuid::from_u128(505)),
                    3 => result["workspace_revision"] = json!(7),
                    4 => result["action"] = json!("unstage"),
                    5 => result["side"] = json!("conflict"),
                    6 => result["path"] = json!("../outside"),
                    7 => result["message"] = json!("x".repeat(1025)),
                    8 => result["phase"] = json!("mystery"),
                    _ => {}
                }
                let response = json!({
                    "protocol": 2, "views": [], "error": null,
                    "workspace_git_index": result
                });
                let bytes = serde_json::to_vec(&response).unwrap();
                stream
                    .write_all(&(bytes.len() as u32).to_le_bytes())
                    .unwrap();
                stream.write_all(&bytes).unwrap();
            });
            let result = product_request(
                &path,
                json!({"command":"workspace_git_index","workspace":workspace,
                    "expected_revision":6,"ticket":ticket,"inspect":true,
                    "_ui_request":request_id}),
            );
            server.join().unwrap();
            std::fs::remove_file(path).unwrap();
            if variant == 0 {
                let reply = result.unwrap_or_else(|error| panic!("{}", error.message));
                assert_eq!(reply.ui_request, Some(request_id));
                let receipt = reply.workspace_git_index.unwrap();
                assert_eq!(receipt.ticket, ticket);
                assert_eq!(receipt.phase, GitIndexPhase::Applied);
                assert_eq!(receipt.action, GitIndexAction::Stage);
            } else {
                let error = match result {
                    Err(error) => error,
                    Ok(_) => panic!("accepted malformed or mismatched Git index receipt"),
                };
                assert_eq!(error.ui_request, Some(request_id));
            }
        }
    }

    #[test]
    fn git_index_request_and_response_require_a_receipt_and_valid_inspect_flag() {
        let request_id = uuid::Uuid::new_v4();
        let invalid = product_request(
            Path::new("/no/such/pigade.sock"),
            json!({"command":"workspace_git_index","workspace":uuid::Uuid::from_u128(501),
                "expected_revision":6,"ticket":uuid::Uuid::from_u128(502),"inspect":"yes",
                "_ui_request":request_id}),
        );
        let invalid = match invalid {
            Err(error) => error,
            Ok(_) => panic!("accepted malformed inspect flag"),
        };
        assert_eq!(invalid.ui_request, Some(request_id));

        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        let path = std::env::temp_dir().join(format!(
            "pigade-git-index-missing-{}.sock",
            uuid::Uuid::new_v4()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut length = [0; 4];
            stream.read_exact(&mut length).unwrap();
            let mut payload = vec![0; u32::from_le_bytes(length) as usize];
            stream.read_exact(&mut payload).unwrap();
            let response = json!({"protocol":2,"views":[],"error":null});
            let bytes = serde_json::to_vec(&response).unwrap();
            stream
                .write_all(&(bytes.len() as u32).to_le_bytes())
                .unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let result = product_request(
            &path,
            json!({"command":"workspace_git_index","workspace":uuid::Uuid::from_u128(501),
                "expected_revision":6,"ticket":uuid::Uuid::from_u128(502),"inspect":false,
                "_ui_request":request_id}),
        );
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("accepted missing Git index receipt"),
        };
        assert_eq!(error.ui_request, Some(request_id));
    }

    #[test]
    fn diff_index_controls_are_optional_but_side_bound_and_mutually_exclusive() {
        let workspace = uuid::Uuid::from_u128(601);
        let request = WorkspaceGitDiffTarget {
            workspace_id: workspace,
            workspace_revision: 2,
            path: "src/main.rs".into(),
            side: GitDiffSide::Worktree,
        };
        let mut diff: WorkspaceGitDiff = serde_json::from_value(json!({
            "workspace_id": workspace,
            "workspace_revision": 2,
            "project_id": uuid::Uuid::from_u128(602),
            "cwd": "/worktree",
            "path": "src/main.rs",
            "side": "worktree",
            "text": "",
            "truncated": false,
            "binary": false
        }))
        .unwrap();
        assert!(diff.validate(Some(&request)).is_ok()); // Legacy host: no index controls.

        diff.index_ticket = Some(GitIndexTicket {
            id: uuid::Uuid::from_u128(603),
            action: GitIndexAction::Stage,
        });
        assert!(diff.validate(Some(&request)).is_ok());
        diff.index_unavailable_reason = Some("unsupported host".into());
        assert!(diff.validate(Some(&request)).is_err());
        diff.index_unavailable_reason = None;
        diff.index_ticket.as_mut().unwrap().action = GitIndexAction::Unstage;
        assert!(diff.validate(Some(&request)).is_err());
        diff.index_ticket.as_mut().unwrap().action = GitIndexAction::Stage;
        diff.index_unavailable_reason = Some("x".repeat(1025));
        assert!(diff.validate(Some(&request)).is_err());
    }

    #[test]
    fn pending_index_receipt_is_snapshot_bound_and_legacy_snapshot_is_supported() {
        let workspace = uuid::Uuid::from_u128(701);
        let project = uuid::Uuid::from_u128(702);
        let base = json!({
            "workspace_id": workspace,
            "workspace_revision": 9,
            "project_id": project,
            "cwd": "/worktree",
            "branch": "main",
            "head": null,
            "detached": false,
            "entries": [],
            "truncated": false
        });
        let legacy: WorkspaceGitSnapshot = serde_json::from_value(base.clone()).unwrap();
        assert!(legacy.validate(Some((workspace, 9))).is_ok());

        let operation = json!({
            "ticket": uuid::Uuid::from_u128(703),
            "workspace_id": workspace,
            "workspace_revision": 8,
            "project_id": project,
            "cwd": "/worktree",
            "path": "src/main.rs",
            "side": "worktree",
            "action": "stage",
            "phase": "unknown",
            "message": null
        });
        let mut pending = base.clone();
        pending["index_operation"] = operation.clone();
        let snapshot: WorkspaceGitSnapshot = serde_json::from_value(pending.clone()).unwrap();
        assert!(snapshot.validate(Some((workspace, 9))).is_ok());

        for (field, value) in [
            ("workspace_id", json!(uuid::Uuid::from_u128(704))),
            ("project_id", json!(uuid::Uuid::from_u128(705))),
            ("cwd", json!("/renamed")),
            ("workspace_revision", json!(10)),
            ("phase", json!("applied")),
            ("acknowledged", json!(true)),
            ("action", json!("unstage")),
        ] {
            let mut malformed = pending.clone();
            malformed["index_operation"][field] = value;
            let snapshot: WorkspaceGitSnapshot = serde_json::from_value(malformed).unwrap();
            assert!(
                snapshot.validate(Some((workspace, 9))).is_err(),
                "accepted {field}"
            );
        }
    }

    #[test]
    fn acknowledge_reply_must_record_ack_or_a_terminal_blocker() {
        let target = WorkspaceGitIndexTarget {
            workspace_id: uuid::Uuid::from_u128(711),
            workspace_revision: 4,
            ticket: uuid::Uuid::from_u128(712),
            acknowledge: true,
        };
        let base = json!({
            "ticket": target.ticket,
            "workspace_id": target.workspace_id,
            "workspace_revision": target.workspace_revision,
            "project_id": uuid::Uuid::from_u128(713),
            "cwd": "/worktree",
            "path": "src/main.rs",
            "side": "worktree",
            "action": "stage",
            "phase": "unknown",
            "message": null
        });
        let mut acknowledged = base.clone();
        acknowledged["acknowledged"] = json!(true);
        let result: WorkspaceGitIndexResult = serde_json::from_value(acknowledged).unwrap();
        assert!(result.validate(Some(&target)).is_ok());

        let rejected: WorkspaceGitIndexResult = serde_json::from_value({
            let mut value = base.clone();
            value["phase"] = json!("rejected");
            value["message"] = json!("receipt could not be cleared");
            value
        })
        .unwrap();
        assert!(rejected.validate(Some(&target)).is_ok());

        let empty_rejection: WorkspaceGitIndexResult = serde_json::from_value({
            let mut value = base.clone();
            value["phase"] = json!("rejected");
            value["message"] = json!("");
            value
        })
        .unwrap();
        assert!(empty_rejection.validate(Some(&target)).is_err());

        let unknown: WorkspaceGitIndexResult = serde_json::from_value(base).unwrap();
        assert!(unknown.validate(Some(&target)).is_err());

        let extra_field = json!({
            "ticket": target.ticket,
            "workspace_id": target.workspace_id,
            "workspace_revision": target.workspace_revision,
            "project_id": uuid::Uuid::from_u128(713),
            "cwd": "/worktree",
            "path": "src/main.rs",
            "side": "worktree",
            "action": "stage",
            "phase": "unknown",
            "message": null,
            "unexpected": true
        });
        assert!(serde_json::from_value::<WorkspaceGitIndexResult>(extra_field.clone()).is_err());
    }

    #[test]
    fn acknowledge_request_omits_inspect_while_index_lookup_requires_it() {
        let workspace = uuid::Uuid::from_u128(721);
        let ticket = uuid::Uuid::from_u128(722);
        let acknowledge = json!({"command":"workspace_git_index_acknowledge",
            "workspace":workspace,"expected_revision":4,"ticket":ticket});
        assert!(
            workspace_git_index_target(&acknowledge)
                .unwrap()
                .unwrap()
                .acknowledge
        );
        let lookup = json!({"command":"workspace_git_index",
            "workspace":workspace,"expected_revision":4,"ticket":ticket});
        assert!(workspace_git_index_target(&lookup).is_err());
    }

    #[test]
    fn unsolicited_git_diff_response_is_rejected() {
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        let path = std::env::temp_dir().join(format!(
            "pigade-git-unsolicited-{}.sock",
            uuid::Uuid::new_v4()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut length = [0; 4];
            stream.read_exact(&mut length).unwrap();
            let mut payload = vec![0; u32::from_le_bytes(length) as usize];
            stream.read_exact(&mut payload).unwrap();
            let response = json!({
                "protocol": 2,
                "views": [],
                "error": null,
                "workspace_git_diff": {
                    "workspace_id": uuid::Uuid::from_u128(401),
                    "workspace_revision": 7,
                    "project_id": uuid::Uuid::from_u128(402),
                    "cwd": "/worktree",
                    "path": "src/main.rs",
                    "side": "worktree",
                    "text": "",
                    "truncated": false,
                    "binary": false
                }
            });
            let bytes = serde_json::to_vec(&response).unwrap();
            stream
                .write_all(&(bytes.len() as u32).to_le_bytes())
                .unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let result = product_request(&path, json!({"command":"list"}));
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn stop_receipt_is_bound_to_the_frozen_generation_and_does_not_open_sessions() {
        use std::os::unix::{fs::PermissionsExt, net::UnixListener};
        for correct_generation in [true, false] {
            let session = uuid::Uuid::from_u128(101);
            let generation = uuid::Uuid::from_u128(102);
            let path =
                std::env::temp_dir().join(format!("pigade-stop-{}.sock", uuid::Uuid::new_v4()));
            let listener = UnixListener::bind(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut length = [0; 4];
                stream.read_exact(&mut length).unwrap();
                let mut payload = vec![0; u32::from_le_bytes(length) as usize];
                stream.read_exact(&mut payload).unwrap();
                let request: Value = serde_json::from_slice(&payload).unwrap();
                assert_eq!(request["command"]["command"], "stop");
                assert_eq!(request["command"]["generation"], generation.to_string());
                let reply = json!({"protocol":2,"error":null,"views":[{
                    "session":session,"generation":if correct_generation {generation} else {uuid::Uuid::from_u128(103)},
                    "sequence":1,"state":"running","recovery_path":"live","pid":123,"storage_error":null,
                    "frame":{"geometry":{"rows":1,"cols":1},"cells":[{"text":" ","fg":"Named(Foreground)","bg":"Named(Background)","flags":0,"underline":null,"hyperlink":null}],
                        "hyperlinks":[],"cursor":[0,0],"cursor_style":"Block","modes":0,"colors":"","history_lines":0,"sync_pending_bytes":0,"title":"Review"}
                }]});
                let bytes = serde_json::to_vec(&reply).unwrap();
                stream
                    .write_all(&(bytes.len() as u32).to_le_bytes())
                    .unwrap();
                stream.write_all(&bytes).unwrap();
            });
            let request_id = uuid::Uuid::new_v4();
            let reply = product_request(
                &path,
                json!({"command":"stop","session":session,"generation":generation,"_ui_request":request_id}),
            );
            server.join().unwrap();
            std::fs::remove_file(path).unwrap();
            if correct_generation {
                let reply = reply.unwrap_or_else(|error| panic!("{}", error.message));
                assert_eq!(reply.ui_request, Some(request_id));
                assert!(reply.sessions.is_none());
                assert!(!reply.launch_confirmed);
                assert_eq!(reply.process_view.unwrap().state, "running");
            } else {
                let error = match reply {
                    Err(error) => error,
                    Ok(_) => panic!("accepted another generation"),
                };
                assert_eq!(error.ui_request, Some(request_id));
                assert!(error.message.contains("поколению"));
            }
        }
    }

    fn error_response(error: &str, rejection: Option<&str>) -> Response {
        Response {
            activity: None,
            activity_error: None,
            protocol: 2,
            catalog: None,
            selection: None,
            context: None,
            launch_preview: None,
            launch_phase: None,
            template_run: None,
            template_runs: None,
            panel_directory: None,
            workspace_git_snapshot: None,
            workspace_git_diff: None,
            workspace_git_index: None,
            workspace_git_commit_plan: None,
            workspace_git_commit: None,
            generations: None,
            generation_selection: None,
            history_view: None,
            providers: None,
            views: Vec::new(),
            error: Some(error.into()),
            rejected_before_admission: rejection.map(str::to_owned),
            selected: None,
            recovery: None,
            change_token: None,
        }
    }

    fn diagnostics(journal: &str, message: &str) -> RecoveryDiagnostics {
        RecoveryDiagnostics {
            issues: vec![RecoveryIssue {
                journal: journal.into(),
                message: message.into(),
            }],
            omitted: 0,
        }
    }

    #[test]
    fn palette_wire_is_backward_compatible_and_bounded_before_replica_merge() {
        #[derive(serde::Deserialize)]
        struct Palette {
            #[serde(default, deserialize_with = "super::deserialize_palette_overrides")]
            values: Vec<(u16, [u8; 3])>,
        }
        assert!(
            serde_json::from_str::<Palette>("{}")
                .unwrap()
                .values
                .is_empty()
        );
        let palette =
            serde_json::from_str::<Palette>(r#"{"values":[[0,[1,2,3]],[268,[253,254,255]]]}"#)
                .unwrap();
        assert_eq!(palette.values, vec![(0, [1, 2, 3]), (268, [253, 254, 255])]);
        for invalid in [
            r#"{"values":[[269,[1,2,3]]]}"#,
            r#"{"values":[[0,[256,0,0]]]}"#,
            r#"{"values":[[3,[1,2,3]],[2,[4,5,6]]]}"#,
            r#"{"values":[[3,[1,2,3]],[3,[4,5,6]]]}"#,
        ] {
            assert!(serde_json::from_str::<Palette>(invalid).is_err());
        }
        let too_many =
            serde_json::json!({"values": (0..270).map(|i| (i,[0,0,0])).collect::<Vec<_>>()});
        assert!(serde_json::from_value::<Palette>(too_many).is_err());
    }

    #[test]
    fn explicit_queue_full_marker_is_typed_as_not_admitted() {
        assert_eq!(
            command_result(error_response("queue full", Some("queue_full"))),
            Err(CommandFailure::NotAdmitted {
                code: "queue_full".into(),
                message: "queue full".into(),
            })
        );
    }

    #[test]
    fn orderly_host_shutdown_is_known_not_applied() {
        assert_eq!(
            command_result(error_response(
                "host is shutting down",
                Some("host_shutting_down")
            )),
            Err(CommandFailure::NotAdmitted {
                code: "host_shutting_down".into(),
                message: "host is shutting down".into()
            })
        );
        assert!(matches!(
            decode_poll(error_response(
                "host is shutting down",
                Some("host_shutting_down")
            )),
            Err(PollError::Busy)
        ));
    }

    #[test]
    fn unmarked_and_unknown_rejections_remain_outcome_unknown() {
        for rejection in [None, Some("future_code")] {
            assert_eq!(
                command_result(error_response("request may have been applied", rejection)),
                Err(CommandFailure::OutcomeUnknown(
                    "request may have been applied".into()
                ))
            );
        }
    }

    fn fixture() -> View {
        let value = json!({"session":"s","generation":"g","sequence":1,"state":"running","recovery_path":"live","pid":1,"storage_error":null,"frame":{"geometry":{"rows":1,"cols":1},"cells":[{"text":"界","fg":"Named(Foreground)","bg":"Named(Background)","flags":32,"underline":null,"hyperlink":0}],"hyperlinks":[["id","https://example.org"]],"cursor":[0,0],"cursor_style":"Block","modes":0,"colors":"","history_lines":0,"sync_pending_bytes":0,"title":""}});
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn request_is_generation_scoped() {
        let view = fixture();
        assert_eq!(
            command(&view, "snapshot"),
            json!({"command":"snapshot","session":"s","generation":"g"})
        );
        assert_eq!(view.frame.cells[0].text, "界");
    }

    #[test]
    fn panel_reply_never_substitutes_another_generation_or_two_models() {
        let session = uuid::Uuid::from_u128(10);
        let old = uuid::Uuid::from_u128(99);
        let new = uuid::Uuid::from_u128(1);
        let mut frame = fixture();
        frame.session = session.to_string();
        frame.generation = old.to_string();
        let target = PanelTarget {
            session,
            generation: Some(old),
        };
        let reply = |views: Vec<View>| {
            let mut reply = error_response("", None);
            reply.error = None;
            reply.selected = Some(views.iter().map(View::known).collect());
            reply.views = views;
            reply
        };
        assert!(
            decode_panel_poll(reply(vec![frame.clone()]), std::slice::from_ref(&target)).is_ok()
        );
        let mut wrong = frame.clone();
        wrong.generation = new.to_string();
        wrong.sequence = 0;
        assert!(
            decode_panel_poll(reply(vec![wrong.clone()]), std::slice::from_ref(&target)).is_err()
        );
        assert!(
            decode_panel_poll(
                reply(vec![frame, wrong]),
                &[PanelTarget {
                    session,
                    generation: None
                }]
            )
            .is_err()
        );
        assert!(decode_panel_poll(reply(vec![]), &[target]).is_ok());
    }
    #[test]
    fn unchanged_poll_reuses_the_existing_cell_allocation() {
        let mut cached = vec![fixture()];
        let mut recovery = None;
        let cells = cached[0].frame.cells.as_ptr();
        let selected = vec![cached[0].known()];
        assert_eq!(
            merge_poll(
                &mut cached,
                &mut recovery,
                Poll {
                    activity: None,
                    activity_error: None,
                    selected,
                    views: vec![],
                    recovery: None,
                }
            )
            .unwrap(),
            PollChanges::default()
        );
        assert_eq!(cells, cached[0].frame.cells.as_ptr());
    }

    #[test]
    fn older_host_response_without_recovery_field_is_compatible() {
        let response: Response = serde_json::from_value(json!({
            "protocol": 2,
            "views": [],
            "error": null
        }))
        .unwrap();
        assert_eq!(response.recovery, None);
    }

    #[test]
    fn poll_response_preserves_recovery_from_an_empty_host_list() {
        let response: Response = serde_json::from_value(json!({
            "protocol": 2,
            "views": [],
            "error": null,
            "selected": [],
            "recovery": {
                "issues": [{"journal":"bad.journal", "message":"unreadable index"}],
                "omitted": 2
            }
        }))
        .unwrap();
        let poll = poll_response(response).unwrap();
        assert!(poll.views.is_empty());
        assert!(poll.selected.is_empty());
        assert_eq!(poll.recovery.unwrap().omitted, 2);
    }

    #[test]
    fn invalid_optional_activity_does_not_fail_terminal_polling() {
        let host = uuid::Uuid::new_v4();
        for activity in [
            json!({"schema": 99}),
            json!({"schema":1,"host_instance":host,"revision":4,"sessions":[]}),
        ] {
            let mut response: Response = serde_json::from_value(json!({
                "protocol":2,"views":[],"selected":[],"error":null,
                "change_token":{"host_instance":host,"revision":3},"activity":activity
            }))
            .unwrap();
            let frame = fixture();
            response.selected = Some(vec![frame.known()]);
            response.views = vec![frame.clone()];
            let poll = poll_response(response).unwrap();
            assert!(poll.activity.is_none());
            assert!(poll.activity_error.is_some());
            let mut replica = Vec::new();
            let changes = merge_poll(&mut replica, &mut None, poll).unwrap();
            assert!(changes.views);
            assert_eq!(
                replica,
                vec![frame],
                "optional metadata must not withhold authoritative cells"
            );
        }
        let response: Response =
            serde_json::from_value(json!({"protocol":2,"views":[],"selected":[],"error":null}))
                .unwrap();
        let poll = poll_response(response).unwrap();
        assert!(poll.activity.is_none());
        assert!(poll.activity_error.is_none());
        let response: Response = serde_json::from_value(json!({"protocol":2,"views":[],"selected":[],"error":null,"activity_error":"summary unavailable"})).unwrap();
        let poll = poll_response(response).unwrap();
        assert!(poll.activity.is_none());
        assert_eq!(poll.activity_error.as_deref(), Some("summary unavailable"));
    }

    #[test]
    fn recovery_diagnostics_are_carried_even_when_there_are_no_views() {
        let recovery = diagnostics("session-7.journal", "checkpoint could not be read");
        let poll = Poll {
            activity: None,
            activity_error: None,
            selected: vec![],
            views: vec![],
            recovery: Some(recovery.clone()),
        };
        let mut cached = Vec::new();
        let mut current_recovery = None;
        assert_eq!(
            merge_poll(&mut cached, &mut current_recovery, poll).unwrap(),
            PollChanges {
                views: false,
                recovery: true,
            }
        );
        assert!(cached.is_empty());
        assert_eq!(current_recovery, Some(recovery));
    }

    #[test]
    fn recovery_diagnostics_reject_excess_count_and_utf8_byte_lengths() {
        let too_many = RecoveryDiagnostics {
            issues: (0..=MAX_RECOVERY_ISSUES)
                .map(|index| RecoveryIssue {
                    journal: format!("{index}.journal"),
                    message: "bad journal".into(),
                })
                .collect(),
            omitted: 0,
        };
        assert!(validate_recovery(Some(&too_many)).is_err());

        let long_name = diagnostics(&"a".repeat(MAX_RECOVERY_TEXT_BYTES + 1), "bad journal");
        assert!(validate_recovery(Some(&long_name)).is_err());

        let long_message = diagnostics("session.journal", &"漢".repeat(342));
        assert!(long_message.issues[0].message.len() > MAX_RECOVERY_TEXT_BYTES);
        assert!(validate_recovery(Some(&long_message)).is_err());

        for basename in ["../session.journal", "folder\\session.journal", ".."] {
            assert!(validate_recovery(Some(&diagnostics(basename, "bad journal"))).is_err());
        }
        let boundary = diagnostics(
            &"a".repeat(MAX_RECOVERY_TEXT_BYTES),
            &"m".repeat(MAX_RECOVERY_TEXT_BYTES),
        );
        assert!(validate_recovery(Some(&boundary)).is_ok());
    }

    #[test]
    fn invalid_poll_preserves_cached_views_and_previous_diagnostics() {
        let expected = vec![fixture()];
        let mut recovery = Some(diagnostics("old.journal", "old warning"));
        let previous_recovery = recovery.clone();
        let mut regression = fixture();
        regression.sequence = 0;
        let invalid = Poll {
            activity: None,
            activity_error: None,
            selected: vec![regression.known()],
            views: vec![regression],
            recovery: Some(diagnostics("new.journal", "new warning")),
        };
        let mut cached = expected.clone();
        assert!(merge_poll(&mut cached, &mut recovery, invalid).is_err());
        assert_eq!(cached, expected);
        assert_eq!(recovery, previous_recovery);

        let invalid_diagnostics = Poll {
            activity: None,
            activity_error: None,
            selected: vec![cached[0].known()],
            views: vec![],
            recovery: Some(diagnostics("bad.journal", &"x".repeat(1025))),
        };
        assert!(merge_poll(&mut cached, &mut recovery, invalid_diagnostics).is_err());
        assert_eq!(cached, expected);
        assert_eq!(recovery, previous_recovery);
    }
    #[test]
    fn invalid_palette_cannot_replace_cached_frame_or_diagnostics() {
        let original = vec![fixture()];
        let previous_recovery = Some(diagnostics("old.journal", "old warning"));
        for overrides in [
            vec![(269, [0, 0, 0])],
            vec![(2, [1, 2, 3]), (2, [4, 5, 6])],
            vec![(3, [1, 2, 3]), (0, [4, 5, 6])],
        ] {
            let mut next = fixture();
            next.sequence += 1;
            next.frame.palette_overrides = overrides;
            let mut cached = original.clone();
            let mut recovery = previous_recovery.clone();
            assert!(
                merge_poll(
                    &mut cached,
                    &mut recovery,
                    Poll {
                        activity: None,
                        activity_error: None,
                        selected: vec![next.known()],
                        views: vec![next],
                        recovery: None
                    }
                )
                .is_err()
            );
            assert_eq!(cached, original);
            assert_eq!(recovery, previous_recovery);
        }
    }

    #[test]
    fn lifecycle_changes_without_output_replace_the_replica() {
        let mut cached = vec![fixture()];
        let mut recovery = None;
        let mut next = cached[0].clone();
        next.state = "degraded".into();
        next.storage_error = Some("disk full".into());
        assert!(
            merge_poll(
                &mut cached,
                &mut recovery,
                Poll {
                    activity: None,
                    activity_error: None,
                    selected: vec![next.known()],
                    views: vec![next.clone()],
                    recovery: None,
                }
            )
            .unwrap()
            .views
        );
        assert_eq!(cached, vec![next]);
    }
    #[test]
    fn new_generation_can_start_at_a_lower_sequence() {
        let mut cached = vec![fixture()];
        let mut recovery = None;
        let mut next = fixture();
        next.generation = "new".into();
        next.sequence = 0;
        assert!(
            merge_poll(
                &mut cached,
                &mut recovery,
                Poll {
                    activity: None,
                    activity_error: None,
                    selected: vec![next.known()],
                    views: vec![next.clone()],
                    recovery: None,
                }
            )
            .unwrap()
            .views
        );
        assert_eq!(cached, vec![next]);
        assert!(
            merge_poll(
                &mut cached,
                &mut recovery,
                Poll {
                    activity: None,
                    activity_error: None,
                    selected: vec![],
                    views: vec![],
                    recovery: None,
                }
            )
            .unwrap()
            .views
        );
        assert!(cached.is_empty());
    }
    #[test]
    fn invalid_poll_is_atomic_and_keeps_the_last_good_replica() {
        let expected = vec![fixture()];
        let mut regression = fixture();
        regression.sequence = 0;
        let invalid = vec![
            Poll {
                activity: None,
                activity_error: None,
                selected: vec![regression.known()],
                views: vec![regression],
                recovery: None,
            },
            Poll {
                activity: None,
                activity_error: None,
                selected: vec![fixture().known(), fixture().known()],
                views: vec![],
                recovery: None,
            },
            Poll {
                activity: None,
                activity_error: None,
                selected: vec![],
                views: vec![fixture()],
                recovery: None,
            },
            Poll {
                activity: None,
                activity_error: None,
                selected: vec![fixture().known()],
                views: vec![fixture(), fixture()],
                recovery: None,
            },
            Poll {
                activity: None,
                activity_error: None,
                selected: vec![Known {
                    sequence: 2,
                    ..fixture().known()
                }],
                views: vec![],
                recovery: None,
            },
        ];
        for reply in invalid {
            let mut cached = expected.clone();
            let mut recovery = None;
            assert!(merge_poll(&mut cached, &mut recovery, reply).is_err());
            assert_eq!(cached, expected);
            assert_eq!(recovery, None);
        }
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;
    #[test]
    fn readonly_poll_rejection_keeps_safe_retry_classification() {
        let reply: Response = serde_json::from_value(json!({"protocol":2,"views":[],"error":"busy","rejected_before_admission":"queue_full"})).unwrap();
        assert!(matches!(decode_poll(reply), Err(PollError::Busy)));
        let reply: Response =
            serde_json::from_value(json!({"protocol":2,"views":[],"error":"unknown"})).unwrap();
        assert!(matches!(decode_poll(reply), Err(PollError::Failed(_))));
    }

    #[test]
    fn notification_token_accepts_only_bounded_uuid_epoch() {
        let valid = "33333333-3333-4333-8333-333333333333";
        assert!(
            ChangeToken {
                host_instance: valid.into(),
                revision: u64::MAX
            }
            .validate()
            .is_ok()
        );
        for invalid in [
            "",
            "33333333_3333-4333-8333-333333333333",
            "33333333-3333-4333-8333-33333333333z",
            "界",
        ] {
            assert!(
                ChangeToken {
                    host_instance: invalid.into(),
                    revision: 0
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn old_host_without_notification_token_remains_compatible() {
        let reply: Response =
            serde_json::from_value(json!({"protocol":2,"views":[],"selected":[],"error":null}))
                .unwrap();
        assert!(reply.change_token.is_none());
        assert!(poll_response(reply).is_ok());
    }
}

#[cfg(test)]
mod workspace_git_commit_tests {
    use super::*;

    fn plan() -> WorkspaceGitCommitPlan {
        WorkspaceGitCommitPlan {
            ticket: uuid::Uuid::from_u128(91),
            workspace_id: uuid::Uuid::from_u128(92),
            workspace_revision: 7,
            project_id: uuid::Uuid::from_u128(93),
            cwd: "/worktree".into(),
            head: Some("a".repeat(40)),
            branch: Some("main".into()),
            files: vec![GitCommitEntry {
                path: "src/main.rs".into(),
                status: 'M',
            }],
            signing_requested: true,
        }
    }

    #[test]
    fn plan_is_bound_to_workspace_and_staged_file_contract() {
        let value = plan();
        assert!(
            value
                .validate(Some((value.workspace_id, value.workspace_revision)))
                .is_ok()
        );
        assert!(
            value
                .validate(Some((value.workspace_id, value.workspace_revision + 1)))
                .is_err()
        );
        let mut invalid = value.clone();
        invalid.files[0].status = '?';
        assert!(
            invalid
                .validate(Some((invalid.workspace_id, invalid.workspace_revision)))
                .is_err()
        );
        let mut invalid = value;
        invalid.ticket = uuid::Uuid::nil();
        assert!(
            invalid
                .validate(Some((invalid.workspace_id, invalid.workspace_revision)))
                .is_err()
        );
    }

    #[test]
    fn commit_receipt_requires_oid_only_for_applied_and_bounds_messages() {
        let mut receipt = WorkspaceGitCommitResult {
            ticket: uuid::Uuid::from_u128(94),
            workspace_id: uuid::Uuid::from_u128(92),
            workspace_revision: 7,
            project_id: uuid::Uuid::from_u128(93),
            cwd: "/worktree".into(),
            phase: GitCommitPhase::Running,
            commit: None,
            acknowledged: false,
            message: None,
        };
        assert!(receipt.validate().is_ok());
        receipt.phase = GitCommitPhase::Applied;
        receipt.commit = Some("b".repeat(64));
        assert!(receipt.validate().is_ok());
        receipt.commit = Some("not-an-oid".into());
        assert!(receipt.validate().is_err());
        receipt.commit = None;
        receipt.phase = GitCommitPhase::Unknown;
        receipt.message = Some("x".repeat(1025));
        assert!(receipt.validate().is_err());
    }

    #[test]
    fn old_snapshot_and_reply_without_commit_fields_remain_supported() {
        let snapshot:WorkspaceGitSnapshot=serde_json::from_value(json!({
            "workspace_id":uuid::Uuid::from_u128(1),"workspace_revision":1,"project_id":uuid::Uuid::from_u128(2),
            "cwd":"/worktree","branch":"main","head":null,"detached":false,"entries":[],"truncated":false
        })).unwrap();
        assert_eq!(snapshot.commit_schema, None);
        assert_eq!(snapshot.commit_operation, None);
        let reply: Response =
            serde_json::from_value(json!({"protocol":2,"views":[],"error":null})).unwrap();
        assert!(reply.workspace_git_commit_plan.is_none());
        assert!(reply.workspace_git_commit.is_none());
    }
}
