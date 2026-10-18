//! Real-host coverage for project creation and bounded provider detection.
use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{Command, GitDiffSide, PROTOCOL, Request, Response, receive, send};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

struct HostFixture {
    state: TempDir,
    bin: TempDir,
    child: Option<Child>,
}

impl HostFixture {
    fn new() -> Result<Self> {
        let state = tempfile::tempdir()?;
        let bin = tempfile::tempdir()?;
        let mut fixture = Self {
            state,
            bin,
            child: None,
        };
        fixture.write_cli("claude", "2.1.259")?;
        fixture.write_cli("codex", "0.162.1")?;
        fixture.write_cli("opencode", "fixture-1")?;
        fixture.start()?;
        Ok(fixture)
    }

    fn state(&self) -> &Path {
        self.state.path()
    }

    fn socket(&self) -> PathBuf {
        self.state().join("host.sock")
    }

    fn write_cli(&self, name: &str, version: &str) -> Result<()> {
        let path = self.bin.path().join(name);
        fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", version))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        let path =
            std::env::join_paths([self.bin.path(), Path::new("/usr/bin"), Path::new("/bin")])?;
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"));
        child
            .arg("serve")
            .arg(self.state())
            .env("PATH", path)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.state().join("host.log"))?);
        self.child = Some(child.spawn()?);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.as_mut().context("host missing")?.try_wait()? {
                bail!(
                    "host exited {status}: {}",
                    fs::read_to_string(self.state().join("host.log"))?
                );
            }
            if exchange(&self.socket(), Command::List).is_ok() {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "host did not expose its socket");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn restart(&mut self) -> Result<()> {
        self.stop()?;
        self.start()
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            child.kill()?;
            child.wait()?;
        }
        Ok(())
    }
}

impl Drop for HostFixture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn exchange(socket: &Path, command: Command) -> Result<Response> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    send(
        &mut stream,
        &Request {
            protocol: PROTOCOL,
            command,
        },
    )?;
    let response: Response = receive(&mut stream)?;
    ensure!(response.protocol == PROTOCOL, "host protocol mismatch");
    if let Some(error) = response.error.as_deref() {
        bail!("host command failed: {error}");
    }
    Ok(response)
}

#[test]
fn create_project_is_git_initialized_and_persisted_across_host_restart() -> Result<()> {
    let mut host = HostFixture::new()?;
    let parent = host.state().join("selected-parent");
    fs::create_dir(&parent)?;

    let response = exchange(
        &host.socket(),
        Command::CreateProject {
            parent: parent.clone(),
            name: "BridgeMind".into(),
        },
    )?;
    let project = response
        .catalog
        .context("create_project did not return catalog")?
        .projects
        .into_iter()
        .next()
        .context("created project missing")?;
    let root = parent.join("BridgeMind");
    ensure!(project.canonical_path == fs::canonicalize(&root)?);
    ensure!(root.join(".git").is_dir());
    ensure!(project.name == "BridgeMind");
    ensure!(
        fs::read_to_string(root.join(".git/HEAD"))? == "ref: refs/heads/main\n",
        "new repository should use main as the initial branch"
    );
    ensure!(
        fs::read_dir(&root)?.count() == 1,
        "no additional files expected"
    );

    let before = fs::read(root.join(".git/HEAD"))?;
    let failed = exchange(
        &host.socket(),
        Command::CreateProject {
            parent: parent.clone(),
            name: "BridgeMind".into(),
        },
    );
    ensure!(failed.is_err(), "existing target must not be reused");
    ensure!(fs::read(root.join(".git/HEAD"))? == before);

    host.restart()?;
    let catalog = exchange(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing after host restart")?;
    ensure!(catalog.projects.iter().any(|entry| entry.id == project.id));
    Ok(())
}

#[test]
fn provider_detection_reports_installed_clis_without_starting_them() -> Result<()> {
    let host = HostFixture::new()?;
    let response = exchange(&host.socket(), Command::DetectProviders)?;
    let providers = response.providers.context("provider list missing")?;
    ensure!(
        providers.len() == 4,
        "expected the four supported CLI choices"
    );
    for expected in ["shell", "claude", "codex", "opencode"] {
        ensure!(
            providers
                .iter()
                .any(|provider| provider.provider == expected)
        );
    }
    for expected in ["claude", "codex", "opencode"] {
        let provider = providers
            .iter()
            .find(|provider| provider.provider == expected)
            .context("fixture provider missing")?;
        ensure!(provider.available, "fixture {expected} should be detected");
        ensure!(
            provider.version.is_some(),
            "fixture {expected} version missing"
        );
        ensure!(
            provider
                .executable
                .as_ref()
                .is_some_and(|path| path.starts_with(host.bin.path()))
        );
    }
    let shell = providers
        .iter()
        .find(|provider| provider.provider == "shell")
        .context("shell record missing")?;
    ensure!(shell.available && shell.executable.is_some());
    Ok(())
}

#[test]
fn create_project_rejects_path_components_before_creating_anything() -> Result<()> {
    let host = HostFixture::new()?;
    let parent = host.state().join("parent");
    fs::create_dir(&parent)?;
    let names = ["..", "../escape", "a/b", "line\nfeed"]
        .into_iter()
        .map(str::to_owned)
        .chain(["é".repeat(49)])
        .collect::<Vec<_>>();
    for name in names {
        let response = exchange(
            &host.socket(),
            Command::CreateProject {
                parent: parent.clone(),
                name: name.clone(),
            },
        );
        ensure!(
            response.is_err(),
            "unsafe name should be rejected: {name:?}"
        );
    }
    ensure!(fs::read_dir(&parent)?.next().is_none());
    ensure!(!host.state().join("escape").exists());
    Ok(())
}

#[test]
fn workspace_git_snapshot_isolated_from_host_commands_and_read_only() -> Result<()> {
    let host = HostFixture::new()?;
    let parent = host.state().join("snapshot-parent");
    fs::create_dir(&parent)?;
    let created = exchange(
        &host.socket(),
        Command::CreateProject {
            parent,
            name: "SnapshotRepo".into(),
        },
    )?;
    let project = created
        .catalog
        .context("CreateProject catalog missing")?
        .projects
        .into_iter()
        .next()
        .context("created project missing")?;
    let workspace_response = exchange(
        &host.socket(),
        Command::CreateWorkspace {
            project: project.id,
            task: "snapshot fixture".into(),
            worktree: false,
        },
    )?;
    let before = workspace_response
        .catalog
        .context("CreateWorkspace catalog missing")?;
    let workspace = before
        .workspaces
        .iter()
        .find(|workspace| workspace.project_id == project.id)
        .context("ready workspace missing")?;
    ensure!(workspace.status == pigade_storage::WorkspaceStatus::Ready);
    let expected_revision = workspace.revision;
    let untracked = workspace.cwd.join("untracked snapshot.txt");
    let untracked_contents = b"untracked\n";
    fs::write(&untracked, untracked_contents)?;
    let index_path = workspace.cwd.join(".git/index");
    let index_before = fs::read(&index_path).ok();

    let stale = exchange(
        &host.socket(),
        Command::WorkspaceGitSnapshot {
            workspace: workspace.id,
            expected_revision: expected_revision + 1,
        },
    );
    ensure!(stale.is_err(), "stale workspace revision was accepted");

    // Gate the Git worker after it has begun status. This lets List and Poll
    // prove actor responsiveness while the filesystem probe is outstanding,
    // without relying on a timing threshold.
    let started = host.state().join("git-status-started");
    let release = host.state().join("git-status-release");
    let wrapper = host.bin.path().join("git");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \" $* \" in *\" status \"*) : > '{}'; while [ ! -e '{}' ]; do sleep 0.01; done ;; esac\nexec /usr/bin/git \"$@\"\n",
            started.display(),
            release.display()
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;

    let mut pending = std::os::unix::net::UnixStream::connect(host.socket())?;
    pending.set_read_timeout(Some(Duration::from_secs(5)))?;
    pending.set_write_timeout(Some(Duration::from_secs(5)))?;
    send(
        &mut pending,
        &Request {
            protocol: PROTOCOL,
            command: Command::WorkspaceGitSnapshot {
                workspace: workspace.id,
                expected_revision,
            },
        },
    )?;
    let start_deadline = Instant::now() + Duration::from_secs(3);
    while !started.exists() {
        ensure!(
            Instant::now() < start_deadline,
            "Git worker did not start status"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let listed = exchange(&host.socket(), Command::List)?;
    ensure!(listed.error.is_none());
    ensure!(listed.views.is_empty(), "status request started a terminal");
    let polled = exchange(&host.socket(), Command::Poll { known: Vec::new() })?;
    ensure!(polled.error.is_none());
    fs::write(&release, "release\n")?;
    let response: Response = receive(&mut pending)?;
    ensure!(
        response.error.is_none(),
        "Git snapshot failed: {:?}",
        response.error
    );
    let snapshot = response
        .workspace_git_snapshot
        .context("snapshot response missing its additive DTO")?;
    ensure!(snapshot.workspace_id == workspace.id);
    ensure!(snapshot.workspace_revision == expected_revision);
    ensure!(
        snapshot
            .entries
            .iter()
            .any(|entry| { entry.path == "untracked snapshot.txt" && entry.untracked })
    );
    ensure!(
        response.views.is_empty(),
        "snapshot response started a terminal"
    );

    let host_pid = host.child.as_ref().context("host process missing")?.id();
    let before_diff_views = exchange(&host.socket(), Command::List)?.views;
    ensure!(
        before_diff_views.is_empty(),
        "Git snapshot left a process view"
    );
    let diff_response = exchange(
        &host.socket(),
        Command::WorkspaceGitDiff {
            workspace: workspace.id,
            expected_revision,
            path: "untracked snapshot.txt".into(),
            side: GitDiffSide::Untracked,
        },
    )?;
    let diff = diff_response
        .workspace_git_diff
        .context("untracked Git diff response missing its additive DTO")?;
    ensure!(diff.workspace_id == workspace.id);
    ensure!(diff.workspace_revision == expected_revision);
    ensure!(diff.project_id == project.id);
    ensure!(diff.cwd == workspace.cwd);
    ensure!(diff.path == "untracked snapshot.txt");
    ensure!(diff.side == GitDiffSide::Untracked);
    ensure!(!diff.binary && !diff.truncated);
    ensure!(
        diff.text.lines().any(|line| line == "--- a/file")
            && diff.text.lines().any(|line| line == "+++ b/file")
            && diff.text.contains("@@ -0,0 +1 @@")
            && diff.text.lines().any(|line| line == "+untracked"),
        "untracked diff did not contain the expected empty-to-file patch: {:?}",
        diff.text
    );
    ensure!(diff_response.catalog.is_none());
    ensure!(
        diff_response.views.is_empty(),
        "Git diff started a terminal"
    );

    for (path, side, revision) in [
        (
            "untracked snapshot.txt",
            GitDiffSide::Untracked,
            expected_revision + 1,
        ),
        (
            "untracked snapshot.txt",
            GitDiffSide::Index,
            expected_revision,
        ),
        (
            "../untracked snapshot.txt",
            GitDiffSide::Untracked,
            expected_revision,
        ),
        (
            "missing snapshot.txt",
            GitDiffSide::Untracked,
            expected_revision,
        ),
    ] {
        let rejected = exchange(
            &host.socket(),
            Command::WorkspaceGitDiff {
                workspace: workspace.id,
                expected_revision: revision,
                path: path.into(),
                side,
            },
        );
        ensure!(
            rejected.is_err(),
            "invalid diff request was accepted: {path} {side:?}"
        );
    }

    let after = exchange(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing after snapshot")?;
    ensure!(
        after == before,
        "read-only Git snapshot/diff changed catalog metadata"
    );
    ensure!(
        fs::read(&index_path).ok() == index_before,
        "read-only Git snapshot/diff wrote Git index"
    );
    ensure!(
        fs::read(&untracked)? == untracked_contents,
        "read-only Git snapshot/diff changed fixture file"
    );
    let after_diff_views = exchange(&host.socket(), Command::List)?.views;
    ensure!(after_diff_views.is_empty(), "Git diff started a terminal");
    ensure!(
        host.child.as_ref().context("host process missing")?.id() == host_pid,
        "Git read changed the host process"
    );
    Ok(())
}
