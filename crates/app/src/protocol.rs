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
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct HistoryCursor {
    pub row: u64,
    pub offset: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HistoryViewQuery {
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

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryViewReply {
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
}

#[derive(Clone, Debug, Deserialize)]
pub struct ResumeSource {
    pub binding: pigade_storage::ConversationBinding,
    pub previous_head: uuid::Uuid,
}

pub struct ProductReply {
    pub generation_selection: Option<bool>,
    pub generation_selected: bool,
    pub generations: Option<GenerationList>,
    pub context_saved: bool,
    pub launch_preview: Option<LaunchPreview>,
    pub launch_phase: Option<String>,
    pub sessions: Option<Vec<View>>,
    pub launch_confirmed: bool,
    pub catalog: Option<pigade_storage::Catalog>,
    pub selection: Option<pigade_storage::Selection>,
    pub context: Option<pigade_storage::ContextDocument>,
    pub history_view: Option<HistoryViewReply>,
    pub history_view_request: bool,
}

pub struct ProductError {
    pub message: String,
    pub rejected_before_launch: bool,
}

pub fn product_request(socket: &Path, command: Value) -> Result<ProductReply, ProductError> {
    let command_name = command.get("command").and_then(Value::as_str);
    let launch_confirmed = matches!(command_name, Some("launch_in_tab" | "start_prepared"));
    let list_sessions = command_name == Some("list");
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
    let response = request_response_with_timeout(socket, command, Duration::from_secs(15))
        .map_err(|message| ProductError {
            message,
            rejected_before_launch: false,
        })?;
    if let Some(message) = response.error {
        return Err(ProductError {
            message,
            rejected_before_launch: launch_confirmed
                && response.rejected_before_admission.as_deref() == Some("launch_preflight"),
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
                message: "Недопустимый список поколений панели".into(),
                rejected_before_launch: false,
            });
        }
    }
    Ok(ProductReply {
        generation_selection: response.generation_selection,
        generation_selected,
        generations: response.generations,
        context_saved,
        launch_preview: response.launch_preview,
        launch_phase: response.launch_phase,
        sessions: list_sessions.then_some(response.views),
        launch_confirmed,
        catalog: response.catalog,
        selection: response.selection,
        context: response.context,
        history_view: response.history_view,
        history_view_request,
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
        return if response.rejected_before_admission.as_deref() == Some("queue_full") {
            Err(CommandFailure::NotAdmitted {
                code: "queue_full".into(),
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
}

fn poll_response(reply: Response) -> Result<Poll, String> {
    if let Some(error) = reply.error {
        return Err(error);
    }
    validate_recovery(reply.recovery.as_ref())?;
    Ok(Poll {
        selected: reply
            .selected
            .ok_or("Host does not support conditional cell polling")?,
        views: reply.views,
        recovery: reply.recovery,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum PollError {
    Busy,
    Failed(String),
}

fn decode_poll(reply: Response) -> Result<(Poll, Option<ChangeToken>), PollError> {
    if reply.error.is_some() && reply.rejected_before_admission.as_deref() == Some("queue_full") {
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

    fn error_response(error: &str, rejection: Option<&str>) -> Response {
        Response {
            protocol: 2,
            catalog: None,
            selection: None,
            context: None,
            launch_preview: None,
            launch_phase: None,
            generations: None,
            generation_selection: None,
            history_view: None,
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
    fn recovery_diagnostics_are_carried_even_when_there_are_no_views() {
        let recovery = diagnostics("session-7.journal", "checkpoint could not be read");
        let poll = Poll {
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
            selected: vec![regression.known()],
            views: vec![regression],
            recovery: Some(diagnostics("new.journal", "new warning")),
        };
        let mut cached = expected.clone();
        assert!(merge_poll(&mut cached, &mut recovery, invalid).is_err());
        assert_eq!(cached, expected);
        assert_eq!(recovery, previous_recovery);

        let invalid_diagnostics = Poll {
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
                selected: vec![regression.known()],
                views: vec![regression],
                recovery: None,
            },
            Poll {
                selected: vec![fixture().known(), fixture().known()],
                views: vec![],
                recovery: None,
            },
            Poll {
                selected: vec![],
                views: vec![fixture()],
                recovery: None,
            },
            Poll {
                selected: vec![fixture().known()],
                views: vec![fixture(), fixture()],
                recovery: None,
            },
            Poll {
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
