//! Whole-status, bounded batch Stage All / Unstage All leases.
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    FileSnapshot, IndexFileId, IndexLock, file_id, git_dir, index_path, is_split_index, read_index,
    run_git_input,
};
#[cfg(test)]
use super::{WriteBoundary, fail_at};
use crate::workspace_git::{GitIndexAction, GitIndexPhase, Target, WorkspaceGitEntry};

const MAX_BATCH_FILES: usize = 128;
const MAX_BATCH_FILE_BYTES: usize = 256 * 1024;
const MAX_BATCH_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_BATCH_LEASES: usize = 2;
const MAX_BATCH_RECEIPTS: usize = 4096;
const MAX_BATCH_RECEIPT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_BATCH_PATHS: usize = MAX_BATCH_FILES * 2;
const LEASE_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitIndexBatchPlan {
    pub ticket: Uuid,
    pub workspace_id: Uuid,
    pub workspace_revision: u64,
    pub project_id: Uuid,
    pub cwd: PathBuf,
    pub action: GitIndexAction,
    pub paths: Vec<String>,
    pub entries: Vec<WorkspaceGitEntry>,
    pub file_bytes: u64,
    pub expires_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitIndexBatchResult {
    pub ticket: Uuid,
    pub workspace_id: Uuid,
    pub workspace_revision: u64,
    pub project_id: Uuid,
    pub cwd: PathBuf,
    pub action: GitIndexAction,
    pub paths: Vec<String>,
    pub phase: GitIndexPhase,
    pub acknowledged: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
struct BatchLease {
    target: Target,
    ticket: Uuid,
    action: GitIndexAction,
    cwd: PathBuf,
    project_root: PathBuf,
    common_dir: PathBuf,
    git_dir: PathBuf,
    index_path: PathBuf,
    index_bytes: Option<Vec<u8>>,
    index_id: Option<IndexFileId>,
    head: Option<String>,
    branch: Option<String>,
    entries: Vec<WorkspaceGitEntry>,
    status_entries: Vec<WorkspaceGitEntry>,
    paths: Vec<String>,
    files: Vec<(String, Option<FileSnapshot>)>,
    expires: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchReceipt {
    version: u8,
    acknowledged: bool,
    result: WorkspaceGitIndexBatchResult,
    target: ReceiptTarget,
    index_len: Option<u64>,
    index_id: Option<IndexFileId>,
    head: Option<String>,
    branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptTarget {
    workspace_id: Uuid,
    workspace_revision: u64,
    project_id: Uuid,
    cwd: PathBuf,
    git_dir: PathBuf,
}

pub(super) struct BatchEngine {
    root: PathBuf,
    leases: HashMap<Uuid, BatchLease>,
}

impl BatchEngine {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            leases: HashMap::new(),
        }
    }

    pub(super) fn prepare(
        &mut self,
        target: Target,
        action: GitIndexAction,
    ) -> Result<WorkspaceGitIndexBatchPlan> {
        let started = Instant::now();
        let identity = super::super::validate_git_identity(
            &target,
            Instant::now() + crate::workspace_git::COMMAND_DEADLINE,
        )?;
        let cwd = identity.cwd.clone();
        let git_dir = git_dir(&cwd)?;
        let index_path = index_path(&cwd)?;
        ensure!(
            index_path.parent().is_some_and(|parent| parent == git_dir),
            "Git index is not a direct child of the per-worktree Git directory"
        );
        ensure!(
            !is_split_index(&cwd)?,
            "sparse or split index is not supported for batch actions"
        );
        let index = read_index(&index_path)?;
        let index_id = file_id(&index_path)?;
        let status = super::super::status_for_diff(&cwd)?;
        validate_complete_status(&status)?;
        validate_supported_modes(
            &cwd,
            &index_path,
            status.head.as_deref(),
            &status.entries,
            started,
        )?;
        let mut entries = Vec::new();
        let mut affected = Vec::new();
        for entry in &status.entries {
            ensure!(
                !entry.unmerged,
                "conflict entries prevent a complete batch action"
            );
            ensure!(
                entry.index_status != 'S' && entry.worktree_status != 'S',
                "submodule entries prevent a complete batch action"
            );
            super::super::validate_diff_path(&entry.path)?;
            if let Some(old) = &entry.original_path {
                super::super::validate_diff_path(old)?;
            }
            let selected = match action {
                GitIndexAction::Stage => entry.untracked || entry.worktree_status != '.',
                GitIndexAction::Unstage => !entry.untracked && entry.index_status != '.',
            };
            if !selected {
                continue;
            }
            entries.push(entry.clone());
            affected.push(entry.path.clone());
            let include_old = match action {
                GitIndexAction::Stage => entry.worktree_status == 'R',
                GitIndexAction::Unstage => matches!(entry.index_status, 'R' | 'C'),
            };
            if include_old && let Some(old) = &entry.original_path {
                affected.push(old.clone());
            }
        }
        ensure!(
            entries.len() <= MAX_BATCH_FILES,
            "batch exceeds 128 status entries"
        );
        affected.sort();
        affected.dedup();
        ensure!(
            !affected.is_empty(),
            "there are no files for this batch action"
        );
        ensure!(
            affected.len() <= MAX_BATCH_PATHS,
            "batch exceeds 256 reviewed paths"
        );
        let mut total = 0usize;
        let mut files = Vec::with_capacity(affected.len());
        let mut freeze_paths = status
            .entries
            .iter()
            .flat_map(|entry| {
                std::iter::once(entry.path.clone()).chain(
                    (entry.index_status == 'R' || entry.worktree_status == 'R')
                        .then(|| entry.original_path.clone())
                        .flatten(),
                )
            })
            .collect::<Vec<_>>();
        freeze_paths.sort();
        freeze_paths.dedup();
        for path in &freeze_paths {
            ensure!(
                started.elapsed() < Duration::from_secs(10),
                "batch preparation exceeded 10 seconds"
            );
            let file = super::super::read_worktree_file(&cwd, path)?.map(|file| FileSnapshot {
                bytes: file.bytes,
                mode: file.mode,
            });
            if let Some(file) = &file {
                ensure!(
                    file.bytes.len() <= MAX_BATCH_FILE_BYTES,
                    "batch file {path} exceeds 256 KiB"
                );
                total = total
                    .checked_add(file.bytes.len())
                    .context("batch file-byte sum overflow")?;
                ensure!(
                    total <= MAX_BATCH_TOTAL_BYTES,
                    "batch exceeds 8 MiB combined working-file bytes"
                );
            }
            files.push((path.clone(), file));
        }
        ensure!(
            Instant::now().duration_since(started) < Duration::from_secs(10),
            "batch preparation exceeded 10 seconds"
        );
        let after = super::super::status_for_diff(&cwd)?;
        ensure!(
            same_status(&status, &after),
            "Git status changed while preparing the batch"
        );
        ensure!(
            read_index(&index_path)?.bytes == index.bytes && file_id(&index_path)? == index_id,
            "Git index changed while preparing the batch"
        );
        validate_files(&cwd, &files, started)?;
        let now = Instant::now();
        self.leases.retain(|_, lease| {
            lease.expires > now && lease.target.workspace_id != target.workspace_id
        });
        ensure!(
            self.leases.len() < MAX_BATCH_LEASES,
            "two batch preview leases are already live"
        );
        let ticket = Uuid::new_v4();
        let expires_unix_ms = unix_ms().saturating_add(LEASE_TTL.as_millis() as u64);
        let lease = BatchLease {
            target: target.clone(),
            ticket,
            action,
            cwd: cwd.clone(),
            project_root: identity.project_root,
            common_dir: identity.common_dir,
            git_dir,
            index_path,
            index_bytes: index.bytes,
            index_id,
            head: status.head,
            branch: status.branch,
            entries: entries.clone(),
            status_entries: status.entries.clone(),
            paths: affected.clone(),
            files,
            expires: Instant::now() + LEASE_TTL,
        };
        self.leases.insert(ticket, lease);
        Ok(WorkspaceGitIndexBatchPlan {
            ticket,
            workspace_id: target.workspace_id,
            workspace_revision: target.workspace_revision,
            project_id: target.project_id,
            cwd,
            action,
            paths: affected,
            entries,
            file_bytes: total as u64,
            expires_unix_ms,
        })
    }

    pub(super) fn request(
        &mut self,
        root: &Path,
        target: Target,
        ticket: Uuid,
        inspect: bool,
        acknowledge: bool,
    ) -> WorkspaceGitIndexBatchResult {
        let path = receipt_path(root, ticket);
        match read_receipt(&path) {
            Ok(mut receipt) => {
                if !receipt_matches(&receipt, &target)
                    || receipt.result.workspace_revision != target.workspace_revision
                {
                    return unavailable(
                        &target,
                        ticket,
                        receipt.result.action,
                        receipt.result.paths,
                        "batch ticket belongs to a different workspace scope",
                    );
                }
                if acknowledge && receipt.result.phase == GitIndexPhase::Unknown {
                    receipt.acknowledged = true;
                    receipt.result.acknowledged = true;
                    if let Err(error) = write_receipt(&path, &receipt) {
                        receipt.result.acknowledged = false;
                        receipt.result.message = Some(super::super::bounded_notice(format!(
                            "could not record acknowledgement: {error:#}"
                        )));
                    }
                }
                return receipt.result;
            }
            Err(error) if path.exists() => {
                return unavailable(
                    &target,
                    ticket,
                    GitIndexAction::Stage,
                    Vec::new(),
                    &format!("batch receipt is corrupt or unavailable: {error:#}"),
                );
            }
            Err(_) => (),
        }
        if inspect || acknowledge {
            return unavailable(
                &target,
                ticket,
                GitIndexAction::Stage,
                Vec::new(),
                "no durable batch receipt exists for this ticket",
            );
        }
        let Some(lease) = self.leases.remove(&ticket) else {
            return unavailable(
                &target,
                ticket,
                GitIndexAction::Stage,
                Vec::new(),
                "batch ticket is missing or expired",
            );
        };
        if lease.expires <= Instant::now() || !target_matches(&lease.target, &target) {
            return result_for(
                &lease,
                GitIndexPhase::Rejected,
                Some("batch ticket expired or workspace scope changed".into()),
            );
        }
        let mut receipt = receipt_for(
            &lease,
            GitIndexPhase::Unknown,
            Some("operation outcome is being recorded".into()),
        );
        if let Err(error) = ensure_capacity(root) {
            return result_for(
                &lease,
                GitIndexPhase::Unavailable,
                Some(format!("batch receipt storage unavailable: {error:#}")),
            );
        }
        if let Err(error) = validate_lease(&lease) {
            receipt.result.phase = GitIndexPhase::Rejected;
            receipt.result.message = Some(super::super::bounded_notice(format!(
                "batch preview is stale: {error:#}"
            )));
            let _ = write_receipt(&path, &receipt);
            return receipt.result;
        }
        let receipt_path = match path.parent() {
            Some(_) => path,
            None => {
                return result_for(
                    &lease,
                    GitIndexPhase::Unavailable,
                    Some("invalid batch receipt path".into()),
                );
            }
        };
        if let Err(error) = write_receipt(&receipt_path, &receipt) {
            return result_for(
                &lease,
                GitIndexPhase::Unavailable,
                Some(format!("could not durably record batch attempt: {error:#}")),
            );
        }
        match apply(&lease) {
            Ok(()) => {
                receipt.result.phase = GitIndexPhase::Applied;
                receipt.result.message = None;
                if write_receipt(&receipt_path, &receipt).is_err() {
                    receipt.result.phase = GitIndexPhase::Unknown;
                    receipt.result.message = Some("index replacement completed, but its durable receipt could not be confirmed".into());
                    let _ = write_receipt(&receipt_path, &receipt);
                }
                receipt.result
            }
            Err(error) => {
                receipt.result.phase = GitIndexPhase::Unknown;
                receipt.result.message = Some(super::super::bounded_notice(format!(
                    "batch outcome is uncertain: {error:#}"
                )));
                let _ = write_receipt(&receipt_path, &receipt);
                receipt.result
            }
        }
    }

    pub(super) fn pending_for_target(
        &self,
        target: &Target,
    ) -> Result<Option<WorkspaceGitIndexBatchResult>> {
        let mut newest: Option<(SystemTime, WorkspaceGitIndexBatchResult)> = None;
        let entries = match fs::read_dir(receipt_root(&self.root)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut count = 0;
        for entry in entries {
            let entry = entry?;
            count += 1;
            ensure!(
                count <= MAX_BATCH_RECEIPTS,
                "batch receipt store exceeds fixed limit"
            );
            let kind = entry.file_type()?;
            ensure!(
                !kind.is_symlink(),
                "batch receipt store contains a symbolic-link entry"
            );
            ensure!(
                kind.is_dir(),
                "batch receipt store contains a non-directory record"
            );
            let path = entry.path().join("receipt.json");
            let receipt = read_receipt(&path)
                .with_context(|| format!("cannot read batch receipt {}", path.display()))?;
            if receipt.acknowledged
                || receipt.result.phase != GitIndexPhase::Unknown
                || !receipt_matches(&receipt, target)
            {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            if newest
                .as_ref()
                .is_none_or(|(current, _)| modified > *current)
            {
                newest = Some((modified, receipt.result));
            }
        }
        Ok(newest.map(|(_, result)| result))
    }
}

fn apply(lease: &BatchLease) -> Result<()> {
    let started = Instant::now();
    #[cfg(test)]
    fail_at(WriteBoundary::AfterDurableAttempt)?;
    validate_lease(lease)?;
    ensure!(
        started.elapsed() < Duration::from_secs(10),
        "batch apply exceeded 10 seconds"
    );
    let parent = lease
        .index_path
        .parent()
        .context("Git index has no parent")?;
    let scratch = tempfile::Builder::new()
        .prefix(".pigade-batch-index-")
        .tempdir_in(parent)?;
    let candidate = scratch.path().join("index");
    if let Some(bytes) = &lease.index_bytes {
        fs::write(&candidate, bytes)?;
    } else {
        run_git_input(
            &lease.cwd,
            Some(&candidate),
            &["read-tree", "--empty"],
            &[],
            4096,
            "git read-tree empty batch",
        )?;
    }
    match lease.action {
        GitIndexAction::Stage => {
            let mut args = vec!["--literal-pathspecs", "add", "--all", "--"];
            let owned = lease.paths.iter().map(String::as_str).collect::<Vec<_>>();
            args.extend(owned);
            run_git_input(
                &lease.cwd,
                Some(&candidate),
                &args,
                &[],
                4096,
                "git add reviewed batch paths",
            )?;
        }
        GitIndexAction::Unstage => {
            let zero = zero_oid(&lease.cwd)?;
            ensure!(
                started.elapsed() < Duration::from_secs(10),
                "batch apply exceeded 10 seconds"
            );
            let mut input = Vec::new();
            for entry in &lease.entries {
                ensure!(
                    started.elapsed() < Duration::from_secs(10),
                    "batch apply exceeded 10 seconds"
                );
                let restore_path = if entry.index_status == 'C' {
                    None
                } else {
                    entry.original_path.as_deref().or(Some(entry.path.as_str()))
                };
                let remove_old = entry.index_status == 'R';
                for path in std::iter::once(entry.path.as_str()).chain(
                    remove_old
                        .then_some(entry.original_path.as_deref())
                        .flatten(),
                ) {
                    input.extend(format!("0 {zero}\t").into_bytes());
                    input.extend_from_slice(path.as_bytes());
                    input.push(0);
                }
                if let (Some(head), Some(path)) = (&lease.head, restore_path)
                    && let Some((mode, oid)) = head_entry(&lease.cwd, head, path)?
                {
                    input.extend(format!("{mode} {oid} 0\t").into_bytes());
                    input.extend_from_slice(path.as_bytes());
                    input.push(0);
                }
                ensure!(
                    started.elapsed() < Duration::from_secs(10),
                    "batch apply exceeded 10 seconds"
                );
            }
            run_git_input(
                &lease.cwd,
                Some(&candidate),
                &["update-index", "-z", "--index-info"],
                &input,
                64 * 1024,
                "git unstage reviewed batch paths",
            )?;
        }
    }
    ensure!(
        started.elapsed() < Duration::from_secs(10),
        "batch candidate preparation exceeded 10 seconds"
    );
    validate_lease(lease)?;
    ensure!(
        started.elapsed() < Duration::from_secs(10),
        "batch apply exceeded 10 seconds"
    );
    let mut lock = IndexLock::acquire(&lease.index_path)?;
    ensure!(
        read_index(&lease.index_path)?.bytes == lease.index_bytes
            && file_id(&lease.index_path)? == lease.index_id,
        "Git index changed before batch lock acquisition"
    );
    validate_lease(lease)?;
    ensure!(
        started.elapsed() < Duration::from_secs(10),
        "batch apply exceeded 10 seconds"
    );
    lock.write_candidate(&candidate)?;
    ensure!(
        read_index(&lease.index_path)?.bytes == lease.index_bytes
            && file_id(&lease.index_path)? == lease.index_id,
        "Git index changed under batch native lock"
    );
    validate_lease(lease)?;
    ensure!(
        started.elapsed() < Duration::from_secs(10),
        "batch apply exceeded 10 seconds"
    );
    lock.commit()?;
    #[cfg(test)]
    fail_at(WriteBoundary::BeforeFinalReceipt)?;
    Ok(())
}

fn validate_lease(lease: &BatchLease) -> Result<()> {
    let started = Instant::now();
    let identity = super::super::validate_git_identity(
        &lease.target,
        Instant::now() + crate::workspace_git::COMMAND_DEADLINE,
    )?;
    ensure!(
        identity.cwd == lease.cwd
            && identity.project_root == lease.project_root
            && identity.common_dir == lease.common_dir,
        "workspace Git identity changed after batch preview"
    );
    ensure!(
        started.elapsed() < Duration::from_secs(10),
        "batch validation exceeded 10 seconds"
    );
    ensure!(
        git_dir(&lease.cwd)? == lease.git_dir && index_path(&lease.cwd)? == lease.index_path,
        "per-worktree Git index identity changed"
    );
    ensure!(
        started.elapsed() < Duration::from_secs(10),
        "batch validation exceeded 10 seconds"
    );
    ensure!(
        read_index(&lease.index_path)?.bytes == lease.index_bytes
            && file_id(&lease.index_path)? == lease.index_id,
        "Git index changed after batch preview"
    );
    ensure!(
        started.elapsed() < Duration::from_secs(10),
        "batch validation exceeded 10 seconds"
    );
    ensure!(
        !is_split_index(&lease.cwd)?,
        "sparse or split index is not supported"
    );
    ensure!(
        started.elapsed() < Duration::from_secs(10),
        "batch validation exceeded 10 seconds"
    );
    let status = super::super::status_for_diff(&lease.cwd)?;
    ensure!(
        started.elapsed() < Duration::from_secs(10),
        "batch validation exceeded 10 seconds"
    );
    validate_complete_status(&status)?;
    ensure!(
        status.head == lease.head
            && status.branch == lease.branch
            && status.entries == lease.status_entries,
        "Git status changed after batch preview"
    );
    validate_files(&lease.cwd, &lease.files, started)?;
    Ok(())
}

fn validate_complete_status(status: &super::Parsed) -> Result<()> {
    ensure!(
        !status.truncated && status.entries.len() <= MAX_BATCH_FILES,
        "Git status is incomplete or exceeds 128 entries"
    );
    for entry in &status.entries {
        ensure!(!entry.unmerged, "conflicts prevent batch action");
        ensure!(
            entry.index_status != 'S' && entry.worktree_status != 'S',
            "submodules prevent batch action"
        );
        super::super::validate_diff_path(&entry.path)?;
        if let Some(path) = &entry.original_path {
            super::super::validate_diff_path(path)?;
        }
    }
    Ok(())
}

fn validate_supported_modes(
    cwd: &Path,
    index: &Path,
    head: Option<&str>,
    entries: &[WorkspaceGitEntry],
    started: Instant,
) -> Result<()> {
    for entry in entries.iter().filter(|entry| !entry.untracked) {
        let rename = entry.index_status == 'R' || entry.worktree_status == 'R';
        for path in std::iter::once(entry.path.as_str())
            .chain(rename.then_some(entry.original_path.as_deref()).flatten())
        {
            let index_mode = super::index_entry_mode(cwd, index, path)?;
            ensure!(
                started.elapsed() < Duration::from_secs(10),
                "batch validation exceeded 10 seconds"
            );
            let head_mode = match head {
                Some(head) => head_entry(cwd, head, path)?.map(|(mode, _)| mode),
                None => None,
            };
            ensure!(
                started.elapsed() < Duration::from_secs(10),
                "batch validation exceeded 10 seconds"
            );
            ensure!(
                index_mode.is_some() || head_mode.is_some(),
                "changed path has no supported regular-file entry in index or HEAD: {path}"
            );
        }
    }
    Ok(())
}

fn validate_files(
    cwd: &Path,
    files: &[(String, Option<FileSnapshot>)],
    started: Instant,
) -> Result<()> {
    for (path, expected) in files {
        ensure!(
            started.elapsed() < Duration::from_secs(10),
            "batch validation exceeded 10 seconds"
        );
        let actual = super::super::read_worktree_file(cwd, path)?.map(|file| FileSnapshot {
            bytes: file.bytes,
            mode: file.mode,
        });
        ensure!(
            same_file(expected, &actual),
            "working file changed after batch preview: {path}"
        );
    }
    Ok(())
}

fn same_file(a: &Option<FileSnapshot>, b: &Option<FileSnapshot>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a.bytes == b.bytes && a.mode == b.mode,
        _ => false,
    }
}
fn same_status(a: &super::Parsed, b: &super::Parsed) -> bool {
    a.branch == b.branch && a.head == b.head && a.entries == b.entries && a.truncated == b.truncated
}
fn target_matches(a: &Target, b: &Target) -> bool {
    a.workspace_id == b.workspace_id
        && a.workspace_revision == b.workspace_revision
        && a.project_id == b.project_id
        && a.cwd == b.cwd
        && a.project_root == b.project_root
        && a.git_common_dir == b.git_common_dir
}
fn result_for(
    lease: &BatchLease,
    phase: GitIndexPhase,
    message: Option<String>,
) -> WorkspaceGitIndexBatchResult {
    WorkspaceGitIndexBatchResult {
        ticket: lease.ticket,
        workspace_id: lease.target.workspace_id,
        workspace_revision: lease.target.workspace_revision,
        project_id: lease.target.project_id,
        cwd: lease.cwd.clone(),
        action: lease.action,
        paths: lease.paths.clone(),
        phase,
        acknowledged: false,
        message: message.map(super::super::bounded_notice),
    }
}
fn unavailable(
    target: &Target,
    ticket: Uuid,
    action: GitIndexAction,
    paths: Vec<String>,
    message: &str,
) -> WorkspaceGitIndexBatchResult {
    WorkspaceGitIndexBatchResult {
        ticket,
        workspace_id: target.workspace_id,
        workspace_revision: target.workspace_revision,
        project_id: target.project_id,
        cwd: target.cwd.clone(),
        action,
        paths,
        phase: GitIndexPhase::Unavailable,
        acknowledged: false,
        message: Some(super::super::bounded_notice(message)),
    }
}

pub(super) fn unavailable_result(
    target: &Target,
    ticket: Uuid,
    message: String,
) -> WorkspaceGitIndexBatchResult {
    unavailable(target, ticket, GitIndexAction::Stage, Vec::new(), &message)
}
fn receipt_for(lease: &BatchLease, phase: GitIndexPhase, message: Option<String>) -> BatchReceipt {
    BatchReceipt {
        version: 1,
        acknowledged: false,
        result: result_for(lease, phase, message),
        target: ReceiptTarget {
            workspace_id: lease.target.workspace_id,
            workspace_revision: lease.target.workspace_revision,
            project_id: lease.target.project_id,
            cwd: lease.cwd.clone(),
            git_dir: lease.git_dir.clone(),
        },
        index_len: lease.index_bytes.as_ref().map(|b| b.len() as u64),
        index_id: lease.index_id,
        head: lease.head.clone(),
        branch: lease.branch.clone(),
    }
}
fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn receipt_root(root: &Path) -> PathBuf {
    root.join("git-index-batch-operations")
}
fn receipt_path(root: &Path, id: Uuid) -> PathBuf {
    receipt_root(root).join(id.to_string()).join("receipt.json")
}
fn ensure_receipt_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "batch receipt directory must be real"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    File::open(path)?.sync_all()?;
    File::open(path.parent().context("receipt directory has no parent")?)?.sync_all()?;
    Ok(())
}
fn ensure_capacity(root: &Path) -> Result<()> {
    let dir = receipt_root(root);
    ensure_receipt_dir(&dir)?;
    let mut count = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        ensure!(
            kind.is_dir() && !kind.is_symlink(),
            "batch receipt store contains an invalid record entry"
        );
        count += 1;
        ensure!(
            count <= MAX_BATCH_RECEIPTS,
            "batch receipt store exceeds fixed limit"
        );
    }
    ensure!(
        count < MAX_BATCH_RECEIPTS,
        "batch receipt store reached 4096-record limit"
    );
    Ok(())
}
fn write_receipt(path: &Path, r: &BatchReceipt) -> Result<()> {
    let dir = path.parent().context("batch receipt has no parent")?;
    ensure_receipt_dir(dir)?;
    let bytes = serde_json::to_vec(r)?;
    ensure!(
        bytes.len() as u64 <= MAX_BATCH_RECEIPT_BYTES,
        "batch receipt exceeds 2 MiB"
    );
    let tmp = tempfile::Builder::new()
        .prefix("receipt-")
        .tempfile_in(dir)?;
    let (mut f, tmp_path) = tmp.keep()?;
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    use std::io::Write;
    f.write_all(&bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(tmp_path, path)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}
fn read_receipt(path: &Path) -> Result<BatchReceipt> {
    let parent = path.parent().context("batch receipt has no parent")?;
    let pm = fs::symlink_metadata(parent)?;
    ensure!(
        pm.is_dir() && !pm.file_type().is_symlink(),
        "batch receipt directory must be real"
    );
    let root = parent
        .parent()
        .context("batch receipt root has no parent")?;
    let root_meta = fs::symlink_metadata(root)?;
    ensure!(
        root_meta.is_dir() && !root_meta.file_type().is_symlink(),
        "batch receipt root must be real"
    );
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let m = f.metadata()?;
    ensure!(
        m.is_file() && m.len() <= MAX_BATCH_RECEIPT_BYTES,
        "invalid batch receipt"
    );
    let mut bytes = Vec::with_capacity(m.len() as usize);
    (&mut f)
        .take(MAX_BATCH_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_BATCH_RECEIPT_BYTES,
        "batch receipt exceeds size bound"
    );
    let r: BatchReceipt = serde_json::from_slice(&bytes)?;
    ensure!(r.version == 1, "unsupported batch receipt version");
    ensure!(
        parent
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            == Some(r.result.ticket)
            && !r.result.ticket.is_nil()
            && r.result.workspace_id == r.target.workspace_id
            && r.result.workspace_revision == r.target.workspace_revision
            && r.result.project_id == r.target.project_id
            && r.result.cwd == r.target.cwd
            && r.result.acknowledged == r.acknowledged
            && (!r.acknowledged || r.result.phase == GitIndexPhase::Unknown)
            && r.result.paths.len() <= MAX_BATCH_PATHS
            && !r.result.paths.is_empty()
            && r.index_len
                .is_none_or(|len| len <= super::MAX_INDEX_BYTES as u64),
        "batch receipt binding is inconsistent"
    );
    ensure!(
        r.result.paths.windows(2).all(|p| p[0] < p[1]),
        "batch receipt paths are invalid"
    );
    for path in &r.result.paths {
        super::super::validate_diff_path(path)?;
    }
    ensure!(
        r.result
            .message
            .as_ref()
            .is_none_or(|message| message.len() <= super::MAX_NOTICE_BYTES),
        "batch receipt message exceeds its bound"
    );
    if r.result.phase == GitIndexPhase::Applied {
        f.sync_all()
            .context("cannot verify applied batch receipt durability")?;
        File::open(parent)?
            .sync_all()
            .context("cannot verify applied batch receipt directory durability")?;
    }
    Ok(r)
}
fn receipt_matches(r: &BatchReceipt, t: &Target) -> bool {
    r.target.workspace_id == t.workspace_id
        && r.target.project_id == t.project_id
        && r.target.cwd == t.cwd
}

// Target-specific scanning is routed through the batch engine's durable root by the wrapper.
fn zero_oid(cwd: &Path) -> Result<String> {
    let (bytes, truncated) = super::super::run_git_capture(
        cwd,
        &["rev-parse", "--show-object-format"],
        128,
        Instant::now() + crate::workspace_git::COMMAND_DEADLINE,
        "git object format",
    )?;
    ensure!(!truncated, "object format probe exceeded bound");
    Ok(if std::str::from_utf8(&bytes)?.trim() == "sha256" {
        "0".repeat(64)
    } else {
        "0".repeat(40)
    })
}
fn head_entry(cwd: &Path, head: &str, path: &str) -> Result<Option<(String, String)>> {
    let args = [
        "--literal-pathspecs",
        "ls-tree",
        "-z",
        "--full-tree",
        head,
        "--",
        path,
    ];
    let (bytes, truncated) = super::super::run_git_capture(
        cwd,
        &args,
        16 * 1024,
        Instant::now() + crate::workspace_git::COMMAND_DEADLINE,
        "git batch HEAD entry",
    )?;
    ensure!(!truncated, "HEAD entry probe exceeded bound");
    let Some(record) = bytes.split(|b| *b == 0).find(|r| !r.is_empty()) else {
        return Ok(None);
    };
    let tab = record
        .iter()
        .position(|b| *b == b'\t')
        .context("malformed HEAD tree entry")?;
    let mut fields = record[..tab].split(|b| *b == b' ');
    let mode = std::str::from_utf8(fields.next().context("missing HEAD mode")?)?.to_owned();
    let kind = fields.next().context("missing HEAD type")?;
    let oid = std::str::from_utf8(fields.next().context("missing HEAD oid")?)?.to_owned();
    ensure!(
        kind == b"blob" && matches!(mode.as_str(), "100644" | "100755"),
        "HEAD entry is not a supported regular file"
    );
    ensure!(
        &record[tab + 1..] == path.as_bytes(),
        "HEAD pathspec returned a different path"
    );
    Ok(Some((mode, oid)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(cwd: &Path, args: &[&str]) -> Result<String> {
        let output = Command::new("git").arg("-C").arg(cwd).args(args).output()?;
        ensure!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?)
    }

    fn repo() -> Result<(tempfile::TempDir, PathBuf, Target)> {
        let temp = tempfile::tempdir()?;
        let cwd = temp.path().join("repo");
        fs::create_dir(&cwd)?;
        git(&cwd, &["init", "-q"])?;
        git(&cwd, &["config", "user.name", "PigADE tests"])?;
        git(&cwd, &["config", "user.email", "pigade@example.invalid"])?;
        let target = Target {
            workspace_id: Uuid::new_v4(),
            workspace_revision: 4,
            project_id: Uuid::new_v4(),
            cwd: cwd.clone(),
            project_root: cwd.clone(),
            git_common_dir: fs::canonicalize(cwd.join(".git"))?,
        };
        Ok((temp, cwd, target))
    }

    fn seed(cwd: &Path) -> Result<()> {
        fs::write(cwd.join("a.txt"), b"a base\n")?;
        fs::write(cwd.join("b.txt"), b"b base\n")?;
        git(cwd, &["add", "a.txt", "b.txt"])?;
        git(cwd, &["commit", "-qm", "base"])?;
        Ok(())
    }

    fn apply(
        engine: &mut BatchEngine,
        root: &Path,
        target: &Target,
        action: GitIndexAction,
    ) -> Result<WorkspaceGitIndexBatchResult> {
        let plan = engine.prepare(target.clone(), action)?;
        Ok(engine.request(root, target.clone(), plan.ticket, false, false))
    }

    #[test]
    fn stage_all_handles_edits_deletions_and_untracked_without_touching_worktree() -> Result<()> {
        let (temp, cwd, target) = repo()?;
        seed(&cwd)?;
        fs::write(cwd.join("a.txt"), b"a changed\n")?;
        fs::remove_file(cwd.join("b.txt"))?;
        fs::write(cwd.join("new.txt"), b"new bytes\n")?;
        let before = [fs::read(cwd.join("a.txt"))?, fs::read(cwd.join("new.txt"))?];
        let mut engine = BatchEngine::new(temp.path().join("receipts"));
        let result = apply(
            &mut engine,
            temp.path().join("receipts").as_path(),
            &target,
            GitIndexAction::Stage,
        )?;
        assert_eq!(result.phase, GitIndexPhase::Applied);
        assert_eq!(fs::read(cwd.join("a.txt"))?, before[0]);
        assert_eq!(fs::read(cwd.join("new.txt"))?, before[1]);
        assert!(!cwd.join("b.txt").exists());
        let staged = git(&cwd, &["diff", "--cached", "--name-only"])?;
        assert!(staged.lines().any(|line| line == "a.txt"));
        assert!(staged.lines().any(|line| line == "b.txt"));
        assert!(staged.lines().any(|line| line == "new.txt"));
        Ok(())
    }

    #[test]
    fn newer_preview_invalidates_an_older_same_workspace_ticket() -> Result<()> {
        let (temp, cwd, target) = repo()?;
        fs::write(cwd.join("new-file"), b"reviewed bytes\n")?;
        let root = temp.path().join("receipts");
        let mut engine = BatchEngine::new(root.clone());
        let first = engine.prepare(target.clone(), GitIndexAction::Stage)?;
        let second = engine.prepare(target.clone(), GitIndexAction::Stage)?;
        assert_ne!(first.ticket, second.ticket);
        let before = read_index(&cwd.join(".git/index"))?.bytes;
        let old = engine.request(&root, target.clone(), first.ticket, false, false);
        assert_eq!(old.phase, GitIndexPhase::Unavailable);
        assert_eq!(read_index(&cwd.join(".git/index"))?.bytes, before);
        let current = engine.request(&root, target, second.ticket, false, false);
        assert_eq!(current.phase, GitIndexPhase::Applied);
        Ok(())
    }

    #[test]
    fn unstage_all_restores_index_for_rename_and_deletion_without_rewriting_files() -> Result<()> {
        let (temp, cwd, target) = repo()?;
        seed(&cwd)?;
        git(&cwd, &["mv", "a.txt", "renamed.txt"])?;
        fs::remove_file(cwd.join("b.txt"))?;
        git(&cwd, &["add", "-A"])?;
        let moved = fs::read(cwd.join("renamed.txt"))?;
        let mut engine = BatchEngine::new(temp.path().join("receipts"));
        let result = apply(
            &mut engine,
            temp.path().join("receipts").as_path(),
            &target,
            GitIndexAction::Unstage,
        )?;
        assert_eq!(result.phase, GitIndexPhase::Applied);
        assert_eq!(git(&cwd, &["show", ":0:a.txt"])?, "a base\n");
        assert_eq!(git(&cwd, &["show", ":0:b.txt"])?, "b base\n");
        assert!(git(&cwd, &["show", ":0:renamed.txt"]).is_err());
        assert_eq!(fs::read(cwd.join("renamed.txt"))?, moved);
        assert!(!cwd.join("a.txt").exists() && !cwd.join("b.txt").exists());
        Ok(())
    }

    #[test]
    fn unborn_stage_and_stale_scope_are_atomic() -> Result<()> {
        let (temp, cwd, target) = repo()?;
        fs::write(cwd.join("one"), b"one")?;
        fs::write(cwd.join("two"), b"two")?;
        let mut engine = BatchEngine::new(temp.path().join("receipts"));
        let plan = engine.prepare(target.clone(), GitIndexAction::Stage)?;
        let result = engine.request(temp.path(), target.clone(), plan.ticket, false, false);
        assert_eq!(result.phase, GitIndexPhase::Applied);
        assert!(git(&cwd, &["diff", "--cached", "--name-only"])?.contains("one"));

        fs::write(cwd.join("one"), b"one dirty\n")?;
        fs::write(cwd.join("two"), b"two dirty\n")?;
        let plan = engine.prepare(target.clone(), GitIndexAction::Stage)?;
        let index_before = fs::read(cwd.join(".git/index"))?;
        fs::write(cwd.join("two"), b"stale second file\n")?;
        let result = engine.request(temp.path(), target, plan.ticket, false, false);
        assert_eq!(result.phase, GitIndexPhase::Rejected);
        assert_eq!(fs::read(cwd.join(".git/index"))?, index_before);
        Ok(())
    }

    #[test]
    fn new_untracked_path_since_preview_rejects_whole_batch() -> Result<()> {
        let (temp, cwd, target) = repo()?;
        fs::write(cwd.join("first"), b"first")?;
        let root = temp.path().join("receipts");
        let mut engine = BatchEngine::new(root.clone());
        let plan = engine.prepare(target.clone(), GitIndexAction::Stage)?;
        let before = read_index(&cwd.join(".git/index"))?.bytes;
        fs::write(cwd.join("arrived-late"), b"late")?;
        let result = engine.request(&root, target, plan.ticket, false, false);
        assert_eq!(result.phase, GitIndexPhase::Rejected);
        assert_eq!(read_index(&cwd.join(".git/index"))?.bytes, before);
        assert!(!git(&cwd, &["diff", "--cached", "--name-only"])?.contains("first"));
        Ok(())
    }

    #[test]
    fn foreign_native_lock_keeps_the_index_unchanged() -> Result<()> {
        let (temp, cwd, target) = repo()?;
        fs::write(cwd.join("file"), b"contents")?;
        let root = temp.path().join("receipts");
        let mut engine = BatchEngine::new(root.clone());
        let plan = engine.prepare(target.clone(), GitIndexAction::Stage)?;
        let before = read_index(&cwd.join(".git/index"))?.bytes;
        fs::write(cwd.join(".git/index.lock"), b"foreign lock")?;
        let result = engine.request(&root, target, plan.ticket, false, false);
        assert_eq!(result.phase, GitIndexPhase::Unknown);
        assert_eq!(read_index(&cwd.join(".git/index"))?.bytes, before);
        assert_eq!(fs::read(cwd.join(".git/index.lock"))?, b"foreign lock");
        Ok(())
    }

    #[test]
    fn batch_and_per_file_unknowns_block_each_other_and_commit_gate() -> Result<()> {
        let (temp, cwd, target) = repo()?;
        seed(&cwd)?;
        fs::write(cwd.join("a.txt"), b"changed\n")?;
        let root = temp.path().join("receipts");
        let mut engine = super::super::Engine::new(root);
        let plan = engine.prepare_batch(target.clone(), GitIndexAction::Stage)?;
        super::super::WRITE_FAULT.with(|fault| fault.set(Some(WriteBoundary::AfterDurableAttempt)));
        let batch_result = engine.request_batch(target.clone(), plan.ticket, false, false);
        assert_eq!(batch_result.phase, GitIndexPhase::Unknown);
        assert!(engine.pending_write_for_target(&target)?.is_some());

        let identity = super::super::super::validate_git_identity(
            &target,
            Instant::now() + crate::workspace_git::COMMAND_DEADLINE,
        )?;
        let status = super::super::super::status_for_diff(&cwd)?;
        let entry = status
            .entries
            .iter()
            .find(|entry| entry.path == "a.txt")
            .unwrap();
        let (old, new) = super::super::super::diff_contents(
            &cwd,
            "a.txt",
            super::super::super::GitDiffSide::Worktree,
            &status,
        )?;
        let (ticket, reason) = engine.ticket_for_diff(
            &target,
            &identity,
            &status,
            entry,
            super::super::super::GitDiffSide::Worktree,
            (&old, &new),
        );
        assert!(ticket.is_none() && reason.is_some());

        let ack = engine.request_batch(target.clone(), plan.ticket, false, true);
        assert!(ack.acknowledged);
        let per_file_plan = engine
            .ticket_for_diff(
                &target,
                &identity,
                &status,
                entry,
                super::super::super::GitDiffSide::Worktree,
                (&old, &new),
            )
            .0
            .expect("acknowledged batch no longer blocks a deliberate per-file preview");
        super::super::WRITE_FAULT.with(|fault| fault.set(Some(WriteBoundary::AfterDurableAttempt)));
        let per_file_result = engine.request(target.clone(), per_file_plan.id, false);
        assert_eq!(per_file_result.phase, GitIndexPhase::Unknown);
        assert!(
            engine
                .prepare_batch(target.clone(), GitIndexAction::Stage)
                .is_err()
        );
        assert!(engine.pending_write_for_target(&target)?.is_some());
        Ok(())
    }

    #[test]
    fn durable_unknown_faults_never_reapply_after_reopen() -> Result<()> {
        for boundary in [
            WriteBoundary::AfterDurableAttempt,
            WriteBoundary::AfterIndexRename,
            WriteBoundary::BeforeFinalReceipt,
        ] {
            let (temp, cwd, target) = repo()?;
            fs::write(cwd.join("a"), b"value\n")?;
            let root = temp.path().join("receipts");
            let mut engine = BatchEngine::new(root.clone());
            let plan = engine.prepare(target.clone(), GitIndexAction::Stage)?;
            super::super::WRITE_FAULT.with(|fault| fault.set(Some(boundary)));
            let first = engine.request(&root, target.clone(), plan.ticket, false, false);
            assert_eq!(first.phase, GitIndexPhase::Unknown);
            let index_after = fs::read(cwd.join(".git/index")).ok();
            let mut restarted = BatchEngine::new(root.clone());
            let second = restarted.request(&root, target, plan.ticket, false, false);
            assert_eq!(second.phase, GitIndexPhase::Unknown);
            assert_eq!(fs::read(cwd.join(".git/index")).ok(), index_after);
        }
        Ok(())
    }

    #[test]
    fn corrupt_applied_receipt_fails_closed_and_never_repeats_the_write() -> Result<()> {
        let (temp, cwd, target) = repo()?;
        fs::write(cwd.join("new-file"), b"reviewed bytes\n")?;
        let root = temp.path().join("receipts");
        let mut engine = BatchEngine::new(root.clone());
        let plan = engine.prepare(target.clone(), GitIndexAction::Stage)?;
        let applied = engine.request(&root, target.clone(), plan.ticket, false, false);
        assert_eq!(applied.phase, GitIndexPhase::Applied);
        let index_after = fs::read(cwd.join(".git/index"))?;
        let receipt = receipt_path(&root, plan.ticket);
        let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&receipt)?)?;
        *record
            .pointer_mut("/result/ticket")
            .context("fixture ticket field missing")? = serde_json::json!(Uuid::new_v4());
        fs::write(&receipt, serde_json::to_vec(&record)?)?;

        let mut restarted = BatchEngine::new(root.clone());
        assert!(restarted.pending_for_target(&target).is_err());
        let repeated = restarted.request(&root, target, plan.ticket, false, false);
        assert_eq!(repeated.phase, GitIndexPhase::Unavailable);
        assert_eq!(fs::read(cwd.join(".git/index"))?, index_after);
        Ok(())
    }

    #[test]
    fn receipt_storage_and_working_file_bounds_reject_whole_batch() -> Result<()> {
        let (temp, cwd, target) = repo()?;
        fs::write(cwd.join("large"), vec![b'x'; MAX_BATCH_FILE_BYTES + 1])?;
        let mut engine = BatchEngine::new(temp.path().join("receipts"));
        assert!(
            engine
                .prepare(target.clone(), GitIndexAction::Stage)
                .is_err()
        );
        assert!(
            !temp
                .path()
                .join("receipts/git-index-batch-operations")
                .exists()
        );
        let mut permissions = fs::metadata(cwd.join("large"))?.permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(cwd.join("large"), permissions)?;
        Ok(())
    }

    #[test]
    fn combined_file_bytes_and_status_entry_limits_reject_whole_batch() -> Result<()> {
        let (temp, cwd, target) = repo()?;
        let per_file = vec![b'x'; MAX_BATCH_FILE_BYTES];
        for n in 0..33 {
            fs::write(cwd.join(format!("file-{n:02}")), &per_file)?;
        }
        let mut engine = BatchEngine::new(temp.path().join("receipts"));
        assert!(
            engine
                .prepare(target.clone(), GitIndexAction::Stage)
                .is_err()
        );
        for n in 33..129 {
            fs::write(cwd.join(format!("file-{n:03}")), b"x")?;
        }
        assert!(engine.prepare(target, GitIndexAction::Stage).is_err());
        Ok(())
    }
}
