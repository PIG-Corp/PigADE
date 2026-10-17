//! Real-host test for immutable reviewed context snapshots across launch/reopen.
use anyhow::{Context, Result, bail, ensure};
use pigade_layout::{Direction, TabLayout};
use pigade_session_host::{
    Command, Launch, PROTOCOL, PrepareRequest, Request, Response, View, receive, send,
};
use pigade_storage::{
    CatalogBatch, CatalogWrite, ContextDocument, MemorySelection, Project, Tab, Workspace,
    WorkspaceMode, WorkspaceStatus,
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
                "import json,os,sys,zlib,time\n",
                "if sys.argv[1:] == ['--version']:\n",
                " print('codex-cli 0.162.1'); raise SystemExit(0)\n",
                "if sys.argv[1:2] == ['resume']:\n",
                " safe_env={k:os.environ[k] for k in ('TERM','COLORTERM') if k in os.environ}\n",
                " reported=['<developer_instructions>' if arg.startswith('developer_instructions=') else arg for arg in sys.argv[1:]]\n",
                " print('FIXTURE_RESUME_ARGS='+json.dumps(reported,ensure_ascii=False))\n",
                " print('FIXTURE_RESUME_CWD='+os.getcwd())\n",
                " config=next((arg.split('=',1)[1] for arg in sys.argv[1:] if arg.startswith('developer_instructions=')),None)\n",
                " if config is not None:\n",
                "  text=json.loads(config)\n",
                "  print('FIXTURE_CONTEXT_CRC32='+str(zlib.crc32(text.encode('utf-8'))))\n",
                " print('FIXTURE_ENV='+json.dumps(safe_env,ensure_ascii=False))\n",
                " deadline=time.monotonic()+8\n",
                " while os.path.exists('hold-resume') and time.monotonic()<deadline: time.sleep(0.01)\n",
                " raise SystemExit(0)\n",
                "safe_env={k:os.environ[k] for k in ('TERM','COLORTERM') if k in os.environ}\n",
                "print('FIXTURE_ARGS='+json.dumps(sys.argv[1:],ensure_ascii=False))\n",
                "text=json.loads(sys.argv[2].split('=',1)[1])\n",
                "print('FIXTURE_CONTEXT_CRC32='+str(zlib.crc32(text.encode('utf-8'))))\n",
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
            &format!(
                "# Факты\nMEMORY_SHOULD_NOT_BE_DELIVERED{}\n## Решения\nSELECTED_MEMORY λ決定\n## Что осталось\nLATER_MEMORY_SHOULD_NOT_BE_DELIVERED\n",
                "x".repeat(40 * 1024)
            ),
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
                memory_selection: None,
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
    ensure!(
        !preview
            .context
            .text
            .contains("MEMORY_SHOULD_NOT_BE_DELIVERED"),
        "memory was included without explicit selection"
    );
    let memory = preview
        .memory
        .as_ref()
        .context("memory section review missing")?;
    ensure!(memory.sections.len() == 3 && memory.sections[0].bytes > 32 * 1024);
    ensure!(memory.selected.is_empty());
    let memory_selection = MemorySelection {
        revision: memory.revision,
        content_hash: memory.content_hash.clone(),
        sections: vec![1],
    };
    let preview = success(
        &host.socket(),
        Command::PrepareLaunch {
            request: PrepareRequest {
                tab,
                provider: "codex".into(),
                side: Direction::Right,
                target: None,
                role_override: "tester".into(),
                agent_instructions: "AGENT_INSTRUCTIONS π".into(),
                include_memory: true,
                memory_selection: Some(memory_selection.clone()),
                deliver_context: true,
            },
        },
    )?
    .launch_preview
    .context("selected-memory preview missing")?;
    ensure!(
        preview
            .context
            .blocks
            .iter()
            .find(|block| block.source == "workspace_memory")
            .context("selected memory missing")?
            .text
            == "## Решения\nSELECTED_MEMORY λ決定\n"
    );
    ensure!(
        !preview
            .context
            .text
            .contains("MEMORY_SHOULD_NOT_BE_DELIVERED")
    );
    let plan_id = preview.plan_id;
    let old_hash = preview.context.content_hash.clone();
    let conversation_id = Uuid::new_v4().to_string();
    ensure!(
        exchange(
            &host.socket(),
            Command::BindConversation {
                session: preview.session,
                generation: preview.generation,
                conversation_id: conversation_id.clone(),
                expected_revision: 0,
            }
        )?
        .error
        .is_some(),
        "unlaunched preview accepted a conversation binding"
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
    let stale = exchange(
        &host.socket(),
        Command::PrepareLaunch {
            request: PrepareRequest {
                tab,
                provider: "codex".into(),
                side: Direction::Right,
                target: None,
                role_override: "tester".into(),
                agent_instructions: "".into(),
                include_memory: true,
                memory_selection: Some(memory_selection),
                deliver_context: true,
            },
        },
    )?;
    ensure!(
        stale
            .error
            .as_deref()
            .is_some_and(|error| error.contains("stale")),
        "changed memory revision accepted a stale selection"
    );
    ensure!(success(&host.socket(), Command::List)?.views.is_empty());
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
        "SELECTED_MEMORY",
    ] {
        ensure!(output.contains(expected), "child argv omitted {expected:?}");
    }
    // Cell text contains wide-character spacer cells; compare the actual UTF-8
    // argument checksum reported by the fixture rather than flattening those cells.
    ensure!(
        output.contains(&format!(
            "FIXTURE_CONTEXT_CRC32={}",
            crc32fast::hash(preview.context.text.as_bytes())
        )),
        "child received different UTF-8 context bytes"
    );
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

    let bound = success(
        &host.socket(),
        Command::BindConversation {
            session: preview.session,
            generation: preview.generation,
            conversation_id: conversation_id.clone(),
            expected_revision: 0,
        },
    )?
    .launch_preview
    .context("binding ACK omitted its preview")?;
    let binding = bound
        .conversation
        .context("binding ACK omitted saved binding")?;
    ensure!(binding.revision == 1 && binding.provider == "codex" && binding.author == "user");
    ensure!(
        binding.session_id == preview.session
            && binding.source_generation == preview.generation
            && binding.plan_id == preview.plan_id
            && binding.context_revision == preview.context.id
            && binding.conversation_id == conversation_id
    );
    ensure!(bound.context.content_hash == old_hash && bound.cwd.as_ref() == Some(&root));
    ensure!(
        exchange(
            &host.socket(),
            Command::BindConversation {
                session: preview.session,
                generation: preview.generation,
                conversation_id: Uuid::new_v4().to_string(),
                expected_revision: 0,
            }
        )?
        .error
        .is_some(),
        "stale binding save overwrote the head"
    );
    ensure!(
        exchange(
            &host.socket(),
            Command::BindConversation {
                session: preview.session,
                generation: preview.generation,
                conversation_id: "--last".into(),
                expected_revision: 1,
            }
        )?
        .error
        .is_some(),
        "non-UUID alias was accepted as exact Codex conversation"
    );
    ensure!(
        success(&host.socket(), Command::List)?.views.len() == 1,
        "binding spawned an extra process"
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
    ensure!(stored_preview.conversation == Some(binding.clone()));
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
    ensure!(reopened_preview.conversation == Some(binding));
    ensure!(reopened_preview.cwd.as_ref() == Some(&root));
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

#[test]
fn prepared_resume_reuses_frozen_context_preserves_history_and_rejects_stale_binding() -> Result<()>
{
    let mut host = TestHost::new()?;
    let root = host.root().canonicalize()?;
    let project = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    let tab = Uuid::new_v4();
    success(
        &host.socket(),
        Command::CatalogApply {
            batch: CatalogBatch {
                projects: vec![CatalogWrite {
                    value: Project {
                        id: project,
                        canonical_path: root.clone(),
                        git_common_dir: root.clone(),
                        name: "Resume fixture".into(),
                        roots: vec![],
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                workspaces: vec![CatalogWrite {
                    value: Workspace {
                        id: workspace,
                        project_id: project,
                        task: "initial task must not be repeated".into(),
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
                        name: "Resume".into(),
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
    let old_project_text = "FROZEN_PROJECT_CONTEXT";
    let old_workspace_text = "FROZEN_WORKSPACE_CONTEXT";
    for document in [
        context(project, 0, old_project_text, "", ""),
        context(workspace, 0, old_workspace_text, "", ""),
    ] {
        success(
            &host.socket(),
            Command::SaveContext {
                document,
                expected_revision: 0,
            },
        )?;
    }

    let initial = success(
        &host.socket(),
        Command::PrepareLaunch {
            request: PrepareRequest {
                tab,
                provider: "codex".into(),
                side: Direction::Right,
                target: None,
                role_override: "".into(),
                agent_instructions: "FROZEN_AGENT_INSTRUCTIONS".into(),
                include_memory: false,
                memory_selection: None,
                deliver_context: true,
            },
        },
    )?
    .launch_preview
    .context("initial launch preview missing")?;
    success(
        &host.socket(),
        Command::StartPrepared {
            plan_id: initial.plan_id,
        },
    )?;
    let initial_views = host.wait_for(|views| {
        views.iter().any(|view| {
            view.session == initial.session
                && view.generation == initial.generation
                && view.state == "exited"
        })
    })?;
    ensure!(initial_views.len() == 1);
    let old_frame = initial_views[0].frame.text();
    let old_context_hash = initial.context.content_hash.clone();
    ensure!(old_frame.contains("FIXTURE_CONTEXT_CRC32="));

    let conversation_id = "01234567-89ab-cdef-0123-456789abcdef";
    let binding = success(
        &host.socket(),
        Command::BindConversation {
            session: initial.session,
            generation: initial.generation,
            conversation_id: conversation_id.into(),
            expected_revision: 0,
        },
    )?
    .launch_preview
    .context("conversation binding preview missing")?
    .conversation
    .context("saved conversation binding missing")?;
    ensure!(binding.revision == 1);

    for document in [
        context(project, 1, "EDITED_PROJECT_CONTEXT", "", ""),
        context(workspace, 1, "EDITED_WORKSPACE_CONTEXT", "", ""),
    ] {
        success(
            &host.socket(),
            Command::SaveContext {
                document,
                expected_revision: 1,
            },
        )?;
    }
    success(
        &host.socket(),
        Command::SelectGeneration {
            session: initial.session,
            generation: Some(initial.generation),
        },
    )?;
    let before = success(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing before resume")?;
    let prepared = success(
        &host.socket(),
        Command::PrepareResume {
            session: initial.session,
        },
    )?
    .launch_preview
    .context("resume preparation omitted preview")?;
    let resume_source = prepared
        .resume
        .as_ref()
        .context("resume preparation omitted source binding")?;
    ensure!(resume_source.binding == binding);
    ensure!(resume_source.previous_head == initial.generation);
    ensure!(prepared.session == initial.session && prepared.generation != initial.generation);
    ensure!(prepared.context.content_hash == old_context_hash);
    ensure!(prepared.context.text.contains(old_project_text));
    ensure!(prepared.context.text.contains(old_workspace_text));
    ensure!(prepared.context.text.contains("FROZEN_AGENT_INSTRUCTIONS"));
    ensure!(!prepared.context.text.contains("EDITED_PROJECT_CONTEXT"));
    ensure!(!prepared.context.text.contains("EDITED_WORKSPACE_CONTEXT"));
    ensure!(prepared.cwd.as_ref() == Some(&root));
    let pinned = success(
        &host.socket(),
        Command::Generations {
            session: initial.session,
            before: None,
        },
    )?
    .generations
    .context("generation history missing while resume is prepared")?;
    ensure!(pinned.head == Some(initial.generation));
    ensure!(pinned.pinned == Some(initial.generation));
    let before_start = success(&host.socket(), Command::List)?.views;
    ensure!(before_start.len() == 1 && before_start[0].frame.text() == old_frame);

    fs::write(root.join("hold-resume"), b"hold")?;
    host.active = Some((prepared.session, prepared.generation));
    success(
        &host.socket(),
        Command::StartPrepared {
            plan_id: prepared.plan_id,
        },
    )?;
    host.wait_for(|views| {
        views.iter().any(|view| {
            view.generation == prepared.generation
                && view.state == "running"
                && view.frame.text().contains("FIXTURE_RESUME_ARGS=")
        })
    })?;
    ensure!(
        exchange(
            &host.socket(),
            Command::BindConversation {
                session: initial.session,
                generation: prepared.generation,
                conversation_id: "89abcdef-0123-4567-89ab-cdef01234567".into(),
                expected_revision: 1,
            }
        )?
        .error
        .is_some(),
        "live resumed dialogue released its provider-ID reservation"
    );
    fs::remove_file(root.join("hold-resume"))?;
    ensure!(
        exchange(
            &host.socket(),
            Command::StartPrepared {
                plan_id: prepared.plan_id,
            }
        )?
        .error
        .is_some(),
        "resume plan was admitted more than once"
    );
    let resumed_views = host.wait_for(|views| {
        views.iter().any(|view| {
            view.session == prepared.session
                && view.generation == prepared.generation
                && view.state == "exited"
                && view.frame.text().contains("FIXTURE_RESUME_ARGS=")
        })
    })?;
    ensure!(
        resumed_views.len() == 2,
        "resume should add exactly one generation"
    );
    let old_view = resumed_views
        .iter()
        .find(|view| view.generation == initial.generation)
        .context("original generation disappeared")?;
    ensure!(old_view.frame.text() == old_frame);
    let resumed_view = resumed_views
        .iter()
        .find(|view| view.generation == prepared.generation)
        .context("resumed generation missing")?;
    // The CLI's JSON may wrap across physical rows. Join the displayed cells
    // before decoding its first value; the following markers are separate data.
    let resume_frame: String = resumed_view
        .frame
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    let resume_args_json = resume_frame
        .split_once("FIXTURE_RESUME_ARGS=")
        .map(|(_, value)| value)
        .context("fixture did not print resume argv")?;
    let resume_args = serde_json::Deserializer::from_str(resume_args_json)
        .into_iter::<Vec<String>>()
        .next()
        .context("resume args JSON missing")??;
    ensure!(
        resume_args
            == vec![
                "resume".to_owned(),
                "--no-daemon".to_owned(),
                "--cd".to_owned(),
                root.to_str()
                    .context("fixture cwd is not UTF-8")?
                    .to_owned(),
                "-c".to_owned(),
                "<developer_instructions>".to_owned(),
                conversation_id.to_owned(),
            ],
        "resume did not receive the reviewed exact argv"
    );
    let resumed_text = resumed_view.frame.text();
    ensure!(
        resumed_text.contains(&format!("FIXTURE_RESUME_CWD={}", root.display())),
        "resume CLI ran in a different working directory"
    );
    ensure!(
        resumed_text.contains(&format!(
            "FIXTURE_CONTEXT_CRC32={}",
            crc32fast::hash(prepared.context.text.as_bytes())
        )),
        "resumed CLI received different frozen context bytes"
    );
    ensure!(!resumed_text.contains("initial task must not be repeated"));
    let after = success(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing after resume")?;
    ensure!(after.layout.revision == before.layout.revision);
    ensure!(after.layout.json == before.layout.json);
    let generations = success(
        &host.socket(),
        Command::Generations {
            session: initial.session,
            before: None,
        },
    )?
    .generations
    .context("generation history missing after resume")?;
    ensure!(generations.head == Some(prepared.generation));
    ensure!(
        generations.pinned.is_none(),
        "resume did not clear the historical pin"
    );

    let stale_prepared = success(
        &host.socket(),
        Command::PrepareResume {
            session: initial.session,
        },
    )?
    .launch_preview
    .context("second resume preview missing")?;
    success(
        &host.socket(),
        Command::BindConversation {
            session: initial.session,
            generation: prepared.generation,
            conversation_id: "89abcdef-0123-4567-89ab-cdef01234567".into(),
            expected_revision: 1,
        },
    )?;
    ensure!(
        exchange(
            &host.socket(),
            Command::StartPrepared {
                plan_id: stale_prepared.plan_id,
            }
        )?
        .error
        .is_some(),
        "resume accepted a binding that changed after review"
    );
    ensure!(success(&host.socket(), Command::List)?.views.len() == 2);

    let changed_binary_plan = success(
        &host.socket(),
        Command::PrepareResume {
            session: initial.session,
        },
    )?
    .launch_preview
    .context("resume review for binary guard missing")?;
    let executable = host.bin.path().join("codex");
    let mut changed_binary = fs::read(&executable)?;
    changed_binary.push(b'\n');
    fs::write(&executable, changed_binary)?;
    let refused = exchange(
        &host.socket(),
        Command::StartPrepared {
            plan_id: changed_binary_plan.plan_id,
        },
    )?;
    ensure!(
        refused.error.is_some()
            && matches!(
                refused.rejected_before_admission,
                Some(pigade_session_host::CommandRejection::LaunchPreflight)
            ),
        "changed executable was not refused before resume admission"
    );
    ensure!(success(&host.socket(), Command::List)?.views.len() == 2);
    Ok(())
}

#[test]
fn panel_generation_selection_uses_ordinal_head_and_persists_history_pin() -> Result<()> {
    let mut host = TestHost::new()?;
    let session = Uuid::new_v4();
    let first_generation = Uuid::from_u128(2);
    let second_generation = Uuid::from_u128(1);

    for (generation, operation, output) in [
        (first_generation, Uuid::new_v4(), "FIRST_GENERATION"),
        (second_generation, Uuid::new_v4(), "SECOND_GENERATION"),
    ] {
        success(
            &host.socket(),
            Command::Launch {
                spec: Launch {
                    session,
                    generation,
                    operation,
                    executable: PathBuf::from("/bin/sh"),
                    args: vec![
                        "-c".into(),
                        format!("printf '\\033[31m{output}\\033[0m\\n'"),
                    ],
                    env: Default::default(),
                    cwd: host.root().to_path_buf(),
                    geometry: pigade_terminal::Geometry::new(12, 48)?,
                },
            },
        )?;
        host.wait_for(|views| {
            views.iter().any(|view| {
                view.session == session
                    && view.generation == generation
                    && view.state == "exited"
                    && view.frame.text().contains(output)
            })
        })?;
    }

    let listed = success(
        &host.socket(),
        Command::Generations {
            session,
            before: None,
        },
    )?
    .generations
    .context("generation response omitted history")?;
    ensure!(listed.head == Some(second_generation));
    ensure!(listed.entries.len() == 2);
    ensure!(listed.entries[0].generation == second_generation);
    ensure!(listed.entries[0].ordinal > listed.entries[1].ordinal);
    ensure!(listed.entries[1].generation == first_generation);
    ensure!(first_generation.as_u128() > second_generation.as_u128());

    let follow_head = success(
        &host.socket(),
        Command::PollPanels {
            panels: vec![pigade_session_host::poll::PanelTarget {
                session,
                generation: None,
            }],
            known: vec![pigade_session_host::poll::Known {
                session,
                generation: first_generation,
                sequence: u64::MAX,
                state: "running".into(),
                pid: Some(u32::MAX),
                storage_error: None,
            }],
        },
    )?;
    let selected = follow_head
        .selected
        .context("panel poll omitted selected metadata")?;
    ensure!(selected.len() == 1 && selected[0].generation == second_generation);
    ensure!(follow_head.views.len() == 1);
    ensure!(follow_head.views[0].generation == second_generation);
    ensure!(
        follow_head.views[0]
            .frame
            .text()
            .contains("SECOND_GENERATION")
    );
    ensure!(
        !follow_head.views[0]
            .frame
            .text()
            .contains("FIRST_GENERATION")
    );

    success(
        &host.socket(),
        Command::SelectGeneration {
            session,
            generation: Some(first_generation),
        },
    )?;
    let pinned = success(
        &host.socket(),
        Command::Generations {
            session,
            before: None,
        },
    )?
    .generations
    .context("pinned generation response omitted history")?;
    ensure!(pinned.head == Some(second_generation));
    ensure!(pinned.pinned == Some(first_generation));

    let old_poll = success(
        &host.socket(),
        Command::PollPanels {
            panels: vec![pigade_session_host::poll::PanelTarget {
                session,
                generation: Some(first_generation),
            }],
            known: vec![],
        },
    )?;
    ensure!(old_poll.views.len() == 1);
    ensure!(old_poll.views[0].generation == first_generation);
    let before_input = success(
        &host.socket(),
        Command::Snapshot {
            session,
            generation: first_generation,
        },
    )?
    .views
    .into_iter()
    .next()
    .context("pinned generation snapshot missing")?;
    let rejected_input = exchange(
        &host.socket(),
        Command::Input {
            session,
            generation: first_generation,
            bytes: b"must-not-write".to_vec(),
        },
    )?;
    ensure!(
        rejected_input
            .error
            .as_deref()
            .is_some_and(|error| error.contains("read-only")),
        "historical generation accepted input"
    );
    let after_input = success(
        &host.socket(),
        Command::Snapshot {
            session,
            generation: first_generation,
        },
    )?
    .views
    .into_iter()
    .next()
    .context("pinned generation snapshot missing after rejected input")?;
    ensure!(before_input.frame == after_input.frame);

    success(
        &host.socket(),
        Command::SelectGeneration {
            session,
            generation: None,
        },
    )?;
    host.restart()?;
    let restarted = success(
        &host.socket(),
        Command::Generations {
            session,
            before: None,
        },
    )?
    .generations
    .context("restarted generation response omitted history")?;
    ensure!(restarted.head == Some(second_generation));
    ensure!(restarted.pinned.is_none());

    success(
        &host.socket(),
        Command::SelectGeneration {
            session,
            generation: Some(first_generation),
        },
    )?;
    let unknown = exchange(
        &host.socket(),
        Command::SelectGeneration {
            session,
            generation: Some(Uuid::new_v4()),
        },
    )?;
    ensure!(
        unknown.error.is_some(),
        "unknown generation selection was accepted"
    );
    host.restart()?;
    let recovered_pin = success(
        &host.socket(),
        Command::Generations {
            session,
            before: None,
        },
    )?
    .generations
    .context("recovered generation response omitted history")?;
    ensure!(recovered_pin.head == Some(second_generation));
    ensure!(recovered_pin.pinned == Some(first_generation));
    let recovered_view = success(
        &host.socket(),
        Command::PollPanels {
            panels: vec![pigade_session_host::poll::PanelTarget {
                session,
                generation: Some(first_generation),
            }],
            known: vec![],
        },
    )?;
    ensure!(recovered_view.views.len() == 1);
    ensure!(recovered_view.views[0].generation == first_generation);
    ensure!(recovered_view.views[0].pid.is_none());
    let all_views = success(&host.socket(), Command::List)?.views;
    ensure!(
        all_views.len() == 2,
        "host restart changed generation count"
    );
    ensure!(all_views.iter().all(|view| view.pid.is_none()));
    Ok(())
}
