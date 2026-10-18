//! Durable native Git commit attempts for a reviewed staged index.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    COMMAND_DEADLINE, FrozenIdentity, Target, clean_git_environment, validate_git_identity,
};
#[path = "workspace_git_commit_hooks.rs"]
mod hooks;

const MAX_INDEX: usize = 8 * 1024 * 1024;
const MAX_RECEIPT: u64 = 64 * 1024;
const MAX_OPERATIONS: usize = 4096;
const MAX_FILES: usize = 512;
const MAX_PATH: usize = 4096;
const DEADLINE: Duration = Duration::from_secs(120);
const OUTPUT_MAX: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitCommitEntry {
    pub path: String,
    pub status: char,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitCommitPlan {
    pub ticket: Uuid,
    pub workspace_id: Uuid,
    pub workspace_revision: u64,
    pub project_id: Uuid,
    pub cwd: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub files: Vec<GitCommitEntry>,
    pub signing_requested: bool,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitCommitPhase {
    Running,
    Applied,
    Rejected,
    Failed,
    Unknown,
    Unavailable,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceGitCommitResult {
    pub ticket: Uuid,
    pub workspace_id: Uuid,
    pub workspace_revision: u64,
    pub project_id: Uuid,
    pub cwd: PathBuf,
    pub phase: GitCommitPhase,
    pub commit: Option<String>,
    pub acknowledged: bool,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    result: WorkspaceGitCommitResult,
    target: TargetBinding,
    message: Option<String>,
    expected_tree: Option<String>,
    expected_head: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetBinding {
    project_root: PathBuf,
    common_dir: PathBuf,
    git_dir: PathBuf,
    git: PathBuf,
    git_identity: (u64, u64),
    head_ref: Option<String>,
    index_id: Option<FileId>,
    index: Option<Vec<u8>>,
}
type FileId = (u64, u64, u64, i64, i64, i64, i64);

struct Job {
    ticket: Uuid,
    child: Child,
    deadline: Instant,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    _pinned: File,
}
pub(super) struct Engine {
    root: PathBuf,
    leases:
        std::collections::HashMap<Uuid, (Target, WorkspaceGitCommitPlan, TargetBinding, Instant)>,
    job: Option<Job>,
    observed: Option<Uuid>,
}

impl Engine {
    pub(super) fn new(root: PathBuf) -> Self {
        let mut engine = Self {
            root,
            leases: Default::default(),
            job: None,
            observed: None,
        };
        engine.recover_live_attempt();
        engine
    }
    pub(super) fn availability(&self, target: &Target) -> Result<()> {
        validate_git_identity(target, Instant::now() + COMMAND_DEADLINE)?;
        let _ = resolve_git(&target.cwd)?;
        Ok(())
    }
    pub(super) fn prepare(&mut self, target: &Target) -> Result<WorkspaceGitCommitPlan> {
        self.leases.retain(|_, (scope, _, _, expires)| {
            *expires > Instant::now() && scope.workspace_id != target.workspace_id
        });
        ensure!(
            self.leases.len() < 2,
            "two other workspace commit previews are already active; wait for an expired preview"
        );
        ensure!(!self.is_running(), "a commit is already running");
        ensure!(
            self.pending_for_target(target)?.is_none(),
            "an unacknowledged commit attempt must be inspected first"
        );
        let identity = validate_git_identity(target, Instant::now() + COMMAND_DEADLINE)?;
        let (git, git_identity) = resolve_git(&identity.cwd)?;
        let (head, branch) = head_branch(&identity.cwd)?;
        ensure!(
            branch
                .as_ref()
                .is_some_and(|b| b.starts_with("refs/heads/")),
            "commits require an attached local branch"
        );
        reject_operation_state(&identity.cwd)?;
        let git_dir = git_path(&identity.cwd, &["--absolute-git-dir"])?;
        let idx = git_path(
            &identity.cwd,
            &["--path-format=absolute", "--git-path", "index"],
        )?;
        ensure!(
            idx.parent()
                .is_some_and(|p| fs::canonicalize(p).ok().as_ref() == Some(&git_dir)),
            "Git index must be directly in the worktree Git directory"
        );
        let (bytes, id) = read_index(&idx)?;
        reject_special_index(&identity.cwd, &git_dir)?;
        ensure!(
            git_capture(&identity.cwd, &["ls-files", "--unmerged", "-z"], 4096)?.is_empty(),
            "unmerged index entries cannot be committed"
        );
        let files = staged_files(&identity.cwd)?;
        ensure!(!files.is_empty(), "there are no staged changes to commit");
        let signing = signing_requested(&identity.cwd)?;
        let plan = WorkspaceGitCommitPlan {
            ticket: Uuid::new_v4(),
            workspace_id: target.workspace_id,
            workspace_revision: target.workspace_revision,
            project_id: target.project_id,
            cwd: identity.cwd.clone(),
            head,
            branch: branch.clone(),
            files,
            signing_requested: signing,
        };
        ensure!(
            read_index(&idx)? == (bytes.clone(), id),
            "index changed while preparing the staged list"
        );
        ensure!(
            head_branch(&identity.cwd)? == (plan.head.clone(), plan.branch.clone()),
            "HEAD changed while preparing the staged list"
        );
        self.leases.insert(
            plan.ticket,
            (
                target.clone(),
                plan.clone(),
                TargetBinding {
                    project_root: identity.project_root,
                    common_dir: identity.common_dir,
                    git_dir,
                    git,
                    git_identity,
                    head_ref: branch,
                    index_id: id,
                    index: bytes,
                },
                Instant::now() + Duration::from_secs(600),
            ),
        );
        Ok(plan)
    }
    pub(super) fn request(
        &mut self,
        target: &Target,
        ticket: Uuid,
        message: Option<String>,
        inspect: bool,
    ) -> WorkspaceGitCommitResult {
        match self.read_receipt(ticket) {
            Ok(Some(mut r)) => {
                if !scope(&r, target)
                    || (!inspect && r.result.workspace_revision != target.workspace_revision)
                {
                    return result(
                        target,
                        ticket,
                        GitCommitPhase::Unavailable,
                        None,
                        Some("ticket belongs to a different workspace scope"),
                    );
                }
                if !inspect && message.as_deref() != r.message.as_deref() {
                    return result(
                        target,
                        ticket,
                        GitCommitPhase::Rejected,
                        None,
                        Some("ticket is already bound to a different commit message"),
                    );
                }
                if self.job.as_ref().is_some_and(|job| job.ticket == ticket) {
                    r.result.phase = GitCommitPhase::Running;
                    return r.result;
                }
                let before = r.result.clone();
                self.reconcile(&mut r);
                if r.result.phase == GitCommitPhase::Running {
                    self.observed = Some(ticket);
                } else if self.observed == Some(ticket) {
                    self.observed = None;
                }
                if before != r.result {
                    let _ = self.write_receipt(&r);
                }
                return r.result;
            }
            Err(e) => {
                return result(
                    target,
                    ticket,
                    GitCommitPhase::Unavailable,
                    None,
                    Some(&format!("receipt unavailable: {e:#}")),
                );
            }
            Ok(None) => (),
        }
        if inspect {
            return result(
                target,
                ticket,
                GitCommitPhase::Unavailable,
                None,
                Some("no durable receipt exists"),
            );
        }
        let Some((leased, plan, binding, expires)) = self.leases.remove(&ticket) else {
            return result(
                target,
                ticket,
                GitCommitPhase::Rejected,
                None,
                Some("commit plan is missing or expired"),
            );
        };
        if plan.ticket != ticket || !target_matches(&leased, target) || expires < Instant::now() {
            return result(
                target,
                ticket,
                GitCommitPhase::Rejected,
                None,
                Some("commit plan expired or workspace scope changed"),
            );
        }
        let Some(message) = message else {
            return result(
                target,
                ticket,
                GitCommitPhase::Rejected,
                None,
                Some("commit message is required"),
            );
        };
        if let Err(e) = validate_message(&message) {
            return result(
                target,
                ticket,
                GitCommitPhase::Rejected,
                None,
                Some(&e.to_string()),
            );
        }
        if let Err(e) = validate_frozen(&leased, &plan, &binding) {
            return result(
                target,
                ticket,
                GitCommitPhase::Rejected,
                None,
                Some(&format!("commit plan is stale: {e:#}")),
            );
        }
        if let Err(e) = self.ensure_capacity() {
            return result(
                target,
                ticket,
                GitCommitPhase::Unavailable,
                None,
                Some(&format!("receipt storage unavailable: {e:#}")),
            );
        }
        let mut attempt = result(
            target,
            ticket,
            GitCommitPhase::Unknown,
            None,
            Some("commit attempt recorded; outcome is being resolved"),
        );
        let mut saved_binding = binding.clone();
        saved_binding.index = None;
        let mut receipt = Receipt {
            version: 1,
            result: attempt.clone(),
            target: saved_binding,
            message: Some(message.clone()),
            expected_tree: None,
            expected_head: plan.head.clone(),
        };
        if let Err(e) = self.write_receipt(&receipt) {
            attempt.phase = GitCommitPhase::Unavailable;
            attempt.message = Some(super::bounded_notice(format!(
                "cannot durably record commit attempt: {e:#}"
            )));
            return attempt;
        }
        match self.start(&leased, &plan, &binding, &message, &mut receipt) {
            Ok(job) => {
                self.job = Some(job);
                attempt.phase = GitCommitPhase::Running;
                attempt.message = None;
                attempt
            }
            Err(e) => {
                receipt.result.phase = GitCommitPhase::Unknown;
                receipt.result.message = Some(super::bounded_notice(format!(
                    "commit outcome is uncertain: {e:#}"
                )));
                let _ = self.write_receipt(&receipt);
                receipt.result
            }
        }
    }
    pub(super) fn acknowledge(
        &mut self,
        target: &Target,
        ticket: Uuid,
    ) -> WorkspaceGitCommitResult {
        match self.read_receipt(ticket) {
            Ok(Some(mut r)) if scope(&r, target) => {
                if self.job.as_ref().is_some_and(|job| job.ticket == ticket) {
                    r.result.phase = GitCommitPhase::Running;
                    return r.result;
                }
                self.reconcile(&mut r);
                if r.result.phase == GitCommitPhase::Running {
                    return r.result;
                }
                r.result.acknowledged = true;
                if let Err(e) = self.write_receipt(&r) {
                    return result(
                        target,
                        ticket,
                        GitCommitPhase::Unavailable,
                        None,
                        Some(&format!("cannot persist acknowledgement: {e:#}")),
                    );
                }
                r.result
            }
            Ok(Some(_)) => result(
                target,
                ticket,
                GitCommitPhase::Unavailable,
                None,
                Some("ticket scope mismatch"),
            ),
            Ok(None) => result(
                target,
                ticket,
                GitCommitPhase::Unavailable,
                None,
                Some("receipt not found"),
            ),
            Err(e) => result(
                target,
                ticket,
                GitCommitPhase::Unavailable,
                None,
                Some(&format!("receipt unavailable: {e:#}")),
            ),
        }
    }
    pub(super) fn pending_for_target(
        &mut self,
        target: &Target,
    ) -> Result<Option<WorkspaceGitCommitResult>> {
        let dir = self.receipts_dir();
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut found = None;
        for ent in entries.take(MAX_OPERATIONS + 1) {
            let ent = ent?;
            ensure!(
                ent.file_type()?.is_dir(),
                "commit receipt store contains a non-directory"
            );
            let id = Uuid::parse_str(&ent.file_name().to_string_lossy())?;
            let Some(mut r) = self.read_receipt(id)? else {
                bail!("commit receipt missing")
            };
            if scope(&r, target)
                && !r.result.acknowledged
                && matches!(
                    r.result.phase,
                    GitCommitPhase::Running | GitCommitPhase::Unknown
                )
            {
                let before = r.result.clone();
                if self.job.as_ref().is_some_and(|job| job.ticket == id) {
                    r.result.phase = GitCommitPhase::Running;
                } else {
                    self.reconcile(&mut r);
                    if r.result.phase == GitCommitPhase::Running {
                        self.observed = Some(id);
                    }
                }
                if before != r.result {
                    self.write_receipt(&r)?;
                }
                if found.is_none() {
                    found = Some(r.result);
                }
            }
        }
        if let Some(r) = found {
            if let Some(job) = &self.job
                && job.ticket == r.ticket
            {
                return Ok(Some(result(
                    target,
                    r.ticket,
                    GitCommitPhase::Running,
                    None,
                    None,
                )));
            }
            return Ok(Some(r));
        }
        Ok(None)
    }
    pub(super) fn tick(&mut self) -> bool {
        let Some(mut job) = self.job.take() else {
            if let Some(t) = self.observed {
                if let Ok(Some(mut r)) = self.read_receipt(t) {
                    let old = r.result.clone();
                    self.reconcile(&mut r);
                    if old != r.result {
                        let _ = self.write_receipt(&r);
                    }
                    if r.result.phase != GitCommitPhase::Running {
                        self.observed = None;
                    }
                    return old != r.result;
                }
                self.observed = None;
            }
            return false;
        };
        let ticket = job.ticket;
        drain(job.child.stdout.as_mut(), &mut job.stdout);
        drain(job.child.stderr.as_mut(), &mut job.stderr);
        let mut changed = false;
        let timed = Instant::now() >= job.deadline;
        let status = job.child.try_wait();
        let finished = matches!(&status, Ok(Some(_)));
        if timed && !finished {
            kill_group(&mut job.child);
            let _ = job.child.wait();
            let proof = self
                .operation_path(job.ticket)
                .ok()
                .and_then(|p| hooks::committed(&p, job.ticket).ok().flatten());
            if let Some(oid) = proof {
                self.finish(
                    job.ticket,
                    GitCommitPhase::Applied,
                    Some(oid),
                    "commit published",
                );
            } else {
                self.finish(
                    job.ticket,
                    GitCommitPhase::Unknown,
                    None,
                    "native Git commit exceeded its 120-second deadline",
                );
            }
            changed = true;
        } else if let Ok(Some(status)) = status {
            let oid = self
                .operation_path(job.ticket)
                .ok()
                .and_then(|p| hooks::committed(&p, job.ticket).ok().flatten());
            if let Some(oid) = oid {
                self.finish(
                    job.ticket,
                    GitCommitPhase::Applied,
                    Some(oid),
                    "commit published",
                );
            } else if status.success() {
                self.finish(
                    job.ticket,
                    GitCommitPhase::Unknown,
                    None,
                    "Git exited successfully without a verified publication proof",
                );
            } else {
                let prepared = self
                    .operation_path(job.ticket)
                    .and_then(|p| hooks::prepared(&p, job.ticket));
                if !matches!(prepared, Ok(None)) {
                    self.finish(
                        job.ticket,
                        GitCommitPhase::Unknown,
                        None,
                        "Git exited after preparing a ref update without a publication proof",
                    );
                } else {
                    self.finish(
                        job.ticket,
                        GitCommitPhase::Failed,
                        None,
                        &format!("Native Git commit exited with {status} before a verified prepared update; check Git hooks, author identity and signing settings. Hooks may have made changes"),
                    );
                }
            }
            changed = true;
        } else {
            self.job = Some(job);
        }
        if self.job.is_none() && self.observed == Some(ticket) {
            self.observed = None;
        }
        changed
    }
    pub(super) fn is_running(&self) -> bool {
        self.job.is_some() || self.observed.is_some()
    }
    fn start(
        &self,
        target: &Target,
        plan: &WorkspaceGitCommitPlan,
        b: &TargetBinding,
        message: &str,
        receipt: &mut Receipt,
    ) -> Result<Job> {
        validate_frozen(target, plan, b)?;
        let idx = &b.index;
        let (git, git_identity) = resolve_git(&plan.cwd)?;
        ensure!(
            git == b.git && git_identity == b.git_identity,
            "Git executable changed after review"
        );
        let op = self.operation_path(plan.ticket)?;
        create_private_dir(&op)?;
        let frozen = op.join("frozen-index");
        write_private(&frozen, idx.as_deref().unwrap_or(&[]))?;
        let tree = git_capture_env(
            &plan.cwd,
            &git,
            &[("GIT_INDEX_FILE", frozen.as_os_str())],
            &["write-tree"],
            256,
        )?;
        let tree = String::from_utf8(tree)?.trim().to_owned();
        ensure!(is_oid(&tree), "Git returned an invalid tree id");
        receipt.expected_tree = Some(tree.clone());
        self.write_receipt(receipt)?;
        write_private(&op.join("message"), message.as_bytes())?;
        let hookdir = git_path(
            &plan.cwd,
            &["--path-format=absolute", "--git-path", "hooks"],
        )?;
        let helper = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC)
            .open("/proc/self/exe")?;
        let fd = helper.as_raw_fd();
        let hooks = hooks::HookPlan {
            ticket: plan.ticket,
            cwd: plan.cwd.clone(),
            git: git.clone(),
            expected_head: plan.head.clone(),
            head_ref: plan.branch.clone(),
            expected_tree: tree,
            hooks_dir: hookdir,
            helper_fd: fd,
            git_identity: file_identity(&git)?,
        };
        write_private(&op.join("hook-plan.json"), &serde_json::to_vec(&hooks)?)?;
        hooks::prepare_hooks(&op, &hooks)?;
        let mut cmd = Command::new(format!("/proc/self/fd/{fd}"));
        cmd.arg("__git_commit_run")
            .arg(&op)
            .current_dir(&plan.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        clean_git_environment(&mut cmd);
        cmd.env("GIT_CONFIG_COUNT", "0");
        // The helper descriptor is the only descriptor deliberately inherited by the helper and its hook children.
        unsafe {
            cmd.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setpgid(0, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        receipt.result.phase = GitCommitPhase::Running;
        receipt.result.message = Some("native Git commit is starting".into());
        self.write_receipt(receipt)?;
        let mut child = cmd.spawn().context("start native commit helper")?;
        let configure = |fd| -> Result<()> {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            ensure!(
                flags >= 0
                    && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
                "cannot configure Git output pipe"
            );
            Ok(())
        };
        let configured = (|| -> Result<()> {
            configure(
                child
                    .stdout
                    .as_ref()
                    .context("missing Git stdout")?
                    .as_raw_fd(),
            )?;
            configure(
                child
                    .stderr
                    .as_ref()
                    .context("missing Git stderr")?
                    .as_raw_fd(),
            )
        })();
        if let Err(error) = configured {
            kill_group(&mut child);
            let _ = child.wait();
            return Err(error);
        }
        Ok(Job {
            ticket: plan.ticket,
            child,
            deadline: Instant::now() + DEADLINE,
            stdout: Vec::new(),
            stderr: Vec::new(),
            _pinned: helper,
        })
    }
    fn finish(&self, t: Uuid, p: GitCommitPhase, c: Option<String>, m: &str) {
        if let Ok(Some(mut r)) = self.read_receipt(t) {
            r.result.phase = p;
            r.result.commit = c;
            r.result.message = Some(super::bounded_notice(m));
            let _ = self.write_receipt(&r);
        }
    }
    fn reconcile(&self, r: &mut Receipt) {
        if !matches!(
            r.result.phase,
            GitCommitPhase::Unknown | GitCommitPhase::Running
        ) {
            return;
        }
        let op = self.operation_path(r.result.ticket).unwrap_or_default();
        if hooks::running(&op).unwrap_or(false) {
            r.result.phase = GitCommitPhase::Running;
            r.result.message = None;
        } else if let Ok(Some(oid)) = hooks::committed(&op, r.result.ticket) {
            r.result.phase = GitCommitPhase::Applied;
            r.result.commit = Some(oid);
            r.result.message = Some("commit publication verified".into());
        } else {
            r.result.phase = GitCommitPhase::Unknown;
            r.result.message = Some(
                "commit outcome is unknown; inspect and acknowledge before another attempt".into(),
            );
        }
    }
    fn recover_live_attempt(&mut self) {
        let Ok(entries) = fs::read_dir(self.receipts_dir()) else {
            return;
        };
        for entry in entries.take(MAX_OPERATIONS) {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            let Ok(Some(mut r)) = self.read_receipt(id) else {
                continue;
            };
            if r.result.phase != GitCommitPhase::Running {
                continue;
            }
            self.reconcile(&mut r);
            if r.result.phase == GitCommitPhase::Running {
                self.observed = Some(id);
                let _ = self.write_receipt(&r);
                break;
            }
            let _ = self.write_receipt(&r);
        }
    }
    fn receipts_dir(&self) -> PathBuf {
        self.root.join("git-commit-operations")
    }
    fn operation_path(&self, t: Uuid) -> Result<PathBuf> {
        Ok(self.receipts_dir().join(t.to_string()))
    }
    fn ensure_capacity(&self) -> Result<()> {
        create_private_dir(&self.receipts_dir())?;
        ensure!(
            fs::read_dir(self.receipts_dir())?
                .take(MAX_OPERATIONS + 1)
                .count()
                < MAX_OPERATIONS,
            "commit receipt store reached its fixed limit"
        );
        Ok(())
    }
    fn write_receipt(&self, r: &Receipt) -> Result<()> {
        let op = self.operation_path(r.result.ticket)?;
        let store = self.receipts_dir();
        let store_existed = store.exists();
        create_private_dir(&store)?;
        if !store_existed {
            File::open(&self.root)?.sync_all()?;
        }
        let operation_existed = op.exists();
        create_private_dir(&op)?;
        if !operation_existed {
            File::open(&store)?.sync_all()?;
        }
        let bytes = serde_json::to_vec(r)?;
        ensure!(
            bytes.len() as u64 <= MAX_RECEIPT,
            "commit receipt exceeds size bound"
        );
        atomic_write(&op.join("receipt.json"), &bytes)
    }
    fn read_receipt(&self, t: Uuid) -> Result<Option<Receipt>> {
        let p = self.operation_path(t)?;
        match fs::symlink_metadata(&p) {
            Ok(metadata) => validate_directory_metadata(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let bytes = match private_read(&p.join("receipt.json")) {
            Ok(bytes) => bytes,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let r: Receipt = serde_json::from_slice(&bytes)?;
        ensure!(
            r.version == 1 && r.result.ticket == t && !t.is_nil(),
            "invalid receipt identity"
        );
        ensure!(
            !r.result.workspace_id.is_nil() && !r.result.project_id.is_nil(),
            "invalid workspace identity"
        );
        ensure!(
            r.result.cwd.is_absolute()
                && r.target.project_root.is_absolute()
                && r.target.common_dir.is_absolute()
                && r.target.git_dir.is_absolute()
                && r.target.git.is_absolute(),
            "non-absolute receipt paths"
        );
        ensure!(
            r.target.index.is_none(),
            "receipt unexpectedly contains an index snapshot"
        );
        validate_message(r.message.as_deref().context("receipt message missing")?)?;
        ensure!(
            r.expected_head.as_deref().is_none_or(is_oid)
                && r.expected_tree.as_deref().is_none_or(is_oid),
            "invalid receipt object identity"
        );
        ensure!(
            r.result
                .message
                .as_ref()
                .is_none_or(|message| message.len() <= super::MAX_NOTICE_BYTES),
            "receipt diagnostic exceeds bound"
        );
        ensure!(
            matches!(
                r.result.phase,
                GitCommitPhase::Running
                    | GitCommitPhase::Unknown
                    | GitCommitPhase::Failed
                    | GitCommitPhase::Applied
            ),
            "invalid durable commit phase"
        );
        ensure!(
            (r.result.phase == GitCommitPhase::Applied) == r.result.commit.is_some(),
            "receipt publication identity mismatch"
        );
        if let Some(oid) = &r.result.commit {
            ensure!(is_oid(oid), "invalid receipt commit OID");
            let plan: hooks::HookPlan =
                serde_json::from_slice(&private_read(&p.join("hook-plan.json"))?)?;
            ensure!(
                plan.ticket == t
                    && plan.cwd == r.result.cwd
                    && plan.git == r.target.git
                    && plan.git_identity == r.target.git_identity
                    && plan.head_ref == r.target.head_ref
                    && plan.expected_head == r.expected_head
                    && Some(&plan.expected_tree) == r.expected_tree.as_ref(),
                "receipt does not match its guarded plan"
            );
            ensure!(
                hooks::committed(&p, t)?.as_ref() == Some(oid),
                "Applied receipt has no matching durable publication proof"
            );
        }
        Ok(Some(r))
    }
}

pub(super) fn run_helper(operation: &Path) -> Result<()> {
    hooks::run(operation)
}
pub(super) fn hook_helper(operation: &Path, name: &str, args: &[String]) -> Result<i32> {
    hooks::hook(operation, name, args)
}
pub(super) fn unavailable_result(
    t: &Target,
    id: Uuid,
    m: impl AsRef<str>,
) -> WorkspaceGitCommitResult {
    result(t, id, GitCommitPhase::Unavailable, None, Some(m.as_ref()))
}
fn result(
    t: &Target,
    id: Uuid,
    p: GitCommitPhase,
    c: Option<String>,
    m: Option<&str>,
) -> WorkspaceGitCommitResult {
    WorkspaceGitCommitResult {
        ticket: id,
        workspace_id: t.workspace_id,
        workspace_revision: t.workspace_revision,
        project_id: t.project_id,
        cwd: t.cwd.clone(),
        phase: p,
        commit: c,
        acknowledged: false,
        message: m.map(super::bounded_notice),
    }
}
fn scope(r: &Receipt, t: &Target) -> bool {
    r.result.workspace_id == t.workspace_id
        && r.result.workspace_revision == t.workspace_revision
        && r.result.project_id == t.project_id
        && r.result.cwd == t.cwd
        && r.target.project_root == t.project_root
        && r.target.common_dir == t.git_common_dir
}
fn target_matches(a: &Target, b: &Target) -> bool {
    a.workspace_id == b.workspace_id
        && a.workspace_revision == b.workspace_revision
        && a.project_id == b.project_id
        && a.cwd == b.cwd
        && a.project_root == b.project_root
        && a.git_common_dir == b.git_common_dir
}
fn validate_frozen(
    target: &Target,
    plan: &WorkspaceGitCommitPlan,
    b: &TargetBinding,
) -> Result<()> {
    ensure!(
        validate_git_identity(target, Instant::now() + COMMAND_DEADLINE)?
            == FrozenIdentity {
                cwd: plan.cwd.clone(),
                project_root: b.project_root.clone(),
                common_dir: b.common_dir.clone(),
            },
        "workspace Git identity changed after review"
    );
    reject_operation_state(&plan.cwd)?;
    ensure!(
        head_branch(&plan.cwd)? == (plan.head.clone(), plan.branch.clone()),
        "HEAD changed after review"
    );
    let (index, identity) = read_index(&git_path(
        &plan.cwd,
        &["--path-format=absolute", "--git-path", "index"],
    )?)?;
    ensure!(
        index == b.index && identity == b.index_id,
        "Git index changed after review"
    );
    reject_special_index(&plan.cwd, &b.git_dir)?;
    ensure!(
        git_capture(&plan.cwd, &["ls-files", "--unmerged", "-z"], 4096)?.is_empty(),
        "unmerged index entries cannot be committed"
    );
    ensure!(
        signing_requested(&plan.cwd)? == plan.signing_requested,
        "commit signing configuration changed after review"
    );
    ensure!(
        resolve_git(&plan.cwd)? == (b.git.clone(), b.git_identity),
        "Git executable changed after review"
    );
    Ok(())
}
fn validate_message(s: &str) -> Result<()> {
    ensure!(!s.trim().is_empty(), "commit message is empty");
    ensure!(s.len() <= 8192, "commit message exceeds 8192 UTF-8 bytes");
    ensure!(
        !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t'),
        "commit message contains unsupported control characters"
    );
    Ok(())
}
fn validate_directory_metadata(metadata: &fs::Metadata) -> Result<()> {
    ensure!(
        metadata.file_type().is_dir()
            && metadata.uid() == unsafe { libc::getuid() }
            && metadata.mode() & 0o077 == 0,
        "operation directory is not private and owned"
    );
    Ok(())
}
fn create_private_dir(p: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if !p.exists() {
        fs::DirBuilder::new().mode(0o700).create(p)?;
    }
    validate_directory_metadata(&fs::symlink_metadata(p)?)
}
fn private_read(p: &Path) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(p)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::getuid() }
            && metadata.mode() & 0o077 == 0
            && metadata.len() <= MAX_RECEIPT,
        "receipt is not private, regular, owned and bounded"
    );
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_RECEIPT,
        "receipt exceeds size bound"
    );
    Ok(bytes)
}
fn write_private(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn atomic_write(p: &Path, b: &[u8]) -> Result<()> {
    let tmp = p.with_extension(format!("tmp-{}", Uuid::new_v4()));
    write_private(&tmp, b)?;
    fs::rename(&tmp, p)?;
    File::open(p.parent().context("receipt has no parent")?)?.sync_all()?;
    Ok(())
}
fn file_id(p: &Path) -> Result<Option<FileId>> {
    match fs::symlink_metadata(p) {
        Ok(m) => {
            ensure!(m.file_type().is_file(), "index is not regular");
            Ok(Some(metadata_id(&m)))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn metadata_id(m: &fs::Metadata) -> FileId {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn read_index(p: &Path) -> Result<(Option<Vec<u8>>, Option<FileId>)> {
    let f = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(p)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
        Err(e) => return Err(e.into()),
    };
    ensure!(f.metadata()?.is_file(), "Git index is not a regular file");
    ensure!(
        f.metadata()?.len() <= MAX_INDEX as u64,
        "Git index exceeds 8 MiB"
    );
    let id = file_id(p)?;
    ensure!(
        id == Some(metadata_id(&f.metadata()?)),
        "Git index identity changed while opening it"
    );
    let mut b = Vec::new();
    f.take((MAX_INDEX + 1) as u64).read_to_end(&mut b)?;
    ensure!(b.len() <= MAX_INDEX, "Git index exceeds 8 MiB");
    Ok((Some(b), id))
}
fn git_path(cwd: &Path, args: &[&str]) -> Result<PathBuf> {
    let mut argv = vec!["rev-parse"];
    argv.extend_from_slice(args);
    let (b, tr) = super::run_git_capture(
        cwd,
        &argv,
        16384,
        Instant::now() + COMMAND_DEADLINE,
        "Git path",
    )?;
    ensure!(!tr, "Git path probe truncated");
    let path = String::from_utf8(b)?;
    let p = PathBuf::from(super::remove_git_line_ending(&path));
    if p.is_absolute() {
        Ok(fs::canonicalize(p.parent().unwrap_or(Path::new("/")))?
            .join(p.file_name().unwrap_or_default()))
    } else {
        Ok(fs::canonicalize(cwd.join(p))?)
    }
}
fn head_branch(cwd: &Path) -> Result<(Option<String>, Option<String>)> {
    // This slice requires an attached local branch. An unborn branch has no
    // matching ref; every other probe error must remain an error.
    let branch = String::from_utf8(git_capture(cwd, &["symbolic-ref", "-q", "HEAD"], 4096)?)?;
    let branch = branch.trim().to_owned();
    ensure!(
        branch.starts_with("refs/heads/"),
        "commits require an attached local branch"
    );
    let refs = git_capture(
        cwd,
        &["for-each-ref", "--format=%(refname) %(objectname)", &branch],
        8192,
    )?;
    let refs = std::str::from_utf8(&refs)?;
    let head = refs.lines().find_map(|line| {
        line.split_once(' ')
            .filter(|(name, _)| *name == branch)
            .map(|(_, oid)| oid.to_owned())
    });
    ensure!(head.as_deref().is_none_or(is_oid), "invalid HEAD object ID");
    Ok((head, Some(branch)))
}
fn git_capture(cwd: &Path, args: &[&str], limit: usize) -> Result<Vec<u8>> {
    let (b, t) = super::run_git_capture(
        cwd,
        args,
        limit,
        Instant::now() + COMMAND_DEADLINE,
        "Git commit preparation",
    )?;
    ensure!(!t, "Git preparation output exceeded bound");
    Ok(b)
}
fn git_capture_env(
    cwd: &Path,
    _git: &Path,
    envs: &[(&str, &std::ffi::OsStr)],
    args: &[&str],
    limit: usize,
) -> Result<Vec<u8>> {
    ensure!(
        envs.len() == 1 && envs[0].0 == "GIT_INDEX_FILE",
        "unsupported private Git environment"
    );
    super::index::run_git_input(
        cwd,
        Some(Path::new(envs[0].1)),
        args,
        &[],
        limit,
        "prepare private Git tree",
    )
}

fn drain<R: Read>(stream: Option<&mut R>, buffer: &mut Vec<u8>) {
    let Some(stream) = stream else { return };
    let mut chunk = [0u8; 8192];
    // Bound work per tick as well as retained output: a continuously writing
    // hook cannot starve receipt inspection or the120-second deadline.
    for _ in 0..8 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let keep = OUTPUT_MAX.saturating_sub(buffer.len()).min(n);
                buffer.extend_from_slice(&chunk[..keep]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}
fn staged_files(cwd: &Path) -> Result<Vec<GitCommitEntry>> {
    let b = git_capture(
        cwd,
        &[
            "diff",
            "--cached",
            "--name-status",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
        ],
        3 * 1024 * 1024,
    )?;
    let mut it = b.split(|x| *x == 0);
    let mut out = Vec::new();
    while let Some(st) = it.next() {
        if st.is_empty() {
            continue;
        }
        let s = std::str::from_utf8(st)?;
        ensure!(s.len() == 1, "unsupported staged status");
        let status = s.chars().next().unwrap();
        ensure!(
            matches!(status, 'A' | 'M' | 'D' | 'T'),
            "conflicted or unsupported staged status"
        );
        let p = it.next().context("missing NUL-delimited staged path")?;
        ensure!(p.len() <= MAX_PATH, "staged path exceeds bound");
        let path = std::str::from_utf8(p)?.to_owned();
        ensure!(
            !path.is_empty() && !path.starts_with('/') && !path.split('/').any(|c| c == ".."),
            "invalid staged path"
        );
        out.push(GitCommitEntry { path, status });
        ensure!(out.len() <= MAX_FILES, "more than 512 staged paths");
    }
    Ok(out)
}
fn reject_special_index(cwd: &Path, git_dir: &Path) -> Result<()> {
    let sparse = bool_config(cwd, "core.sparseCheckout", false)?;
    ensure!(
        !sparse,
        "sparse-checkout indexes are not supported by commit review"
    );
    let shared = !git_capture(cwd, &["rev-parse", "--shared-index-path"], 4096)?
        .iter()
        .all(u8::is_ascii_whitespace);
    ensure!(!shared, "split indexes are not supported by commit review");
    for entry in fs::read_dir(git_dir)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("sharedindex.")
        {
            bail!("split-index data is present");
        }
    }
    Ok(())
}
fn reject_operation_state(cwd: &Path) -> Result<()> {
    for n in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "BISECT_LOG",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        let p = git_path(cwd, &["--path-format=absolute", "--git-path", n])?;
        ensure!(!p.exists(), "Git operation in progress: {n}");
    }
    Ok(())
}
fn resolve_git(cwd: &Path) -> Result<(PathBuf, (u64, u64))> {
    let p = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("git"))
        .find(|p| p.is_file())
        .context("Git executable not found")?;
    let p = fs::canonicalize(p)?;
    let mut f = File::open(&p)?;
    let mut magic = [0; 4];
    f.read_exact(&mut magic)?;
    ensure!(
        &magic == b"\x7fELF",
        "Git executable must be an actual ELF binary"
    );
    let (v, truncated) = super::run_capture_program(
        &p,
        cwd,
        &["--version"],
        256,
        Instant::now() + COMMAND_DEADLINE,
        "native Git version",
    )?;
    ensure!(!truncated, "Git version output exceeded its bound");
    ensure!(v == b"git version 2.55.0\n", "expected native Git 2.55.0");
    let m = fs::metadata(&p)?;
    Ok((p, (m.dev(), m.ino())))
}
fn file_identity(p: &Path) -> Result<(u64, u64)> {
    let m = fs::metadata(p)?;
    Ok((m.dev(), m.ino()))
}
fn signing_requested(cwd: &Path) -> Result<bool> {
    bool_config(cwd, "commit.gpgsign", false)
}
fn bool_config(cwd: &Path, key: &str, default: bool) -> Result<bool> {
    let bytes = git_capture(
        cwd,
        &[
            "config",
            "--bool",
            if default {
                "--default=true"
            } else {
                "--default=false"
            },
            "--get",
            key,
        ],
        256,
    )?;
    match bytes.as_slice() {
        b"true\n" => Ok(true),
        b"false\n" => Ok(false),
        _ => bail!("invalid boolean Git configuration for {key}"),
    }
}
fn is_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn kill_group(c: &mut Child) {
    unsafe {
        libc::kill(-(c.id() as i32), libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn message_requires_text_and_accepts_multiline_unicode_without_controls() {
        for text in [
            "",
            " \n\t",
            "bad\0text",
            "bad\rtext",
            "bad\u{1b}text",
            &"x".repeat(8193),
        ] {
            assert!(validate_message(text).is_err());
        }
        assert!(validate_message("Исправление\n\nОписание\tизменения").is_ok());
        assert!(validate_message(&"x".repeat(8192)).is_ok());
    }
    #[test]
    fn continuously_readable_hook_output_cannot_starve_a_tick_or_exceed_retained_bound() {
        struct Flood(usize);
        impl Read for Flood {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.0 += 1;
                bytes.fill(b'x');
                Ok(bytes.len())
            }
        }
        let mut stream = Flood(0);
        let mut buffer = Vec::new();
        drain(Some(&mut stream), &mut buffer);
        assert_eq!(stream.0, 8);
        for _ in 0..20 {
            drain(Some(&mut stream), &mut buffer);
        }
        assert_eq!(buffer.len(), OUTPUT_MAX);
    }
}
