//! Bounded, read-only Git snapshot requests for one catalog workspace.
use std::ffi::CString;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::MetadataExt,
};

use anyhow::{Context, Result, bail, ensure};
use pigade_storage::{Catalog, WorkspaceStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[path = "workspace_git_commit.rs"]
mod commit;
#[path = "workspace_git_index.rs"]
mod index;
pub use commit::{
    GitCommitEntry, GitCommitPhase, WorkspaceGitCommitPlan, WorkspaceGitCommitResult,
};

pub(crate) fn run_commit_helper(operation: &Path) -> Result<()> {
    commit::run_helper(operation)
}
pub(crate) fn commit_hook_helper(operation: &Path, name: &str, args: &[String]) -> Result<i32> {
    commit::hook_helper(operation, name, args)
}

const OUTPUT_LIMIT: usize = 256 * 1024;
// Each identity-validation phase and the status command receive a fresh
// bounded deadline, so a timed-out status can still be revalidated and
// returned as `truncated` (unknown) instead of being mistaken for clean.
const COMMAND_DEADLINE: Duration = Duration::from_secs(3);
const OUTPUT_POLL: Duration = Duration::from_millis(10);
const MAX_ENTRIES: usize = 128;
const DIFF_OUTPUT_LIMIT: usize = 128 * 1024;
const DIFF_LINE_LIMIT: usize = 2000;
const MAX_DIFF_PATH_BYTES: usize = 4096;
const MAX_NOTICE_BYTES: usize = 1024;

pub(super) fn bounded_notice(value: impl AsRef<str>) -> String {
    let value = value.as_ref();
    if value.len() <= MAX_NOTICE_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_NOTICE_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitSnapshot {
    pub workspace_id: Uuid,
    pub workspace_revision: u64,
    pub project_id: Uuid,
    pub cwd: PathBuf,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitEntry {
    pub path: String,
    pub original_path: Option<String>,
    pub index_status: char,
    pub worktree_status: char,
    pub untracked: bool,
    pub unmerged: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitDiffSide {
    Index,
    Worktree,
    Untracked,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitDiff {
    pub workspace_id: Uuid,
    pub workspace_revision: u64,
    pub project_id: Uuid,
    pub cwd: PathBuf,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitIndexAction {
    Stage,
    Unstage,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitIndexTicket {
    pub id: Uuid,
    pub action: GitIndexAction,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitIndexPhase {
    Applied,
    Rejected,
    Unknown,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitIndexResult {
    pub ticket: Uuid,
    pub workspace_id: Uuid,
    pub workspace_revision: u64,
    pub project_id: Uuid,
    pub cwd: PathBuf,
    pub path: String,
    pub side: GitDiffSide,
    pub action: GitIndexAction,
    pub phase: GitIndexPhase,
    #[serde(default)]
    pub acknowledged: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub workspace_id: Uuid,
    pub workspace_revision: u64,
    pub project_id: Uuid,
    pub cwd: PathBuf,
    pub project_root: PathBuf,
    pub git_common_dir: PathBuf,
}

#[derive(Debug)]
enum Task {
    Snapshot {
        target: Target,
        reply: mpsc::SyncSender<crate::Response>,
    },
    Diff {
        target: Target,
        path: String,
        side: GitDiffSide,
        reply: mpsc::SyncSender<crate::Response>,
    },
    Index {
        target: Target,
        ticket: Uuid,
        inspect: bool,
        reply: mpsc::SyncSender<crate::Response>,
    },
    IndexAck {
        target: Target,
        ticket: Uuid,
        reply: mpsc::SyncSender<crate::Response>,
    },
    CommitPrepare {
        target: Target,
        reply: mpsc::SyncSender<crate::Response>,
    },
    Commit {
        target: Target,
        ticket: Uuid,
        message: Option<String>,
        inspect: bool,
        acknowledge: bool,
        reply: mpsc::SyncSender<crate::Response>,
    },
}

pub(crate) struct Reader {
    sender: mpsc::SyncSender<Task>,
    index_busy: Arc<AtomicBool>,
}

impl Reader {
    pub(crate) fn new(root: PathBuf) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Task>(1);
        // Until recovery has inspected durable commit attempts, metadata edits
        // must not rebind a workspace beneath a surviving native Git process.
        let index_busy = Arc::new(AtomicBool::new(true));
        let worker_busy = index_busy.clone();
        let receiver = Arc::new(Mutex::new(receiver));
        let worker_receiver = receiver.clone();
        thread::Builder::new()
            .name("pigade-workspace-git".into())
            .spawn(move || {
                let mut index = index::Engine::new(root.clone());
                let mut commit = commit::Engine::new(root);
                let mut commit_active = commit.is_running();
                worker_busy.store(commit_active, Ordering::Release);
                loop {
                    let was_running = commit_active;
                    commit.tick();
                    if was_running && !commit.is_running() {
                        worker_busy.store(false, Ordering::Release);
                    }
                    commit_active = commit.is_running();
                    let task = {
                        let receiver = worker_receiver.lock().expect("workspace Git worker queue");
                        if commit.is_running() {
                            match receiver.recv_timeout(Duration::from_millis(50)) {
                                Ok(task) => Some(task),
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(mpsc::RecvTimeoutError::Disconnected) => None,
                            }
                        } else { receiver.recv().ok() }
                    };
                    let Some(task) = task else { break };
                    match task {
                        Task::Snapshot { target, reply } => {
                            let response = match snapshot(target.clone()) {
                                Ok(mut snapshot) => {
                                    match index.pending_for_target(&target) {
                                        Ok(operation) => snapshot.index_operation = operation,
                                        Err(error) => snapshot.index_operation_unavailable_reason = Some(bounded_notice(format!("Git index operation status unavailable: {error:#}"))),
                                    }
                                    if let Err(error) = commit.availability(&target) {
                                        snapshot.commit_unavailable_reason = Some(bounded_notice(format!("Git commit unavailable: {error:#}")));
                                    }
                                    match commit.pending_for_target(&target) {
                                        Ok(operation) => snapshot.commit_operation = operation,
                                        Err(error) => snapshot.commit_unavailable_reason = Some(bounded_notice(format!("Git commit status unavailable: {error:#}"))),
                                    }
                                    if commit.is_running() { worker_busy.store(true, Ordering::Release); }
                                    response(Some(snapshot), None)
                                }
                                Err(error) => response(
                                    None,
                                    Some(format!("workspace Git status unavailable: {error:#}")),
                                ),
                            };
                            let _ = reply.try_send(response);
                        }
                        Task::Diff {
                            target,
                            path,
                            side,
                            reply,
                        } => {
                            let commit_block = match commit.pending_for_target(&target) {
                                Ok(Some(_)) => Some("Inspect and resolve the previous Git commit before changing the index".to_owned()),
                                Err(error) => Some(bounded_notice(format!("Git commit status unavailable: {error:#}"))),
                                Ok(None) => None,
                            };
                            let response = match diff(target, path, side, &mut index) {
                                Ok(mut diff) => {
                                    if let Some(reason) = commit_block {
                                        diff.index_ticket = None;
                                        diff.index_unavailable_reason = Some(reason);
                                    }
                                    diff_response(Some(diff), None)
                                },
                                Err(error) => diff_response(
                                    None,
                                    Some(format!("workspace Git diff unavailable: {error:#}")),
                                ),
                            };
                            let _ = reply.try_send(response);
                        }
                        Task::Index { target, ticket, inspect, reply } => {
                            let result = if !inspect {
                                match commit.pending_for_target(&target) {
                                    Ok(None) if !commit.is_running() => index.request(target, ticket, inspect),
                                    Ok(_) => index::unavailable_result(&target, ticket, "A Git commit is unresolved; inspect its result first".into()),
                                    Err(error) => index::unavailable_result(&target, ticket, bounded_notice(format!("Git commit status unavailable: {error:#}"))),
                                }
                            } else { index.request(target, ticket, inspect) };
                            let mut response = response(None, None);
                            response.workspace_git_index = Some(result);
                            let _ = reply.try_send(response);
                            if !inspect {
                                worker_busy.store(commit.is_running(), Ordering::Release);
                            }
                        }
                        Task::IndexAck { target, ticket, reply } => {
                            let result = index.acknowledge(&target, ticket);
                            let mut response = response(None, None);
                            response.workspace_git_index = Some(result);
                            let _ = reply.try_send(response);
                        }
                        Task::CommitPrepare { target, reply } => {
                            let result = index.pending_for_target(&target).and_then(|pending| {
                                ensure!(pending.is_none(), "A Git index operation has an unknown outcome; inspect and acknowledge it first");
                                commit.prepare(&target)
                            });
                            let mut response = response(None, None);
                            match result {
                                Ok(plan) => response.workspace_git_commit_plan = Some(plan),
                                Err(error) => response.error = Some(bounded_notice(format!("Git commit preview unavailable: {error:#}"))),
                            }
                            let _ = reply.try_send(response);
                        }
                        Task::Commit { target, ticket, message, inspect, acknowledge, reply } => {
                            let result = if acknowledge { commit.acknowledge(&target, ticket) }
                            else if inspect { commit.request(&target, ticket, message, true) }
                            else {
                                match index.pending_for_target(&target) {
                                    Ok(None) => commit.request(&target, ticket, message, false),
                                    Ok(Some(_)) => commit::unavailable_result(&target, ticket, "A Git index operation has an unknown outcome; inspect and acknowledge it first"),
                                    Err(error) => commit::unavailable_result(&target, ticket, bounded_notice(format!("Git index operation status unavailable: {error:#}"))),
                                }
                            };
                            let mut response = response(None, None);
                            response.workspace_git_commit = Some(result);
                            if !inspect || commit_active || commit.is_running() {
                                worker_busy.store(commit.is_running(), Ordering::Release);
                            }
                            commit_active = commit.is_running();
                            let _ = reply.try_send(response);
                        }
                    }
                }
            })?;
        Ok(Self { sender, index_busy })
    }

    pub(crate) fn index_busy(&self) -> bool {
        self.index_busy.load(Ordering::Acquire)
    }

    pub(crate) fn submit(&self, target: Target, reply: mpsc::SyncSender<crate::Response>) {
        match self.sender.try_send(Task::Snapshot { target, reply }) {
            Ok(()) => (),
            Err(mpsc::TrySendError::Full(Task::Snapshot { reply, .. })) => {
                let _ = reply.try_send(response(
                    None,
                    Some("workspace Git status queue is full".into()),
                ));
            }
            Err(mpsc::TrySendError::Full(Task::Diff { reply, .. })) => {
                let _ = reply.try_send(response(
                    None,
                    Some("workspace Git status queue is full".into()),
                ));
            }
            Err(mpsc::TrySendError::Disconnected(Task::Snapshot { reply, .. })) => {
                let _ = reply.try_send(response(
                    None,
                    Some("workspace Git status worker unavailable".into()),
                ));
            }
            Err(mpsc::TrySendError::Disconnected(Task::Diff { reply, .. })) => {
                let _ = reply.try_send(response(
                    None,
                    Some("workspace Git status worker unavailable".into()),
                ));
            }
            Err(mpsc::TrySendError::Full(_)) | Err(mpsc::TrySendError::Disconnected(_)) => {
                unreachable!("task variant changed")
            }
        }
    }

    pub(crate) fn submit_diff(
        &self,
        target: Target,
        path: String,
        side: GitDiffSide,
        reply: mpsc::SyncSender<crate::Response>,
    ) {
        match self.sender.try_send(Task::Diff {
            target,
            path,
            side,
            reply,
        }) {
            Ok(()) => (),
            Err(mpsc::TrySendError::Full(Task::Snapshot { reply, .. }))
            | Err(mpsc::TrySendError::Full(Task::Diff { reply, .. })) => {
                let _ = reply.try_send(diff_response(
                    None,
                    Some("workspace Git diff queue is full".into()),
                ));
            }
            Err(mpsc::TrySendError::Disconnected(Task::Snapshot { reply, .. }))
            | Err(mpsc::TrySendError::Disconnected(Task::Diff { reply, .. })) => {
                let _ = reply.try_send(diff_response(
                    None,
                    Some("workspace Git diff worker unavailable".into()),
                ));
            }
            Err(mpsc::TrySendError::Full(_)) | Err(mpsc::TrySendError::Disconnected(_)) => {
                unreachable!("task variant changed")
            }
        }
    }

    pub(crate) fn submit_index(
        &self,
        target: Target,
        ticket: Uuid,
        inspect: bool,
        reply: mpsc::SyncSender<crate::Response>,
    ) {
        if !inspect
            && self
                .index_busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            let mut response = response(None, None);
            response.workspace_git_index = Some(index::unavailable_result(
                &target,
                ticket,
                "another Git index operation is in progress".into(),
            ));
            let _ = reply.try_send(response);
            return;
        }
        match self.sender.try_send(Task::Index {
            target,
            ticket,
            inspect,
            reply,
        }) {
            Ok(()) => (),
            Err(mpsc::TrySendError::Full(Task::Index { target, reply, .. })) => {
                if !inspect {
                    self.index_busy.store(false, Ordering::Release);
                }
                let mut response = response(None, None);
                response.workspace_git_index = Some(index::unavailable_result(
                    &target,
                    ticket,
                    "workspace Git index queue is full".into(),
                ));
                let _ = reply.try_send(response);
            }
            Err(mpsc::TrySendError::Disconnected(Task::Index { target, reply, .. })) => {
                if !inspect {
                    self.index_busy.store(false, Ordering::Release);
                }
                let mut response = response(None, None);
                response.workspace_git_index = Some(index::unavailable_result(
                    &target,
                    ticket,
                    "workspace Git index worker unavailable".into(),
                ));
                let _ = reply.try_send(response);
            }
            Err(mpsc::TrySendError::Full(_)) | Err(mpsc::TrySendError::Disconnected(_)) => {
                unreachable!("task variant changed")
            }
        }
    }

    pub(crate) fn submit_commit_prepare(
        &self,
        target: Target,
        reply: mpsc::SyncSender<crate::Response>,
    ) {
        if let Err(error) = self.sender.try_send(Task::CommitPrepare { target, reply }) {
            let task = match error {
                mpsc::TrySendError::Full(task) | mpsc::TrySendError::Disconnected(task) => task,
            };
            if let Task::CommitPrepare { reply, .. } = task {
                let _ = reply.try_send(response(
                    None,
                    Some("Git commit preview worker is busy or unavailable".into()),
                ));
            }
        }
    }

    pub(crate) fn submit_commit(
        &self,
        target: Target,
        ticket: Uuid,
        message: Option<String>,
        inspect: bool,
        acknowledge: bool,
        reply: mpsc::SyncSender<crate::Response>,
    ) {
        let write = !inspect && !acknowledge;
        if write
            && self
                .index_busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            let mut response = response(None, None);
            response.workspace_git_commit = Some(commit::unavailable_result(
                &target,
                ticket,
                "Another Git write is in progress",
            ));
            let _ = reply.try_send(response);
            return;
        }
        if let Err(error) = self.sender.try_send(Task::Commit {
            target,
            ticket,
            message,
            inspect,
            acknowledge,
            reply,
        }) {
            if write {
                self.index_busy.store(false, Ordering::Release);
            }
            let task = match error {
                mpsc::TrySendError::Full(task) | mpsc::TrySendError::Disconnected(task) => task,
            };
            if let Task::Commit { target, reply, .. } = task {
                let mut response = response(None, None);
                response.workspace_git_commit = Some(commit::unavailable_result(
                    &target,
                    ticket,
                    "Git commit worker is busy or unavailable",
                ));
                let _ = reply.try_send(response);
            }
        }
    }

    pub(crate) fn submit_index_ack(
        &self,
        target: Target,
        ticket: Uuid,
        reply: mpsc::SyncSender<crate::Response>,
    ) {
        match self.sender.try_send(Task::IndexAck {
            target,
            ticket,
            reply,
        }) {
            Ok(()) => (),
            Err(mpsc::TrySendError::Full(Task::IndexAck { target, reply, .. }))
            | Err(mpsc::TrySendError::Disconnected(Task::IndexAck { target, reply, .. })) => {
                let mut response = response(None, None);
                response.workspace_git_index = Some(index::unavailable_result(
                    &target,
                    ticket,
                    "workspace Git index acknowledgement unavailable".into(),
                ));
                let _ = reply.try_send(response);
            }
            Err(mpsc::TrySendError::Full(_)) | Err(mpsc::TrySendError::Disconnected(_)) => {
                unreachable!("task variant changed")
            }
        }
    }
}

pub(crate) fn receipt_target(catalog: &Catalog, workspace: Uuid, revision: u64) -> Result<Target> {
    let current = catalog
        .workspaces
        .iter()
        .find(|value| value.id == workspace)
        .context("workspace does not exist")?;
    let project = catalog
        .projects
        .iter()
        .find(|value| value.id == current.project_id)
        .context("workspace project does not exist")?;
    Ok(Target {
        workspace_id: workspace,
        workspace_revision: revision,
        project_id: current.project_id,
        cwd: current.cwd.clone(),
        project_root: project.canonical_path.clone(),
        git_common_dir: project.git_common_dir.clone(),
    })
}

pub(crate) fn target(
    catalog: &Catalog,
    workspace_id: Uuid,
    expected_revision: u64,
) -> Result<Target> {
    let workspace = catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .context("workspace does not exist")?;
    ensure!(
        workspace.status == WorkspaceStatus::Ready,
        "Git status requires a Ready workspace"
    );
    ensure!(
        workspace.revision == expected_revision,
        "workspace changed; refresh and retry Git status"
    );
    let project = catalog
        .projects
        .iter()
        .find(|project| project.id == workspace.project_id)
        .context("workspace project does not exist")?;
    Ok(Target {
        workspace_id,
        workspace_revision: workspace.revision,
        project_id: project.id,
        // Freeze catalog values only on the actor. Filesystem and Git probes
        // belong to the worker below.
        cwd: workspace.cwd.clone(),
        project_root: project.canonical_path.clone(),
        git_common_dir: project.git_common_dir.clone(),
    })
}

fn snapshot(target: Target) -> Result<WorkspaceGitSnapshot> {
    let identity = validate_git_identity(&target, Instant::now() + COMMAND_DEADLINE)?;
    let (bytes, truncated) = run_status(&identity.cwd, Instant::now() + COMMAND_DEADLINE)?;
    // Recheck after the subprocess so an external move/rebind cannot make a
    // stale path look like a result for the originally validated repository.
    ensure!(
        validate_git_identity(&target, Instant::now() + COMMAND_DEADLINE)? == identity,
        "workspace Git identity changed during status"
    );
    let mut bytes = bytes;
    if truncated {
        if let Some(end) = bytes.iter().rposition(|byte| *byte == 0) {
            bytes.truncate(end + 1);
        } else {
            bytes.clear();
        }
    }
    let parsed = parse_porcelain_v2(&bytes, MAX_ENTRIES)?;
    Ok(WorkspaceGitSnapshot {
        workspace_id: target.workspace_id,
        workspace_revision: target.workspace_revision,
        project_id: target.project_id,
        cwd: identity.cwd,
        branch: parsed.branch,
        head: parsed.head,
        detached: parsed.detached,
        entries: parsed.entries,
        truncated: truncated || parsed.truncated,
        index_operation: None,
        index_operation_unavailable_reason: None,
        commit_schema: Some(1),
        commit_operation: None,
        commit_unavailable_reason: None,
    })
}

fn diff(
    target: Target,
    path: String,
    side: GitDiffSide,
    index: &mut index::Engine,
) -> Result<WorkspaceGitDiff> {
    validate_diff_path(&path)?;
    ensure!(
        !path.contains('\u{fffd}'),
        "Git diff does not support paths that cannot be represented exactly as UTF-8"
    );
    let identity = validate_git_identity(&target, Instant::now() + COMMAND_DEADLINE)?;
    let before = status_for_diff(&identity.cwd)?;
    let entry = before
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .with_context(|| format!("path is not present in the current Git status: {path}"))?;
    ensure!(
        diff_side_present(entry, side),
        "requested diff side is no longer present"
    );
    match side {
        GitDiffSide::Index => {
            if entry.index_status != 'D' {
                ensure_index_file_mode(&identity.cwd, &path)?;
            }
            if before.head.is_some() && entry.index_status != 'A' {
                ensure_tree_file_mode(
                    &identity.cwd,
                    "HEAD",
                    entry.original_path.as_deref().unwrap_or(&path),
                )?;
            }
        }
        GitDiffSide::Worktree if entry.index_status != 'D' => {
            let index_path = if matches!(entry.worktree_status, 'R' | 'C') {
                entry.original_path.as_deref().unwrap_or(&path)
            } else {
                &path
            };
            ensure_index_file_mode(&identity.cwd, index_path)?;
        }
        GitDiffSide::Worktree | GitDiffSide::Untracked | GitDiffSide::Conflict => (),
    }

    let (old, new) = diff_contents(&identity.cwd, &path, side, &before)?;
    let binary = is_binary(&old) || is_binary(&new);
    let (text, truncated) = if binary {
        (String::new(), false)
    } else {
        render_diff(&old, &new)?
    };

    ensure!(
        validate_git_identity(&target, Instant::now() + COMMAND_DEADLINE)? == identity,
        "workspace Git identity changed during diff"
    );
    let after = status_for_diff(&identity.cwd)?;
    let after_entry = after.entries.iter().find(|entry| entry.path == path);
    ensure!(
        after_entry.is_some_and(|entry| diff_side_present(entry, side)),
        "Git status changed during diff; refresh and retry"
    );
    let (after_old, after_new) = diff_contents(&identity.cwd, &path, side, &after)?;
    ensure!(
        after_old == old && after_new == new,
        "workspace diff inputs changed during diff; refresh and retry"
    );
    let (index_ticket, index_unavailable_reason) =
        index.ticket_for_diff(&target, &identity, &before, entry, side, (&old, &new));
    Ok(WorkspaceGitDiff {
        workspace_id: target.workspace_id,
        workspace_revision: target.workspace_revision,
        project_id: target.project_id,
        cwd: identity.cwd,
        path,
        side,
        text,
        truncated,
        binary,
        index_ticket,
        index_unavailable_reason: index_unavailable_reason.map(bounded_notice),
    })
}

fn status_for_diff(cwd: &Path) -> Result<Parsed> {
    let (bytes, truncated) = run_status(cwd, Instant::now() + COMMAND_DEADLINE)?;
    ensure!(
        !truncated,
        "Git status exceeded its bound during diff validation"
    );
    let parsed = parse_porcelain_v2(&bytes, MAX_ENTRIES)?;
    ensure!(
        !parsed.truncated,
        "Git status entry limit exceeded during diff validation"
    );
    Ok(parsed)
}

fn diff_side_present(entry: &WorkspaceGitEntry, side: GitDiffSide) -> bool {
    match side {
        GitDiffSide::Index => !entry.untracked && !entry.unmerged && entry.index_status != '.',
        GitDiffSide::Worktree => {
            !entry.untracked && !entry.unmerged && entry.worktree_status != '.'
        }
        GitDiffSide::Untracked => entry.untracked,
        GitDiffSide::Conflict => entry.unmerged,
    }
}

fn validate_diff_path(path: &str) -> Result<()> {
    ensure!(!path.is_empty(), "Git diff path is empty");
    ensure!(
        path.len() <= MAX_DIFF_PATH_BYTES,
        "Git diff path exceeds 4096 bytes"
    );
    ensure!(
        !path.starts_with('/') && !path.starts_with('\\'),
        "Git diff path must be relative"
    );
    ensure!(
        !path.as_bytes().get(1).is_some_and(|byte| *byte == b':')
            || !path.as_bytes()[0].is_ascii_alphabetic(),
        "Git diff path must not use a drive prefix"
    );
    ensure!(
        !path.chars().any(|character| character.is_control()),
        "Git diff path contains a control character"
    );
    ensure!(
        !path
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == ".."),
        "Git diff path contains an unsafe component"
    );
    ensure!(
        !path
            .split('/')
            .next()
            .is_some_and(|component| component.starts_with('-')),
        "Git diff path must not begin with a dash"
    );
    Ok(())
}

fn diff_contents(
    cwd: &Path,
    path: &str,
    side: GitDiffSide,
    status: &Parsed,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let entry = status
        .entries
        .iter()
        .find(|entry| entry.path == path)
        .context("Git status entry disappeared")?;
    match side {
        GitDiffSide::Index => {
            let old_path = entry.original_path.as_deref().unwrap_or(path);
            ensure!(
                !old_path.contains('\u{fffd}'),
                "Git diff does not support renamed paths that cannot be represented exactly as UTF-8"
            );
            validate_diff_path(old_path)?;
            let old = if status.head.is_some() && entry.index_status != 'A' {
                git_blob(cwd, &format!("HEAD:{old_path}"))?
            } else {
                Vec::new()
            };
            let new = if entry.index_status == 'D' {
                Vec::new()
            } else {
                git_blob(cwd, &format!(":0:{path}"))?
            };
            Ok((old, new))
        }
        GitDiffSide::Worktree => {
            let old_path = if matches!(entry.worktree_status, 'R' | 'C') {
                entry.original_path.as_deref().unwrap_or(path)
            } else {
                path
            };
            validate_diff_path(old_path)?;
            let old = if entry.index_status == 'D' {
                Vec::new()
            } else {
                git_blob(cwd, &format!(":0:{old_path}"))?
            };
            let new = read_worktree_file(cwd, path)?;
            Ok((old, new.map(|file| file.bytes).unwrap_or_default()))
        }
        GitDiffSide::Untracked => {
            let new = read_worktree_file(cwd, path)?;
            Ok((
                Vec::new(),
                new.context("untracked path is not a regular file")?.bytes,
            ))
        }
        GitDiffSide::Conflict => {
            let new = read_worktree_file(cwd, path)?;
            Ok((Vec::new(), new.map(|file| file.bytes).unwrap_or_default()))
        }
    }
}

fn git_blob(cwd: &Path, revision_path: &str) -> Result<Vec<u8>> {
    let (bytes, truncated) = run_git_capture(
        cwd,
        &[
            "show",
            "--no-ext-diff",
            "--no-textconv",
            "--format=",
            revision_path,
        ],
        OUTPUT_LIMIT,
        Instant::now() + COMMAND_DEADLINE,
        "git show blob",
    )?;
    ensure!(!truncated, "Git blob exceeds 256 KiB");
    Ok(bytes)
}

fn ensure_index_file_mode(cwd: &Path, path: &str) -> Result<()> {
    let (bytes, truncated) = run_git_capture(
        cwd,
        &[
            "--literal-pathspecs",
            "ls-files",
            "--stage",
            "-z",
            "--",
            path,
        ],
        16 * 1024,
        Instant::now() + COMMAND_DEADLINE,
        "git ls-files stage",
    )?;
    ensure!(!truncated, "Git index mode query exceeded its bound");
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let mode = record
            .split(|byte| *byte == b' ')
            .next()
            .context("malformed Git index mode")?;
        if matches!(mode, b"100644" | b"100755") {
            return Ok(());
        }
    }
    bail!("Git diff preview supports regular files only; symlinks and submodules are unavailable")
}

fn ensure_tree_file_mode(cwd: &Path, tree: &str, path: &str) -> Result<()> {
    validate_diff_path(path)?;
    let (bytes, truncated) = run_git_capture(
        cwd,
        &["--literal-pathspecs", "ls-tree", "-z", tree, "--", path],
        16 * 1024,
        Instant::now() + COMMAND_DEADLINE,
        "git ls-tree mode",
    )?;
    ensure!(!truncated, "Git tree mode query exceeded its bound");
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let mode = record
            .split(|byte| *byte == b' ')
            .next()
            .context("malformed Git tree mode")?;
        if matches!(mode, b"100644" | b"100755") {
            return Ok(());
        }
    }
    bail!("Git diff preview supports regular files only; symlinks and submodules are unavailable")
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn render_diff(old: &[u8], new: &[u8]) -> Result<(String, bool)> {
    let temp = tempfile::Builder::new()
        .prefix("pigade-git-diff-")
        .tempdir()
        .context("cannot create private diff directory")?;
    let old_path = temp.path().join("old");
    let new_path = temp.path().join("new");
    fs::write(&old_path, old).context("cannot write private diff baseline")?;
    fs::write(&new_path, new).context("cannot write private diff target")?;
    let (mut bytes, mut truncated) = run_git_diff(temp.path())?;
    if let Err(error) = std::str::from_utf8(&bytes) {
        bytes.truncate(error.valid_up_to());
        truncated = true;
    }
    let mut text = String::from_utf8(bytes).context("Git diff output was not UTF-8")?;
    if let Some(body_start) = text.find("\n@@") {
        let (headers, body) = text.split_at(body_start + 1);
        let headers = headers
            .replace("diff --git a/old b/new\n", "diff --git a/file b/file\n")
            .replace("diff --git 1/old 2/new\n", "diff --git a/file b/file\n")
            .replace("--- a/old\n", "--- a/file\n")
            .replace("--- 1/old\n", "--- a/file\n")
            .replace("+++ b/new\n", "+++ b/file\n")
            .replace("+++ 2/new\n", "+++ b/file\n");
        text = format!("{headers}{body}");
    }
    if let Some(line_end) = text
        .match_indices('\n')
        .nth(DIFF_LINE_LIMIT - 1)
        .map(|(i, _)| i + 1)
        .filter(|line_end| *line_end < text.len())
    {
        text.truncate(line_end);
        truncated = true;
    }
    if text.len() > DIFF_OUTPUT_LIMIT {
        let mut end = DIFF_OUTPUT_LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if let Some(line_end) = text[..end].rfind('\n') {
            end = line_end + 1;
        }
        text.truncate(end);
        truncated = true;
    }
    Ok((text, truncated))
}

#[cfg(unix)]
pub(super) struct WorktreeFile {
    pub bytes: Vec<u8>,
    pub mode: u32,
}

#[cfg(unix)]
pub(super) fn read_worktree_file(root: &Path, path: &str) -> Result<Option<WorktreeFile>> {
    let components = path.split('/').collect::<Vec<_>>();
    let root_fd = open_path(root, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .context("cannot open workspace root for safe file read")?;
    let mut parent = root_fd;
    for component in &components[..components.len() - 1] {
        let component = CString::new(*component)?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error).context("cannot safely open a workspace path component");
        }
        parent = unsafe { File::from_raw_fd(fd) };
    }
    let leaf = CString::new(*components.last().context("empty validated path")?)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            leaf.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        if error.raw_os_error() == Some(libc::ELOOP) {
            bail!("symbolic-link files are not supported in Git diff previews");
        }
        return Err(error).context("cannot safely open workspace file");
    }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let before = FileStamp::from_metadata(&file.metadata()?);
    ensure!(
        before.regular,
        "Git diff preview supports regular files only"
    );
    ensure!(
        before.size <= OUTPUT_LIMIT as u64,
        "workspace file exceeds 256 KiB"
    );
    let mut bytes = Vec::with_capacity(before.size as usize);
    (&mut file)
        .take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .context("cannot read workspace file")?;
    ensure!(
        bytes.len() <= OUTPUT_LIMIT,
        "workspace file exceeds 256 KiB"
    );
    let after = FileStamp::from_metadata(&file.metadata()?);
    ensure!(before == after, "workspace file changed while being read");
    ensure!(
        bytes.len() as u64 == after.size,
        "workspace file changed while being read"
    );
    // Re-open the exact path under the held directory descriptors. Comparing
    // the fd identity with the current directory entry detects a replacement
    // while the bytes were being read.
    let current_stamp = stat_at(parent.as_raw_fd(), &leaf)?;
    ensure!(current_stamp == after, "workspace file changed during diff");
    let mode = if after.mode & 0o111 != 0 {
        0o100755
    } else {
        0o100644
    };
    Ok(Some(WorktreeFile { bytes, mode }))
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    size: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
    regular: bool,
    mode: u32,
}

#[cfg(unix)]
impl FileStamp {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        let file_type = metadata.file_type();
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
            regular: file_type.is_file(),
            mode: metadata.mode() & 0o7777,
        }
    }
}

#[cfg(unix)]
fn open_path(path: &Path, flags: libc::c_int) -> Result<File> {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes())?;
    let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_NOFOLLOW) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("cannot open path");
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn stat_at(parent_fd: libc::c_int, leaf: &CString) -> Result<FileStamp> {
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::fstatat(
            parent_fd,
            leaf.as_ptr(),
            &mut metadata,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot recheck workspace file");
    }
    Ok(FileStamp {
        device: metadata.st_dev,
        inode: metadata.st_ino,
        size: metadata.st_size as u64,
        mtime: metadata.st_mtime,
        mtime_nsec: metadata.st_mtime_nsec,
        ctime: metadata.st_ctime,
        ctime_nsec: metadata.st_ctime_nsec,
        regular: (metadata.st_mode & libc::S_IFMT) == libc::S_IFREG,
        mode: metadata.st_mode as u32 & 0o7777,
    })
}

fn run_git_diff(cwd: &Path) -> Result<(Vec<u8>, bool)> {
    run_capture_program_allow_diff_exit(
        Path::new("git"),
        cwd,
        &[
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--text",
            "--",
            "old",
            "new",
        ],
        DIFF_OUTPUT_LIMIT,
        Instant::now() + COMMAND_DEADLINE,
        "git diff",
    )
}

fn run_capture_program_allow_diff_exit(
    program: &Path,
    cwd: &Path,
    args: &[&str],
    output_limit: usize,
    deadline: Instant,
    label: &'static str,
) -> Result<(Vec<u8>, bool)> {
    // Reuse the same bounded process-group reader, accepting Git's documented
    // status 1 for a nonempty `diff --no-index` result.
    run_capture_program_inner(program, cwd, args, output_limit, deadline, label, true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenIdentity {
    cwd: PathBuf,
    project_root: PathBuf,
    common_dir: PathBuf,
}

fn validate_git_identity(target: &Target, deadline: Instant) -> Result<FrozenIdentity> {
    let cwd = fs::canonicalize(&target.cwd).context("cannot resolve workspace directory")?;
    ensure!(cwd.is_dir(), "workspace directory is unavailable");
    let project_root =
        fs::canonicalize(&target.project_root).context("cannot resolve project root")?;
    ensure!(project_root.is_dir(), "project root is unavailable");
    let root = git_root_bounded(&cwd, deadline)?;
    ensure!(root == cwd, "workspace is no longer a Git worktree root");
    let common_dir = git_common_dir_bounded(&root, deadline)?;
    ensure!(
        common_dir == fs::canonicalize(&target.git_common_dir)?,
        "workspace Git identity changed during Git status"
    );
    ensure!(
        git_root_bounded(&project_root, deadline)? == project_root,
        "project root is no longer a Git worktree root"
    );
    ensure!(
        git_common_dir_bounded(&project_root, deadline)? == common_dir,
        "project Git identity changed during Git status"
    );
    Ok(FrozenIdentity {
        cwd,
        project_root,
        common_dir,
    })
}

struct OutputRead {
    bytes: Vec<u8>,
    overflow: bool,
}

struct OwnedChild {
    child: Child,
    cleanup_on_drop: bool,
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            kill_process_group(&mut self.child);
            let _ = self.child.wait();
        }
    }
}

fn kill_process_group(child: &mut Child) {
    let Ok(process_group) = i32::try_from(child.id()) else {
        return;
    };
    // SAFETY: each request starts Git as leader of its own process group.
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result != 0 {
        let _ = child.kill();
    }
}

fn clean_git_environment(command: &mut ProcessCommand) {
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG",
    ] {
        command.env_remove(key);
    }
    // Environment-injected config values can override core.worktree and
    // related identity settings even when GIT_DIR itself is absent.
    command.env("GIT_CONFIG_COUNT", "0");
    command.env("GIT_OPTIONAL_LOCKS", "0");
    command.env("GIT_NO_LAZY_FETCH", "1");
}

fn git_root_bounded(path: &Path, deadline: Instant) -> Result<PathBuf> {
    let (bytes, incomplete) = run_git_capture(
        path,
        &["rev-parse", "--show-toplevel"],
        16 * 1024,
        deadline,
        "git rev-parse workspace root",
    )?;
    ensure!(!incomplete, "Git root probe exceeded its bound");
    let value = std::str::from_utf8(&bytes)?;
    let value = remove_git_line_ending(value);
    fs::canonicalize(value).context("cannot resolve Git worktree root")
}

fn git_common_dir_bounded(path: &Path, deadline: Instant) -> Result<PathBuf> {
    let (bytes, incomplete) = run_git_capture(
        path,
        &["rev-parse", "--git-common-dir"],
        16 * 1024,
        deadline,
        "git rev-parse common directory",
    )?;
    ensure!(!incomplete, "Git common-directory probe exceeded its bound");
    let value = PathBuf::from(remove_git_line_ending(std::str::from_utf8(&bytes)?));
    let value = if value.is_absolute() {
        value
    } else {
        path.join(value)
    };
    fs::canonicalize(value).context("cannot resolve Git common directory")
}

fn remove_git_line_ending(value: &str) -> &str {
    // Unix Git paths are emitted verbatim followed by exactly one LF.
    value.strip_suffix('\n').unwrap_or(value)
}

fn run_status(cwd: &Path, deadline: Instant) -> Result<(Vec<u8>, bool)> {
    run_git_capture(
        cwd,
        &[
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "status.submoduleSummary=false",
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
            "--ignore-submodules=none",
            "--no-ahead-behind",
        ],
        OUTPUT_LIMIT,
        deadline,
        "git status",
    )
}

fn run_git_capture(
    cwd: &Path,
    args: &[&str],
    output_limit: usize,
    deadline: Instant,
    label: &'static str,
) -> Result<(Vec<u8>, bool)> {
    run_capture_program(Path::new("git"), cwd, args, output_limit, deadline, label)
}

fn run_capture_program(
    program: &Path,
    cwd: &Path,
    args: &[&str],
    output_limit: usize,
    deadline: Instant,
    label: &'static str,
) -> Result<(Vec<u8>, bool)> {
    run_capture_program_inner(program, cwd, args, output_limit, deadline, label, false)
}

fn run_capture_program_inner(
    program: &Path,
    cwd: &Path,
    args: &[&str],
    output_limit: usize,
    deadline: Instant,
    label: &'static str,
    allow_diff_exit: bool,
) -> Result<(Vec<u8>, bool)> {
    use std::os::unix::process::CommandExt;
    let mut command = ProcessCommand::new(program);
    command.process_group(0);
    command
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(cwd)
        .args(args);
    clean_git_environment(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = OwnedChild {
        child: command
            .spawn()
            .with_context(|| format!("failed to start {label}"))?,
        cleanup_on_drop: true,
    };
    let Some(stdout) = child.child.stdout.take() else {
        bail!("{label} stdout unavailable");
    };
    let (sender, receiver) = mpsc::sync_channel::<Result<OutputRead>>(1);
    let reader = thread::Builder::new()
        .name("pigade-git-output".into())
        .spawn(move || {
            let mut stdout = stdout;
            let mut bytes = Vec::with_capacity(16 * 1024);
            let mut chunk = [0u8; 8192];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => {
                        let accepted = output_limit.saturating_sub(bytes.len()).min(read);
                        bytes.extend_from_slice(&chunk[..accepted]);
                        if accepted != read {
                            let _ = sender.send(Ok(OutputRead {
                                bytes,
                                overflow: true,
                            }));
                            return;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = sender.send(Err(error.into()));
                        return;
                    }
                }
            }
            let _ = sender.send(Ok(OutputRead {
                bytes,
                overflow: false,
            }));
        })?;

    let mut output = None;
    let mut failure = None;
    let mut timed_out = false;
    let mut overflow = false;
    let mut status = None;
    loop {
        if output.is_none() {
            match receiver.recv_timeout(OUTPUT_POLL) {
                Ok(Ok(read)) => {
                    overflow = read.overflow;
                    output = Some(read.bytes);
                }
                Ok(Err(error)) => {
                    failure = Some(error);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    failure = Some(anyhow::anyhow!(
                        "{label} output reader stopped unexpectedly"
                    ));
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
            }
        }
        if overflow {
            break;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break;
        }
        match child.child.try_wait() {
            Ok(Some(exit)) => {
                status = Some(exit);
                // A descendant may still hold stdout open after Git exits.
                // Continue until EOF or the same deadline, then kill the
                // entire owned process group before joining the reader.
                if output.is_some() {
                    break;
                }
            }
            Ok(None) => (),
            Err(error) => {
                failure = Some(error.into());
                break;
            }
        }
        if output.is_some() {
            thread::sleep(OUTPUT_POLL);
        }
    }

    let must_kill_group = failure.is_some() || timed_out || overflow || output.is_none();
    if must_kill_group {
        kill_process_group(&mut child.child);
    }
    let wait_result = child.child.wait();
    let join_result = reader.join();
    child.cleanup_on_drop = false;
    if failure.is_none()
        && let Err(error) = wait_result
    {
        failure = Some(error.into());
    }
    if failure.is_none() && join_result.is_err() {
        failure = Some(anyhow::anyhow!("{label} output reader panicked"));
    }
    if let Some(error) = failure {
        return Err(error);
    }
    if output.is_none() {
        match receiver.try_recv() {
            Ok(Ok(read)) => {
                overflow |= read.overflow;
                output = Some(read.bytes);
            }
            Ok(Err(error)) => return Err(error),
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => (),
        }
    }
    let bytes = output.unwrap_or_default();
    if timed_out || overflow {
        return Ok((bytes, true));
    }
    let exit = status.context("Git command exited without an exit status")?;
    ensure!(
        exit.success() || (allow_diff_exit && exit.code() == Some(1)),
        "{label} exited with {exit}"
    );
    Ok((bytes, false))
}

#[cfg(test)]
fn run_status_bounded(
    cwd: &Path,
    output_limit: usize,
    deadline_after: Duration,
) -> Result<(Vec<u8>, bool)> {
    // Keep the exact production status argv while allowing tests to shorten
    // its deadline and output cap.
    run_git_capture(
        cwd,
        &[
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "status.submoduleSummary=false",
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
            "--ignore-submodules=none",
            "--no-ahead-behind",
        ],
        output_limit,
        Instant::now() + deadline_after,
        "git status",
    )
}

struct Parsed {
    branch: Option<String>,
    head: Option<String>,
    detached: bool,
    entries: Vec<WorkspaceGitEntry>,
    truncated: bool,
}

fn parse_porcelain_v2(bytes: &[u8], limit: usize) -> Result<Parsed> {
    let mut branch = None;
    let mut head = None;
    let mut detached = false;
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut fields = bytes.split(|byte| *byte == 0).peekable();
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        if let Some(line) = field.strip_prefix(b"# ") {
            if let Some(value) = line.strip_prefix(b"branch.head ") {
                if value == b"(detached)" {
                    detached = true;
                    branch = None;
                } else {
                    branch = Some(display(value));
                }
            } else if let Some(value) = line.strip_prefix(b"branch.oid ")
                && value != b"(initial)"
            {
                head = Some(display(value));
            }
            continue;
        }
        let record = field;
        let kind = record[0];
        let (entry, _) = parse_entry(record, kind, &mut fields)?;
        if let Some(entry) = entry {
            if entries.len() == limit {
                truncated = true;
                break;
            }
            entries.push(entry);
        }
    }
    Ok(Parsed {
        branch,
        head,
        detached,
        entries,
        truncated,
    })
}

fn parse_entry<'a>(
    record: &'a [u8],
    kind: u8,
    fields: &mut impl Iterator<Item = &'a [u8]>,
) -> Result<(Option<WorkspaceGitEntry>, bool)> {
    if kind == b'?' {
        let path = record
            .strip_prefix(b"? ")
            .context("malformed untracked Git entry")?;
        return Ok((
            Some(WorkspaceGitEntry {
                path: display(path),
                original_path: None,
                index_status: '?',
                worktree_status: '?',
                untracked: true,
                unmerged: false,
            }),
            false,
        ));
    }
    if kind == b'!' {
        return Ok((None, false));
    }
    let rename = kind == b'2';
    let columns = match kind {
        b'1' => 9,
        b'2' => 10,
        b'u' => 11,
        _ => bail!("unsupported porcelain v2 record type {kind}"),
    };
    let mut split = record.splitn(columns, |byte| *byte == b' ');
    let tag = split.next().context("Git status record lacks type")?;
    ensure!(tag == [kind], "Git status record type mismatch");
    let xy = split.next().context("Git status record lacks XY state")?;
    ensure!(xy.len() == 2, "invalid Git XY status");
    let (path, rename_kind) = if rename {
        let score = split
            .nth(columns - 4)
            .context("renamed Git status record lacks rename/copy score")?;
        ensure!(
            score
                .first()
                .is_some_and(|kind| matches!(*kind, b'R' | b'C')),
            "unsupported Git rename/copy score"
        );
        (
            split
                .next()
                .context("renamed Git status record lacks path")?,
            score[0] as char,
        )
    } else {
        (
            split
                .nth(columns - 3)
                .context("Git status record lacks path")?,
            '\0',
        )
    };
    let original_path = if matches!(rename_kind, 'R' | 'C') {
        Some(display(
            fields
                .next()
                .context("renamed Git status record lacks original path")?,
        ))
    } else {
        None
    };
    Ok((
        Some(WorkspaceGitEntry {
            path: display(path),
            original_path,
            index_status: xy[0] as char,
            worktree_status: xy[1] as char,
            untracked: false,
            unmerged: kind == b'u',
        }),
        rename,
    ))
}

fn display(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub(crate) fn response(
    snapshot: Option<WorkspaceGitSnapshot>,
    error: Option<String>,
) -> crate::Response {
    crate::Response {
        protocol: crate::PROTOCOL,
        views: Vec::new(),
        error,
        rejected_before_admission: None,
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
        providers: None,
        template_run: None,
        template_runs: None,
        panel_directory: None,
        workspace_git_snapshot: snapshot,
        workspace_git_diff: None,
        workspace_git_index: None,
        workspace_git_commit_plan: None,
        workspace_git_commit: None,
    }
}

fn diff_response(diff: Option<WorkspaceGitDiff>, error: Option<String>) -> crate::Response {
    let mut response = response(None, error);
    response.workspace_git_diff = diff;
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_storage::{LayoutDocument, Project, Selection, Workspace, WorkspaceMode};
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn parse_git_fixture(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()?;
        ensure!(
            output.status.success(),
            "git fixture command failed: {:?}",
            args
        );
        Ok(output.stdout)
    }

    fn init_repo(path: &Path) -> Result<()> {
        fs::create_dir_all(path)?;
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(["init", "--initial-branch=main"])
            .output()?;
        ensure!(output.status.success(), "git init fixture failed");
        parse_git_fixture(path, &["config", "user.name", "Fixture"])?;
        parse_git_fixture(path, &["config", "user.email", "fixture@example.test"])?;
        fs::write(path.join("tracked.txt"), "base\n")?;
        parse_git_fixture(path, &["add", "tracked.txt"])?;
        parse_git_fixture(path, &["commit", "-m", "base"])?;
        Ok(())
    }

    fn target_for(repo: &Path, cwd: &Path) -> Result<Target> {
        let repo = fs::canonicalize(repo)?;
        let cwd = fs::canonicalize(cwd)?;
        Ok(Target {
            workspace_id: Uuid::from_u128(1),
            workspace_revision: 7,
            project_id: Uuid::from_u128(2),
            cwd,
            project_root: repo.clone(),
            git_common_dir: crate::projects::git_common_dir(&repo)?,
        })
    }

    fn git_diff(repo: &Path, path: &str, side: GitDiffSide) -> Result<WorkspaceGitDiff> {
        let mut engine = index::Engine::new(std::env::temp_dir().join("pigade-workspace-git-test"));
        diff(target_for(repo, repo)?, path.to_owned(), side, &mut engine)
    }

    fn assert_patch(diff: &WorkspaceGitDiff, removed: &str, added: &str) {
        assert!(!diff.binary);
        assert!(!diff.truncated);
        assert!(
            diff.text.contains(removed),
            "missing {removed:?}: {}",
            diff.text
        );
        assert!(
            diff.text.contains(added),
            "missing {added:?}: {}",
            diff.text
        );
    }

    #[test]
    fn parses_porcelain_v2_headers_statuses_renames_and_unusual_paths() -> Result<()> {
        let bytes = concat!(
            "# branch.oid abc123\0",
            "# branch.head feature/test\0",
            "# branch.upstream origin/feature/test\0",
            "1 M. N... 100644 100644 100644 abc abc tracked\nwith-newline\0",
            "2 R. N... 100644 100644 100644 abc def R100 moved name\0old name\0",
            "2 C. N... 100644 100644 100644 abc def C100 copied name\0source name\0",
            "u UU N... 100644 100644 100644 100644 a b c conflict\0",
            "? untracked file\0",
        )
        .as_bytes();
        let parsed = parse_porcelain_v2(bytes, 10)?;
        assert_eq!(parsed.branch.as_deref(), Some("feature/test"));
        assert_eq!(parsed.head.as_deref(), Some("abc123"));
        assert!(!parsed.detached);
        assert_eq!(parsed.entries.len(), 5);
        assert_eq!(parsed.entries[0].path, "tracked\nwith-newline");
        assert_eq!(parsed.entries[1].path, "moved name");
        assert_eq!(parsed.entries[1].original_path.as_deref(), Some("old name"));
        assert_eq!(parsed.entries[2].path, "copied name");
        assert_eq!(parsed.entries[2].index_status, 'C');
        assert_eq!(
            parsed.entries[2].original_path.as_deref(),
            Some("source name")
        );
        assert!(parsed.entries[3].unmerged);
        assert!(parsed.entries[4].untracked);
        Ok(())
    }

    #[test]
    fn parser_marks_entry_limit_and_detached_head() -> Result<()> {
        let bytes = b"# branch.oid abc\0# branch.head (detached)\0? first\0? second\0";
        let parsed = parse_porcelain_v2(bytes, 1)?;
        assert!(parsed.detached);
        assert_eq!(parsed.branch, None);
        assert_eq!(parsed.entries.len(), 1);
        assert!(parsed.truncated);
        Ok(())
    }

    #[test]
    fn git_identity_override_environment_is_removed() {
        let mut command = ProcessCommand::new("git");
        command.env("GIT_DIR", "/attacker/repo");
        command.env("GIT_WORK_TREE", "/attacker/tree");
        command.env("GIT_CONFIG_COUNT", "1");
        command.env("GIT_CONFIG_KEY_0", "core.worktree");
        command.env("GIT_CONFIG_VALUE_0", "/attacker/tree");
        clean_git_environment(&mut command);
        for key in ["GIT_DIR", "GIT_WORK_TREE"] {
            assert!(
                command
                    .get_envs()
                    .any(|(name, value)| { name == key && value.is_none() })
            );
        }
        assert!(command.get_envs().any(|(name, value)| {
            name == "GIT_CONFIG_COUNT" && value.is_some_and(|value| value == "0")
        }));
    }

    #[test]
    fn live_snapshot_reports_staged_unstaged_untracked_without_index_write() -> Result<()> {
        let root = TempDir::new()?;
        let repo = root.path().join("repo with spaces");
        init_repo(&repo)?;
        fs::write(repo.join("tracked.txt"), "worktree edit\n")?;
        fs::write(repo.join("staged file.txt"), "staged\n")?;
        parse_git_fixture(&repo, &["add", "staged file.txt"])?;
        fs::create_dir(repo.join("untracked dir"))?;
        fs::write(repo.join("untracked dir").join("line\nbreak.txt"), "new\n")?;
        let index_before = fs::read(repo.join(".git/index"))?;
        let result = snapshot(target_for(&repo, &repo)?)?;
        let index_after = fs::read(repo.join(".git/index"))?;

        assert_eq!(result.branch.as_deref(), Some("main"));
        assert!(!result.detached);
        assert!(!result.truncated);
        assert_eq!(index_before, index_after);
        assert!(result.entries.iter().any(|entry| {
            entry.path == "tracked.txt" && entry.index_status == '.' && entry.worktree_status == 'M'
        }));
        assert!(result.entries.iter().any(|entry| {
            entry.path == "staged file.txt"
                && entry.index_status == 'A'
                && entry.worktree_status == '.'
        }));
        assert!(
            result
                .entries
                .iter()
                .any(|entry| { entry.untracked && entry.path == "untracked dir/line\nbreak.txt" })
        );
        Ok(())
    }

    #[test]
    fn diff_reads_index_worktree_untracked_and_deleted_sides() -> Result<()> {
        let root = TempDir::new()?;
        let repo = root.path().join("repo with spaces");
        init_repo(&repo)?;

        fs::write(repo.join("tracked.txt"), "staged version\n")?;
        parse_git_fixture(&repo, &["add", "tracked.txt"])?;
        fs::write(repo.join("tracked.txt"), "worktree version\n")?;
        let staged = git_diff(&repo, "tracked.txt", GitDiffSide::Index)?;
        assert_patch(&staged, "-base", "+staged version");
        assert_eq!(staged.side, GitDiffSide::Index);
        assert_eq!(staged.path, "tracked.txt");
        let worktree = git_diff(&repo, "tracked.txt", GitDiffSide::Worktree)?;
        assert_patch(&worktree, "-staged version", "+worktree version");

        fs::write(repo.join("untracked file.txt"), "new content\n")?;
        let untracked = git_diff(&repo, "untracked file.txt", GitDiffSide::Untracked)?;
        assert!(!untracked.binary && !untracked.truncated);
        assert!(untracked.text.contains("+new content"));
        assert!(untracked.text.contains("a/file"));

        fs::write(repo.join("deleted.txt"), "remove me\n")?;
        parse_git_fixture(&repo, &["add", "deleted.txt"])?;
        parse_git_fixture(&repo, &["commit", "-m", "add deletion fixture"])?;
        fs::remove_file(repo.join("deleted.txt"))?;
        let worktree_delete = git_diff(&repo, "deleted.txt", GitDiffSide::Worktree)?;
        assert!(worktree_delete.text.contains("-remove me"));
        assert!(worktree_delete.text.contains("@@ -1 +0,0 @@"));
        parse_git_fixture(&repo, &["add", "-u", "deleted.txt"])?;
        let staged_delete = git_diff(&repo, "deleted.txt", GitDiffSide::Index)?;
        assert!(staged_delete.text.contains("-remove me"));
        assert!(staged_delete.text.contains("@@ -1 +0,0 @@"));
        Ok(())
    }

    #[test]
    fn index_blob_lookup_handles_colon_digit_filename() -> Result<()> {
        let root = TempDir::new()?;
        let repo = root.path().join("repo");
        init_repo(&repo)?;
        fs::write(repo.join("1:foo"), "before\n")?;
        parse_git_fixture(&repo, &["add", "1:foo"])?;
        parse_git_fixture(&repo, &["commit", "-m", "colon filename"])?;
        fs::write(repo.join("1:foo"), "after\n")?;
        parse_git_fixture(&repo, &["add", "1:foo"])?;

        let diff = git_diff(&repo, "1:foo", GitDiffSide::Index)?;
        assert_patch(&diff, "-before", "+after");
        Ok(())
    }

    #[test]
    fn diff_supports_unborn_index_and_reports_binary_without_bytes() -> Result<()> {
        let root = TempDir::new()?;
        let repo = root.path().join("unborn");
        fs::create_dir_all(&repo)?;
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "--initial-branch=main"])
            .output()?;
        ensure!(output.status.success(), "git init fixture failed");
        parse_git_fixture(&repo, &["config", "user.name", "Fixture"])?;
        parse_git_fixture(&repo, &["config", "user.email", "fixture@example.test"])?;
        fs::write(repo.join("first.txt"), "first\n")?;
        parse_git_fixture(&repo, &["add", "first.txt"])?;
        let first = git_diff(&repo, "first.txt", GitDiffSide::Index)?;
        assert!(first.text.contains("+first"));

        parse_git_fixture(&repo, &["commit", "-m", "first"])?;
        fs::write(repo.join("first.txt"), b"binary\0payload")?;
        let binary = git_diff(&repo, "first.txt", GitDiffSide::Worktree)?;
        assert!(binary.binary);
        assert!(binary.text.is_empty());
        assert!(!binary.text.contains("payload"));
        Ok(())
    }

    #[test]
    fn conflict_preview_uses_worktree_against_empty_and_rejects_unsafe_paths() -> Result<()> {
        let root = TempDir::new()?;
        let repo = root.path().join("repo");
        init_repo(&repo)?;
        parse_git_fixture(&repo, &["checkout", "-b", "other"])?;
        fs::write(repo.join("tracked.txt"), "other change\n")?;
        parse_git_fixture(&repo, &["commit", "-am", "other"])?;
        parse_git_fixture(&repo, &["checkout", "main"])?;
        fs::write(repo.join("tracked.txt"), "main change\n")?;
        parse_git_fixture(&repo, &["commit", "-am", "main"])?;
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["merge", "other"])
            .output()?;
        assert!(!output.status.success(), "fixture merge should conflict");
        let conflict = git_diff(&repo, "tracked.txt", GitDiffSide::Conflict)?;
        assert!(conflict.text.contains("+<<<<<<<"));
        assert!(conflict.text.contains("+>>>>>>>"));

        for invalid in [
            "",
            "/etc/passwd",
            "../escape",
            "dir/../escape",
            "a//b",
            "-option",
            "C:/file",
            "\\\\host\\share",
            "bad\npath",
        ] {
            assert!(validate_diff_path(invalid).is_err(), "{invalid:?}");
        }
        assert!(git_diff(&repo, "not-in-current-status.txt", GitDiffSide::Worktree).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn diff_rejects_symlinked_parent_and_oversized_worktree_file() -> Result<()> {
        use std::os::unix::fs::symlink;
        let root = TempDir::new()?;
        let repo = root.path().join("repo");
        fs::create_dir_all(repo.join("nested"))?;
        parse_git_fixture(&repo, &["init", "--initial-branch=main"])?;
        parse_git_fixture(&repo, &["config", "user.name", "Fixture"])?;
        parse_git_fixture(&repo, &["config", "user.email", "fixture@example.test"])?;
        fs::write(repo.join("nested/tracked.txt"), "tracked\n")?;
        parse_git_fixture(&repo, &["add", "."])?;
        parse_git_fixture(&repo, &["commit", "-m", "base"])?;

        let outside = root.path().join("outside");
        fs::create_dir(&outside)?;
        fs::write(outside.join("tracked.txt"), "secret\n")?;
        fs::remove_file(repo.join("nested/tracked.txt"))?;
        fs::remove_dir(repo.join("nested"))?;
        symlink(&outside, repo.join("nested"))?;
        let symlink_result = git_diff(&repo, "nested/tracked.txt", GitDiffSide::Worktree);
        assert!(
            symlink_result
                .unwrap_err()
                .to_string()
                .contains("safely open a workspace path component")
        );

        fs::remove_file(repo.join("nested"))?;
        fs::create_dir(repo.join("nested"))?;
        fs::write(
            repo.join("nested/tracked.txt"),
            vec![b'x'; OUTPUT_LIMIT + 1],
        )?;
        let oversized = git_diff(&repo, "nested/tracked.txt", GitDiffSide::Worktree);
        assert!(oversized.unwrap_err().to_string().contains("256 KiB"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn diff_ignores_external_diff_and_textconv_configuration_and_caps_lines() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new()?;
        let repo = root.path().join("repo");
        init_repo(&repo)?;
        let marker = root.path().join("driver-ran");
        let driver = root.path().join("trap.sh");
        fs::write(
            &driver,
            format!("#!/bin/sh\nprintf ran >> '{}'\nexit 99\n", marker.display()),
        )?;
        fs::set_permissions(&driver, fs::Permissions::from_mode(0o755))?;
        parse_git_fixture(
            &repo,
            &["config", "diff.external", driver.to_str().unwrap()],
        )?;
        parse_git_fixture(
            &repo,
            &[
                "config",
                "diff.pigade-spy.textconv",
                driver.to_str().unwrap(),
            ],
        )?;
        fs::write(repo.join(".gitattributes"), "*.txt diff=pigade-spy\n")?;
        parse_git_fixture(&repo, &["add", ".gitattributes"])?;
        parse_git_fixture(&repo, &["commit", "-m", "attributes"])?;

        fs::write(repo.join("tracked.txt"), "new\n".repeat(3000))?;
        let preview = git_diff(&repo, "tracked.txt", GitDiffSide::Worktree)?;
        assert!(preview.truncated);
        assert!(preview.text.lines().count() <= DIFF_LINE_LIMIT);
        assert!(preview.text.len() <= DIFF_OUTPUT_LIMIT);

        fs::write(
            repo.join("tracked.txt"),
            vec![b'y'; OUTPUT_LIMIT.saturating_sub(1)],
        )?;
        let byte_capped = git_diff(&repo, "tracked.txt", GitDiffSide::Worktree)?;
        assert!(byte_capped.truncated);
        assert!(byte_capped.text.len() <= DIFF_OUTPUT_LIMIT);
        assert!(!marker.exists(), "external diff/textconv command executed");
        Ok(())
    }

    #[test]
    fn linked_worktree_is_bound_to_its_project_common_directory() -> Result<()> {
        let root = TempDir::new()?;
        let repo = root.path().join("repo");
        init_repo(&repo)?;
        let worktree = root.path().join("linked tree");
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "worktree",
                "add",
                "--quiet",
                "-b",
                "fixture-linked",
                worktree.to_str().unwrap(),
            ])
            .output()?;
        ensure!(output.status.success(), "git worktree add fixture failed");

        let result = snapshot(target_for(&repo, &worktree)?)?;
        assert_eq!(result.cwd, fs::canonicalize(&worktree)?);
        assert_eq!(result.branch.as_deref(), Some("fixture-linked"));
        Ok(())
    }

    #[test]
    fn identity_probe_preserves_newline_ending_in_repository_path() -> Result<()> {
        let root = TempDir::new()?;
        let repo = root.path().join("repo\n");
        init_repo(&repo)?;
        let expected = fs::canonicalize(&repo)?;
        let result = snapshot(target_for(&repo, &repo)?)?;
        assert_eq!(result.cwd, expected);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn identity_recheck_detects_symlink_retarget_with_same_common_directory() -> Result<()> {
        use std::os::unix::fs::symlink;
        let root = TempDir::new()?;
        let repo = root.path().join("repo");
        init_repo(&repo)?;
        let first = root.path().join("first-worktree");
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "worktree",
                "add",
                "--quiet",
                "-b",
                "first",
                first.to_str().unwrap(),
            ])
            .output()?;
        ensure!(output.status.success(), "first worktree fixture failed");
        let second = root.path().join("second-worktree");
        let output = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "worktree",
                "add",
                "--quiet",
                "-b",
                "second",
                second.to_str().unwrap(),
            ])
            .output()?;
        ensure!(output.status.success(), "second worktree fixture failed");
        let link = root.path().join("workspace-link");
        symlink(&first, &link)?;
        let target = Target {
            workspace_id: Uuid::from_u128(1),
            workspace_revision: 2,
            project_id: Uuid::from_u128(2),
            cwd: link.clone(),
            project_root: fs::canonicalize(&repo)?,
            git_common_dir: crate::projects::git_common_dir(&repo)?,
        };
        let first_identity = validate_git_identity(&target, Instant::now() + COMMAND_DEADLINE)?;
        fs::remove_file(&link)?;
        symlink(&second, &link)?;
        let second_identity = validate_git_identity(&target, Instant::now() + COMMAND_DEADLINE)?;
        assert_ne!(first_identity, second_identity);
        assert_ne!(first_identity.cwd, second_identity.cwd);
        Ok(())
    }

    #[test]
    fn status_output_limit_and_deadline_return_incomplete_not_clean() -> Result<()> {
        let root = TempDir::new()?;
        let repo = root.path().join("repo");
        init_repo(&repo)?;
        fs::write(repo.join("untracked.txt"), "content\n")?;

        let (limited, hit_limit) = run_status_bounded(&repo, 16, Duration::from_secs(2))?;
        assert!(hit_limit);
        assert!(limited.len() <= 16);

        let (timed, timed_out) = run_status_bounded(&repo, OUTPUT_LIMIT, Duration::from_nanos(1))?;
        assert!(timed_out);
        assert!(timed.len() <= OUTPUT_LIMIT);
        let (empty_timeout, empty_timeout_flag) =
            run_status_bounded(&repo, OUTPUT_LIMIT, Duration::ZERO)?;
        assert!(empty_timeout_flag);
        // Any bytes captured before the deadline are partial because the
        // completion bit remains false; an empty result is likewise unknown.
        let _unknown_bytes = empty_timeout;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn exited_git_descendant_holding_stdout_is_killed_before_reader_join() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = TempDir::new()?;
        let repo = root.path().join("repo");
        fs::create_dir(&repo)?;
        let fake_git = root.path().join("fake-git");
        fs::write(
            &fake_git,
            "#!/bin/sh\nsleep 30 &\nprintf '# branch.head fake\\n'\nexit 0\n",
        )?;
        fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755))?;
        let started = Instant::now();
        let (bytes, incomplete) = run_capture_program(
            &fake_git,
            &repo,
            &["ignored"],
            OUTPUT_LIMIT,
            Instant::now() + Duration::from_millis(150),
            "test git",
        )?;
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(incomplete);
        assert!(!bytes.is_empty());
        Ok(())
    }

    #[test]
    fn request_target_requires_ready_workspace_and_exact_revision() -> Result<()> {
        let root = TempDir::new()?;
        let repo = root.path().join("repo");
        init_repo(&repo)?;
        let root_path = fs::canonicalize(&repo)?;
        let project_id = Uuid::from_u128(2);
        let workspace_id = Uuid::from_u128(1);
        let project = Project {
            id: project_id,
            canonical_path: root_path.clone(),
            git_common_dir: crate::projects::git_common_dir(&root_path)?,
            name: "fixture".into(),
            roots: Vec::new(),
            revision: 0,
        };
        let workspace = Workspace {
            id: workspace_id,
            project_id,
            task: "task".into(),
            cwd: root_path.clone(),
            mode: WorkspaceMode::SharedCheckout,
            status: WorkspaceStatus::Ready,
            error: None,
            context_revision: None,
            revision: 4,
        };
        let catalog = Catalog {
            projects: vec![project],
            workspaces: vec![workspace.clone()],
            tabs: Vec::new(),
            selection: Selection::default(),
            layout: LayoutDocument::default(),
            layout_templates: Some(Vec::new()),
            template_launch_schema: Some(1),
            pane_labels: Some(Vec::new()),
            agent_profiles: None,
            agent_profile_roster: None,
            prompts: None,
            tab_close_schema: None,
            engine_options_schema: Some(1),
            pane_tab_drop_schema: Some(1),
            history_grid_schema: Some(1),
        };
        assert!(target(&catalog, workspace_id, 3).is_err());
        assert_eq!(target(&catalog, workspace_id, 4)?.workspace_revision, 4);
        let mut unavailable_path = catalog.clone();
        unavailable_path.workspaces[0].cwd = root.path().join("missing-workspace");
        let frozen = target(&unavailable_path, workspace_id, 4)?;
        assert!(snapshot(frozen).is_err());
        let mut archived = catalog;
        archived.workspaces[0].status = WorkspaceStatus::Archived;
        assert!(target(&archived, workspace_id, 4).is_err());
        Ok(())
    }
}
