//! Product metadata on the existing sole SQLite writer. Git identity/path
//! validation belongs to the host; this module never invokes Git or a shell.
use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::MetadataStore;
use crate::agent_profile::{
    AgentProfile, AgentProfileDelete, MAX_AGENT_PROFILE_SETTINGS_BYTES, MAX_AGENT_PROFILES,
    validate_agent_profile_settings,
};
use crate::prompt::{MAX_PROMPTS, Prompt, PromptDelete, delete_prompt, read_prompts, write_prompt};

const MAX_PROJECTS: usize = 256;
const MAX_WORKSPACES: usize = 4096;
const MAX_TABS: usize = 16384;
const MAX_LAYOUT_TEMPLATES: usize = 128;
const MAX_TEMPLATE_LAUNCHES: usize = 32;
// Source bytes bound SQLite reads; JSON escaping and entity metadata need their
// own wire bound. Leave 1 MiB of the host's 8 MiB message cap for its envelope.
const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENCODED_CATALOG_BYTES: usize = 7 * 1024 * 1024;
const MAX_LAYOUT_BYTES: usize = 1024 * 1024;
const MAX_TEMPLATE_LAUNCH_BYTES: usize = 3 * 1024 * 1024;

/// Counts serializer output without materializing an encoded catalog buffer.
struct JsonByteCounter(usize);
impl Write for JsonByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_ENCODED_CATALOG_BYTES - self.0 {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedRoot {
    pub id: Uuid,
    pub name: String,
    pub relative_path: PathBuf,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub canonical_path: PathBuf,
    pub git_common_dir: PathBuf,
    pub name: String,
    pub roots: Vec<NamedRoot>,
    pub revision: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    Worktree,
    SharedCheckout,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Preparing,
    Ready,
    Failed,
    Archived,
    MissingPath,
}
impl WorkspaceMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Worktree => "worktree",
            Self::SharedCheckout => "shared_checkout",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "worktree" => Ok(Self::Worktree),
            "shared_checkout" => Ok(Self::SharedCheckout),
            _ => anyhow::bail!("unknown workspace mode"),
        }
    }
}
impl WorkspaceStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Archived => "archived",
            Self::MissingPath => "missing_path",
        }
    }
    fn parse(value: &str) -> Result<Self> {
        match value {
            "preparing" => Ok(Self::Preparing),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            "archived" => Ok(Self::Archived),
            "missing_path" => Ok(Self::MissingPath),
            _ => anyhow::bail!("unknown workspace status"),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: Uuid,
    pub project_id: Uuid,
    pub task: String,
    pub cwd: PathBuf,
    pub mode: WorkspaceMode,
    pub status: WorkspaceStatus,
    pub error: Option<String>,
    pub context_revision: Option<Uuid>,
    pub revision: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    /// Valid bounded JSON; the layout owner validates pane/tree invariants.
    pub layout_json: String,
    pub active_pane: Option<Uuid>,
    pub revision: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutTemplate {
    pub id: Uuid,
    pub name: String,
    pub geometry: pigade_layout::GeometryTemplate,
    #[serde(default)]
    pub launches: Option<Vec<LaunchSettings>>,
    pub revision: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSettings {
    pub provider: String,
    pub role: String,
    pub instructions: String,
    pub deliver_context: bool,
}

impl LaunchSettings {
    /// Validate the deliberately small, credential-free launch profile stored
    /// alongside a reusable geometry template.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            matches!(
                self.provider.as_str(),
                "shell" | "claude" | "codex" | "opencode"
            ),
            "unsupported template launch provider"
        );
        ensure!(
            self.role.len() <= 8 * 1024
                && self.instructions.len() <= 64 * 1024
                && !self.role.contains('\0')
                && !self.instructions.contains('\0'),
            "template launch settings exceed bounds or contain NUL"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutTemplateDelete {
    pub id: Uuid,
    pub expected_revision: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneLabel {
    pub session: Uuid,
    /// Empty names are revisioned tombstones that reset the visible title.
    pub name: String,
    pub revision: u64,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CanvasOffset {
    pub x: f64,
    pub y: f64,
}

impl CanvasOffset {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.x.is_finite()
                && self.y.is_finite()
                && (0.0..=1.0e9).contains(&self.x)
                && (0.0..=1.0e9).contains(&self.y),
            "canvas offset outside bounds"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub sidebar_width: u16,
    pub sidebar_collapsed: bool,
    pub monitor: Option<String>,
    #[serde(default)]
    pub canvas_offsets: BTreeMap<Uuid, CanvasOffset>,
    #[serde(default)]
    pub panel_generations: BTreeMap<Uuid, Uuid>,
}
impl Default for WindowState {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 1280,
            height: 800,
            scale: 1.0,
            sidebar_width: 240,
            sidebar_collapsed: false,
            monitor: None,
            canvas_offsets: BTreeMap::new(),
            panel_generations: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Selection {
    pub project_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub tab_id: Option<Uuid>,
    pub window: WindowState,
    pub revision: u64,
}
/// Global placement history, including atomic cross-workspace undo/redo. The
/// host validates its LayoutHistory schema and synchronizes current tab trees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutDocument {
    pub json: String,
    pub revision: u64,
}
impl Default for LayoutDocument {
    fn default() -> Self {
        Self {
            json: "null".into(),
            revision: 0,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub projects: Vec<Project>,
    pub workspaces: Vec<Workspace>,
    pub tabs: Vec<Tab>,
    pub selection: Selection,
    pub layout: LayoutDocument,
    #[serde(default)]
    pub layout_templates: Option<Vec<LayoutTemplate>>,
    #[serde(default)]
    pub template_launch_schema: Option<u16>,
    #[serde(default)]
    pub pane_labels: Option<Vec<PaneLabel>>,
    /// `None` means the connected host predates agent profile support.
    #[serde(default)]
    pub agent_profiles: Option<Vec<AgentProfile>>,
    /// `None` means the connected host predates prompt library support.
    #[serde(default)]
    pub prompts: Option<Vec<Prompt>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogWrite<T> {
    pub value: T,
    pub expected_revision: Option<u64>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CatalogBatch {
    pub projects: Vec<CatalogWrite<Project>>,
    pub workspaces: Vec<CatalogWrite<Workspace>>,
    pub tabs: Vec<CatalogWrite<Tab>>,
    pub selection: Option<CatalogWrite<Selection>>,
    pub layout: Option<CatalogWrite<LayoutDocument>>,
    #[serde(default)]
    pub layout_templates: Vec<CatalogWrite<LayoutTemplate>>,
    #[serde(default)]
    pub remove_layout_templates: Vec<LayoutTemplateDelete>,
    #[serde(default)]
    pub agent_profiles: Vec<CatalogWrite<AgentProfile>>,
    #[serde(default)]
    pub remove_agent_profiles: Vec<AgentProfileDelete>,
    #[serde(default)]
    pub prompts: Vec<CatalogWrite<Prompt>>,
    #[serde(default)]
    pub remove_prompts: Vec<PromptDelete>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionConflict {
    pub entity: &'static str,
    pub id: Uuid,
    pub expected: u64,
}
impl fmt::Display for RevisionConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} revision conflict (expected {})",
            self.entity, self.id, self.expected
        )
    }
}
impl std::error::Error for RevisionConflict {}

/// Schema mutation executes in the caller's migration transaction, after a
/// durable SQLite-generated backup of any existing older database.
pub(crate) fn migrate_catalog(connection: &Connection) -> Result<()> {
    connection.execute_batch("CREATE TABLE projects (
        id TEXT PRIMARY KEY, canonical_path TEXT NOT NULL UNIQUE,
        git_common_dir TEXT NOT NULL, name TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK(revision>0));
      CREATE TABLE project_roots (
        id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
        name TEXT NOT NULL, relative_path TEXT NOT NULL, UNIQUE(project_id,name));
      CREATE TABLE workspaces (
        id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
        task TEXT NOT NULL, cwd TEXT NOT NULL, mode TEXT NOT NULL CHECK(mode IN ('worktree','shared_checkout')),
        status TEXT NOT NULL CHECK(status IN ('preparing','ready','failed','archived','missing_path')),
        error TEXT, context_revision TEXT, revision INTEGER NOT NULL CHECK(revision>0));
      CREATE TABLE tabs (
        id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id),
        name TEXT NOT NULL, layout_json TEXT NOT NULL, active_pane TEXT,
        revision INTEGER NOT NULL CHECK(revision>0));
      CREATE INDEX workspace_project ON workspaces(project_id);
      CREATE INDEX tab_workspace ON tabs(workspace_id);
      CREATE TABLE catalog_selection (
        singleton INTEGER PRIMARY KEY CHECK(singleton=1),
        project_id TEXT REFERENCES projects(id), workspace_id TEXT REFERENCES workspaces(id),
        tab_id TEXT REFERENCES tabs(id), window_json TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK(revision>=0));")?;
    connection.execute(
        "INSERT INTO catalog_selection(singleton,window_json,revision) VALUES(1,?1,0)",
        [serde_json::to_string(&WindowState::default())?],
    )?;
    Ok(())
}

/// Schema3 adds the global history document. Early experimental v2 writers may
/// already have this identical table; retain its content and revision verbatim.
pub(crate) fn migrate_layout_document(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS layout_revisions (
        singleton INTEGER PRIMARY KEY CHECK(singleton=1), json TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK(revision>=0));",
    )?;
    connection.execute(
        "INSERT OR IGNORE INTO layout_revisions(singleton,json,revision)VALUES(1,'null',0)",
        [],
    )?;
    Ok(())
}

pub(crate) fn migrate_layout_templates(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE layout_templates (
        id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, geometry_json TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK(revision>0));",
    )?;
    Ok(())
}

pub(crate) fn migrate_template_launches(connection: &Connection) -> Result<()> {
    connection.execute_batch("ALTER TABLE layout_templates ADD COLUMN launches_json TEXT;")?;
    Ok(())
}

pub(crate) fn migrate_pane_labels(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE pane_labels (
        session_id TEXT PRIMARY KEY, name TEXT NOT NULL,
        revision INTEGER NOT NULL CHECK(revision>0)
    ) WITHOUT ROWID;
    CREATE INDEX prepared_launch_generation_lookup
        ON prepared_launches(
            json_extract(payload,'$.launch.session'),
            json_extract(payload,'$.launch.generation')
        );",
    )?;
    Ok(())
}

pub(crate) fn migrate_agent_profiles(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE agent_profiles (
          id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, settings_json TEXT NOT NULL,
          revision INTEGER NOT NULL CHECK(revision>0)
        );",
    )?;
    Ok(())
}

impl MetadataStore {
    pub fn catalog(&self) -> Result<Catalog> {
        read_catalog(&self.connection)
    }
    /// All entity writes, selection and global layout are one durable transaction. Parent
    /// ownership cannot change through an update; multi-tab layout transfers
    /// update both tab records in this batch. Source and encoded JSON byte
    /// bounds are checked before commit. No process lifecycle is affected.
    pub fn apply_catalog(&mut self, batch: CatalogBatch) -> Result<Catalog> {
        self.apply_catalog_internal(batch, None)
    }

    /// Compare-and-swap one durable pane alias without exposing label writes to
    /// generic catalog batches.
    pub fn save_pane_label(&mut self, write: CatalogWrite<PaneLabel>) -> Result<Catalog> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        write_pane_label(&tx, write)?;
        let catalog = read_catalog(&tx)?;
        tx.commit()?;
        Ok(catalog)
    }

    /// Atomically save catalog/layout changes with the prepared template-run
    /// record that owns those sessions. Neither half is visible if validation
    /// or compare-and-swap fails.
    pub fn apply_catalog_with_template_run(
        &mut self,
        batch: CatalogBatch,
        run: CatalogWrite<crate::StoredTemplateRun>,
    ) -> Result<Catalog> {
        self.apply_catalog_internal(batch, Some(run))
    }

    fn apply_catalog_internal(
        &mut self,
        batch: CatalogBatch,
        run: Option<CatalogWrite<crate::StoredTemplateRun>>,
    ) -> Result<Catalog> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let catalog = apply_catalog_batch(&tx, batch, run)?;
        tx.commit()?;
        Ok(catalog)
    }
}

fn apply_catalog_batch(
    tx: &Transaction<'_>,
    batch: CatalogBatch,
    run: Option<CatalogWrite<crate::StoredTemplateRun>>,
) -> Result<Catalog> {
    ensure!(
        batch.projects.len() <= MAX_PROJECTS
            && batch.workspaces.len() <= MAX_WORKSPACES
            && batch.tabs.len() <= MAX_TABS,
        "catalog batch exceeds bounded entity capacity"
    );
    ensure!(
        batch.layout_templates.len() <= MAX_LAYOUT_TEMPLATES,
        "too many layout templates"
    );
    ensure!(
        batch.remove_layout_templates.len() <= MAX_LAYOUT_TEMPLATES,
        "too many layout template deletions"
    );
    ensure!(
        batch.agent_profiles.len() <= MAX_AGENT_PROFILES,
        "too many agent profiles"
    );
    ensure!(
        batch.remove_agent_profiles.len() <= MAX_AGENT_PROFILES,
        "too many agent profile deletions"
    );
    ensure!(
        batch.prompts.len() <= MAX_PROMPTS && batch.remove_prompts.len() <= MAX_PROMPTS,
        "too many prompt writes or deletions"
    );
    unique_ids(batch.layout_templates.iter().map(|write| write.value.id))?;
    unique_ids(batch.remove_layout_templates.iter().map(|delete| delete.id))?;
    ensure!(
        batch.layout_templates.iter().all(|write| !batch
            .remove_layout_templates
            .iter()
            .any(|delete| delete.id == write.value.id)),
        "template cannot be written and deleted in one batch"
    );
    unique_ids(batch.agent_profiles.iter().map(|write| write.value.id))?;
    unique_ids(batch.remove_agent_profiles.iter().map(|delete| delete.id))?;
    ensure!(
        batch.agent_profiles.iter().all(|write| !batch
            .remove_agent_profiles
            .iter()
            .any(|delete| delete.id == write.value.id)),
        "agent profile cannot be written and deleted in one batch"
    );
    unique_ids(batch.prompts.iter().map(|write| write.value.id))?;
    unique_ids(batch.remove_prompts.iter().map(|delete| delete.id))?;
    ensure!(
        batch.prompts.iter().all(|write| !batch
            .remove_prompts
            .iter()
            .any(|delete| delete.id == write.value.id)),
        "prompt cannot be written and deleted in one batch"
    );
    unique_ids(batch.projects.iter().map(|write| write.value.id))?;
    unique_ids(batch.workspaces.iter().map(|write| write.value.id))?;
    unique_ids(batch.tabs.iter().map(|write| write.value.id))?;
    for write in batch.projects {
        write_project(tx, write)?;
    }
    for write in batch.workspaces {
        write_workspace(tx, write)?;
    }
    for write in batch.tabs {
        write_tab(tx, write)?;
    }
    for write in batch.layout_templates {
        write_layout_template(tx, write)?;
    }
    for delete in batch.remove_layout_templates {
        delete_layout_template(tx, delete)?;
    }
    for write in batch.agent_profiles {
        write_agent_profile(tx, write)?;
    }
    for delete in batch.remove_agent_profiles {
        delete_agent_profile(tx, delete)?;
    }
    for write in batch.prompts {
        write_prompt(tx, write.value, write.expected_revision)?;
    }
    for delete in batch.remove_prompts {
        delete_prompt(tx, delete)?;
    }
    if let Some(write) = batch.selection {
        write_selection(tx, write)?;
    }
    if let Some(write) = batch.layout {
        write_layout(tx, write)?;
    }
    if let Some(run) = run {
        crate::template_run::write_template_run(tx, run)?;
    }
    read_catalog(tx)
}

fn unique_ids(ids: impl Iterator<Item = Uuid>) -> Result<()> {
    let mut seen = HashSet::new();
    for id in ids {
        ensure!(seen.insert(id), "duplicate entity ID in catalog batch");
    }
    Ok(())
}
fn text(value: &str, maximum: usize, empty: bool) -> Result<()> {
    ensure!(
        (empty || !value.trim().is_empty()) && value.len() <= maximum && !value.contains('\0'),
        "catalog text exceeds bounds or is empty"
    );
    Ok(())
}
fn validate_launch_settings(settings: &LaunchSettings) -> Result<()> {
    settings.validate()
}
fn absolute_path(path: &Path) -> Result<&str> {
    let value = path.to_str().context("catalog paths require UTF-8")?;
    ensure!(
        path.is_absolute() && !value.contains('\0') && value.len() <= 16384,
        "catalog path must be absolute and bounded"
    );
    Ok(value)
}
fn revision(current: u64, expected: Option<u64>) -> Result<i64> {
    ensure!(
        current == expected.unwrap_or(0),
        "record revision must match expected revision"
    );
    let next = match expected {
        Some(value) => {
            ensure!(
                value > 0 && value < i64::MAX as u64,
                "invalid entity revision"
            );
            value + 1
        }
        None => 1,
    };
    Ok(next as i64)
}
fn conflict(changed: usize, entity: &'static str, id: Uuid, expected: u64) -> Result<()> {
    if changed != 1 {
        return Err(RevisionConflict {
            entity,
            id,
            expected,
        }
        .into());
    }
    Ok(())
}
fn write_project(tx: &Transaction<'_>, write: CatalogWrite<Project>) -> Result<()> {
    let p = write.value;
    let next = revision(p.revision, write.expected_revision)?;
    text(&p.name, 256, false)?;
    let canonical = absolute_path(&p.canonical_path)?;
    let common = absolute_path(&p.git_common_dir)?;
    ensure!(p.roots.len() <= 128, "too many named roots");
    unique_ids(p.roots.iter().map(|root| root.id))?;
    let mut names = HashSet::new();
    for root in &p.roots {
        text(&root.name, 128, false)?;
        ensure!(names.insert(root.name.as_str()), "duplicate named root");
        let relative = root
            .relative_path
            .to_str()
            .context("named root requires UTF-8")?;
        text(relative, 16384, false)?;
        ensure!(
            !root.relative_path.components().any(|part| matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )),
            "named root escapes project"
        );
    }
    match write.expected_revision {
        None => {
            tx.execute("INSERT INTO projects(id,canonical_path,git_common_dir,name,revision) VALUES(?1,?2,?3,?4,1)",params![p.id.to_string(),canonical,common,p.name])?;
        }
        Some(expected) => {
            let changed=tx.execute("UPDATE projects SET canonical_path=?2,git_common_dir=?3,name=?4,revision=?5 WHERE id=?1 AND revision=?6",params![p.id.to_string(),canonical,common,p.name,next,expected as i64])?;
            conflict(changed, "project", p.id, expected)?;
        }
    }
    tx.execute(
        "DELETE FROM project_roots WHERE project_id=?1",
        [p.id.to_string()],
    )?;
    for root in p.roots {
        tx.execute(
            "INSERT INTO project_roots(id,project_id,name,relative_path) VALUES(?1,?2,?3,?4)",
            params![
                root.id.to_string(),
                p.id.to_string(),
                root.name,
                root.relative_path.to_str().unwrap()
            ],
        )?;
    }
    Ok(())
}
fn write_workspace(tx: &Transaction<'_>, write: CatalogWrite<Workspace>) -> Result<()> {
    let w = write.value;
    let next = revision(w.revision, write.expected_revision)?;
    text(&w.task, 65536, false)?;
    let cwd = absolute_path(&w.cwd)?;
    if let Some(error) = &w.error {
        text(error, 16384, false)?;
    }
    match w.status {
        WorkspaceStatus::Failed | WorkspaceStatus::MissingPath => ensure!(
            w.error.is_some(),
            "unavailable workspace requires a visible error"
        ),
        WorkspaceStatus::Ready | WorkspaceStatus::Preparing => ensure!(
            w.error.is_none(),
            "ready/preparing workspace cannot hide an error"
        ),
        WorkspaceStatus::Archived => (),
    }
    let mode = w.mode.as_str();
    let status = w.status.as_str();
    match write.expected_revision {
        None => {
            tx.execute("INSERT INTO workspaces(id,project_id,task,cwd,mode,status,error,context_revision,revision) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1)",params![w.id.to_string(),w.project_id.to_string(),w.task,cwd,mode,status,w.error,w.context_revision.map(|id|id.to_string())])?;
        }
        Some(expected) => {
            let owner: Option<String> = tx
                .query_row(
                    "SELECT project_id FROM workspaces WHERE id=?1",
                    [w.id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            ensure!(
                owner.as_deref() == Some(w.project_id.to_string().as_str()),
                "workspace owner cannot change"
            );
            let changed=tx.execute("UPDATE workspaces SET task=?2,cwd=?3,mode=?4,status=?5,error=?6,context_revision=?7,revision=?8 WHERE id=?1 AND revision=?9",params![w.id.to_string(),w.task,cwd,mode,status,w.error,w.context_revision.map(|id|id.to_string()),next,expected as i64])?;
            conflict(changed, "workspace", w.id, expected)?;
        }
    }
    Ok(())
}
fn write_tab(tx: &Transaction<'_>, write: CatalogWrite<Tab>) -> Result<()> {
    let t = write.value;
    let next = revision(t.revision, write.expected_revision)?;
    text(&t.name, 256, false)?;
    ensure!(
        t.layout_json.len() <= MAX_LAYOUT_BYTES,
        "layout JSON exceeds byte bound"
    );
    let _: serde_json::Value =
        serde_json::from_str(&t.layout_json).context("invalid layout JSON")?;
    match write.expected_revision {
        None => {
            tx.execute("INSERT INTO tabs(id,workspace_id,name,layout_json,active_pane,revision) VALUES(?1,?2,?3,?4,?5,1)",params![t.id.to_string(),t.workspace_id.to_string(),t.name,t.layout_json,t.active_pane.map(|id|id.to_string())])?;
        }
        Some(expected) => {
            let owner: Option<String> = tx
                .query_row(
                    "SELECT workspace_id FROM tabs WHERE id=?1",
                    [t.id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            ensure!(
                owner.as_deref() == Some(t.workspace_id.to_string().as_str()),
                "tab owner cannot change"
            );
            let changed=tx.execute("UPDATE tabs SET name=?2,layout_json=?3,active_pane=?4,revision=?5 WHERE id=?1 AND revision=?6",params![t.id.to_string(),t.name,t.layout_json,t.active_pane.map(|id|id.to_string()),next,expected as i64])?;
            conflict(changed, "tab", t.id, expected)?;
        }
    }
    Ok(())
}
fn write_pane_label(tx: &Transaction<'_>, write: CatalogWrite<PaneLabel>) -> Result<()> {
    let label = write.value;
    ensure!(!label.session.is_nil(), "pane label session cannot be nil");
    ensure!(
        label.name.len() <= 512
            && label.name.chars().count() <= 128
            && label.name.trim() == label.name
            && !label.name.chars().any(char::is_control),
        "pane label must be trimmed, bounded, and contain no control characters"
    );
    let next = revision(label.revision, write.expected_revision)?;
    match write.expected_revision {
        None => {
            tx.execute(
                "INSERT INTO pane_labels(session_id,name,revision) VALUES(?1,?2,1)",
                params![label.session.to_string(), label.name],
            )?;
        }
        Some(expected) => {
            let changed = tx.execute(
                "UPDATE pane_labels SET name=?2,revision=?3 WHERE session_id=?1 AND revision=?4",
                params![label.session.to_string(), label.name, next, expected as i64],
            )?;
            conflict(changed, "pane label", label.session, expected)?;
        }
    }
    Ok(())
}
fn write_layout_template(tx: &Transaction<'_>, write: CatalogWrite<LayoutTemplate>) -> Result<()> {
    let template = write.value;
    let next = revision(template.revision, write.expected_revision)?;
    text(&template.name, 256, false)?;
    ensure!(
        template.name.trim() == template.name,
        "layout template name must not have surrounding whitespace"
    );
    template.geometry.validate()?;
    ensure!(
        template.geometry.pane_count() > 0,
        "layout template must contain at least one pane"
    );
    if let Some(launches) = &template.launches {
        ensure!(
            !launches.is_empty()
                && launches.len() <= MAX_TEMPLATE_LAUNCHES
                && launches.len() == template.geometry.pane_count(),
            "template launch settings must match 1..=32 geometry panes"
        );
        for settings in launches {
            validate_launch_settings(settings)?;
        }
    }
    let geometry = serde_json::to_string(&template.geometry)?;
    ensure!(
        geometry.len() <= MAX_LAYOUT_BYTES,
        "layout template exceeds byte bound"
    );
    let launches = template
        .launches
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    ensure!(
        launches
            .as_ref()
            .is_none_or(|json| json.len() <= MAX_TEMPLATE_LAUNCH_BYTES),
        "template launch settings exceed byte bound"
    );
    match write.expected_revision {
        None => {
            tx.execute(
                "INSERT INTO layout_templates(id,name,geometry_json,launches_json,revision) VALUES(?1,?2,?3,?4,1)",
                params![template.id.to_string(), template.name, geometry, launches],
            )?;
        }
        Some(expected) => {
            let changed = tx.execute("UPDATE layout_templates SET name=?2,geometry_json=?3,launches_json=?4,revision=?5 WHERE id=?1 AND revision=?6", params![template.id.to_string(), template.name, geometry, launches, next, expected as i64])?;
            conflict(changed, "layout template", template.id, expected)?;
        }
    }
    Ok(())
}

fn write_agent_profile(tx: &Transaction<'_>, write: CatalogWrite<AgentProfile>) -> Result<()> {
    let profile = write.value;
    ensure!(!profile.id.is_nil(), "agent profile ID cannot be nil");
    let next = revision(profile.revision, write.expected_revision)?;
    text(&profile.name, 256, false)?;
    ensure!(
        profile.name.trim() == profile.name,
        "agent profile name must not have surrounding whitespace"
    );
    validate_agent_profile_settings(&profile.settings)?;
    let settings = serde_json::to_string(&profile.settings)?;
    ensure!(
        settings.len() <= MAX_AGENT_PROFILE_SETTINGS_BYTES,
        "agent profile settings exceed byte bound"
    );
    match write.expected_revision {
        None => {
            tx.execute(
                "INSERT INTO agent_profiles(id,name,settings_json,revision) VALUES(?1,?2,?3,1)",
                params![profile.id.to_string(), profile.name, settings],
            )?;
        }
        Some(expected) => {
            let changed = tx.execute(
                "UPDATE agent_profiles SET name=?2,settings_json=?3,revision=?4 WHERE id=?1 AND revision=?5",
                params![profile.id.to_string(), profile.name, settings, next, expected as i64],
            )?;
            conflict(changed, "agent profile", profile.id, expected)?;
        }
    }
    Ok(())
}

fn delete_agent_profile(tx: &Transaction<'_>, delete: AgentProfileDelete) -> Result<()> {
    ensure!(
        delete.expected_revision > 0 && delete.expected_revision < i64::MAX as u64,
        "invalid agent profile deletion revision"
    );
    let changed = tx.execute(
        "DELETE FROM agent_profiles WHERE id=?1 AND revision=?2",
        params![delete.id.to_string(), delete.expected_revision as i64],
    )?;
    conflict(
        changed,
        "agent profile",
        delete.id,
        delete.expected_revision,
    )
}
fn delete_layout_template(tx: &Transaction<'_>, delete: LayoutTemplateDelete) -> Result<()> {
    ensure!(
        delete.expected_revision > 0 && delete.expected_revision < i64::MAX as u64,
        "invalid layout template revision"
    );
    let changed = tx.execute(
        "DELETE FROM layout_templates WHERE id=?1 AND revision=?2",
        params![delete.id.to_string(), delete.expected_revision as i64],
    )?;
    conflict(
        changed,
        "layout template",
        delete.id,
        delete.expected_revision,
    )
}
fn write_selection(tx: &Transaction<'_>, write: CatalogWrite<Selection>) -> Result<()> {
    let s = write.value;
    let expected = write
        .expected_revision
        .context("selection requires compare-and-swap revision")?;
    ensure!(
        s.revision == expected && expected < i64::MAX as u64,
        "invalid selection revision"
    );
    let window = &s.window;
    ensure!(
        (800..=16384).contains(&window.width)
            && (600..=16384).contains(&window.height)
            && window.scale.is_finite()
            && (0.5..=8.0).contains(&window.scale)
            && (180..=360).contains(&window.sidebar_width),
        "window state outside bounds"
    );
    if let Some(monitor) = &window.monitor {
        text(monitor, 256, false)?;
    }
    validate_canvas_offsets(tx, &window.canvas_offsets)?;
    validate_panel_generations(&window.panel_generations)?;
    if let Some(project) = s.project_id {
        ensure!(
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                [project.to_string()],
                |row| row.get::<_, bool>(0)
            )?,
            "selected project missing"
        );
    }
    if let Some(workspace) = s.workspace_id {
        let project = s
            .project_id
            .context("workspace selection requires project")?;
        ensure!(
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workspaces WHERE id=?1 AND project_id=?2)",
                params![workspace.to_string(), project.to_string()],
                |row| row.get::<_, bool>(0)
            )?,
            "selected workspace belongs to another project"
        );
    }
    if let Some(tab) = s.tab_id {
        let workspace = s.workspace_id.context("tab selection requires workspace")?;
        ensure!(
            tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM tabs WHERE id=?1 AND workspace_id=?2)",
                params![tab.to_string(), workspace.to_string()],
                |row| row.get::<_, bool>(0)
            )?,
            "selected tab belongs to another workspace"
        );
    }
    let changed=tx.execute("UPDATE catalog_selection SET project_id=?1,workspace_id=?2,tab_id=?3,window_json=?4,revision=revision+1 WHERE singleton=1 AND revision=?5",params![s.project_id.map(|id|id.to_string()),s.workspace_id.map(|id|id.to_string()),s.tab_id.map(|id|id.to_string()),serde_json::to_string(&s.window)?,expected as i64])?;
    conflict(changed, "selection", Uuid::nil(), expected)
}

fn validate_canvas_offsets(
    connection: &Connection,
    offsets: &BTreeMap<Uuid, CanvasOffset>,
) -> Result<()> {
    ensure!(offsets.len() <= MAX_TABS, "too many canvas offsets");
    for (tab, offset) in offsets {
        offset.validate()?;
        ensure!(
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM tabs WHERE id=?1)",
                [tab.to_string()],
                |row| row.get::<_, bool>(0)
            )?,
            "canvas offset references a missing tab"
        );
    }
    Ok(())
}

fn validate_panel_generations(generations: &BTreeMap<Uuid, Uuid>) -> Result<()> {
    ensure!(
        generations.len() <= 100_000,
        "too many persisted panel generations"
    );
    ensure!(
        generations
            .iter()
            .all(|(session, generation)| !session.is_nil() && !generation.is_nil()),
        "panel generation map contains a nil identity"
    );
    Ok(())
}

fn write_layout(tx: &Transaction<'_>, write: CatalogWrite<LayoutDocument>) -> Result<()> {
    let layout = write.value;
    let expected = write
        .expected_revision
        .context("layout history requires compare-and-swap revision")?;
    ensure!(
        layout.revision == expected && expected < i64::MAX as u64,
        "invalid layout history revision"
    );
    ensure!(
        layout.json.len() <= 2 * 1024 * 1024,
        "layout history exceeds 2 MiB bound"
    );
    let _: serde_json::Value =
        serde_json::from_str(&layout.json).context("invalid layout history JSON")?;
    let changed = tx.execute(
        "UPDATE layout_revisions SET json=?1,revision=revision+1 WHERE singleton=1 AND revision=?2",
        params![layout.json, expected as i64],
    )?;
    conflict(changed, "layout history", Uuid::nil(), expected)
}

fn read_pane_labels(connection: &Connection, count: usize) -> Result<Vec<PaneLabel>> {
    let mut labels = Vec::with_capacity(count);
    let mut statement = connection
        .prepare("SELECT session_id,name,revision FROM pane_labels ORDER BY session_id")?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u64>(2)?,
        ))
    })? {
        let (session, name, revision) = row?;
        let label = PaneLabel {
            session: Uuid::parse_str(&session)?,
            name,
            revision,
        };
        ensure!(
            label.name.len() <= 512
                && label.name.chars().count() <= 128
                && label.name.trim() == label.name
                && !label.name.chars().any(char::is_control),
            "stored pane label is outside bounds"
        );
        labels.push(label);
    }
    Ok(labels)
}

fn read_agent_profiles(connection: &Connection, count: usize) -> Result<Vec<AgentProfile>> {
    let mut profiles = Vec::with_capacity(count);
    let mut statement = connection
        .prepare("SELECT id,name,settings_json,revision FROM agent_profiles ORDER BY rowid")?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, u64>(3)?,
        ))
    })? {
        let (id, name, settings_json, revision) = row?;
        ensure!(
            settings_json.len() <= MAX_AGENT_PROFILE_SETTINGS_BYTES,
            "stored agent profile settings exceed byte bound"
        );
        let settings: LaunchSettings = serde_json::from_str(&settings_json)
            .context("invalid stored agent profile settings")?;
        validate_agent_profile_settings(&settings)?;
        profiles.push(AgentProfile {
            id: Uuid::parse_str(&id)?,
            name,
            settings,
            revision,
        });
    }
    Ok(profiles)
}

fn read_catalog(connection: &Connection) -> Result<Catalog> {
    let counts:(usize,usize,usize,usize)=connection.query_row("SELECT (SELECT COUNT(*) FROM projects),(SELECT COUNT(*) FROM workspaces),(SELECT COUNT(*) FROM tabs),(SELECT COUNT(*) FROM project_roots)",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
    ensure!(
        counts.0 <= MAX_PROJECTS
            && counts.1 <= MAX_WORKSPACES
            && counts.2 <= MAX_TABS
            && counts.3 <= MAX_PROJECTS * 128,
        "catalog exceeds bounded manifest entity count"
    );
    let template_count: usize =
        connection.query_row("SELECT COUNT(*) FROM layout_templates", [], |row| {
            row.get(0)
        })?;
    ensure!(
        template_count <= MAX_LAYOUT_TEMPLATES,
        "too many stored layout templates"
    );
    let pane_label_count: usize =
        connection.query_row("SELECT COUNT(*) FROM pane_labels", [], |row| row.get(0))?;
    ensure!(pane_label_count <= 100_000, "too many stored pane labels");
    let agent_profile_count: usize =
        connection.query_row("SELECT COUNT(*) FROM agent_profiles", [], |row| row.get(0))?;
    ensure!(
        agent_profile_count <= MAX_AGENT_PROFILES,
        "too many stored agent profiles"
    );
    let bytes:usize=connection.query_row("SELECT
      COALESCE((SELECT SUM(length(CAST(name AS BLOB))+length(CAST(canonical_path AS BLOB))+length(CAST(git_common_dir AS BLOB))) FROM projects),0)+
      COALESCE((SELECT SUM(length(CAST(name AS BLOB))+length(CAST(relative_path AS BLOB))) FROM project_roots),0)+
      COALESCE((SELECT SUM(length(CAST(task AS BLOB))+length(CAST(cwd AS BLOB))+COALESCE(length(CAST(error AS BLOB)),0)) FROM workspaces),0)+
      COALESCE((SELECT SUM(length(CAST(name AS BLOB))+length(CAST(layout_json AS BLOB))) FROM tabs),0)+
      COALESCE((SELECT length(CAST(window_json AS BLOB)) FROM catalog_selection WHERE singleton=1),0)+
      COALESCE((SELECT length(CAST(json AS BLOB)) FROM layout_revisions WHERE singleton=1),0)+
      COALESCE((SELECT SUM(length(CAST(name AS BLOB))+length(CAST(geometry_json AS BLOB))+COALESCE(length(CAST(launches_json AS BLOB)),0)) FROM layout_templates),0)+
      COALESCE((SELECT SUM(length(CAST(name AS BLOB))+length(CAST(settings_json AS BLOB))) FROM agent_profiles),0)+
      COALESCE((SELECT SUM(length(CAST(title AS BLOB))+length(CAST(content AS BLOB))) FROM prompt_records WHERE deleted=0),0)+
      COALESCE((SELECT SUM(length(CAST(name AS BLOB))) FROM pane_labels),0)",[],|row|row.get(0))?;
    ensure!(
        bytes <= MAX_MANIFEST_BYTES,
        "catalog exceeds bounded manifest bytes; load/archive fewer records"
    );
    let mut projects = Vec::with_capacity(counts.0);
    let mut stmt = connection.prepare(
        "SELECT id,canonical_path,git_common_dir,name,revision FROM projects ORDER BY rowid",
    )?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, u64>(4)?,
        ))
    })? {
        let (id, canonical, common, name, revision) = row?;
        let id = Uuid::parse_str(&id)?;
        let mut roots_stmt = connection.prepare(
            "SELECT id,name,relative_path FROM project_roots WHERE project_id=?1 ORDER BY rowid",
        )?;
        let mut roots = Vec::new();
        for root in roots_stmt.query_map([id.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (id, name, path) = root?;
            roots.push(NamedRoot {
                id: Uuid::parse_str(&id)?,
                name,
                relative_path: path.into(),
            });
        }
        projects.push(Project {
            id,
            canonical_path: canonical.into(),
            git_common_dir: common.into(),
            name,
            roots,
            revision,
        });
    }
    let mut workspaces = Vec::with_capacity(counts.1);
    let mut stmt=connection.prepare("SELECT id,project_id,task,cwd,mode,status,error,context_revision,revision FROM workspaces ORDER BY rowid")?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, u64>(8)?,
        ))
    })? {
        let (id, project, task, cwd, mode, status, error, context, revision) = row?;
        workspaces.push(Workspace {
            id: Uuid::parse_str(&id)?,
            project_id: Uuid::parse_str(&project)?,
            task,
            cwd: cwd.into(),
            mode: WorkspaceMode::parse(&mode)?,
            status: WorkspaceStatus::parse(&status)?,
            error,
            context_revision: context.map(|id| Uuid::parse_str(&id)).transpose()?,
            revision,
        });
    }
    let mut tabs = Vec::with_capacity(counts.2);
    let mut stmt = connection.prepare(
        "SELECT id,workspace_id,name,layout_json,active_pane,revision FROM tabs ORDER BY rowid",
    )?;
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, u64>(5)?,
        ))
    })? {
        let (id, workspace, name, layout_json, active, revision) = row?;
        tabs.push(Tab {
            id: Uuid::parse_str(&id)?,
            workspace_id: Uuid::parse_str(&workspace)?,
            name,
            layout_json,
            active_pane: active.map(|id| Uuid::parse_str(&id)).transpose()?,
            revision,
        });
    }
    let selection=connection.query_row("SELECT project_id,workspace_id,tab_id,window_json,revision FROM catalog_selection WHERE singleton=1",[],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?,r.get::<_,u64>(4)?)))?;
    let selection = Selection {
        project_id: selection.0.map(|id| Uuid::parse_str(&id)).transpose()?,
        workspace_id: selection.1.map(|id| Uuid::parse_str(&id)).transpose()?,
        tab_id: selection.2.map(|id| Uuid::parse_str(&id)).transpose()?,
        window: serde_json::from_str(&selection.3)?,
        revision: selection.4,
    };
    validate_canvas_offsets(connection, &selection.window.canvas_offsets)?;
    validate_panel_generations(&selection.window.panel_generations)?;
    let layout = connection.query_row(
        "SELECT json,revision FROM layout_revisions WHERE singleton=1",
        [],
        |row| {
            Ok(LayoutDocument {
                json: row.get(0)?,
                revision: row.get(1)?,
            })
        },
    )?;
    let mut layout_templates = Vec::with_capacity(template_count);
    let mut stmt = connection.prepare(
        "SELECT id,name,geometry_json,launches_json,revision FROM layout_templates ORDER BY rowid",
    )?;
    for row in stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, u64>(4)?,
        ))
    })? {
        let (id, name, geometry, launches, revision) = row?;
        let geometry: pigade_layout::GeometryTemplate =
            serde_json::from_str(&geometry).context("invalid stored layout template")?;
        geometry.validate()?;
        ensure!(geometry.pane_count() > 0, "stored layout template is empty");
        let launches: Option<Vec<LaunchSettings>> = launches
            .map(|json| serde_json::from_str(&json).context("invalid stored template launches"))
            .transpose()?;
        if let Some(launches) = &launches {
            ensure!(
                !launches.is_empty()
                    && launches.len() <= MAX_TEMPLATE_LAUNCHES
                    && launches.len() == geometry.pane_count(),
                "stored template launches do not match geometry panes"
            );
            for settings in launches {
                validate_launch_settings(settings)?;
            }
        }
        layout_templates.push(LayoutTemplate {
            id: Uuid::parse_str(&id)?,
            name,
            geometry,
            launches,
            revision,
        });
    }
    let catalog = Catalog {
        projects,
        workspaces,
        tabs,
        selection,
        layout,
        layout_templates: Some(layout_templates),
        template_launch_schema: Some(1),
        pane_labels: Some(read_pane_labels(connection, pane_label_count)?),
        agent_profiles: Some(read_agent_profiles(connection, agent_profile_count)?),
        prompts: Some(read_prompts(connection)?),
    };
    // This shared reader also runs inside apply_catalog's uncommitted
    // transaction. Existing oversized state fails explicitly without changing
    // source records; newly oversized writes roll back before durable commit.
    serde_json::to_writer(JsonByteCounter(0), &catalog)
        .context("catalog exceeds encoded JSON byte bound (7 MiB)")?;
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LaunchState;
    use std::os::unix::fs::PermissionsExt;

    fn one_pane_template() -> pigade_layout::GeometryTemplate {
        pigade_layout::GeometryTemplate {
            schema: 1,
            root: Some(pigade_layout::TemplateNode::Pane),
        }
    }

    fn profile(id: Uuid, revision: u64, provider: &str) -> AgentProfile {
        AgentProfile {
            id,
            name: "Review profile".into(),
            settings: LaunchSettings {
                provider: provider.into(),
                role: "reviewer".into(),
                instructions: "Review the change".into(),
                deliver_context: false,
            },
            revision,
        }
    }

    #[test]
    fn agent_profiles_use_cas_and_survive_reopen() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let id = Uuid::new_v4();
        {
            let mut store = MetadataStore::open(&path)?;
            let catalog = store.apply_catalog(CatalogBatch {
                agent_profiles: vec![CatalogWrite {
                    value: profile(id, 0, "claude"),
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            })?;
            assert_eq!(catalog.agent_profiles.as_ref().unwrap()[0].revision, 1);
            let mut updated = profile(id, 1, "codex");
            updated.name = "Updated profile".into();
            let catalog = store.apply_catalog(CatalogBatch {
                agent_profiles: vec![CatalogWrite {
                    value: updated,
                    expected_revision: Some(1),
                }],
                ..CatalogBatch::default()
            })?;
            let saved = catalog.agent_profiles.as_ref().unwrap();
            assert_eq!(saved.len(), 1);
            assert_eq!(saved[0].revision, 2);
            assert_eq!(saved[0].settings.provider, "codex");
            assert!(
                store
                    .apply_catalog(CatalogBatch {
                        agent_profiles: vec![CatalogWrite {
                            value: profile(id, 1, "claude"),
                            expected_revision: Some(1),
                        }],
                        ..CatalogBatch::default()
                    })
                    .is_err()
            );
        }
        let mut store = MetadataStore::open(&path)?;
        assert_eq!(
            store.catalog()?.agent_profiles.unwrap()[0]
                .settings
                .provider,
            "codex"
        );
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    remove_agent_profiles: vec![AgentProfileDelete {
                        id,
                        expected_revision: 1,
                    }],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        store.apply_catalog(CatalogBatch {
            remove_agent_profiles: vec![AgentProfileDelete {
                id,
                expected_revision: 2,
            }],
            ..CatalogBatch::default()
        })?;
        assert!(store.catalog()?.agent_profiles.unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn agent_profiles_reject_shell_provider_and_capacity_overflow() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("state.sqlite"))?;
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    agent_profiles: vec![CatalogWrite {
                        value: profile(Uuid::new_v4(), 0, "shell"),
                        expected_revision: None,
                    }],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        let writes = (0..=MAX_AGENT_PROFILES)
            .map(|_| CatalogWrite {
                value: profile(Uuid::new_v4(), 0, "opencode"),
                expected_revision: None,
            })
            .collect();
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    agent_profiles: writes,
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn schema_v10_migration_adds_profiles_and_preserves_catalog_backup() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        {
            let mut store = MetadataStore::open(&path)?;
            store.apply_catalog(CatalogBatch {
                layout_templates: vec![CatalogWrite {
                    value: LayoutTemplate {
                        id: Uuid::new_v4(),
                        name: "Existing template".into(),
                        geometry: one_pane_template(),
                        launches: None,
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            })?;
        }
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch(
                "DROP TABLE prompt_records; DROP TABLE agent_profiles; PRAGMA user_version=10;",
            )?;
        }
        let store = MetadataStore::open(&path)?;
        let catalog = store.catalog()?;
        assert_eq!(catalog.agent_profiles, Some(Vec::new()));
        assert_eq!(
            catalog.layout_templates.unwrap()[0].name,
            "Existing template"
        );
        assert_eq!(
            store
                .connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            12
        );
        let backup_path = store
            .migration_backup_path()
            .context("schema-10 backup absent")?;
        assert!(backup_path.to_string_lossy().contains(".pre-v12."));
        let backup =
            Connection::open_with_flags(backup_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        assert_eq!(
            backup.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            10
        );
        assert_eq!(
            backup.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='agent_profiles'",
                [],
                |row| row.get(0),
            )?,
            0
        );
        Ok(())
    }

    #[test]
    fn schema_v11_migration_adds_prompt_library_and_preserves_catalog_backup() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite");
        let profile_id = Uuid::new_v4();
        {
            let mut store = MetadataStore::open(&path)?;
            store.apply_catalog(CatalogBatch {
                agent_profiles: vec![CatalogWrite {
                    value: profile(profile_id, 0, "codex"),
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            })?;
        }
        {
            let connection = Connection::open(&path)?;
            connection.execute_batch("DROP TABLE prompt_records; PRAGMA user_version=11;")?;
        }
        let store = MetadataStore::open(&path)?;
        let catalog = store.catalog()?;
        assert_eq!(catalog.prompts, Some(Vec::new()));
        assert_eq!(catalog.agent_profiles.unwrap()[0].id, profile_id);
        assert_eq!(
            store
                .connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            12
        );
        let backup_path = store
            .migration_backup_path()
            .context("schema-11 backup absent")?;
        assert!(backup_path.to_string_lossy().contains(".pre-v12."));
        let backup =
            Connection::open_with_flags(backup_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        assert_eq!(
            backup.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            11
        );
        assert_eq!(
            backup.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='agent_profiles'",
                [],
                |row| row.get(0),
            )?,
            1
        );
        assert_eq!(
            backup.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='prompt_records'",
                [],
                |row| row.get(0),
            )?,
            0
        );
        Ok(())
    }

    #[test]
    fn geometry_templates_use_cas_and_atomic_delete() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("state.sqlite"))?;
        let id = Uuid::new_v4();
        let mut saved = store.apply_catalog(CatalogBatch {
            layout_templates: vec![CatalogWrite {
                value: LayoutTemplate {
                    id,
                    name: "Compact".into(),
                    geometry: one_pane_template(),
                    launches: None,
                    revision: 0,
                },
                expected_revision: None,
            }],
            ..CatalogBatch::default()
        })?;
        let template = saved.layout_templates.as_mut().unwrap().remove(0);
        assert_eq!(template.revision, 1);
        let mut update = template.clone();
        update.name = "Wide".into();
        saved = store.apply_catalog(CatalogBatch {
            layout_templates: vec![CatalogWrite {
                value: LayoutTemplate {
                    revision: template.revision,
                    ..update
                },
                expected_revision: Some(template.revision),
            }],
            ..CatalogBatch::default()
        })?;
        let updated = saved.layout_templates.unwrap().remove(0);
        assert_eq!((updated.name.as_str(), updated.revision), ("Wide", 2));
        let failed = store.apply_catalog(CatalogBatch {
            remove_layout_templates: vec![LayoutTemplateDelete {
                id,
                expected_revision: 1,
            }],
            ..CatalogBatch::default()
        });
        assert!(failed.is_err());
        assert_eq!(store.catalog()?.layout_templates.unwrap().len(), 1);
        store.apply_catalog(CatalogBatch {
            remove_layout_templates: vec![LayoutTemplateDelete {
                id,
                expected_revision: 2,
            }],
            ..CatalogBatch::default()
        })?;
        assert!(store.catalog()?.layout_templates.unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn invalid_or_duplicate_geometry_templates_rollback_batch() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("state.sqlite"))?;
        let invalid = pigade_layout::GeometryTemplate {
            schema: 99,
            root: Some(pigade_layout::TemplateNode::Pane),
        };
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    layout_templates: vec![CatalogWrite {
                        value: LayoutTemplate {
                            id: Uuid::new_v4(),
                            name: "Bad".into(),
                            geometry: invalid,
                            launches: None,
                            revision: 0
                        },
                        expected_revision: None
                    }],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        let make = |id| CatalogWrite {
            value: LayoutTemplate {
                id,
                name: "Same".into(),
                geometry: one_pane_template(),
                launches: None,
                revision: 0,
            },
            expected_revision: None,
        };
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    layout_templates: vec![make(Uuid::new_v4()), make(Uuid::new_v4())],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        assert!(store.catalog()?.layout_templates.unwrap().is_empty());
        Ok(())
    }

    #[test]
    fn template_launch_settings_are_additive_bounded_and_match_geometry() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("state.sqlite"))?;
        let settings = LaunchSettings {
            provider: "codex".into(),
            role: "reviewer".into(),
            instructions: "Review the implementation".into(),
            deliver_context: true,
        };
        settings.validate()?;
        let id = Uuid::new_v4();
        let saved = store.apply_catalog(CatalogBatch {
            layout_templates: vec![CatalogWrite {
                value: LayoutTemplate {
                    id,
                    name: "Configured".into(),
                    geometry: one_pane_template(),
                    launches: Some(vec![settings.clone()]),
                    revision: 0,
                },
                expected_revision: None,
            }],
            ..CatalogBatch::default()
        })?;
        assert_eq!(saved.template_launch_schema, Some(1));
        assert_eq!(
            saved.layout_templates.unwrap()[0].launches,
            Some(vec![settings])
        );

        for invalid in [
            LaunchSettings {
                provider: "other".into(),
                role: String::new(),
                instructions: String::new(),
                deliver_context: false,
            },
            LaunchSettings {
                provider: "shell".into(),
                role: "x".repeat(8 * 1024 + 1),
                instructions: String::new(),
                deliver_context: false,
            },
            LaunchSettings {
                provider: "claude".into(),
                role: String::new(),
                instructions: "x".repeat(64 * 1024 + 1),
                deliver_context: false,
            },
            LaunchSettings {
                provider: "opencode".into(),
                role: "\0".into(),
                instructions: String::new(),
                deliver_context: false,
            },
        ] {
            assert!(invalid.validate().is_err());
            let result = store.apply_catalog(CatalogBatch {
                layout_templates: vec![CatalogWrite {
                    value: LayoutTemplate {
                        id: Uuid::new_v4(),
                        name: "invalid".into(),
                        geometry: one_pane_template(),
                        launches: Some(vec![invalid]),
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            });
            assert!(result.is_err());
        }
        let result = store.apply_catalog(CatalogBatch {
            layout_templates: vec![CatalogWrite {
                value: LayoutTemplate {
                    id: Uuid::new_v4(),
                    name: "mismatch".into(),
                    geometry: one_pane_template(),
                    launches: Some(vec![
                        LaunchSettings {
                            provider: "shell".into(),
                            role: String::new(),
                            instructions: String::new(),
                            deliver_context: false,
                        };
                        2
                    ]),
                    revision: 0,
                },
                expected_revision: None,
            }],
            ..CatalogBatch::default()
        });
        assert!(result.is_err());
        assert_eq!(store.catalog()?.layout_templates.unwrap().len(), 1);
        Ok(())
    }

    #[test]
    fn pane_labels_use_revisioned_tombstones_and_validate_names() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir()?;
        let mut store = MetadataStore::open(dir.path().join("state.sqlite"))?;
        let session = Uuid::new_v4();
        let saved = store.save_pane_label(CatalogWrite {
            value: PaneLabel {
                session,
                name: "Reviewer".into(),
                revision: 0,
            },
            expected_revision: None,
        })?;
        assert_eq!(
            saved.pane_labels.as_ref().unwrap(),
            &[PaneLabel {
                session,
                name: "Reviewer".into(),
                revision: 1,
            }]
        );
        let reset = store.save_pane_label(CatalogWrite {
            value: PaneLabel {
                session,
                name: String::new(),
                revision: 1,
            },
            expected_revision: Some(1),
        })?;
        assert_eq!(reset.pane_labels.as_ref().unwrap()[0].name, "");
        assert_eq!(reset.pane_labels.as_ref().unwrap()[0].revision, 2);
        assert!(
            store
                .save_pane_label(CatalogWrite {
                    value: PaneLabel {
                        session,
                        name: "stale".into(),
                        revision: 1,
                    },
                    expected_revision: Some(1),
                })
                .is_err()
        );
        for name in [
            " bad".to_string(),
            "bad ".to_string(),
            "line\nbreak".to_string(),
            "nul\0name".to_string(),
            "x".repeat(513),
        ] {
            assert!(
                store
                    .save_pane_label(CatalogWrite {
                        value: PaneLabel {
                            session: Uuid::new_v4(),
                            name,
                            revision: 0,
                        },
                        expected_revision: None,
                    })
                    .is_err()
            );
        }
        Ok(())
    }

    fn project(id: Uuid, name: &str) -> Project {
        Project {
            id,
            canonical_path: PathBuf::from(format!("/catalog-test/{id}")),
            git_common_dir: PathBuf::from(format!("/catalog-test/{id}/.git")),
            name: name.into(),
            roots: vec![NamedRoot {
                id: Uuid::new_v4(),
                name: "backend".into(),
                relative_path: "server".into(),
            }],
            revision: 0,
        }
    }
    fn workspace(project_id: Uuid) -> Workspace {
        Workspace {
            id: Uuid::new_v4(),
            project_id,
            task: "Реализовать задачу".into(),
            cwd: "/catalog-test/worktree".into(),
            mode: WorkspaceMode::Worktree,
            status: WorkspaceStatus::Ready,
            error: None,
            context_revision: None,
            revision: 0,
        }
    }
    fn tab(workspace_id: Uuid) -> Tab {
        Tab {
            id: Uuid::new_v4(),
            workspace_id,
            name: "Разработка".into(),
            layout_json: "{\"tree\":null}".into(),
            active_pane: None,
            revision: 0,
        }
    }
    fn new<T>(value: T) -> CatalogWrite<T> {
        CatalogWrite {
            value,
            expected_revision: None,
        }
    }
    fn update<T>(value: T, revision: u64) -> CatalogWrite<T> {
        CatalogWrite {
            value,
            expected_revision: Some(revision),
        }
    }
    fn seed(store: &mut MetadataStore) -> Result<Catalog> {
        let p = project(Uuid::new_v4(), "PigADE");
        let w = workspace(p.id);
        let t = tab(w.id);
        let selection = Selection {
            project_id: Some(p.id),
            workspace_id: Some(w.id),
            tab_id: Some(t.id),
            window: WindowState {
                width: 1440,
                height: 900,
                scale: 1.25,
                ..WindowState::default()
            },
            revision: 0,
        };
        store.apply_catalog(CatalogBatch {
            projects: vec![new(p)],
            workspaces: vec![new(w)],
            tabs: vec![new(t)],
            selection: Some(update(selection, 0)),
            layout: None,
            ..CatalogBatch::default()
        })
    }
    #[test]
    fn hierarchy_selection_and_window_commit_and_reopen_together() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let path = root.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        assert_eq!(store.catalog()?.selection, Selection::default());
        let committed = seed(&mut store)?;
        assert_eq!(committed.projects[0].revision, 1);
        assert_eq!(committed.workspaces[0].revision, 1);
        assert_eq!(committed.tabs[0].revision, 1);
        assert_eq!(committed.selection.revision, 1);
        drop(store);
        let store = MetadataStore::open(path)?;
        assert_eq!(store.catalog()?, committed);
        assert!(store.migration_backup_path().is_none());
        Ok(())
    }

    #[test]
    fn canvas_offsets_are_bounded_tab_scoped_and_survive_reopen() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let path = root.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        let seeded = seed(&mut store)?;
        let tab = seeded.tabs[0].id;
        let expected_offset = CanvasOffset { x: 42.5, y: 900.0 };
        let mut selection = seeded.selection.clone();
        selection.window.canvas_offsets.insert(tab, expected_offset);
        let committed = store.apply_catalog(CatalogBatch {
            selection: Some(update(selection, seeded.selection.revision)),
            ..CatalogBatch::default()
        })?;
        assert_eq!(
            committed.selection.window.canvas_offsets.get(&tab),
            Some(&expected_offset)
        );
        drop(store);

        let reopened = MetadataStore::open(path)?;
        assert_eq!(
            reopened
                .catalog()?
                .selection
                .window
                .canvas_offsets
                .get(&tab),
            Some(&expected_offset)
        );

        let legacy: WindowState = serde_json::from_str(
            r#"{"x":0,"y":0,"width":1280,"height":800,"scale":1.0,"sidebar_width":240,"sidebar_collapsed":false,"monitor":null}"#,
        )?;
        assert!(legacy.canvas_offsets.is_empty());

        for invalid in [
            CanvasOffset { x: -0.1, y: 0.0 },
            CanvasOffset {
                x: f64::INFINITY,
                y: 0.0,
            },
            CanvasOffset {
                x: 1.0e9 + 1.0,
                y: 0.0,
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        let mut invalid_selection = reopened.catalog()?.selection;
        invalid_selection.window.canvas_offsets.insert(
            tab,
            CanvasOffset {
                x: f64::NAN,
                y: 0.0,
            },
        );
        drop(reopened);
        let mut writable = MetadataStore::open(root.path().join("state.sqlite"))?;
        assert!(
            writable
                .apply_catalog(CatalogBatch {
                    selection: Some(update(invalid_selection, committed.selection.revision)),
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn panel_generation_map_is_additive_bounded_and_survives_catalog_cas() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let old_window: WindowState = serde_json::from_value(serde_json::json!({
            "x": 10,
            "y": 20,
            "width": 1280,
            "height": 800,
            "scale": 1.0,
            "sidebar_width": 240,
            "sidebar_collapsed": false,
            "monitor": null,
            "canvas_offsets": {}
        }))?;
        assert!(old_window.panel_generations.is_empty());
        let mut roundtrip = old_window.clone();
        let session = Uuid::new_v4();
        let generation = Uuid::new_v4();
        roundtrip.panel_generations.insert(session, generation);
        assert_eq!(
            serde_json::from_str::<WindowState>(&serde_json::to_string(&roundtrip)?)?,
            roundtrip
        );

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        let seeded = seed(&mut store)?;
        let mut selection = seeded.selection.clone();
        selection.window.panel_generations = roundtrip.panel_generations.clone();
        let committed = store.apply_catalog(CatalogBatch {
            selection: Some(update(selection, seeded.selection.revision)),
            ..CatalogBatch::default()
        })?;
        assert_eq!(
            committed.selection.window.panel_generations,
            roundtrip.panel_generations
        );
        drop(store);
        let mut store = MetadataStore::open(&path)?;
        let current = store.catalog()?;
        assert_eq!(
            current.selection.window.panel_generations,
            roundtrip.panel_generations
        );

        let mut viewport_update = current.selection.clone();
        viewport_update
            .window
            .canvas_offsets
            .insert(current.tabs[0].id, CanvasOffset { x: 20.0, y: 30.0 });
        let after_viewport = store.apply_catalog(CatalogBatch {
            selection: Some(update(viewport_update, current.selection.revision)),
            ..CatalogBatch::default()
        })?;
        assert_eq!(
            after_viewport.selection.window.panel_generations,
            roundtrip.panel_generations
        );

        let mut invalid = after_viewport.selection.clone();
        invalid
            .window
            .panel_generations
            .insert(Uuid::nil(), generation);
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    selection: Some(update(invalid, after_viewport.selection.revision)),
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        let mut over_bound = BTreeMap::new();
        for value in 1..=100_001 {
            over_bound.insert(Uuid::from_u128(value), Uuid::new_v4());
        }
        assert!(validate_panel_generations(&over_bound).is_err());
        Ok(())
    }
    #[test]
    fn encoded_catalog_bound_rejects_escaping_growth_before_commit() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let path = root.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        let mut before = seed(&mut store)?;
        let session = Uuid::new_v4();
        let operation = Uuid::new_v4();
        store.begin_launch(session, Uuid::new_v4(), operation, Uuid::new_v4())?;
        let launch = store.mark_unknown(operation, "wire-budget regression")?;
        let mut rejected = false;
        for index in 0..22 {
            let mut added = workspace(before.projects[0].id);
            added.task = format!("Task{}", "\u{1}".repeat(65_532));
            let mut project = before.projects[0].clone();
            project.name = format!("Revision {index}");
            let mut selection = before.selection.clone();
            selection.window.sidebar_collapsed = !selection.window.sidebar_collapsed;

            let mut candidate = before.clone();
            let mut persisted_added = added.clone();
            persisted_added.revision = 1;
            candidate.workspaces.push(persisted_added);
            candidate.projects[0] = project.clone();
            candidate.projects[0].revision += 1;
            candidate.selection = selection.clone();
            candidate.selection.revision += 1;
            let encoded = serde_json::to_vec(&candidate)?.len();
            let result = store.apply_catalog(CatalogBatch {
                projects: vec![update(project, before.projects[0].revision)],
                workspaces: vec![new(added.clone())],
                selection: Some(update(selection, before.selection.revision)),
                ..CatalogBatch::default()
            });
            if encoded <= MAX_ENCODED_CATALOG_BYTES {
                before = result?;
                assert_eq!(before, candidate);
            } else {
                let error = result.unwrap_err();
                assert!(error.to_string().contains("encoded JSON byte bound"));
                assert_eq!(store.catalog()?, before);
                assert_eq!(
                    store.connection.query_row::<usize, _, _>(
                        "SELECT COUNT(*) FROM workspaces WHERE id=?1",
                        [added.id.to_string()],
                        |row| row.get(0)
                    )?,
                    0
                );
                assert!(added.task.len() * (index + 1) < MAX_MANIFEST_BYTES);
                assert_eq!(store.get_launch(operation)?.unwrap(), launch);
                assert!(
                    store
                        .begin_launch(session, Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())
                        .is_err()
                );
                eprintln!(
                    "catalog wire limit: accepted={} bytes, rejected={} bytes, attempted control tasks={}",
                    serde_json::to_vec(&before)?.len(),
                    encoded,
                    index + 1
                );
                rejected = true;
                break;
            }
        }
        assert!(
            rejected,
            "regression must cross the encoded limit below the SQL source limit"
        );
        drop(store);
        let store = MetadataStore::open(path)?;
        assert_eq!(store.catalog()?, before);
        assert_eq!(store.get_launch(operation)?.unwrap(), launch);
        Ok(())
    }
    #[test]
    fn existing_oversized_wire_catalog_is_rejected_without_truncation() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let path = root.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        let before = seed(&mut store)?;
        let session = Uuid::new_v4();
        let operation = Uuid::new_v4();
        store.begin_launch(session, Uuid::new_v4(), operation, Uuid::new_v4())?;
        let launch = store.mark_unknown(operation, "legacy oversized catalog")?;
        let task = format!("Task{}", "\u{1}".repeat(65_532));
        // Emulate already-committed state produced by the former source-only
        // bound. Bypass apply_catalog only to construct this legacy fixture.
        let tx = store.connection.transaction()?;
        for _ in 0..22 {
            let mut added = workspace(before.projects[0].id);
            added.task = task.clone();
            write_workspace(&tx, new(added))?;
        }
        tx.commit()?;
        for reopened in [false, true] {
            if reopened {
                drop(store);
                store = MetadataStore::open(&path)?;
            }
            assert!(
                store
                    .catalog()
                    .unwrap_err()
                    .to_string()
                    .contains("encoded JSON byte bound")
            );
            assert_eq!(
                store.connection.query_row::<usize, _, _>(
                    "SELECT COUNT(*) FROM workspaces",
                    [],
                    |row| row.get(0)
                )?,
                23
            );
            assert_eq!(
                store.connection.query_row::<usize, _, _>(
                    "SELECT COUNT(*) FROM workspaces WHERE task=?1 AND revision=1",
                    [&task],
                    |row| row.get(0)
                )?,
                22
            );
            assert_eq!(
                store.connection.query_row::<u64, _, _>(
                    "SELECT revision FROM catalog_selection WHERE singleton=1",
                    [],
                    |row| row.get(0)
                )?,
                before.selection.revision
            );
            assert_eq!(store.get_launch(operation)?.unwrap(), launch);
            assert!(
                store
                    .begin_launch(session, Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())
                    .is_err()
            );
        }
        Ok(())
    }
    #[test]
    fn catalog_keeps_valid_newlines_unicode_and_json_escapes() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let path = root.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        let before = seed(&mut store)?;
        let mut changed = before.workspaces[0].clone();
        changed.task = "Задача\n日本語 🐷\t\"quoted\"\\\u{1}".into();
        let after = store.apply_catalog(CatalogBatch {
            workspaces: vec![update(changed.clone(), 1)],
            ..CatalogBatch::default()
        })?;
        assert_eq!(after.workspaces[0].task, changed.task);
        drop(store);
        assert_eq!(MetadataStore::open(path)?.catalog()?, after);
        Ok(())
    }
    #[test]
    fn stale_update_rolls_back_all_tabs_and_selection() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let mut store = MetadataStore::open(root.path().join("state.sqlite"))?;
        let first = seed(&mut store)?;
        let second_tab = tab(first.workspaces[0].id);
        let before = store.apply_catalog(CatalogBatch {
            tabs: vec![new(second_tab)],
            ..CatalogBatch::default()
        })?;
        let mut a = before.tabs[0].clone();
        a.layout_json = "{\"pane\":\"moved\"}".into();
        let mut b = before.tabs[1].clone();
        b.layout_json = "{}".into();
        b.revision = 2;
        let mut selected = before.selection.clone();
        selected.tab_id = Some(b.id);
        let error = store
            .apply_catalog(CatalogBatch {
                tabs: vec![update(a, 1), update(b, 2)],
                selection: Some(update(selected, 1)),
                ..CatalogBatch::default()
            })
            .unwrap_err();
        assert!(error.downcast_ref::<RevisionConflict>().is_some());
        assert_eq!(store.catalog()?, before);
        let mut a = before.tabs[0].clone();
        a.layout_json = "{\"pane\":\"moved\"}".into();
        let mut b = before.tabs[1].clone();
        b.layout_json = "{}".into();
        let after = store.apply_catalog(CatalogBatch {
            tabs: vec![update(a, 1), update(b, 1)],
            ..CatalogBatch::default()
        })?;
        assert!(after.tabs.iter().all(|tab| tab.revision == 2));
        Ok(())
    }
    #[test]
    fn global_layout_conflict_rolls_back_tabs_and_selection() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let path = root.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        let before = seed(&mut store)?;
        let mut changed_tab = before.tabs[0].clone();
        changed_tab.layout_json = "{\"tree\":\"moved\"}".into();
        let mut selected = before.selection.clone();
        selected.window.sidebar_collapsed = true;
        let error = store
            .apply_catalog(CatalogBatch {
                tabs: vec![update(changed_tab.clone(), 1)],
                selection: Some(update(selected.clone(), 1)),
                layout: Some(update(
                    LayoutDocument {
                        json: "{\"undo\":[\"transfer\"]}".into(),
                        revision: 1,
                    },
                    1,
                )),
                ..CatalogBatch::default()
            })
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<RevisionConflict>().unwrap().entity,
            "layout history"
        );
        assert_eq!(store.catalog()?, before);
        let committed = store.apply_catalog(CatalogBatch {
            tabs: vec![update(changed_tab, 1)],
            selection: Some(update(selected, 1)),
            layout: Some(update(
                LayoutDocument {
                    json: "{\"undo\":[\"transfer\"]}".into(),
                    revision: 0,
                },
                0,
            )),
            ..CatalogBatch::default()
        })?;
        assert_eq!(committed.tabs[0].revision, 2);
        assert_eq!(committed.selection.revision, 2);
        assert_eq!(committed.layout.revision, 1);
        drop(store);
        assert_eq!(MetadataStore::open(path)?.catalog()?, committed);
        Ok(())
    }
    #[test]
    fn owners_foreign_selection_roots_and_duplicate_ids_are_checked() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let mut store = MetadataStore::open(root.path().join("state.sqlite"))?;
        let first = seed(&mut store)?;
        let p = project(Uuid::new_v4(), "Other");
        let w = workspace(p.id);
        let t = tab(w.id);
        let before = store.apply_catalog(CatalogBatch {
            projects: vec![new(p)],
            workspaces: vec![new(w)],
            tabs: vec![new(t)],
            ..CatalogBatch::default()
        })?;
        let mut stolen = first.workspaces[0].clone();
        stolen.project_id = before.projects[1].id;
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    workspaces: vec![update(stolen, 1)],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        let mut stolen = first.tabs[0].clone();
        stolen.workspace_id = before.workspaces[1].id;
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    tabs: vec![update(stolen, 1)],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        let mut selected = before.selection.clone();
        selected.tab_id = Some(before.tabs[1].id);
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    selection: Some(update(selected, 1)),
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        let mut bad_root = before.projects[0].clone();
        bad_root.roots[0].relative_path = "../secret".into();
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    projects: vec![update(bad_root, 1)],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        let existing = before.tabs[0].clone();
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    tabs: vec![update(existing.clone(), 1), update(existing, 1)],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        for json in ["{".into(), format!("\"{}\"", "x".repeat(2 * 1024 * 1024))] {
            assert!(
                store
                    .apply_catalog(CatalogBatch {
                        layout: Some(update(LayoutDocument { json, revision: 0 }, 0)),
                        ..CatalogBatch::default()
                    })
                    .is_err()
            );
        }
        assert_eq!(store.catalog()?, before);
        Ok(())
    }
    #[test]
    fn prepared_worktree_failure_is_saved_without_checkout_substitution() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let mut store = MetadataStore::open(root.path().join("state.sqlite"))?;
        let before = seed(&mut store)?;
        let mut failed = workspace(before.projects[0].id);
        failed.cwd = "/missing/proposed-worktree".into();
        failed.status = WorkspaceStatus::Failed;
        failed.error = Some("git worktree add failed".into());
        let after = store.apply_catalog(CatalogBatch {
            workspaces: vec![new(failed.clone())],
            ..CatalogBatch::default()
        })?;
        assert_eq!(after.workspaces[1].cwd, failed.cwd);
        assert_eq!(after.workspaces[1].mode, WorkspaceMode::Worktree);
        assert_eq!(after.workspaces[1].error, failed.error);
        Ok(())
    }
    #[test]
    fn invalid_json_and_oversized_manifest_roll_back() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let mut store = MetadataStore::open(root.path().join("state.sqlite"))?;
        let before = seed(&mut store)?;
        let mut invalid = tab(before.workspaces[0].id);
        invalid.layout_json = "{".into();
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    tabs: vec![new(invalid)],
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        let mut tabs = Vec::new();
        for _ in 0..5 {
            let mut large = tab(before.workspaces[0].id);
            large.layout_json = format!("\"{}\"", "x".repeat(MAX_LAYOUT_BYTES - 2));
            tabs.push(new(large));
        }
        assert!(
            store
                .apply_catalog(CatalogBatch {
                    tabs,
                    ..CatalogBatch::default()
                })
                .is_err()
        );
        assert_eq!(store.catalog()?, before);
        Ok(())
    }
    fn legacy(path: &Path) -> Result<(Connection, Uuid)> {
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "wal_autocheckpoint", 0)?;
        connection.execute_batch("CREATE TABLE launch_intents (operation_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,generation TEXT NOT NULL UNIQUE,nonce TEXT NOT NULL,state TEXT NOT NULL,identity TEXT,reason TEXT);
          CREATE TABLE generation_locks(session_id TEXT PRIMARY KEY,generation TEXT NOT NULL UNIQUE,operation_id TEXT NOT NULL UNIQUE REFERENCES launch_intents(operation_id)); PRAGMA user_version=1;")?;
        let op = Uuid::new_v4();
        let session = Uuid::new_v4();
        let generation = Uuid::new_v4();
        connection.execute("INSERT INTO launch_intents(operation_id,session_id,generation,nonce,state)VALUES(?1,?2,?3,?4,'unknown')",params![op.to_string(),session.to_string(),generation.to_string(),Uuid::new_v4().to_string()])?;
        connection.execute(
            "INSERT INTO generation_locks(session_id,generation,operation_id)VALUES(?1,?2,?3)",
            params![session.to_string(), generation.to_string(), op.to_string()],
        )?;
        Ok((connection, op))
    }
    #[test]
    fn v1_migration_backup_includes_committed_wal_and_preserves_launch_locks() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let path = root.path().join("state'quoted.sqlite");
        let (_legacy, op) = legacy(&path)?;
        let mut wal_path = path.as_os_str().to_os_string();
        wal_path.push("-wal");
        assert!(std::fs::metadata(wal_path)?.len() > 0);
        let mut store = MetadataStore::open(&path)?;
        assert_eq!(
            store
                .connection
                .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            12
        );
        let backup = store.migration_backup_path().context("v1 backup absent")?;
        assert_eq!(
            std::fs::metadata(backup)?.permissions().mode() & 0o777,
            0o600
        );
        let reader =
            Connection::open_with_flags(backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        assert_eq!(
            reader.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            1
        );
        let state: String = reader.query_row(
            "SELECT state FROM launch_intents WHERE operation_id=?1",
            [op.to_string()],
            |row| row.get(0),
        )?;
        assert_eq!(state, "unknown");
        assert_eq!(
            reader.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM generation_locks",
                [],
                |row| row.get(0)
            )?,
            1
        );
        let record = store.get_launch(op)?.unwrap();
        assert_eq!(record.state, LaunchState::Unknown);
        assert!(
            store
                .begin_launch(
                    record.session_id,
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    Uuid::new_v4()
                )
                .is_err()
        );
        assert_eq!(store.catalog()?.selection.revision, 0);
        Ok(())
    }
    #[test]
    fn v2_migration_preserves_catalog_launch_locks_and_optional_early_layout() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        for early_layout in [false, true] {
            let root = tempfile::tempdir()?;
            let path = root.path().join("state.sqlite");
            let (mut legacy, op) = legacy(&path)?;
            let p = project(Uuid::new_v4(), "Existing v2 project");
            let w = workspace(p.id);
            let t = tab(w.id);
            let selected = Selection {
                project_id: Some(p.id),
                workspace_id: Some(w.id),
                tab_id: Some(t.id),
                ..Selection::default()
            };
            let expected_layout = if early_layout {
                LayoutDocument {
                    json: "{\"undo\":[\"preserved\"]}".into(),
                    revision: 7,
                }
            } else {
                LayoutDocument::default()
            };
            let tx = legacy.transaction_with_behavior(TransactionBehavior::Immediate)?;
            migrate_catalog(&tx)?;
            write_project(&tx, new(p.clone()))?;
            write_workspace(&tx, new(w.clone()))?;
            write_tab(&tx, new(t.clone()))?;
            write_selection(&tx, update(selected, 0))?;
            if early_layout {
                migrate_layout_document(&tx)?;
                tx.execute(
                    "UPDATE layout_revisions SET json=?1, revision=?2 WHERE singleton=1",
                    params![expected_layout.json, expected_layout.revision],
                )?;
            }
            tx.pragma_update(None, "user_version", 2)?;
            tx.commit()?;

            let mut store = MetadataStore::open(&path)?;
            let catalog = store.catalog()?;
            assert_eq!(catalog.projects[0].id, p.id);
            assert_eq!(catalog.projects[0].roots, p.roots);
            assert_eq!(catalog.workspaces[0].id, w.id);
            assert_eq!(catalog.tabs[0].id, t.id);
            assert_eq!(catalog.selection.tab_id, Some(t.id));
            assert_eq!(catalog.selection.revision, 1);
            assert_eq!(catalog.layout, expected_layout);
            assert_eq!(
                store
                    .connection
                    .pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
                12
            );
            let backup = Connection::open_with_flags(
                store.migration_backup_path().context("v2 backup absent")?,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )?;
            assert_eq!(
                backup.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
                2
            );
            assert_eq!(
                backup.query_row::<String, _, _>(
                    "SELECT name FROM projects WHERE id=?1",
                    [p.id.to_string()],
                    |row| row.get(0)
                )?,
                p.name
            );
            assert_eq!(
                backup.query_row::<usize, _, _>("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='layout_revisions'", [], |row| row.get(0))?,
                usize::from(early_layout)
            );
            let record = store.get_launch(op)?.unwrap();
            assert_eq!(record.state, LaunchState::Unknown);
            assert!(
                store
                    .begin_launch(
                        record.session_id,
                        Uuid::new_v4(),
                        Uuid::new_v4(),
                        Uuid::new_v4()
                    )
                    .is_err()
            );
            drop(store);
            let reopened = MetadataStore::open(&path)?;
            assert_eq!(reopened.catalog()?, catalog);
            assert!(reopened.migration_backup_path().is_none());
        }
        Ok(())
    }
    #[test]
    fn failed_migration_rolls_back_schema_and_retains_backup() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let path = root.path().join("state.sqlite");
        let (legacy, op) = legacy(&path)?;
        legacy.execute_batch("CREATE TABLE projects(existing TEXT)")?;
        assert!(MetadataStore::open(&path).is_err());
        assert_eq!(
            legacy.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            1
        );
        assert_eq!(
            legacy.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='project_roots'",
                [],
                |row| row.get(0)
            )?,
            0
        );
        assert_eq!(
            legacy.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM launch_intents WHERE operation_id=?1",
                [op.to_string()],
                |row| row.get(0)
            )?,
            1
        );
        assert_eq!(
            std::fs::read_dir(root.path())?
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".backup.sqlite"))
                .count(),
            1
        );
        Ok(())
    }
    #[test]
    fn inability_to_publish_backup_prevents_migration() -> Result<()> {
        let _serial = crate::STORAGE_TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir()?;
        let path = root.path().join("state.sqlite");
        let (legacy, _) = legacy(&path)?;
        let lock = root.path().join("state.sqlite.writer.lock");
        std::fs::write(lock, [])?;
        struct RestorePermissions(PathBuf);
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            }
        }
        let _restore = RestorePermissions(root.path().into());
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o500))?;
        assert!(MetadataStore::open(&path).is_err());
        assert_eq!(
            legacy.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            1
        );
        assert_eq!(
            legacy.query_row::<usize, _, _>(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='projects'",
                [],
                |row| row.get(0)
            )?,
            0
        );
        Ok(())
    }
}
