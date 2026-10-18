//! Real-host coverage for project creation and bounded provider detection.
use anyhow::{Context, Result, bail, ensure};
use pigade_session_host::{
    Command, GitDiffSide, GitIndexAction, GitIndexPhase, PROTOCOL, Request, Response, receive, send,
};
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
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
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

struct FileHold(PathBuf);

impl FileHold {
    fn release(&self) -> Result<()> {
        if self.0.exists() {
            fs::remove_file(&self.0)?;
        }
        Ok(())
    }
}

impl Drop for FileHold {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[test]
fn profile_roster_edits_are_atomic_metadata_and_survive_host_restart() -> Result<()> {
    use pigade_storage::{AgentProfile, CatalogBatch, CatalogWrite, LaunchSettings};
    let mut host = HostFixture::new()?;
    let ids = [
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
    ];
    let seeded = exchange(
        &host.socket(),
        Command::CatalogApply {
            batch: Box::new(CatalogBatch {
                agent_profiles: ids
                    .iter()
                    .enumerate()
                    .map(|(index, id)| CatalogWrite {
                        value: AgentProfile {
                            id: *id,
                            name: format!("Role {index}"),
                            revision: 0,
                            settings: LaunchSettings {
                                provider: "codex".into(),
                                role: "Review".into(),
                                instructions: "Inspect changes".into(),
                                deliver_context: false,
                                engine: Default::default(),
                                custom_command: None,
                            },
                        },
                        expected_revision: None,
                    })
                    .collect(),
                ..Default::default()
            }),
        },
    )?
    .catalog
    .context("seed catalog missing")?;
    let roster = seeded
        .agent_profile_roster
        .as_ref()
        .context("roster capability missing")?;
    ensure!(roster.schema == 1 && roster.ordered_ids == ids);
    let order = vec![ids[2], ids[0], ids[1]];
    let pins = vec![ids[0], ids[2]];
    let saved = exchange(
        &host.socket(),
        Command::SetAgentProfileRoster {
            expected_revision: roster.revision,
            ordered_ids: order.clone(),
            pinned_ids: pins.clone(),
        },
    )?
    .catalog
    .context("roster result missing")?;
    let current = saved
        .agent_profile_roster
        .as_ref()
        .context("saved roster missing")?;
    ensure!(current.ordered_ids == order && current.pinned_ids == pins);
    ensure!(current.revision == roster.revision + 1);
    ensure!(
        saved.agent_profiles == seeded.agent_profiles
            && saved.selection == seeded.selection
            && saved.layout == seeded.layout
            && saved.projects == seeded.projects
            && saved.workspaces == seeded.workspaces
            && saved.tabs == seeded.tabs
    );
    for (revision, proposed_order, proposed_pins) in [
        (roster.revision, order.clone(), pins.clone()),
        (current.revision, vec![ids[2], ids[0], ids[0]], pins.clone()),
        (current.revision, vec![ids[2], ids[0]], pins.clone()),
        (current.revision, order.clone(), vec![uuid::Uuid::new_v4()]),
    ] {
        ensure!(
            exchange(
                &host.socket(),
                Command::SetAgentProfileRoster {
                    expected_revision: revision,
                    ordered_ids: proposed_order,
                    pinned_ids: proposed_pins,
                }
            )
            .is_err(),
            "invalid/stale roster was accepted"
        );
        ensure!(
            exchange(&host.socket(), Command::Catalog)?.catalog.as_ref() == Some(&saved),
            "rejected roster changed metadata"
        );
    }
    ensure!(
        exchange(&host.socket(), Command::List)?.views.is_empty(),
        "profile arrangement launched a CLI"
    );
    host.restart()?;
    ensure!(exchange(&host.socket(), Command::Catalog)?.catalog.as_ref() == Some(&saved));
    ensure!(exchange(&host.socket(), Command::List)?.views.is_empty());
    Ok(())
}

fn exchange(socket: &Path, command: Command) -> Result<Response> {
    let response = exchange_allow_error(socket, command)?;
    if let Some(error) = response.error.as_deref() {
        bail!("host command failed: {error}");
    }
    Ok(response)
}

fn exchange_allow_error(socket: &Path, command: Command) -> Result<Response> {
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

#[test]
fn workspace_recovery_git_probe_is_off_actor_single_flight_and_idle_blocking() -> Result<()> {
    use pigade_storage::{CatalogBatch, CatalogWrite, WorkspaceStatus};

    let mut host = HostFixture::new()?;
    let parent = host.state().join("recovery-parent");
    fs::create_dir(&parent)?;
    let project = exchange(
        &host.socket(),
        Command::CreateProject {
            parent,
            name: "RecoveryRepo".into(),
        },
    )?
    .catalog
    .context("project catalog missing")?
    .projects
    .into_iter()
    .next()
    .context("project missing")?;
    let seeded_commit = ProcessCommand::new("/usr/bin/git")
        .arg("-C")
        .arg(&project.canonical_path)
        .args([
            "-c",
            "user.name=Recovery Fixture",
            "-c",
            "user.email=recovery@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "--no-gpg-sign",
            "-m",
            "base",
        ])
        .status()?;
    ensure!(
        seeded_commit.success(),
        "could not seed private recovery repo"
    );
    let created = exchange(
        &host.socket(),
        Command::CreateWorkspace {
            project: project.id,
            task: "recovery fixture".into(),
            worktree: true,
        },
    )?
    .catalog
    .context("workspace catalog missing")?;
    let ready = created
        .workspaces
        .iter()
        .find(|workspace| workspace.project_id == project.id)
        .context("workspace missing")?
        .clone();
    ensure!(ready.status == WorkspaceStatus::Ready);
    let dirty = ready.cwd.join("user-owned.txt");
    fs::write(&dirty, b"preserve this file\n")?;
    let mut preparing = ready.clone();
    preparing.status = WorkspaceStatus::Preparing;
    let before = exchange(
        &host.socket(),
        Command::CatalogApply {
            batch: Box::new(CatalogBatch {
                workspaces: vec![CatalogWrite {
                    value: preparing,
                    expected_revision: Some(ready.revision),
                }],
                ..CatalogBatch::default()
            }),
        },
    )?
    .catalog
    .context("Preparing catalog missing")?;
    let preparing = before
        .workspaces
        .iter()
        .find(|workspace| workspace.id == ready.id)
        .context("Preparing workspace missing")?;
    ensure!(preparing.status == WorkspaceStatus::Preparing);
    let tab_before = before
        .tabs
        .iter()
        .find(|tab| tab.workspace_id == ready.id)
        .context("existing development tab missing")?;
    let expected_revision = preparing.revision;

    let hold = host.state().join("recovery-git-hold");
    let hold_guard = FileHold(hold.clone());
    let started = host.state().join("recovery-git-started");
    let wrapper = host.bin.path().join("git");
    let quote =
        |path: &Path| -> String { format!("'{}'", path.to_string_lossy().replace('\'', "'\\''")) };
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ -e {hold} ]; then printf '%s\\n' \"$$\" >> {started}; while [ -e {hold} ]; do sleep 0.01; done; fi\nexec /usr/bin/git \"$@\"\n",
            hold = quote(&hold),
            started = quote(&started),
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
    fs::write(&hold, b"hold\n")?;

    let socket = host.socket();
    let workspace_id = ready.id;
    let recovery = thread::spawn(move || {
        exchange(
            &socket,
            Command::RecoverWorkspace {
                workspace: workspace_id,
                expected_revision,
            },
        )
    });
    let start_deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        ensure!(
            Instant::now() < start_deadline,
            "workspace recovery Git reader did not enter the hold"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let recorded_pid = fs::read_to_string(&started)?;
    ensure!(
        recorded_pid.lines().count() == 1,
        "more than one Git probe started"
    );
    let git_pid: i32 = recorded_pid.trim().parse()?;
    let git_alive = || unsafe { libc::kill(git_pid, 0) == 0 };
    ensure!(
        hold.exists() && git_alive(),
        "Git inspection did not remain held"
    );

    ensure!(exchange(&host.socket(), Command::List)?.views.is_empty());
    ensure!(
        exchange(&host.socket(), Command::Catalog)?.catalog.as_ref() == Some(&before),
        "metadata request stalled or changed while Git inspection was held"
    );
    let status = exchange(&host.socket(), Command::HostStatus)?
        .host_status
        .context("host status missing while recovery is held")?;
    ensure!(status.git_busy && !status.can_shutdown);
    let shutdown = exchange_allow_error(
        &host.socket(),
        Command::ShutdownIfIdle {
            expected_incarnation: status.incarnation,
        },
    )?;
    ensure!(
        shutdown.error.is_some(),
        "idle shutdown ignored active recovery"
    );
    ensure!(
        exchange(
            &host.socket(),
            Command::RecoverWorkspace {
                workspace: ready.id,
                expected_revision,
            },
        )
        .is_err(),
        "concurrent workspace recovery was not rejected"
    );
    ensure!(
        fs::read_to_string(&started)?.lines().count() == 1,
        "a second Git inspection worker was started"
    );
    ensure!(
        hold.exists() && git_alive(),
        "Git inspection ended before release"
    );
    hold_guard.release()?;

    let recovered = recovery
        .join()
        .map_err(|_| anyhow::anyhow!("recovery IPC thread panicked"))??
        .catalog
        .context("recovery catalog missing")?;
    let workspace = recovered
        .workspaces
        .iter()
        .find(|workspace| workspace.id == ready.id)
        .context("recovered workspace missing")?;
    ensure!(workspace.status == WorkspaceStatus::Ready);
    ensure!(recovered.selection == before.selection);
    ensure!(recovered.layout == before.layout);
    ensure!(recovered.tabs.len() == before.tabs.len());
    let tab_after = recovered
        .tabs
        .iter()
        .find(|tab| tab.id == tab_before.id)
        .context("existing development tab was replaced")?;
    ensure!(
        tab_after == tab_before,
        "existing tab changed during recovery"
    );
    ensure!(fs::read(&dirty)? == b"preserve this file\n");
    ensure!(exchange(&host.socket(), Command::List)?.views.is_empty());

    host.restart()?;
    let after_restart = exchange(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing after recovery restart")?;
    ensure!(
        after_restart == recovered,
        "recovered catalog changed on restart"
    );
    ensure!(
        exchange(
            &host.socket(),
            Command::RecoverWorkspace {
                workspace: ready.id,
                expected_revision: workspace.revision,
            },
        )
        .is_err(),
        "Ready workspace was recoverable again"
    );
    ensure!(
        exchange(&host.socket(), Command::List)?.views.is_empty(),
        "workspace recovery launched a CLI"
    );
    ensure!(
        exchange(&host.socket(), Command::Catalog)?.catalog.as_ref() == Some(&recovered),
        "repeat recovery changed tabs or metadata"
    );
    ensure!(
        fs::read_to_string(&started)?.lines().count() == 1,
        "repeated recovery launched another Git inspection"
    );
    drop(hold_guard);
    Ok(())
}

#[test]
fn index_actions_bind_preview_and_survive_host_restart_without_repeating_filters() -> Result<()> {
    let mut host = HostFixture::new()?;
    let parent = host.state().join("index-parent");
    fs::create_dir(&parent)?;
    let project = exchange(
        &host.socket(),
        Command::CreateProject {
            parent,
            name: "IndexRepo".into(),
        },
    )?
    .catalog
    .context("project catalog missing")?
    .projects
    .into_iter()
    .next()
    .context("project missing")?;
    let catalog = exchange(
        &host.socket(),
        Command::CreateWorkspace {
            project: project.id,
            task: "index workflow".into(),
            worktree: false,
        },
    )?
    .catalog
    .context("workspace catalog missing")?;
    let workspace = catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.project_id == project.id)
        .context("workspace missing")?
        .clone();
    let path = "safe stage.txt";
    let file = workspace.cwd.join(path);
    fs::write(&file, "reviewed content\n")?;
    let preview = |host: &HostFixture, side| -> Result<_> {
        exchange(
            &host.socket(),
            Command::WorkspaceGitDiff {
                workspace: workspace.id,
                expected_revision: workspace.revision,
                path: path.into(),
                side,
            },
        )?
        .workspace_git_diff
        .context("diff missing")
    };
    let apply = |host: &HostFixture, ticket, inspect| -> Result<_> {
        exchange(
            &host.socket(),
            Command::WorkspaceGitIndex {
                workspace: workspace.id,
                expected_revision: workspace.revision,
                ticket,
                inspect,
            },
        )?
        .workspace_git_index
        .context("index receipt missing")
    };
    let private_git = |args: &[&str]| -> Result<Vec<u8>> {
        let output = ProcessCommand::new("/usr/bin/git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(&workspace.cwd)
            .args(args)
            .output()?;
        ensure!(
            output.status.success(),
            "private Git fixture failed: {:?}",
            args
        );
        Ok(output.stdout)
    };
    let index = workspace.cwd.join(".git/index");
    let stale = preview(&host, GitDiffSide::Untracked)?
        .index_ticket
        .context("stage ticket missing")?;
    fs::write(&file, "new content after preview\n")?;
    let rejected = apply(&host, stale.id, false)?;
    ensure!(
        rejected.phase == GitIndexPhase::Rejected,
        "stale preview was not rejected"
    );
    ensure!(!index.exists(), "stale preview changed the unborn index");

    let ticket = preview(&host, GitDiffSide::Untracked)?
        .index_ticket
        .context("fresh stage ticket missing")?;
    let staged = apply(&host, ticket.id, false)?;
    ensure!(
        staged.phase == GitIndexPhase::Applied,
        "stage failed: {:?}",
        staged
    );
    ensure!(private_git(&["show", ":0:safe stage.txt"])? == fs::read(&file)?);
    let staged_index = fs::read(&index)?;
    ensure!(
        apply(&host, ticket.id, false)? == staged,
        "same-ticket retry changed its receipt"
    );
    ensure!(
        fs::read(&index)? == staged_index,
        "duplicate stage rewrote the index"
    );
    host.restart()?;
    ensure!(
        apply(&host, ticket.id, true)? == staged,
        "restart inspection lost the durable receipt"
    );
    ensure!(
        apply(&host, ticket.id, false)? == staged,
        "restart replay repeated a completed action"
    );
    ensure!(fs::read(&index)? == staged_index);

    let unstage = preview(&host, GitDiffSide::Index)?
        .index_ticket
        .context("unstage ticket missing")?;
    let unstaged = apply(&host, unstage.id, false)?;
    ensure!(
        unstaged.phase == GitIndexPhase::Applied,
        "unstage failed: {:?}",
        unstaged
    );
    ensure!(private_git(&["ls-files", "--stage", "-z"])?.is_empty());
    ensure!(fs::read_to_string(&file)? == "new content after preview\n");

    private_git(&[
        "config",
        "--local",
        "filter.pigade-index-fixture.clean",
        "sh -c 'printf x >> .git/pigade-filter-calls; exit 1'",
    ])?;
    private_git(&[
        "config",
        "--local",
        "filter.pigade-index-fixture.required",
        "true",
    ])?;
    fs::write(
        workspace.cwd.join(".gitattributes"),
        "*.txt filter=pigade-index-fixture\n",
    )?;
    let unknown_ticket = preview(&host, GitDiffSide::Untracked)?
        .index_ticket
        .context("filter stage ticket missing")?;
    let before_unknown = fs::read(&index)?;
    let unknown = apply(&host, unknown_ticket.id, false)?;
    ensure!(
        unknown.phase == GitIndexPhase::Unknown,
        "failed external filter did not retain Unknown: {:?}",
        unknown
    );
    let calls = workspace.cwd.join(".git/pigade-filter-calls");
    ensure!(fs::read(&calls)? == b"x", "filter did not run exactly once");
    ensure!(
        fs::read(&index)? == before_unknown,
        "failed filter changed index"
    );
    let pending = exchange(
        &host.socket(),
        Command::WorkspaceGitSnapshot {
            workspace: workspace.id,
            expected_revision: workspace.revision,
        },
    )?
    .workspace_git_snapshot
    .context("snapshot missing")?;
    ensure!(
        pending
            .index_operation
            .as_ref()
            .is_some_and(|operation| operation.ticket == unknown_ticket.id
                && operation.phase == GitIndexPhase::Unknown
                && !operation.acknowledged)
    );
    ensure!(
        preview(&host, GitDiffSide::Untracked)?
            .index_ticket
            .is_none(),
        "unresolved outcome issued a new write ticket"
    );
    host.restart()?;
    ensure!(apply(&host, unknown_ticket.id, true)?.phase == GitIndexPhase::Unknown);
    ensure!(apply(&host, unknown_ticket.id, false)?.phase == GitIndexPhase::Unknown);
    ensure!(
        fs::read(&calls)? == b"x",
        "inspection/replay reran the external filter"
    );
    let ack = exchange(
        &host.socket(),
        Command::WorkspaceGitIndexAcknowledge {
            workspace: workspace.id,
            expected_revision: workspace.revision,
            ticket: unknown_ticket.id,
        },
    )?
    .workspace_git_index
    .context("ack receipt missing")?;
    ensure!(
        ack.phase == GitIndexPhase::Unknown && ack.acknowledged,
        "acknowledgement invented an applied outcome"
    );
    ensure!(
        preview(&host, GitDiffSide::Untracked)?
            .index_ticket
            .is_some(),
        "explicit acknowledgement did not release the blocker"
    );
    ensure!(fs::read(&calls)? == b"x" && fs::read(&index)? == before_unknown);
    ensure!(exchange(&host.socket(), Command::Catalog)?.catalog == Some(catalog));
    ensure!(
        exchange(&host.socket(), Command::List)?.views.is_empty(),
        "index operation launched a terminal"
    );
    Ok(())
}

#[test]
fn batch_index_actions_bind_whole_status_and_survive_host_restart() -> Result<()> {
    let mut host = HostFixture::new()?;
    let parent = host.state().join("batch-parent");
    fs::create_dir(&parent)?;
    let project = exchange(
        &host.socket(),
        Command::CreateProject {
            parent,
            name: "BatchRepo".into(),
        },
    )?
    .catalog
    .context("project catalog missing")?
    .projects
    .into_iter()
    .next()
    .context("project missing")?;
    let catalog = exchange(
        &host.socket(),
        Command::CreateWorkspace {
            project: project.id,
            task: "batch index workflow".into(),
            worktree: false,
        },
    )?
    .catalog
    .context("workspace catalog missing")?;
    let workspace = catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.project_id == project.id)
        .context("workspace missing")?
        .clone();
    let files = [
        ("batch-a.txt", b"batch a contents\n".as_slice()),
        ("batch-b.txt", b"batch b contents\n".as_slice()),
    ];
    for (path, bytes) in files {
        fs::write(workspace.cwd.join(path), bytes)?;
    }
    let index = workspace.cwd.join(".git/index");
    let private_git = |args: &[&str]| -> Result<Vec<u8>> {
        let output = ProcessCommand::new("/usr/bin/git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(&workspace.cwd)
            .args(args)
            .output()?;
        ensure!(
            output.status.success(),
            "private Git fixture failed: {:?}",
            args
        );
        Ok(output.stdout)
    };
    let prepare = |host: &HostFixture, action| -> Result<_> {
        exchange(
            &host.socket(),
            Command::WorkspaceGitIndexBatchPrepare {
                workspace: workspace.id,
                expected_revision: workspace.revision,
                action,
            },
        )?
        .workspace_git_index_batch_plan
        .context("batch plan missing")
    };
    let request = |host: &HostFixture, ticket, inspect| -> Result<_> {
        exchange(
            &host.socket(),
            Command::WorkspaceGitIndexBatch {
                workspace: workspace.id,
                expected_revision: workspace.revision,
                ticket,
                inspect,
            },
        )?
        .workspace_git_index_batch
        .context("batch receipt missing")
    };
    let path_set = |mut paths: Vec<String>| {
        paths.sort();
        paths
    };
    let expected_paths = path_set(files.iter().map(|(path, _)| (*path).to_owned()).collect());

    // A whole-status lease must reject even a newly added path, and leave the
    // unborn index untouched when its status snapshot no longer matches.
    let stale = prepare(&host, GitIndexAction::Stage)?;
    fs::write(workspace.cwd.join("batch-late.txt"), b"late change\n")?;
    let stale_result = request(&host, stale.ticket, false)?;
    ensure!(
        stale_result.phase == GitIndexPhase::Rejected,
        "stale whole-status lease was accepted: {stale_result:?}"
    );
    ensure!(!index.exists(), "stale batch changed the index");

    fs::remove_file(workspace.cwd.join("batch-late.txt"))?;
    let stage = prepare(&host, GitIndexAction::Stage)?;
    ensure!(
        path_set(stage.paths.clone()) == expected_paths,
        "Stage All selected unexpected files: {:?}",
        stage.paths
    );
    let worktree_before = files
        .iter()
        .map(|(path, _)| fs::read(workspace.cwd.join(path)))
        .collect::<std::io::Result<Vec<_>>>()?;
    let staged = request(&host, stage.ticket, false)?;
    ensure!(
        staged.phase == GitIndexPhase::Applied,
        "Stage All failed: {staged:?}"
    );
    ensure!(
        path_set(staged.paths.clone()) == expected_paths,
        "Stage All receipt named unexpected files"
    );
    ensure!(
        files
            .iter()
            .map(|(path, _)| path.as_bytes())
            .collect::<Vec<_>>()
            == private_git(&["ls-files", "-z"])?
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .collect::<Vec<_>>(),
        "Stage All changed paths outside the two requested files"
    );
    for ((path, expected), actual) in files.iter().zip(worktree_before.iter()) {
        ensure!(
            actual.as_slice() == *expected,
            "Stage All altered worktree file {path}"
        );
        ensure!(
            private_git(&["show", &format!(":0:{path}")])? == *expected,
            "Stage All staged incorrect bytes for {path}"
        );
    }
    let staged_index = fs::read(&index)?;
    ensure!(
        request(&host, stage.ticket, false)? == staged,
        "same-ticket Stage All changed receipt"
    );
    ensure!(
        fs::read(&index)? == staged_index,
        "same-ticket Stage All rewrote the index"
    );
    host.restart()?;
    ensure!(
        request(&host, stage.ticket, true)? == staged,
        "restart inspection lost Stage All receipt"
    );
    ensure!(
        fs::read(&index)? == staged_index,
        "restart inspection changed the index"
    );

    fs::write(workspace.cwd.join("batch-a.txt"), b"modified after stage\n")?;
    let changed_worktree = fs::read(workspace.cwd.join("batch-a.txt"))?;
    let unstage = prepare(&host, GitIndexAction::Unstage)?;
    ensure!(
        path_set(unstage.paths.clone()) == expected_paths,
        "Unstage All selected unexpected files"
    );
    let unstaged = request(&host, unstage.ticket, false)?;
    ensure!(
        unstaged.phase == GitIndexPhase::Applied,
        "Unstage All failed: {unstaged:?}"
    );
    ensure!(
        path_set(unstaged.paths.clone()) == expected_paths,
        "Unstage All receipt named unexpected files"
    );
    ensure!(
        private_git(&["ls-files", "--stage", "-z"])?.is_empty(),
        "Unstage All left staged paths"
    );
    ensure!(
        fs::read(workspace.cwd.join("batch-a.txt"))? == changed_worktree,
        "Unstage All changed modified worktree bytes"
    );
    ensure!(
        fs::read(workspace.cwd.join("batch-b.txt"))? == files[1].1,
        "Unstage All changed worktree bytes"
    );

    // A lock collision makes the attempted batch outcome Unknown; acknowledge
    // it through the public IPC command and prove that it does not write data.
    let unknown_plan = prepare(&host, GitIndexAction::Stage)?;
    let lock = workspace.cwd.join(".git/index.lock");
    fs::write(&lock, b"held by another writer\n")?;
    let unknown = request(&host, unknown_plan.ticket, false)?;
    fs::remove_file(&lock)?;
    ensure!(
        unknown.phase == GitIndexPhase::Unknown,
        "index lock collision did not retain Unknown: {unknown:?}"
    );
    host.restart()?;
    ensure!(
        request(&host, unknown_plan.ticket, true)? == unknown,
        "restart inspection lost Unknown batch receipt"
    );
    let before_ack_index = fs::read(&index).ok();
    let before_ack_a = fs::read(workspace.cwd.join("batch-a.txt"))?;
    let before_ack_b = fs::read(workspace.cwd.join("batch-b.txt"))?;
    let acknowledged = exchange(
        &host.socket(),
        Command::WorkspaceGitIndexBatchAcknowledge {
            workspace: workspace.id,
            expected_revision: workspace.revision,
            ticket: unknown_plan.ticket,
        },
    )?
    .workspace_git_index_batch
    .context("acknowledged batch receipt missing")?;
    ensure!(
        acknowledged.phase == GitIndexPhase::Unknown && acknowledged.acknowledged,
        "acknowledgement invented an applied outcome: {acknowledged:?}"
    );
    ensure!(
        fs::read(&index).ok() == before_ack_index,
        "batch acknowledgement changed index bytes"
    );
    ensure!(fs::read(workspace.cwd.join("batch-a.txt"))? == before_ack_a);
    ensure!(fs::read(workspace.cwd.join("batch-b.txt"))? == before_ack_b);
    ensure!(exchange(&host.socket(), Command::Catalog)?.catalog == Some(catalog));
    ensure!(
        exchange(&host.socket(), Command::List)?.views.is_empty(),
        "batch index operation restarted a CLI"
    );
    Ok(())
}

struct CommitFixture {
    host: HostFixture,
    workspace: pigade_storage::Workspace,
}
impl CommitFixture {
    fn new() -> Result<Self> {
        let host = HostFixture::new()?;
        let parent = host.state().join("commit-parent");
        fs::create_dir(&parent)?;
        let project = exchange(
            &host.socket(),
            Command::CreateProject {
                parent,
                name: "CommitRepo".into(),
            },
        )?
        .catalog
        .context("project catalog")?
        .projects
        .into_iter()
        .next()
        .context("project")?;
        let workspace = exchange(
            &host.socket(),
            Command::CreateWorkspace {
                project: project.id,
                task: "native commit".into(),
                worktree: false,
            },
        )?
        .catalog
        .context("workspace catalog")?
        .workspaces
        .into_iter()
        .next()
        .context("workspace")?;
        let fixture = Self { host, workspace };
        fixture.git(&["config", "user.name", "PigADE Fixture"])?;
        fixture.git(&["config", "user.email", "fixture@example.test"])?;
        fixture.git(&["config", "commit.gpgsign", "false"])?;
        fs::write(
            fixture.workspace.cwd.join("staged.txt"),
            "reviewed staged text\n",
        )?;
        fixture.git(&["add", "--", "staged.txt"])?;
        Ok(fixture)
    }
    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        let output = ProcessCommand::new("/usr/bin/git")
            .arg("-C")
            .arg(&self.workspace.cwd)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()?;
        ensure!(
            output.status.success(),
            "private Git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(output.stdout)
    }
    fn prepare(&self) -> Result<pigade_session_host::WorkspaceGitCommitPlan> {
        exchange(
            &self.host.socket(),
            Command::WorkspaceGitCommitPrepare {
                workspace: self.workspace.id,
                expected_revision: self.workspace.revision,
            },
        )?
        .workspace_git_commit_plan
        .context("commit preview missing")
    }
    fn start(
        &self,
        ticket: uuid::Uuid,
        message: &str,
    ) -> Result<pigade_session_host::WorkspaceGitCommitResult> {
        exchange(
            &self.host.socket(),
            Command::WorkspaceGitCommit {
                workspace: self.workspace.id,
                expected_revision: self.workspace.revision,
                ticket,
                message: Some(message.into()),
                inspect: false,
            },
        )?
        .workspace_git_commit
        .context("commit result missing")
    }
    fn inspect(&self, ticket: uuid::Uuid) -> Result<pigade_session_host::WorkspaceGitCommitResult> {
        exchange(
            &self.host.socket(),
            Command::WorkspaceGitCommit {
                workspace: self.workspace.id,
                expected_revision: self.workspace.revision,
                ticket,
                message: None,
                inspect: true,
            },
        )?
        .workspace_git_commit
        .context("commit inspection missing")
    }
    fn finish(&self, ticket: uuid::Uuid) -> Result<pigade_session_host::WorkspaceGitCommitResult> {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let result = self.inspect(ticket)?;
            if result.phase != pigade_session_host::GitCommitPhase::Running {
                return Ok(result);
            }
            ensure!(
                Instant::now() < deadline,
                "native commit remained running: {:?}",
                result
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn hook(&self, name: &str, body: &str) -> Result<()> {
        let hooks = self.workspace.cwd.join(".git/fixture-hooks");
        fs::create_dir_all(&hooks)?;
        self.git(&["config", "core.hooksPath", ".git/fixture-hooks"])?;
        let path = hooks.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n"))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }
}

#[test]
fn native_commit_keeps_git_hooks_and_signature_and_is_idempotent_after_restart() -> Result<()> {
    use pigade_session_host::GitCommitPhase;
    let mut f = CommitFixture::new()?;
    for name in [
        "pre-commit",
        "prepare-commit-msg",
        "commit-msg",
        "post-commit",
    ] {
        f.hook(name, &format!("printf '%s\\n' '{name}' >> .git/hook-calls"))?;
    }
    // A configured named hook must remain native and run exactly once as well.
    f.git(&["config", "hook.pigade-fixture.event", "pre-commit"])?;
    f.git(&[
        "config",
        "hook.pigade-fixture.command",
        "printf 'named-hook\\n' >> .git/hook-calls",
    ])?;
    let key = f.host.state().join("fixture-signing-key");
    let status = ProcessCommand::new("/usr/bin/ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()?;
    ensure!(
        status.success(),
        "fixture SSH signing key generation failed"
    );
    f.git(&["config", "gpg.format", "ssh"])?;
    f.git(&[
        "config",
        "user.signingkey",
        key.to_str().context("key path")?,
    ])?;
    f.git(&["config", "commit.gpgsign", "true"])?;
    let signers = f.host.state().join("allowed-signers");
    fs::write(
        &signers,
        format!(
            "fixture@example.test {}",
            fs::read_to_string(key.with_extension("pub"))?
        ),
    )?;
    f.git(&[
        "config",
        "gpg.ssh.allowedSignersFile",
        signers.to_str().context("signers path")?,
    ])?;
    let plan = f.prepare()?;
    ensure!(plan.signing_requested && plan.head.is_none() && plan.files.len() == 1);
    ensure!(plan.files[0].path == "staged.txt");
    let started = Instant::now();
    let running = f.start(plan.ticket, "Commit reviewed staged text\n")?;
    ensure!(
        running.phase == GitCommitPhase::Running,
        "start: {:?}",
        running
    );
    ensure!(
        started.elapsed() < Duration::from_secs(4),
        "start blocked on native Git exit"
    );
    let result = f.finish(plan.ticket)?;
    ensure!(
        result.phase == GitCommitPhase::Applied,
        "native commit: {:?}",
        result
    );
    let commit = result.commit.as_deref().context("commit OID missing")?;
    ensure!(String::from_utf8(f.git(&["rev-parse", "HEAD"])?)?.trim() == commit);
    f.git(&["verify-commit", commit])?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while !fs::read_to_string(f.workspace.cwd.join(".git/hook-calls"))?
        .lines()
        .any(|line| line == "post-commit")
    {
        ensure!(
            Instant::now() < deadline,
            "native post-commit hook did not finish after publication: {:?}; {}",
            result,
            fs::read_to_string(f.host.state().join("host.log"))?
        );
        thread::sleep(Duration::from_millis(10));
    }
    let calls = fs::read_to_string(f.workspace.cwd.join(".git/hook-calls"))?;
    for name in [
        "named-hook",
        "pre-commit",
        "prepare-commit-msg",
        "commit-msg",
        "post-commit",
    ] {
        ensure!(
            calls.lines().filter(|line| *line == name).count() == 1,
            "hook was skipped/duplicated: {name}: {calls:?}"
        );
    }
    ensure!(f.git(&["config", "--get", "core.hooksPath"])? == b".git/fixture-hooks\n");
    ensure!(f.start(plan.ticket, "Commit reviewed staged text\n")? == result);
    f.host.restart()?;
    ensure!(f.inspect(plan.ticket)? == result);
    ensure!(f.start(plan.ticket, "Commit reviewed staged text\n")? == result);
    ensure!(fs::read_to_string(f.workspace.cwd.join(".git/hook-calls"))? == calls);
    ensure!(
        exchange(&f.host.socket(), Command::List)?.views.is_empty(),
        "commit spawned a terminal"
    );
    Ok(())
}

#[test]
fn commit_rejects_stale_index_and_hook_changed_tree_without_publishing_head() -> Result<()> {
    use pigade_session_host::GitCommitPhase;
    let f = CommitFixture::new()?;
    let stale = f.prepare()?;
    fs::write(f.workspace.cwd.join("later.txt"), "later staged text\n")?;
    f.git(&["add", "--", "later.txt"])?;
    ensure!(f.start(stale.ticket, "stale")?.phase == GitCommitPhase::Rejected);
    let plan = f.prepare()?;
    f.git(&["config", "hook.reference-transaction.enabled", "false"])?;
    f.hook(
        "pre-commit",
        "printf 'hook change\\n' > staged.txt\ngit add -- staged.txt",
    )?;
    ensure!(f.start(plan.ticket, "hook changes the tree")?.phase == GitCommitPhase::Running);
    let result = f.finish(plan.ticket)?;
    ensure!(
        result.phase == GitCommitPhase::Failed,
        "changed tree was not refused: {:?}",
        result
    );
    ensure!(
        !ProcessCommand::new("/usr/bin/git")
            .arg("-C")
            .arg(&f.workspace.cwd)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()?
            .status
            .success()
    );
    // Hook side effects are native Git behavior and are deliberately not rolled back.
    ensure!(f.git(&["show", ":0:staged.txt"])? == b"hook change\n");
    Ok(())
}

#[test]
fn commit_survives_host_kill_and_inspection_never_repeats_the_hook() -> Result<()> {
    use pigade_session_host::GitCommitPhase;
    let mut f = CommitFixture::new()?;
    f.hook("pre-commit", "printf x >> .git/hook-calls\ntouch .git/hook-started\ni=0\nwhile ! test -f .git/hook-release; do\n  i=$((i+1)); test \"$i\" -lt 200 || exit 1\n  sleep 0.05\ndone")?;
    let plan = f.prepare()?;
    ensure!(f.start(plan.ticket, "survive host restart")?.phase == GitCommitPhase::Running);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f.workspace.cwd.join(".git/hook-started").exists() {
        ensure!(Instant::now() < deadline, "pre-commit hook did not start");
        thread::sleep(Duration::from_millis(10));
    }
    // Actor commands remain usable while native hooks wait.
    ensure!(
        exchange(&f.host.socket(), Command::Catalog)?
            .catalog
            .is_some()
    );
    f.host.restart()?;
    ensure!(
        f.inspect(plan.ticket)?.phase == GitCommitPhase::Running,
        "live native Git lost on host reopen"
    );
    fs::write(f.workspace.cwd.join(".git/hook-release"), b"release")?;
    let result = f.finish(plan.ticket)?;
    ensure!(
        result.phase == GitCommitPhase::Applied,
        "surviving native Git result: {:?}",
        result
    );
    ensure!(fs::read(f.workspace.cwd.join(".git/hook-calls"))? == b"x");
    ensure!(f.start(plan.ticket, "survive host restart")? == result);
    Ok(())
}

#[test]
fn lost_publication_proof_stays_unknown_across_restart_and_cannot_be_repeated() -> Result<()> {
    use pigade_session_host::GitCommitPhase;
    let mut f = CommitFixture::new()?;
    f.hook("pre-commit", "printf x >> .git/fault-hook-calls\ntouch .git/hook-started\ni=0\nwhile ! test -f .git/hook-release; do\n i=$((i+1)); test \"$i\" -lt 200 || exit 1\n sleep 0.05\ndone")?;
    let plan = f.prepare()?;
    ensure!(f.start(plan.ticket, "prepared fault")?.phase == GitCommitPhase::Running);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f.workspace.cwd.join(".git/hook-started").exists() {
        ensure!(
            Instant::now() < deadline,
            "fault fixture pre-commit did not start"
        );
        thread::sleep(Duration::from_millis(10));
    }
    // Prevent the committed hook from recording publication after the real guard
    // durably publishes its prepared proof. Only this private fixture
    // operation is altered; the application has no production failpoint flag.
    let operation = f
        .host
        .state()
        .join("git-commit-operations")
        .join(plan.ticket.to_string());
    let wrapper = operation.join("hooks/reference-transaction");
    let original = fs::read_to_string(&wrapper)?;
    ensure!(original.contains("exec /proc/self/fd/"));
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif test \"$1\" = committed; then exit 1; fi\n{}",
            original
                .strip_prefix("#!/bin/sh\n")
                .context("wrapper shebang missing")?
        ),
    )?;
    fs::write(f.workspace.cwd.join(".git/hook-release"), b"release")?;
    let result = f.finish(plan.ticket)?;
    ensure!(
        result.phase == GitCommitPhase::Unknown && !result.acknowledged,
        "fault fabricated a known outcome: {:?}",
        result
    );
    ensure!(
        operation.join("prepared.json").is_file() && !operation.join("committed.json").exists()
    );
    ensure!(fs::read(f.workspace.cwd.join(".git/fault-hook-calls"))? == b"x");
    f.host.restart()?;
    ensure!(f.inspect(plan.ticket)?.phase == GitCommitPhase::Unknown);
    ensure!(f.start(plan.ticket, "prepared fault")?.phase == GitCommitPhase::Unknown);
    ensure!(fs::read(f.workspace.cwd.join(".git/fault-hook-calls"))? == b"x");
    let ack = exchange(
        &f.host.socket(),
        Command::WorkspaceGitCommitAcknowledge {
            workspace: f.workspace.id,
            expected_revision: f.workspace.revision,
            ticket: plan.ticket,
        },
    )?
    .workspace_git_commit
    .context("acknowledgement missing")?;
    ensure!(ack.phase == GitCommitPhase::Unknown && ack.acknowledged);
    ensure!(f.start(plan.ticket, "prepared fault")?.acknowledged);
    ensure!(fs::read(f.workspace.cwd.join(".git/fault-hook-calls"))? == b"x");
    // JSON-shaped corruption must not be trusted as proof of publication.
    let receipt_path = operation.join("receipt.json");
    let mut receipt: serde_json::Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
    let proof: serde_json::Value =
        serde_json::from_slice(&fs::read(operation.join("prepared.json"))?)?;
    ensure!(
        String::from_utf8(f.git(&["rev-parse", "HEAD"])?)?.trim()
            == proof["commit"].as_str().context("prepared OID missing")?
    );
    receipt["result"]["phase"] = "applied".into();
    receipt["result"]["commit"] = proof["commit"].clone();
    fs::write(&receipt_path, serde_json::to_vec(&receipt)?)?;
    ensure!(
        f.inspect(plan.ticket)?.phase == GitCommitPhase::Unavailable,
        "forged Applied receipt was accepted without committed proof"
    );
    Ok(())
}

#[test]
fn workspace_git_watch_events_are_scoped_read_only_and_reopen_with_a_new_epoch() -> Result<()> {
    let mut host = HostFixture::new()?;
    let parent = host.state().join("watch-parent");
    fs::create_dir(&parent)?;
    let created = exchange(
        &host.socket(),
        Command::CreateProject {
            parent,
            name: "WatchRepo".into(),
        },
    )?
    .catalog
    .context("missing project catalog")?;
    let project = &created.projects[0];
    let catalog = exchange(
        &host.socket(),
        Command::CreateWorkspace {
            project: project.id,
            task: "watch fixture".into(),
            worktree: false,
        },
    )?
    .catalog
    .context("missing workspace catalog")?;
    let workspace = catalog
        .workspaces
        .iter()
        .find(|ws| ws.project_id == project.id)
        .context("missing Ready workspace")?
        .clone();
    let command = || Command::WorkspaceGitWatch {
        workspace: workspace.id,
        expected_revision: workspace.revision,
    };
    let wait_watch = |host: &HostFixture,
                      after: Option<(uuid::Uuid, u64)>|
     -> Result<pigade_session_host::WorkspaceGitWatch> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let watch = exchange(&host.socket(), command())?
                .workspace_git_watch
                .context("watch response missing")?;
            ensure!(
                watch.workspace_id == workspace.id
                    && watch.project_id == project.id
                    && watch.workspace_revision == workspace.revision
                    && watch.cwd == workspace.cwd,
                "watch response belongs to another workspace"
            );
            if watch.complete
                && watch.settled
                && after.is_none_or(|(epoch, sequence)| {
                    watch.epoch == epoch && watch.sequence > sequence
                })
            {
                return Ok(watch);
            }
            ensure!(Instant::now() < deadline, "watch did not settle: {watch:?}");
            thread::sleep(Duration::from_millis(40));
        }
    };
    let before = wait_watch(&host, None)?;
    let index = fs::read(workspace.cwd.join(".git/index")).ok();
    let config = fs::read(workspace.cwd.join(".git/config"))?;
    ensure!(
        exchange(
            &host.socket(),
            Command::WorkspaceGitWatch {
                workspace: workspace.id,
                expected_revision: workspace.revision + 1,
            }
        )
        .is_err(),
        "stale watch revision accepted"
    );
    let path = workspace.cwd.join("native watch.txt");
    fs::write(&path, b"watched exact bytes\n")?;
    let added = wait_watch(&host, Some((before.epoch, before.sequence)))?;
    let snapshot = exchange(
        &host.socket(),
        Command::WorkspaceGitSnapshot {
            workspace: workspace.id,
            expected_revision: workspace.revision,
        },
    )?
    .workspace_git_snapshot
    .context("missing status")?;
    ensure!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.path == "native watch.txt" && entry.untracked)
    );
    ensure!(fs::read(&path)? == b"watched exact bytes\n");
    fs::remove_file(&path)?;
    let removed = wait_watch(&host, Some((added.epoch, added.sequence)))?;
    ensure!(removed.epoch == before.epoch);
    ensure!(
        fs::read(workspace.cwd.join(".git/index")).ok() == index
            && fs::read(workspace.cwd.join(".git/config"))? == config,
        "watch mutated Git metadata"
    );
    ensure!(
        exchange(&host.socket(), Command::List)?.views.is_empty(),
        "watch launched a CLI"
    );
    ensure!(
        exchange(&host.socket(), Command::Catalog)?.catalog == Some(catalog.clone()),
        "watch changed catalog, selection or layout"
    );
    host.restart()?;
    let reopened = wait_watch(&host, None)?;
    ensure!(
        reopened.epoch != before.epoch,
        "host reopen reused an old watcher epoch"
    );
    ensure!(exchange(&host.socket(), Command::Catalog)?.catalog == Some(catalog));
    Ok(())
}
