//! Real-host test for immutable reviewed context snapshots across launch/reopen.
use anyhow::{Context, Result, bail, ensure};
use pigade_layout::{Direction, TabLayout};
use pigade_session_host::{
    Command, PROTOCOL, PrepareRequest, Request, Response, View, receive, send,
};
use pigade_storage::{
    CatalogBatch, CatalogWrite, ContextDocument, Project, Tab, Workspace, WorkspaceMode,
    WorkspaceStatus,
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
use uuid::Uuid;

const STARTUP_LIMIT: Duration = Duration::from_secs(8);

fn exchange(socket: &Path, command: Command) -> Result<Response> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
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

fn success(socket: &Path, command: Command) -> Result<Response> {
    let response = exchange(socket, command)?;
    if let Some(error) = response.error.as_deref() {
        bail!("host command failed: {error}");
    }
    Ok(response)
}

struct TestHost {
    directory: TempDir,
    bin: TempDir,
    child: Option<Child>,
    active: Option<(Uuid, Uuid)>,
}

impl TestHost {
    fn new() -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let bin = tempfile::tempdir()?;
        let codex = bin.path().join("codex");
        fs::write(
            &codex,
            concat!(
                "#!/usr/bin/python3\n",
                "import json,os,sys\n",
                "if sys.argv[1:] == ['--version']:\n",
                " print('codex-cli 0.162.1'); raise SystemExit(0)\n",
                "safe_env={k:os.environ[k] for k in ('TERM','COLORTERM') if k in os.environ}\n",
                "print('FIXTURE_ARGS='+json.dumps(sys.argv[1:],ensure_ascii=False))\n",
                "print('FIXTURE_ENV='+json.dumps(safe_env,ensure_ascii=False))\n",
            ),
        )?;
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o755))?;
        let mut host = Self {
            directory,
            bin,
            child: None,
            active: None,
        };
        host.start()?;
        Ok(host)
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn socket(&self) -> PathBuf {
        self.root().join("host.sock")
    }

    fn start(&mut self) -> Result<()> {
        let path =
            std::env::join_paths([self.bin.path(), Path::new("/usr/bin"), Path::new("/bin")])?;
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"));
        child
            .arg("serve")
            .arg(self.root())
            .env("PATH", path)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.root().join("host.log"))?);
        self.child = Some(child.spawn()?);
        let deadline = Instant::now() + STARTUP_LIMIT;
        loop {
            if let Some(status) = self
                .child
                .as_mut()
                .context("host process missing")?
                .try_wait()?
            {
                bail!(
                    "host exited {status}: {}",
                    fs::read_to_string(self.root().join("host.log"))?
                );
            }
            if success(&self.socket(), Command::List).is_ok() {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "host did not expose its socket");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn stop_host(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            child.kill()?;
            child.wait()?;
        }
        Ok(())
    }

    fn restart(&mut self) -> Result<()> {
        self.stop_host()?;
        self.start()
    }

    fn wait_for(&self, predicate: impl Fn(&[View]) -> bool) -> Result<Vec<View>> {
        let deadline = Instant::now() + STARTUP_LIMIT;
        loop {
            let views = success(&self.socket(), Command::List)?.views;
            if predicate(&views) {
                return Ok(views);
            }
            ensure!(Instant::now() < deadline, "fixture PTY did not finish");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for TestHost {
    fn drop(&mut self) {
        if let Some((session, generation)) = self.active {
            let _ = exchange(
                &self.socket(),
                Command::Stop {
                    session,
                    generation,
                },
            );
        }
        let _ = self.stop_host();
    }
}

fn context(
    scope_id: Uuid,
    revision: u64,
    instructions: &str,
    role: &str,
    memory: &str,
) -> ContextDocument {
    ContextDocument {
        scope_id,
        revision,
        instructions: instructions.to_owned(),
        role: role.to_owned(),
        memory: memory.to_owned(),
        content_hash: String::new(),
        saved_at_unix_ms: None,
        author: String::new(),
    }
}

#[test]
fn reviewed_context_is_immutable_during_launch_and_survives_host_reopen() -> Result<()> {
    let mut host = TestHost::new()?;
    let root = host.root().canonicalize()?;
    let project = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    let tab = Uuid::new_v4();
    let response = success(
        &host.socket(),
        Command::CatalogApply {
            batch: CatalogBatch {
                projects: vec![CatalogWrite {
                    value: Project {
                        id: project,
                        canonical_path: root.clone(),
                        git_common_dir: root.clone(),
                        name: "Prepared context fixture".into(),
                        roots: vec![],
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                workspaces: vec![CatalogWrite {
                    value: Workspace {
                        id: workspace,
                        project_id: project,
                        task: "integration task".into(),
                        cwd: root.clone(),
                        mode: WorkspaceMode::SharedCheckout,
                        status: WorkspaceStatus::Ready,
                        error: None,
                        context_revision: None,
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                tabs: vec![CatalogWrite {
                    value: Tab {
                        id: tab,
                        workspace_id: workspace,
                        name: "Fixture".into(),
                        layout_json: serde_json::to_string(&TabLayout::default())?,
                        active_pane: None,
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            },
        },
    )?;
    let catalog = response.catalog.context("catalog fixture was not saved")?;
    let offset = pigade_storage::CanvasOffset { x: 120.0, y: 75.0 };
    let viewport_reply = success(&host.socket(), Command::SetCanvasOffset { tab, offset })?;
    ensure!(
        viewport_reply.catalog.is_none() && viewport_reply.views.is_empty(),
        "saving a viewport must not resend the catalog or terminal cells"
    );
    let selection = viewport_reply
        .selection
        .context("viewport ACK omitted selection revision")?;
    ensure!(
        selection.tab_id == catalog.selection.tab_id
            && selection.window.canvas_offsets.get(&tab) == Some(&offset)
    );
    for value in [
        context(project, 0, "OLD_PROJECT_CONTEXT", "", ""),
        context(
            workspace,
            0,
            "OLD_WORKSPACE_CONTEXT",
            "implementer",
            "MEMORY_SHOULD_NOT_BE_DELIVERED",
        ),
    ] {
        success(
            &host.socket(),
            Command::SaveContext {
                document: value,
                expected_revision: 0,
            },
        )?;
    }

    let prepared = success(
        &host.socket(),
        Command::PrepareLaunch {
            request: PrepareRequest {
                tab,
                provider: "codex".into(),
                side: Direction::Right,
                target: None,
                role_override: "tester".into(),
                agent_instructions: "AGENT_INSTRUCTIONS π".into(),
                include_memory: false,
                deliver_context: true,
            },
        },
    )?;
    ensure!(
        prepared.views.is_empty(),
        "preparation must not launch a PTY child"
    );
    let preview = prepared
        .launch_preview
        .context("prepared launch omitted its preview")?;
    let plan_id = preview.plan_id;
    let old_hash = preview.context.content_hash.clone();
    ensure!(
        !preview
            .context
            .text
            .contains("MEMORY_SHOULD_NOT_BE_DELIVERED"),
        "memory was included without explicit selection"
    );
    ensure!(
        preview.context.text.contains("OLD_PROJECT_CONTEXT")
            && preview.context.text.contains("OLD_WORKSPACE_CONTEXT")
            && preview.context.text.contains("Role: validate behavior")
            && preview.context.text.contains("AGENT_INSTRUCTIONS π"),
        "preview omitted an expected ordered context component"
    );
    ensure!(
        success(&host.socket(), Command::List)?.views.is_empty(),
        "PrepareLaunch unexpectedly created a session"
    );

    success(
        &host.socket(),
        Command::SaveContext {
            document: context(
                workspace,
                1,
                "NEW_WORKSPACE_CONTEXT_MUST_NOT_REPLACE_REVIEW",
                "implementer",
                "NEW_MEMORY_MUST_NOT_BE_DELIVERED",
            ),
            expected_revision: 1,
        },
    )?;
    host.active = Some((preview.session, preview.generation));
    let started = success(&host.socket(), Command::StartPrepared { plan_id })?;
    ensure!(started.launch_phase.as_deref() == Some("delivered_unconfirmed"));
    let repeated = exchange(&host.socket(), Command::StartPrepared { plan_id })?;
    ensure!(
        repeated.error.is_some(),
        "a prepared plan must not be started twice"
    );

    let views = host.wait_for(|views| {
        views.iter().any(|view| {
            view.session == preview.session
                && view.generation == preview.generation
                && view.state == "exited"
                && view.frame.text().contains("FIXTURE_ARGS")
        })
    })?;
    ensure!(views.len() == 1, "fixture created more than one generation");
    let output = views[0].frame.text();
    for expected in [
        "OLD_PROJECT_CONTEXT",
        "OLD_WORKSPACE_CONTEXT",
        "AGENT_INSTRUCTIONS π",
        "Role: validate behavior",
    ] {
        ensure!(output.contains(expected), "child argv omitted {expected:?}");
    }
    ensure!(
        !output.contains("NEW_WORKSPACE_CONTEXT_MUST_NOT_REPLACE_REVIEW")
            && !output.contains("MEMORY_SHOULD_NOT_BE_DELIVERED")
            && !output.contains("NEW_MEMORY_MUST_NOT_BE_DELIVERED"),
        "child argv included changed or unselected context"
    );
    ensure!(
        output.contains("FIXTURE_ENV={\"TERM\": \"xterm-256color\", \"COLORTERM\": \"truecolor\"}"),
        "child did not receive the expected terminal environment"
    );
    ensure!(
        views
            .iter()
            .filter(|view| view.session == preview.session)
            .count()
            == 1,
        "repeat start created a duplicate generation"
    );

    let recovered_context = success(
        &host.socket(),
        Command::GetSessionContext {
            session: preview.session,
            generation: preview.generation,
        },
    )?;
    let stored_preview = recovered_context
        .launch_preview
        .context("session context lookup omitted the reviewed snapshot")?;
    ensure!(stored_preview.context.content_hash == old_hash);
    ensure!(recovered_context.launch_phase.as_deref() == Some("delivered_unconfirmed"));

    host.restart()?;
    let reopened_catalog = success(&host.socket(), Command::Catalog)?
        .catalog
        .context("reopened catalog missing")?;
    ensure!(
        reopened_catalog.selection.window.canvas_offsets.get(&tab) == Some(&offset),
        "host reopen lost the saved canvas position"
    );
    let after_restart = success(
        &host.socket(),
        Command::GetSessionContext {
            session: preview.session,
            generation: preview.generation,
        },
    )?;
    let reopened_preview = after_restart
        .launch_preview
        .context("reopened host lost its reviewed context snapshot")?;
    ensure!(reopened_preview.context.content_hash == old_hash);
    ensure!(after_restart.launch_phase.as_deref() == Some("delivered_unconfirmed"));
    let history = success(&host.socket(), Command::List)?.views;
    ensure!(history.len() == 1, "reopen created or lost a generation");
    ensure!(history[0].generation == preview.generation);
    ensure!(
        history[0].pid.is_none(),
        "reopen respawned the historical PTY"
    );
    Ok(())
}
