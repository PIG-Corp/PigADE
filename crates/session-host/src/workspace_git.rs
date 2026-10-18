//! Bounded, read-only Git snapshot requests for one catalog workspace.
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use pigade_storage::{Catalog, WorkspaceStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const OUTPUT_LIMIT: usize = 256 * 1024;
// Each identity-validation phase and the status command receive a fresh
// bounded deadline, so a timed-out status can still be revalidated and
// returned as `truncated` (unknown) instead of being mistaken for clean.
const COMMAND_DEADLINE: Duration = Duration::from_secs(3);
const OUTPUT_POLL: Duration = Duration::from_millis(10);
const MAX_ENTRIES: usize = 128;

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
struct Task {
    target: Target,
    reply: mpsc::SyncSender<crate::Response>,
}

pub(crate) struct Reader {
    sender: mpsc::SyncSender<Task>,
}

impl Reader {
    pub(crate) fn new() -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Task>(1);
        let receiver = Arc::new(Mutex::new(receiver));
        let worker_receiver = receiver.clone();
        thread::Builder::new()
            .name("pigade-workspace-git".into())
            .spawn(move || {
                loop {
                    let task = worker_receiver
                        .lock()
                        .expect("workspace Git worker queue")
                        .recv();
                    let Ok(task) = task else { break };
                    let result = snapshot(task.target);
                    let response = match result {
                        Ok(snapshot) => response(Some(snapshot), None),
                        Err(error) => response(
                            None,
                            Some(format!("workspace Git status unavailable: {error:#}")),
                        ),
                    };
                    let _ = task.reply.try_send(response);
                }
            })?;
        Ok(Self { sender })
    }

    pub(crate) fn submit(&self, target: Target, reply: mpsc::SyncSender<crate::Response>) {
        match self.sender.try_send(Task { target, reply }) {
            Ok(()) => (),
            Err(mpsc::TrySendError::Full(task)) => {
                let _ = task.reply.try_send(response(
                    None,
                    Some("workspace Git status queue is full".into()),
                ));
            }
            Err(mpsc::TrySendError::Disconnected(task)) => {
                let _ = task.reply.try_send(response(
                    None,
                    Some("workspace Git status worker unavailable".into()),
                ));
            }
        }
    }
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
    })
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
            "--untracked-files=normal",
            "--ignore-submodules=none",
            "--no-renames",
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
    ensure!(exit.success(), "{label} exited with {exit}");
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
            "--untracked-files=normal",
            "--ignore-submodules=none",
            "--no-renames",
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
    let path = split
        .nth(columns - 3)
        .context("Git status record lacks path")?;
    let original_path = if rename {
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
    }
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

    #[test]
    fn parses_porcelain_v2_headers_statuses_renames_and_unusual_paths() -> Result<()> {
        let bytes = concat!(
            "# branch.oid abc123\0",
            "# branch.head feature/test\0",
            "# branch.upstream origin/feature/test\0",
            "1 M. N... 100644 100644 100644 abc abc tracked\nwith-newline\0",
            "2 R. N... 100644 100644 100644 abc def R100 moved name\0old name\0",
            "u UU N... 100644 100644 100644 100644 a b c conflict\0",
            "? untracked file\0",
        )
        .as_bytes();
        let parsed = parse_porcelain_v2(bytes, 10)?;
        assert_eq!(parsed.branch.as_deref(), Some("feature/test"));
        assert_eq!(parsed.head.as_deref(), Some("abc123"));
        assert!(!parsed.detached);
        assert_eq!(parsed.entries.len(), 4);
        assert_eq!(parsed.entries[0].path, "tracked\nwith-newline");
        assert_eq!(parsed.entries[1].path, "moved name");
        assert_eq!(parsed.entries[1].original_path.as_deref(), Some("old name"));
        assert!(parsed.entries[2].unmerged);
        assert!(parsed.entries[3].untracked);
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
                .any(|entry| { entry.untracked && entry.path == "untracked dir/" })
        );
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
            prompts: None,
            tab_close_schema: None,
            engine_options_schema: Some(1),
            pane_tab_drop_schema: Some(1),
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
