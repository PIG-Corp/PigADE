//! Durable, diff-bound per-file index mutations for workspace Git.
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    COMMAND_DEADLINE, GitDiffSide, GitIndexAction, GitIndexPhase, GitIndexTicket,
    MAX_DIFF_PATH_BYTES, OUTPUT_LIMIT, Parsed, Target, WorkspaceGitEntry, WorkspaceGitIndexResult,
};

const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILE_BYTES: usize = OUTPUT_LIMIT;
const MAX_NOTICE_BYTES: usize = super::MAX_NOTICE_BYTES;
const MAX_LEASES: usize = 8;
const LEASE_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_RECEIPTS: usize = 4096;
const MAX_RECEIPT_BYTES: u64 = 64 * 1024;

type IndexFileId = (u64, u64, u64, i64, i64, i64, i64);

#[derive(Debug, Clone)]
struct FileSnapshot {
    bytes: Vec<u8>,
    mode: u32,
}

#[derive(Debug, Clone)]
struct Lease {
    target: Target,
    ticket: GitIndexTicket,
    cwd: PathBuf,
    project_root: PathBuf,
    common_dir: PathBuf,
    git_dir: PathBuf,
    index_path: PathBuf,
    index_bytes: Option<Vec<u8>>,
    index_file_id: Option<IndexFileId>,
    head: Option<String>,
    branch: Option<String>,
    path: String,
    old_path: Option<String>,
    side: GitDiffSide,
    displayed_old: Vec<u8>,
    displayed_new: Vec<u8>,
    worktree: Option<FileSnapshot>,
    expected_entry: WorkspaceGitEntry,
    expires: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    #[serde(default)]
    acknowledged: bool,
    result: WorkspaceGitIndexResult,
    target: ReceiptTarget,
    payload: ReceiptPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptTarget {
    workspace_id: Uuid,
    workspace_revision: u64,
    project_id: Uuid,
    cwd: PathBuf,
    git_dir: PathBuf,
    head: Option<String>,
    branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptPayload {
    path: String,
    old_path: Option<String>,
    side: GitDiffSide,
    index_len: Option<u64>,
    index_file_id: Option<IndexFileId>,
    displayed_old_len: u64,
    displayed_new_len: u64,
    worktree_len: Option<u64>,
    worktree_mode: Option<u32>,
}

pub(super) struct Engine {
    root: PathBuf,
    leases: HashMap<Uuid, Lease>,
}

impl Engine {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            leases: HashMap::new(),
        }
    }

    pub(super) fn ticket_for_diff(
        &mut self,
        target: &Target,
        identity: &super::FrozenIdentity,
        status: &Parsed,
        entry: &WorkspaceGitEntry,
        side: GitDiffSide,
        contents: (&[u8], &[u8]),
    ) -> (Option<GitIndexTicket>, Option<String>) {
        match self.pending_for_target(target) {
            Ok(Some(_)) => return (None, Some("A previous index operation has an unknown outcome; inspect and acknowledge it before starting another".into())),
            Err(error) => return (None, Some(format!("Index action unavailable because operation receipts could not be checked: {error:#}"))),
            Ok(None) => (),
        }
        match self.create_lease(target, identity, status, entry, side, contents) {
            Ok(ticket) => (Some(ticket), None),
            Err(error) => (None, Some(format!("Index action unavailable: {error:#}"))),
        }
    }

    fn create_lease(
        &mut self,
        target: &Target,
        identity: &super::FrozenIdentity,
        status: &Parsed,
        entry: &WorkspaceGitEntry,
        side: GitDiffSide,
        contents: (&[u8], &[u8]),
    ) -> Result<GitIndexTicket> {
        let path = entry.path.as_str();
        let (old, new) = contents;
        let action = match side {
            GitDiffSide::Index => GitIndexAction::Unstage,
            GitDiffSide::Worktree | GitDiffSide::Untracked => GitIndexAction::Stage,
            GitDiffSide::Conflict => bail!("conflict entries cannot be staged here"),
        };
        super::validate_diff_path(path)?;
        ensure!(path.len() <= MAX_DIFF_PATH_BYTES, "path is too long");
        ensure!(!status.truncated, "Git status is incomplete");
        let git_dir = git_dir(&identity.cwd)?;
        let index_path = index_path(&identity.cwd)?;
        ensure!(
            index_path.parent().is_some_and(|parent| parent == git_dir),
            "Git index is not a direct child of the per-worktree Git directory"
        );
        let index_before = read_index(&index_path)?;
        let index_id_before = file_id(&index_path)?;
        let worktree = super::read_worktree_file(&identity.cwd, path)?.map(|file| FileSnapshot {
            bytes: file.bytes,
            mode: file.mode,
        });
        match side {
            GitDiffSide::Worktree | GitDiffSide::Untracked => {
                ensure!(
                    worktree
                        .as_ref()
                        .map(|file| file.bytes.as_slice())
                        .unwrap_or(&[])
                        == new,
                    "worktree bytes changed while preparing the preview"
                );
            }
            GitDiffSide::Index => (),
            GitDiffSide::Conflict => bail!("conflict entries cannot be staged here"),
        }
        let status_now = super::status_for_diff(&identity.cwd)?;
        ensure!(
            status_now.branch == status.branch
                && status_now.head == status.head
                && status_now
                    .entries
                    .iter()
                    .find(|candidate| candidate.path == path)
                    .is_some_and(|candidate| candidate == entry),
            "Git status changed while preparing the preview"
        );
        let index_after = read_index(&index_path)?;
        ensure!(
            index_before.bytes == index_after.bytes,
            "Git index changed while preparing the preview"
        );
        let index_id_after = file_id(&index_path)?;
        ensure!(
            index_id_before == index_id_after,
            "Git index identity changed while preparing the preview"
        );
        let worktree_after =
            super::read_worktree_file(&identity.cwd, path)?.map(|file| FileSnapshot {
                bytes: file.bytes,
                mode: file.mode,
            });
        ensure!(
            same_file(&worktree, &worktree_after),
            "worktree file changed while preparing the preview"
        );
        let id = Uuid::new_v4();
        let ticket = GitIndexTicket { id, action };
        let lease = Lease {
            target: target.clone(),
            ticket,
            cwd: identity.cwd.clone(),
            project_root: identity.project_root.clone(),
            common_dir: identity.common_dir.clone(),
            git_dir,
            index_path,
            index_bytes: index_before.bytes,
            index_file_id: index_id_before,
            head: status.head.clone(),
            branch: status.branch.clone(),
            path: path.to_owned(),
            old_path: entry.original_path.clone(),
            side,
            displayed_old: old.to_vec(),
            displayed_new: new.to_vec(),
            worktree: worktree_after,
            expected_entry: entry.clone(),
            expires: Instant::now() + LEASE_TTL,
        };
        self.expire_leases();
        if self.leases.len() >= MAX_LEASES {
            let oldest = self
                .leases
                .iter()
                .min_by_key(|(_, lease)| lease.expires)
                .map(|(id, _)| *id);
            if let Some(oldest) = oldest {
                self.leases.remove(&oldest);
            }
        }
        let _binding = self.binding_receipt(&lease)?;
        self.leases.insert(id, lease);
        Ok(ticket)
    }

    pub(super) fn request(
        &mut self,
        target: Target,
        ticket: Uuid,
        inspect: bool,
    ) -> WorkspaceGitIndexResult {
        let receipt_path = self.receipt_path(ticket);
        if let Ok(path) = &receipt_path
            && let Ok(receipt) = read_receipt(path)
        {
            if !receipt_scope_matches(&receipt, &target)
                || receipt.target.workspace_revision != target.workspace_revision
            {
                return unavailable_for(
                    &target,
                    ticket,
                    "ticket belongs to a different workspace scope",
                );
            }
            return receipt.result;
        }
        if inspect {
            return unavailable_for(&target, ticket, "no durable receipt exists for this ticket");
        }
        let Some(lease) = self.leases.remove(&ticket) else {
            return unavailable_for(&target, ticket, "diff-bound ticket is missing or expired");
        };
        if lease.expires <= Instant::now() || !target_matches(&lease.target, &target) {
            return result_for(
                &lease,
                GitIndexPhase::Rejected,
                Some("ticket expired or workspace scope changed".into()),
            );
        }
        if lease.ticket.id != ticket {
            return result_for(
                &lease,
                GitIndexPhase::Rejected,
                Some("ticket mismatch".into()),
            );
        }
        if let Err(error) = self.ensure_receipt_capacity() {
            return result_for(
                &lease,
                GitIndexPhase::Unavailable,
                Some(format!("operation receipt storage unavailable: {error:#}")),
            );
        }
        if let Err(error) = validate_lease(&lease) {
            let result = result_for(
                &lease,
                GitIndexPhase::Rejected,
                Some(format!("preview is stale: {error:#}")),
            );
            if let Ok(mut receipt) = self.binding_receipt(&lease) {
                receipt.result = result.clone();
                if let Ok(path) = &receipt_path {
                    let _ = write_receipt(path, &receipt);
                }
            }
            return result;
        }
        let mut receipt = match self.binding_receipt(&lease) {
            Ok(receipt) => receipt,
            Err(error) => {
                return result_for(
                    &lease,
                    GitIndexPhase::Unavailable,
                    Some(format!("cannot bind operation receipt: {error:#}")),
                );
            }
        };
        receipt.result.phase = GitIndexPhase::Unknown;
        receipt.result.message = Some("operation outcome is being recorded".into());
        let receipt_path = match receipt_path {
            Ok(path) => path,
            Err(error) => {
                return result_for(
                    &lease,
                    GitIndexPhase::Unavailable,
                    Some(format!("receipt path unavailable: {error:#}")),
                );
            }
        };
        if let Err(error) = write_receipt(&receipt_path, &receipt) {
            return result_for(
                &lease,
                GitIndexPhase::Unavailable,
                Some(format!(
                    "could not durably record operation attempt: {error:#}"
                )),
            );
        }

        match apply(&lease) {
            Ok(()) => {
                receipt.result.phase = GitIndexPhase::Applied;
                receipt.result.message = None;
                if write_receipt(&receipt_path, &receipt).is_err() {
                    receipt.result.phase = GitIndexPhase::Unknown;
                    receipt.result.message = Some("index replacement completed, but its durable receipt could not be confirmed".into());
                    // Best-effort restoration of the durable Attempting marker.
                    // If this also fails, read_receipt fsyncs Applied records
                    // before allowing a later inspect to report success.
                    let mut attempted = receipt.clone();
                    attempted.result.phase = GitIndexPhase::Unknown;
                    attempted.result.message = Some(
                        "operation outcome is uncertain; receipt durability needs inspection"
                            .into(),
                    );
                    let _ = write_receipt(&receipt_path, &attempted);
                }
                receipt.result
            }
            Err(error) => {
                // Once the attempt receipt exists, an error may follow an
                // externally visible filter/object/index side effect. Never
                // turn that state into a retryable rejection.
                receipt.result.phase = GitIndexPhase::Unknown;
                receipt.result.message = Some(super::bounded_notice(format!(
                    "operation outcome is uncertain: {error:#}"
                )));
                let _ = write_receipt(&receipt_path, &receipt);
                receipt.result
            }
        }
    }

    fn binding_receipt(&self, lease: &Lease) -> Result<Receipt> {
        ensure!(
            lease.displayed_old.len() <= MAX_FILE_BYTES
                && lease.displayed_new.len() <= MAX_FILE_BYTES,
            "displayed diff input exceeds bounds"
        );
        Ok(Receipt {
            version: 1,
            acknowledged: false,
            result: result_for(
                lease,
                GitIndexPhase::Unknown,
                Some("operation outcome is uncertain".into()),
            ),
            target: ReceiptTarget {
                workspace_id: lease.target.workspace_id,
                workspace_revision: lease.target.workspace_revision,
                project_id: lease.target.project_id,
                cwd: lease.cwd.clone(),
                git_dir: lease.git_dir.clone(),
                head: lease.head.clone(),
                branch: lease.branch.clone(),
            },
            payload: ReceiptPayload {
                path: lease.path.clone(),
                old_path: lease.old_path.clone(),
                side: lease.side,
                index_len: lease.index_bytes.as_ref().map(|bytes| bytes.len() as u64),
                index_file_id: lease.index_file_id,
                displayed_old_len: lease.displayed_old.len() as u64,
                displayed_new_len: lease.displayed_new.len() as u64,
                worktree_len: lease.worktree.as_ref().map(|file| file.bytes.len() as u64),
                worktree_mode: lease.worktree.as_ref().map(|file| file.mode),
            },
        })
    }

    fn expire_leases(&mut self) {
        self.leases
            .retain(|_, lease| lease.expires > Instant::now());
    }

    fn receipts_dir(&self) -> PathBuf {
        self.root.join("git-index-operations")
    }

    fn receipt_path(&self, id: Uuid) -> Result<PathBuf> {
        Ok(self
            .receipts_dir()
            .join(id.to_string())
            .join("receipt.json"))
    }

    fn ensure_receipt_capacity(&self) -> Result<()> {
        let dir = self.receipts_dir();
        ensure_receipt_dir(&dir)?;
        let count = fs::read_dir(dir)?.take(MAX_RECEIPTS + 1).count();
        ensure!(
            count < MAX_RECEIPTS,
            "receipt store has reached its fixed 4096-record limit"
        );
        Ok(())
    }

    pub(super) fn pending_for_target(
        &self,
        target: &Target,
    ) -> Result<Option<WorkspaceGitIndexResult>> {
        let entries = match fs::read_dir(self.receipts_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut newest: Option<(SystemTime, WorkspaceGitIndexResult)> = None;
        let mut count = 0;
        for entry in entries.take(MAX_RECEIPTS + 1) {
            let entry = entry?;
            count += 1;
            ensure!(
                count <= MAX_RECEIPTS,
                "receipt store exceeds its fixed record limit"
            );
            let file_type = entry.file_type()?;
            ensure!(
                !file_type.is_symlink(),
                "receipt store contains a symbolic-link entry"
            );
            if !file_type.is_dir() {
                continue;
            }
            let path = entry.path().join("receipt.json");
            let receipt = read_receipt(&path)
                .with_context(|| format!("cannot read operation receipt {}", path.display()))?;
            if receipt.acknowledged
                || receipt.result.phase != GitIndexPhase::Unknown
                || !receipt_scope_matches(&receipt, target)
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

    pub(super) fn acknowledge(&mut self, target: &Target, ticket: Uuid) -> WorkspaceGitIndexResult {
        let path = match self.receipt_path(ticket) {
            Ok(path) => path,
            Err(error) => {
                return unavailable_for(target, ticket, &format!("receipt unavailable: {error:#}"));
            }
        };
        let mut receipt = match read_receipt(&path) {
            Ok(receipt) => receipt,
            Err(_) => {
                return unavailable_for(
                    target,
                    ticket,
                    "no durable operation receipt exists for this ticket",
                );
            }
        };
        if !receipt_scope_matches(&receipt, target)
            || receipt.target.workspace_revision != target.workspace_revision
        {
            return unavailable_for(
                target,
                ticket,
                "ticket belongs to a different workspace scope",
            );
        }
        if receipt.result.phase != GitIndexPhase::Unknown {
            return receipt.result;
        }
        receipt.acknowledged = true;
        receipt.result.acknowledged = true;
        if let Err(error) = write_receipt(&path, &receipt) {
            let mut result = receipt.result;
            result.acknowledged = false;
            result.message = Some(format!("could not record acknowledgement: {error:#}"));
            result.message = Some(super::bounded_notice(
                result.message.as_deref().unwrap_or_default(),
            ));
            return result;
        }
        receipt.result
    }
}

pub(super) fn unavailable_result(
    target: &Target,
    ticket: Uuid,
    message: String,
) -> WorkspaceGitIndexResult {
    WorkspaceGitIndexResult {
        ticket,
        workspace_id: target.workspace_id,
        workspace_revision: target.workspace_revision,
        project_id: target.project_id,
        cwd: target.cwd.clone(),
        path: "unknown".into(),
        side: GitDiffSide::Worktree,
        action: GitIndexAction::Stage,
        phase: GitIndexPhase::Unavailable,
        acknowledged: false,
        message: Some(super::bounded_notice(message)),
    }
}

fn result_for(
    lease: &Lease,
    phase: GitIndexPhase,
    message: Option<String>,
) -> WorkspaceGitIndexResult {
    WorkspaceGitIndexResult {
        ticket: lease.ticket.id,
        workspace_id: lease.target.workspace_id,
        workspace_revision: lease.target.workspace_revision,
        project_id: lease.target.project_id,
        cwd: lease.cwd.clone(),
        path: lease.path.clone(),
        side: lease.side,
        action: lease.ticket.action,
        phase,
        acknowledged: false,
        message: message.map(super::bounded_notice),
    }
}

fn unavailable_for(target: &Target, ticket: Uuid, message: &str) -> WorkspaceGitIndexResult {
    WorkspaceGitIndexResult {
        ticket,
        workspace_id: target.workspace_id,
        workspace_revision: target.workspace_revision,
        project_id: target.project_id,
        cwd: target.cwd.clone(),
        path: "unknown".into(),
        side: GitDiffSide::Worktree,
        action: GitIndexAction::Stage,
        phase: GitIndexPhase::Unavailable,
        acknowledged: false,
        message: Some(super::bounded_notice(message)),
    }
}

fn target_matches(first: &Target, second: &Target) -> bool {
    first.workspace_id == second.workspace_id
        && first.workspace_revision == second.workspace_revision
        && first.project_id == second.project_id
        && first.cwd == second.cwd
        && first.project_root == second.project_root
        && first.git_common_dir == second.git_common_dir
}

fn receipt_scope_matches(receipt: &Receipt, target: &Target) -> bool {
    receipt.target.workspace_id == target.workspace_id
        && receipt.target.project_id == target.project_id
        && receipt.target.cwd == target.cwd
}

fn same_file(left: &Option<FileSnapshot>, right: &Option<FileSnapshot>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.bytes == right.bytes && left.mode == right.mode,
        _ => false,
    }
}

#[derive(Debug)]
struct IndexSnapshot {
    bytes: Option<Vec<u8>>,
}

fn read_index(path: &Path) -> Result<IndexSnapshot> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(IndexSnapshot { bytes: None });
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot safely open Git index {}", path.display()));
        }
    };
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Git index is not a regular file");
    ensure!(
        metadata.len() <= MAX_INDEX_BYTES as u64,
        "Git index exceeds 8 MiB bound"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_INDEX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_INDEX_BYTES,
        "Git index exceeds 8 MiB bound"
    );
    Ok(IndexSnapshot { bytes: Some(bytes) })
}

fn git_dir(cwd: &Path) -> Result<PathBuf> {
    let (bytes, truncated) = super::run_git_capture(
        cwd,
        &["rev-parse", "--absolute-git-dir"],
        16 * 1024,
        Instant::now() + COMMAND_DEADLINE,
        "git per-worktree directory",
    )?;
    ensure!(!truncated, "Git directory probe exceeded its bound");
    let value = std::str::from_utf8(&bytes)?.trim_end_matches('\n');
    fs::canonicalize(value).context("cannot canonicalize per-worktree Git directory")
}

fn index_path(cwd: &Path) -> Result<PathBuf> {
    let (bytes, truncated) = super::run_git_capture(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        16 * 1024,
        Instant::now() + COMMAND_DEADLINE,
        "git index path",
    )?;
    ensure!(!truncated, "Git index-path probe exceeded its bound");
    let value = PathBuf::from(std::str::from_utf8(&bytes)?.trim_end_matches('\n'));
    let parent = fs::canonicalize(value.parent().context("Git index path has no parent")?)?;
    Ok(parent.join(value.file_name().context("Git index path has no name")?))
}

fn file_id(path: &Path) -> Result<Option<IndexFileId>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_file(),
                "Git index path is not a regular file"
            );
            Ok(Some((
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn ensure_receipt_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "receipt directory must be a real directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    // Sync the directory entry too: a synced receipt inside an unsynced new
    // UUID directory is not a durable pre-effect operation record.
    File::open(path)?.sync_all()?;
    File::open(path.parent().context("receipt directory has no parent")?)?.sync_all()?;
    Ok(())
}

fn read_receipt(path: &Path) -> Result<Receipt> {
    let parent = path.parent().context("receipt has no parent directory")?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    ensure!(
        parent_metadata.is_dir() && !parent_metadata.file_type().is_symlink(),
        "receipt directory must be real"
    );
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_RECEIPT_BYTES,
        "invalid receipt file"
    );
    let receipt: Receipt = serde_json::from_reader(&file)?;
    ensure!(receipt.version == 1, "unsupported receipt version");
    let result = &receipt.result;
    ensure!(
        parent
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| Uuid::parse_str(name).ok())
            == Some(result.ticket)
            && !result.ticket.is_nil()
            && result.workspace_id == receipt.target.workspace_id
            && result.workspace_revision == receipt.target.workspace_revision
            && result.project_id == receipt.target.project_id
            && result.cwd == receipt.target.cwd
            && result.path == receipt.payload.path
            && result.side == receipt.payload.side
            && result.acknowledged == receipt.acknowledged
            && (!receipt.acknowledged || result.phase == GitIndexPhase::Unknown)
            && matches!(
                (result.action, result.side),
                (
                    GitIndexAction::Stage,
                    GitDiffSide::Worktree | GitDiffSide::Untracked
                ) | (GitIndexAction::Unstage, GitDiffSide::Index)
            ),
        "operation receipt binding is inconsistent"
    );
    super::validate_diff_path(&result.path)?;
    if let Some(path) = &receipt.payload.old_path {
        super::validate_diff_path(path)?;
    }
    ensure!(
        receipt
            .result
            .message
            .as_ref()
            .is_none_or(|message| message.len() <= MAX_NOTICE_BYTES),
        "operation receipt message exceeds its bound"
    );
    if receipt.result.phase == GitIndexPhase::Applied {
        file.sync_all()
            .context("cannot verify applied receipt durability")?;
        File::open(parent)?
            .sync_all()
            .context("cannot verify applied receipt directory durability")?;
    }
    Ok(receipt)
}

fn apply(lease: &Lease) -> Result<()> {
    #[cfg(test)]
    fail_at(WriteBoundary::AfterDurableAttempt)?;
    validate_lease(lease)?;
    let parent = lease
        .index_path
        .parent()
        .context("Git index has no parent")?;
    let scratch = tempfile::Builder::new()
        .prefix(".pigade-index-")
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
            "git read-tree empty",
        )?;
    }
    prepare_index(lease, &candidate)?;
    validate_lease(lease)?;
    let mut lock = IndexLock::acquire(&lease.index_path)?;
    ensure!(
        read_index(&lease.index_path)?.bytes == lease.index_bytes
            && file_id(&lease.index_path)? == lease.index_file_id,
        "Git index changed before lock acquisition"
    );
    lock.write_candidate(&candidate)?;
    ensure!(
        read_index(&lease.index_path)?.bytes == lease.index_bytes
            && file_id(&lease.index_path)? == lease.index_file_id,
        "Git index changed under native lock"
    );
    lock.commit()?;
    #[cfg(test)]
    fail_at(WriteBoundary::BeforeFinalReceipt)?;
    Ok(())
}

fn validate_lease(lease: &Lease) -> Result<()> {
    let identity = super::validate_git_identity(&lease.target, Instant::now() + COMMAND_DEADLINE)?;
    ensure!(
        identity.cwd == lease.cwd
            && identity.project_root == lease.project_root
            && identity.common_dir == lease.common_dir,
        "workspace Git identity changed after preview"
    );
    ensure!(
        git_dir(&lease.cwd)? == lease.git_dir && index_path(&lease.cwd)? == lease.index_path,
        "per-worktree Git index identity changed"
    );
    ensure!(
        read_index(&lease.index_path)?.bytes == lease.index_bytes
            && file_id(&lease.index_path)? == lease.index_file_id,
        "Git index changed after preview"
    );
    ensure!(
        !is_split_index(&lease.cwd)?,
        "split index is not supported for safe per-file staging"
    );
    let current_status = super::status_for_diff(&lease.cwd)?;
    ensure!(
        current_status.head == lease.head && current_status.branch == lease.branch,
        "HEAD or branch changed after preview"
    );
    let entry = current_status
        .entries
        .iter()
        .find(|entry| entry.path == lease.path)
        .context("selected Git status entry disappeared")?;
    ensure!(
        entry == &lease.expected_entry,
        "selected Git status entry changed after preview"
    );
    ensure!(
        super::diff_side_present(entry, lease.side),
        "selected Git status side is no longer present"
    );
    let (old, new) = super::diff_contents(&lease.cwd, &lease.path, lease.side, &current_status)?;
    ensure!(
        old == lease.displayed_old && new == lease.displayed_new,
        "selected diff changed after preview"
    );
    let current_file =
        super::read_worktree_file(&lease.cwd, &lease.path)?.map(|file| FileSnapshot {
            bytes: file.bytes,
            mode: file.mode,
        });
    ensure!(
        same_file(&current_file, &lease.worktree),
        "worktree file changed after preview"
    );
    Ok(())
}

fn is_split_index(cwd: &Path) -> Result<bool> {
    let (bytes, truncated) = super::run_git_capture(
        cwd,
        &["rev-parse", "--shared-index-path"],
        4096,
        Instant::now() + COMMAND_DEADLINE,
        "git split-index probe",
    )?;
    ensure!(!truncated, "split-index probe exceeded its bound");
    if !bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(true);
    }
    let (sparse, truncated) = super::run_git_capture(
        cwd,
        &["config", "--bool", "--default=false", "index.sparse"],
        128,
        Instant::now() + COMMAND_DEADLINE,
        "git sparse-index probe",
    )?;
    ensure!(!truncated, "sparse-index probe exceeded its bound");
    let sparse_index = std::str::from_utf8(&sparse).is_ok_and(|value| value.trim() == "true");
    let (checkout, truncated) = super::run_git_capture(
        cwd,
        &["config", "--bool", "--default=false", "core.sparseCheckout"],
        128,
        Instant::now() + COMMAND_DEADLINE,
        "git sparse-checkout probe",
    )?;
    ensure!(!truncated, "sparse-checkout probe exceeded its bound");
    let sparse_checkout = std::str::from_utf8(&checkout).is_ok_and(|value| value.trim() == "true");
    Ok(sparse_index || sparse_checkout)
}

struct IndexLock {
    path: PathBuf,
    file: Option<File>,
    index: PathBuf,
    dev: u64,
    ino: u64,
}

impl IndexLock {
    fn acquire(index: &Path) -> Result<Self> {
        let parent = index.parent().context("Git index has no parent")?;
        let name = index
            .file_name()
            .context("Git index has no name")?
            .to_string_lossy();
        let path = parent.join(format!("{name}.lock"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o666)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .with_context(|| format!("cannot acquire native Git index lock {}", path.display()))?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file(),
            "native Git index lock is not a regular file"
        );
        Ok(Self {
            path,
            file: Some(file),
            index: index.to_path_buf(),
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }

    fn ensure_owned(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.path)?;
        ensure!(
            metadata.file_type().is_file()
                && metadata.dev() == self.dev
                && metadata.ino() == self.ino,
            "native Git index lock path was replaced"
        );
        if let Some(file) = &self.file {
            let opened = file.metadata()?;
            ensure!(
                opened.dev() == self.dev && opened.ino() == self.ino,
                "native Git index lock handle changed"
            );
        }
        Ok(())
    }

    fn write_candidate(&mut self, candidate: &Path) -> Result<()> {
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(candidate)?;
        let metadata = source.metadata()?;
        ensure!(
            metadata.is_file(),
            "private Git index candidate is not a regular file"
        );
        ensure!(
            metadata.len() <= MAX_INDEX_BYTES as u64,
            "private Git index candidate exceeds 8 MiB bound"
        );
        self.ensure_owned()?;
        let file = self
            .file
            .as_mut()
            .context("native Git index lock is closed")?;
        match fs::symlink_metadata(&self.index) {
            Ok(metadata) => {
                ensure!(metadata.is_file(), "native Git index is not a regular file");
                file.set_permissions(metadata.permissions())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        std::io::copy(&mut source, file)?;
        file.sync_all()?;
        self.ensure_owned()?;
        Ok(())
    }

    fn commit(mut self) -> Result<()> {
        self.ensure_owned()?;
        self.file.take();
        fs::rename(&self.path, &self.index)?;
        #[cfg(test)]
        fail_at(WriteBoundary::AfterIndexRename)?;
        File::open(self.index.parent().context("Git index has no parent")?)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WriteBoundary {
    AfterDurableAttempt,
    AfterIndexRename,
    BeforeFinalReceipt,
}

#[cfg(test)]
thread_local! {
    static WRITE_FAULT: std::cell::Cell<Option<WriteBoundary>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn fail_at(boundary: WriteBoundary) -> Result<()> {
    if WRITE_FAULT.with(|fault| {
        if fault.get() == Some(boundary) {
            fault.set(None);
            true
        } else {
            false
        }
    }) {
        bail!("injected index write failure at {boundary:?}");
    }
    Ok(())
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        if self.ensure_owned().is_ok() && self.file.is_some() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn prepare_index(lease: &Lease, private_index: &Path) -> Result<bool> {
    match lease.ticket.action {
        GitIndexAction::Stage => {
            ensure!(
                matches!(lease.side, GitDiffSide::Worktree | GitDiffSide::Untracked),
                "only worktree/untracked files can be staged here"
            );
            let mut input = Vec::new();
            if let Some(file) = &lease.worktree {
                let oid = run_git_input(
                    &lease.cwd,
                    Some(private_index),
                    &[
                        "hash-object",
                        "-w",
                        "--stdin",
                        &format!("--path={}", lease.path),
                    ],
                    &file.bytes,
                    256,
                    "git hash-object stage",
                )?;
                let oid = std::str::from_utf8(&oid)?.trim().to_owned();
                ensure!(
                    oid.bytes().all(|byte| byte.is_ascii_hexdigit())
                        && matches!(oid.len(), 40 | 64),
                    "Git returned an invalid object id"
                );
                let mode = if core_file_mode(&lease.cwd)? {
                    file.mode
                } else {
                    index_entry_mode(&lease.cwd, private_index, &lease.path)?.unwrap_or(0o100644)
                };
                let mode = format!("{mode:o}");
                input.extend(format!("{mode} {oid} 0\t").into_bytes());
                input.extend_from_slice(lease.path.as_bytes());
                input.push(0);
            } else {
                ensure!(
                    lease.side == GitDiffSide::Worktree && lease.displayed_new.is_empty(),
                    "missing worktree file is not a staged deletion"
                );
                input.extend(format!("0 {}\t", zero_oid(&lease.cwd)?).into_bytes());
                input.extend_from_slice(lease.path.as_bytes());
                input.push(0);
            }
            if lease.expected_entry.worktree_status == 'R' {
                let old_path = lease
                    .old_path
                    .as_deref()
                    .context("worktree rename source is missing")?;
                super::validate_diff_path(old_path)?;
                input.extend(format!("0 {}\t", zero_oid(&lease.cwd)?).into_bytes());
                input.extend_from_slice(old_path.as_bytes());
                input.push(0);
            }
            run_git_input(
                &lease.cwd,
                Some(private_index),
                &["update-index", "-z", "--index-info"],
                &input,
                4096,
                "git update-index stage",
            )?;
        }
        GitIndexAction::Unstage => {
            ensure!(
                lease.side == GitDiffSide::Index,
                "only index entries can be unstaged here"
            );
            let restore = if lease.expected_entry.index_status == 'C' {
                None
            } else {
                Some(lease.old_path.as_deref().unwrap_or(&lease.path))
            };
            let zero = zero_oid(&lease.cwd)?;
            let mut remove_new = format!("0 {zero}\t").into_bytes();
            remove_new.extend_from_slice(lease.path.as_bytes());
            remove_new.push(0);
            let mut input = remove_new;
            if let (Some(head), Some(restore)) = (&lease.head, restore)
                && let Some((mode, oid)) = head_entry(&lease.cwd, head, restore)?
            {
                input.extend(format!("{mode} {oid} 0\t").into_bytes());
                input.extend_from_slice(restore.as_bytes());
                input.push(0);
            }
            run_git_input(
                &lease.cwd,
                Some(private_index),
                &["update-index", "-z", "--index-info"],
                &input,
                4096,
                "git update-index unstage",
            )?;
        }
    }
    Ok(true)
}

fn core_file_mode(cwd: &Path) -> Result<bool> {
    let (bytes, truncated) = super::run_git_capture(
        cwd,
        &["config", "--bool", "--default=true", "core.fileMode"],
        128,
        Instant::now() + COMMAND_DEADLINE,
        "git file-mode probe",
    )?;
    ensure!(!truncated, "file-mode probe exceeded its bound");
    Ok(std::str::from_utf8(&bytes).is_ok_and(|value| value.trim() == "true"))
}

fn index_entry_mode(cwd: &Path, index: &Path, path: &str) -> Result<Option<u32>> {
    let bytes = run_git_input(
        cwd,
        Some(index),
        &[
            "--literal-pathspecs",
            "ls-files",
            "--stage",
            "-z",
            "--",
            path,
        ],
        &[],
        16 * 1024,
        "git index mode",
    )?;
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("malformed staged index entry")?;
        let (header, path_with_tab) = record.split_at(tab);
        let found_path = &path_with_tab[1..];
        if found_path != path.as_bytes() {
            continue;
        }
        let fields = header.split(|byte| *byte == b' ').collect::<Vec<_>>();
        ensure!(
            fields.len() == 3 && fields[2] == b"0",
            "index entry is not stage zero"
        );
        let mode = std::str::from_utf8(fields[0])?;
        let mode = u32::from_str_radix(mode, 8)?;
        ensure!(
            matches!(mode, 0o100644 | 0o100755),
            "staged file mode is unsupported"
        );
        return Ok(Some(mode));
    }
    Ok(None)
}

fn zero_oid(cwd: &Path) -> Result<String> {
    let (bytes, truncated) = super::run_git_capture(
        cwd,
        &["rev-parse", "--show-object-format"],
        128,
        Instant::now() + COMMAND_DEADLINE,
        "git object format",
    )?;
    ensure!(!truncated, "object-format probe exceeded its bound");
    match std::str::from_utf8(&bytes)?.trim() {
        "sha1" => Ok("0".repeat(40)),
        "sha256" => Ok("0".repeat(64)),
        _ => bail!("unsupported Git object format"),
    }
}

fn head_entry(cwd: &Path, head: &str, path: &str) -> Result<Option<(String, String)>> {
    let (bytes, truncated) = super::run_git_capture(
        cwd,
        &["--literal-pathspecs", "ls-tree", "-z", head, "--", path],
        16 * 1024,
        Instant::now() + COMMAND_DEADLINE,
        "git ls-tree unstage",
    )?;
    ensure!(!truncated, "HEAD tree entry exceeded its bound");
    for record in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .context("malformed HEAD tree entry")?;
        let (header, path_with_tab) = record.split_at(tab);
        let found_path = &path_with_tab[1..];
        if found_path != path.as_bytes() {
            continue;
        }
        let fields = header.split(|byte| *byte == b' ').collect::<Vec<_>>();
        ensure!(
            fields.len() == 3 && fields[1] == b"blob",
            "HEAD entry is not a regular blob"
        );
        ensure!(
            fields[0] == b"100644" || fields[0] == b"100755",
            "HEAD entry mode is unsupported"
        );
        return Ok(Some((
            std::str::from_utf8(fields[0])?.to_owned(),
            std::str::from_utf8(fields[2])?.to_owned(),
        )));
    }
    Ok(None)
}

pub(super) fn run_git_input(
    cwd: &Path,
    private_index: Option<&Path>,
    args: &[&str],
    input: &[u8],
    output_limit: usize,
    label: &'static str,
) -> Result<Vec<u8>> {
    use std::os::unix::process::CommandExt;
    let mut stdin_file = tempfile::tempfile()?;
    stdin_file.write_all(input)?;
    stdin_file.seek(SeekFrom::Start(0))?;
    let mut command = Command::new("git");
    command
        .process_group(0)
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(cwd)
        .args(args);
    super::clean_git_environment(&mut command);
    if let Some(index) = private_index {
        command.env("GIT_INDEX_FILE", index);
    }
    command
        .stdin(Stdio::from(stdin_file))
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start {label}"))?;
    let mut stdout = child.stdout.take().context("Git stdout unavailable")?;
    let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        let _ = child.kill();
        let _ = child.wait();
        bail!("cannot configure bounded Git output for {label}");
    }
    let deadline = Instant::now() + COMMAND_DEADLINE;
    let mut output = Vec::with_capacity(output_limit.min(64 * 1024));
    let mut status = None;
    let mut eof = false;
    loop {
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            bail!("{label} timed out");
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(exit) => status = exit,
                Err(error) => {
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            }
        }
        if !eof {
            let mut chunk = [0_u8; 8192];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(count) => {
                        output.extend_from_slice(&chunk[..count]);
                        if output.len() > output_limit {
                            unsafe {
                                libc::kill(-(child.id() as i32), libc::SIGKILL);
                            }
                            let _ = child.kill();
                            let _ = child.wait();
                            bail!("{label} output exceeded its bound");
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error).context("cannot read Git output");
                    }
                }
            }
        }
        if let Some(status) = status
            && eof
        {
            ensure!(status.success(), "{label} failed with status {status}");
            return Ok(output);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn write_receipt(path: &Path, receipt: &Receipt) -> Result<()> {
    let dir = path.parent().context("receipt path has no parent")?;
    ensure_receipt_dir(dir)?;
    ensure!(
        receipt
            .result
            .message
            .as_ref()
            .is_none_or(|message| message.len() <= MAX_NOTICE_BYTES),
        "operation receipt message exceeds its bound"
    );
    let bytes = serde_json::to_vec(receipt)?;
    ensure!(
        bytes.len() as u64 <= MAX_RECEIPT_BYTES,
        "receipt exceeds 64 KiB bound"
    );
    let mut tmp = tempfile::Builder::new()
        .prefix(".receipt-")
        .tempfile_in(dir)?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|error| error.error)?;
    File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()?;
        ensure!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(output.stdout)
    }

    fn committed_repo() -> Result<(TempDir, PathBuf)> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("repo");
        fs::create_dir(&repo)?;
        git(&repo, &["init", "--initial-branch=main"])?;
        git(&repo, &["config", "user.name", "Test"])?;
        git(&repo, &["config", "user.email", "test@example.invalid"])?;
        fs::write(repo.join("file.txt"), b"base\n")?;
        git(&repo, &["add", "file.txt"])?;
        git(&repo, &["commit", "-m", "base"])?;
        Ok((temp, repo))
    }

    fn target(repo: &Path) -> Result<Target> {
        let repo = fs::canonicalize(repo)?;
        Ok(Target {
            workspace_id: Uuid::from_u128(1),
            workspace_revision: 3,
            project_id: Uuid::from_u128(2),
            cwd: repo.clone(),
            project_root: repo.clone(),
            git_common_dir: crate::projects::git_common_dir(&repo)?,
        })
    }

    fn ticket_for(
        engine: &mut Engine,
        target: &Target,
        path: &str,
        side: GitDiffSide,
    ) -> Result<GitIndexTicket> {
        let identity =
            super::super::validate_git_identity(target, Instant::now() + COMMAND_DEADLINE)?;
        let status = super::super::status_for_diff(&identity.cwd)?;
        let entry = status
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .context("fixture status entry missing")?;
        let (old, new) = super::super::diff_contents(&identity.cwd, path, side, &status)?;
        let (ticket, reason) =
            engine.ticket_for_diff(target, &identity, &status, entry, side, (&old, &new));
        ensure!(reason.is_none(), "index ticket unavailable: {reason:?}");
        ticket.context("index ticket missing")
    }

    #[test]
    fn stages_then_unstages_only_the_index_and_preserves_worktree() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        fs::write(repo.join("file.txt"), b"base\nworktree\n")?;
        let target = target(&repo)?;
        let mut engine = Engine::new(repo.join("host-state"));
        let stage = ticket_for(&mut engine, &target, "file.txt", GitDiffSide::Worktree)?;
        assert_eq!(
            engine.request(target.clone(), stage.id, false).phase,
            GitIndexPhase::Applied
        );
        assert_eq!(git(&repo, &["show", ":file.txt"])?, b"base\nworktree\n");
        assert_eq!(fs::read(repo.join("file.txt"))?, b"base\nworktree\n");

        let unstage = ticket_for(&mut engine, &target, "file.txt", GitDiffSide::Index)?;
        assert_eq!(
            engine.request(target, unstage.id, false).phase,
            GitIndexPhase::Applied
        );
        assert_eq!(git(&repo, &["show", ":file.txt"])?, b"base\n");
        assert_eq!(fs::read(repo.join("file.txt"))?, b"base\nworktree\n");
        Ok(())
    }

    #[test]
    fn write_boundary_failures_remain_unknown_and_never_repeat_after_restart() -> Result<()> {
        for boundary in [
            WriteBoundary::AfterDurableAttempt,
            WriteBoundary::AfterIndexRename,
            WriteBoundary::BeforeFinalReceipt,
        ] {
            let (_temp, repo) = committed_repo()?;
            fs::write(repo.join("file.txt"), b"reviewed change\n")?;
            let target = target(&repo)?;
            let state = repo.join("host-state");
            let mut engine = Engine::new(state.clone());
            let ticket = ticket_for(&mut engine, &target, "file.txt", GitDiffSide::Worktree)?;
            let before = fs::read(repo.join(".git/index"))?;
            WRITE_FAULT.with(|fault| fault.set(Some(boundary)));
            let result = engine.request(target.clone(), ticket.id, false);
            assert!(
                WRITE_FAULT.with(|fault| fault.take()).is_none(),
                "boundary was not reached: {boundary:?}"
            );
            assert_eq!(
                result.phase,
                GitIndexPhase::Unknown,
                "{boundary:?}: {result:?}"
            );
            let after = fs::read(repo.join(".git/index"))?;
            if boundary == WriteBoundary::AfterDurableAttempt {
                assert_eq!(after, before);
                assert_eq!(git(&repo, &["show", ":0:file.txt"])?, b"base\n");
            } else {
                assert_ne!(after, before);
                assert_eq!(git(&repo, &["show", ":0:file.txt"])?, b"reviewed change\n");
            }
            assert!(!repo.join(".git/index.lock").exists());
            drop(engine);

            // A new working change must never be staged by replaying the ticket.
            fs::write(repo.join("file.txt"), b"later unreviewed change\n")?;
            let mut restarted = Engine::new(state);
            for inspect in [true, false, true] {
                assert_eq!(
                    restarted.request(target.clone(), ticket.id, inspect).phase,
                    GitIndexPhase::Unknown
                );
                assert_eq!(fs::read(repo.join(".git/index"))?, after);
            }
            assert_eq!(
                restarted
                    .pending_for_target(&target)?
                    .context("pending receipt missing")?
                    .ticket,
                ticket.id
            );
            let identity =
                super::super::validate_git_identity(&target, Instant::now() + COMMAND_DEADLINE)?;
            let status = super::super::status_for_diff(&repo)?;
            let entry = status
                .entries
                .iter()
                .find(|entry| entry.path == "file.txt")
                .context("fixture entry missing")?;
            let (old, new) =
                super::super::diff_contents(&repo, "file.txt", GitDiffSide::Worktree, &status)?;
            assert!(
                restarted
                    .ticket_for_diff(
                        &target,
                        &identity,
                        &status,
                        entry,
                        GitDiffSide::Worktree,
                        (&old, &new)
                    )
                    .0
                    .is_none()
            );

            // Catalog rename does not hide the old operation, but an inspection
            // or acknowledgement still has to address its original revision.
            let mut renamed = target.clone();
            renamed.workspace_revision += 1;
            assert_eq!(
                restarted
                    .pending_for_target(&renamed)?
                    .context("renamed pending missing")?
                    .workspace_revision,
                target.workspace_revision
            );
            assert_eq!(
                restarted.acknowledge(&renamed, ticket.id).phase,
                GitIndexPhase::Unavailable
            );
            assert!(restarted.acknowledge(&target, ticket.id).acknowledged);
            assert!(restarted.pending_for_target(&target)?.is_none());
            assert_eq!(
                restarted.request(target, ticket.id, false).phase,
                GitIndexPhase::Unknown
            );
            assert_eq!(fs::read(repo.join(".git/index"))?, after);
            assert_eq!(
                fs::read(repo.join("file.txt"))?,
                b"later unreviewed change\n"
            );
        }
        Ok(())
    }

    #[test]
    fn corrupted_receipt_bindings_cannot_be_reported_or_reapplied() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        fs::write(repo.join("file.txt"), b"reviewed change\n")?;
        let target = target(&repo)?;
        let mut engine = Engine::new(repo.join("host-state"));
        let ticket = ticket_for(&mut engine, &target, "file.txt", GitDiffSide::Worktree)?;
        assert_eq!(
            engine.request(target.clone(), ticket.id, false).phase,
            GitIndexPhase::Applied
        );
        let path = engine.receipt_path(ticket.id)?;
        let valid = fs::read(&path)?;
        let index_after = fs::read(repo.join(".git/index"))?;
        for pointer in [
            "/result/ticket",
            "/result/path",
            "/result/workspace_id",
            "/result/workspace_revision",
            "/result/side",
            "/result/acknowledged",
        ] {
            let mut record: serde_json::Value = serde_json::from_slice(&valid)?;
            *record
                .pointer_mut(pointer)
                .context("fixture field missing")? = match pointer {
                "/result/workspace_revision" => serde_json::json!(100),
                "/result/acknowledged" => serde_json::json!(true),
                "/result/side" => serde_json::json!("index"),
                _ => serde_json::json!(Uuid::new_v4().to_string()),
            };
            fs::write(&path, serde_json::to_vec(&record)?)?;
            assert!(
                read_receipt(&path).is_err(),
                "accepted corrupt binding {pointer}"
            );
            assert!(engine.pending_for_target(&target).is_err());
            assert_eq!(
                engine.request(target.clone(), ticket.id, false).phase,
                GitIndexPhase::Unavailable
            );
            assert_eq!(fs::read(repo.join(".git/index"))?, index_after);
        }
        Ok(())
    }

    #[test]
    fn stage_preserves_native_index_permissions() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        fs::set_permissions(repo.join(".git/index"), fs::Permissions::from_mode(0o660))?;
        fs::write(repo.join("file.txt"), b"reviewed change\n")?;
        let target = target(&repo)?;
        let mut engine = Engine::new(repo.join("host-state"));
        let ticket = ticket_for(&mut engine, &target, "file.txt", GitDiffSide::Worktree)?;
        assert_eq!(
            engine.request(target, ticket.id, false).phase,
            GitIndexPhase::Applied
        );
        assert_eq!(
            fs::metadata(repo.join(".git/index"))?.permissions().mode() & 0o777,
            0o660
        );
        Ok(())
    }

    #[test]
    fn unknown_receipt_survives_engine_restart_until_explicit_ack() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        fs::write(repo.join("file.txt"), b"base\nworktree\n")?;
        let target = target(&repo)?;
        let state = repo.join("host-state");
        let mut engine = Engine::new(state.clone());
        let ticket = ticket_for(&mut engine, &target, "file.txt", GitDiffSide::Worktree)?;
        let lease = engine
            .leases
            .remove(&ticket.id)
            .context("fixture lease missing")?;
        let mut receipt = engine.binding_receipt(&lease)?;
        receipt.result.phase = GitIndexPhase::Unknown;
        receipt.result.message = Some("fixture interrupted after durable attempt".into());
        let path = engine.receipt_path(ticket.id)?;
        write_receipt(&path, &receipt)?;
        drop(engine);

        let mut restarted = Engine::new(state);
        assert_eq!(
            restarted
                .pending_for_target(&target)?
                .map(|result| result.ticket),
            Some(ticket.id)
        );
        let ack = restarted.acknowledge(&target, ticket.id);
        assert_eq!(ack.phase, GitIndexPhase::Unknown);
        assert!(ack.acknowledged);
        assert!(restarted.pending_for_target(&target)?.is_none());
        let inspected = restarted.request(target.clone(), ticket.id, true);
        assert_eq!(inspected.phase, GitIndexPhase::Unknown);
        assert!(inspected.acknowledged);
        assert!(restarted.request(target, ticket.id, false).acknowledged);
        Ok(())
    }

    #[test]
    fn foreign_native_lock_prevents_index_replacement() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        fs::write(repo.join("file.txt"), b"changed\n")?;
        let target = target(&repo)?;
        let mut engine = Engine::new(repo.join("host-state"));
        let ticket = ticket_for(&mut engine, &target, "file.txt", GitDiffSide::Worktree)?;
        let before = fs::read(repo.join(".git/index"))?;
        fs::write(repo.join(".git/index.lock"), b"foreign lock")?;
        let result = engine.request(target, ticket.id, false);
        assert_eq!(result.phase, GitIndexPhase::Unknown);
        assert_eq!(fs::read(repo.join(".git/index"))?, before);
        assert_eq!(fs::read(repo.join(".git/index.lock"))?, b"foreign lock");
        Ok(())
    }

    #[test]
    fn dropping_lock_does_not_remove_replacement_lock() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        let index = repo.join(".git/index");
        let path = repo.join(".git/index.lock");
        let lock = IndexLock::acquire(&index)?;
        fs::remove_file(&path)?;
        fs::write(&path, b"replacement lock")?;
        drop(lock);
        assert_eq!(fs::read(path)?, b"replacement lock");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn core_filemode_false_preserves_index_executable_bit() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, repo) = committed_repo()?;
        fs::set_permissions(repo.join("file.txt"), fs::Permissions::from_mode(0o755))?;
        git(&repo, &["add", "file.txt"])?;
        git(&repo, &["commit", "-m", "executable"])?;
        git(&repo, &["config", "core.fileMode", "false"])?;
        fs::set_permissions(repo.join("file.txt"), fs::Permissions::from_mode(0o644))?;
        fs::write(repo.join("file.txt"), b"changed\n")?;
        let target = target(&repo)?;
        let mut engine = Engine::new(repo.join("host-state"));
        let ticket = ticket_for(&mut engine, &target, "file.txt", GitDiffSide::Worktree)?;
        assert_eq!(
            engine.request(target, ticket.id, false).phase,
            GitIndexPhase::Applied
        );
        let stage = git(&repo, &["ls-files", "--stage", "--", "file.txt"])?;
        assert!(stage.starts_with(b"100755 "), "unexpected stage: {stage:?}");
        Ok(())
    }

    #[test]
    fn sparse_checkout_configuration_disables_index_tickets() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        git(&repo, &["config", "core.sparseCheckout", "true"])?;
        assert!(is_split_index(&repo)?);
        Ok(())
    }

    #[test]
    fn stages_untracked_file_in_an_unborn_repository() -> Result<()> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("repo");
        fs::create_dir(&repo)?;
        git(&repo, &["init", "--initial-branch=main"])?;
        fs::write(repo.join("new.txt"), b"first\n")?;
        let target = target(&repo)?;
        let mut engine = Engine::new(temp.path().join("host-state"));
        let ticket = ticket_for(&mut engine, &target, "new.txt", GitDiffSide::Untracked)?;
        assert_eq!(
            engine.request(target, ticket.id, false).phase,
            GitIndexPhase::Applied
        );
        assert_eq!(git(&repo, &["show", ":new.txt"])?, b"first\n");
        assert!(git(&repo, &["rev-parse", "--verify", "HEAD"]).is_err());
        Ok(())
    }

    #[test]
    fn changed_worktree_rejects_a_diff_bound_ticket_without_index_mutation() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        fs::write(repo.join("file.txt"), b"previewed\n")?;
        let target = target(&repo)?;
        let mut engine = Engine::new(repo.join("host-state"));
        let ticket = ticket_for(&mut engine, &target, "file.txt", GitDiffSide::Worktree)?;
        let index_before = fs::read(repo.join(".git/index"))?;
        fs::write(repo.join("file.txt"), b"changed after preview\n")?;
        let result = engine.request(target, ticket.id, false);
        assert_eq!(result.phase, GitIndexPhase::Rejected);
        assert_eq!(fs::read(repo.join(".git/index"))?, index_before);
        assert_eq!(fs::read(repo.join("file.txt"))?, b"changed after preview\n");
        Ok(())
    }

    #[test]
    fn staging_a_followup_edit_to_a_newly_staged_file_uses_the_index_baseline() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        fs::write(repo.join("new.txt"), b"first staged version\n")?;
        git(&repo, &["add", "new.txt"])?;
        fs::write(repo.join("new.txt"), b"second working version\n")?;
        let target = target(&repo)?;
        let mut engine = Engine::new(repo.join("host-state"));
        let diff = super::super::diff(
            target.clone(),
            "new.txt".into(),
            GitDiffSide::Worktree,
            &mut engine,
        )?;
        assert!(
            diff.text.contains("-first staged version")
                && diff.text.contains("+second working version")
        );
        let ticket = diff.index_ticket.context("stage ticket missing")?;
        let result = engine.request(target, ticket.id, false);
        assert_eq!(result.phase, GitIndexPhase::Applied, "{result:?}");
        assert_eq!(
            git(&repo, &["show", ":0:new.txt"])?,
            b"second working version\n"
        );
        Ok(())
    }

    #[test]
    fn rename_unstage_restores_source_index_without_touching_renamed_worktree() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        git(&repo, &["mv", "file.txt", "renamed.txt"])?;
        let target = target(&repo)?;
        let mut engine = Engine::new(repo.join("host-state"));
        let ticket = ticket_for(&mut engine, &target, "renamed.txt", GitDiffSide::Index)?;
        let result = engine.request(target, ticket.id, false);
        assert_eq!(result.phase, GitIndexPhase::Applied, "{result:?}");
        assert_eq!(git(&repo, &["show", ":0:file.txt"])?, b"base\n");
        assert!(git(&repo, &["show", ":0:renamed.txt"]).is_err());
        assert!(!repo.join("file.txt").exists());
        assert_eq!(fs::read(repo.join("renamed.txt"))?, b"base\n");
        Ok(())
    }

    #[test]
    fn copy_unstage_preserves_separate_staged_source_edits() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        let base = (0..30)
            .map(|line| format!("original line {line}\n"))
            .collect::<String>();
        fs::write(repo.join("file.txt"), &base)?;
        git(&repo, &["commit", "-am", "copy base"])?;
        let changed = format!("{base}new source edit\n");
        fs::write(repo.join("file.txt"), &changed)?;
        fs::write(repo.join("copy.txt"), &changed)?;
        git(&repo, &["add", "file.txt", "copy.txt"])?;
        git(&repo, &["config", "status.renames", "copies"])?;
        let parsed = super::super::status_for_diff(&repo)?;
        let copy = parsed
            .entries
            .iter()
            .find(|entry| entry.path == "copy.txt")
            .context("copy entry missing")?;
        assert_eq!(
            copy.index_status, 'C',
            "fixture did not reach native copy status: {copy:?}"
        );
        let target = target(&repo)?;
        let mut engine = Engine::new(repo.join("host-state"));
        let ticket = ticket_for(&mut engine, &target, "copy.txt", GitDiffSide::Index)?;
        let result = engine.request(target, ticket.id, false);
        assert_eq!(result.phase, GitIndexPhase::Applied, "{result:?}");
        assert_eq!(git(&repo, &["show", ":0:file.txt"])?, changed.as_bytes());
        assert!(git(&repo, &["show", ":0:copy.txt"]).is_err());
        assert_eq!(fs::read(repo.join("copy.txt"))?, changed.as_bytes());
        Ok(())
    }

    #[test]
    fn staging_worktree_deletion_removes_only_the_index_entry() -> Result<()> {
        let (_temp, repo) = committed_repo()?;
        fs::remove_file(repo.join("file.txt"))?;
        let target = target(&repo)?;
        let mut engine = Engine::new(repo.join("host-state"));
        let ticket = ticket_for(&mut engine, &target, "file.txt", GitDiffSide::Worktree)?;
        assert_eq!(
            engine.request(target, ticket.id, false).phase,
            GitIndexPhase::Applied
        );
        assert!(git(&repo, &["ls-files", "--stage"])?.is_empty());
        assert!(!repo.join("file.txt").exists());
        Ok(())
    }
}
