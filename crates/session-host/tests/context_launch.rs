//! Real-host test for immutable reviewed context snapshots across launch/reopen.
use anyhow::{Context, Result, bail, ensure};
use pigade_layout::{Direction, TabLayout};
use pigade_session_host::{
    Command, Launch, PROTOCOL, PrepareRequest, Request, Response, View, receive, send,
};
use pigade_storage::{
    AgentProfile, AgentProfileDelete, CatalogBatch, CatalogWrite, ContextDocument, LaunchSettings,
    MemorySelection, Project, Tab, Workspace, WorkspaceMode, WorkspaceStatus,
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
                "model=sys.argv[sys.argv.index('--model')+1] if '--model' in sys.argv else '<default>'\n",
                "print('FIXTURE_MODEL='+model)\n",
                "effort=next((arg for arg in sys.argv[1:] if arg.startswith('model_reasoning_effort=')),'<default>')\n",
                "print('FIXTURE_EFFORT='+effort)\n",
                "text=json.loads(next(arg.split('=',1)[1] for arg in sys.argv[1:] if arg.startswith('developer_instructions=')))\n",
                "print('FIXTURE_CONTEXT_CRC32='+str(zlib.crc32(text.encode('utf-8'))))\n",
                "print('FIXTURE_ENV='+json.dumps(safe_env,ensure_ascii=False))\n",
            ),
        )?;
        fs::set_permissions(&codex, fs::Permissions::from_mode(0o755))?;
        let claude = bin.path().join("claude");
        fs::write(
            &claude,
            concat!(
                "#!/usr/bin/python3\n",
                "import json,os,sys,zlib,time\n",
                "if sys.argv[1:] == ['--version']:\n",
                " print('2.1.259 (Claude Code)'); raise SystemExit(0)\n",
                "args=sys.argv[1:]\n",
                "print('FIXTURE_CLAUDE_START',flush=True)\n",
                "context=args[args.index('--append-system-prompt')+1] if '--append-system-prompt' in args else None\n",
                "if context is not None:\n",
                " print('FIXTURE_CONTEXT_CRC32='+str(zlib.crc32(context.encode('utf-8'))))\n",
                "if '--resume' in args:\n",
                " reported=[('<append-system-prompt>' if i>0 and args[i-1]=='--append-system-prompt' else arg) for i,arg in enumerate(args)]\n",
                " print('FIXTURE_RESUME_ARGS='+json.dumps(reported,ensure_ascii=False),flush=True)\n",
                " print('FIXTURE_RESUME_CWD='+os.getcwd(),flush=True)\n",
                " deadline=time.monotonic()+8\n",
                " while os.path.exists('hold-resume') and time.monotonic()<deadline: time.sleep(0.01)\n",
            ),
        )?;
        fs::set_permissions(&claude, fs::Permissions::from_mode(0o755))?;
        let opencode = bin.path().join("opencode");
        fs::write(bin.path().join("opencode.version"), "1.19.3\n")?;
        fs::write(
            &opencode,
            concat!(
                "#!/usr/bin/python3\n",
                "import base64,json,os,sys,time\n",
                "version_path=os.path.join(os.path.dirname(__file__),'opencode.version')\n",
                "fixture_dir=os.path.dirname(__file__)\n",
                "if sys.argv[1:] == ['--version']:\n",
                " print('opencode '+open(version_path,encoding='utf-8').read().strip()); raise SystemExit(0)\n",
                "if sys.argv[1:2] == ['serve']:\n",
                " from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer\n",
                " from urllib.parse import unquote,urlsplit\n",
                " class Handler(BaseHTTPRequestHandler):\n",
                "  def log_message(self,*args): pass\n",
                "  def do_GET(self):\n",
                "   expected='Basic '+base64.b64encode((os.environ.get('OPENCODE_SERVER_USERNAME','')+':'+os.environ.get('OPENCODE_SERVER_PASSWORD','')).encode()).decode()\n",
                "   if self.headers.get('Authorization') != expected:\n",
                "    self.send_error(401); return\n",
                "   path=urlsplit(self.path).path\n",
                "   if not path.startswith('/session/'):\n",
                "    self.send_error(404); return\n",
                "   session_id=unquote(path[len('/session/'):])\n",
                "   hold=os.path.join(fixture_dir,'opencode.probe-hold')\n",
                "   if os.path.exists(hold):\n",
                "    open(os.path.join(fixture_dir,'opencode.probe-entered'),'w').close()\n",
                "    deadline=time.monotonic()+3\n",
                "    while os.path.exists(hold) and time.monotonic()<deadline: time.sleep(0.005)\n",
                "    if os.path.exists(hold): self.send_error(503); return\n",
                "   directory=os.getcwd()\n",
                "   if os.path.exists(os.path.join(fixture_dir,'opencode.probe-wrong-directory')):\n",
                "    directory=os.path.dirname(directory)\n",
                "   workspace_id='workspace-fixture' if os.path.exists(os.path.join(fixture_dir,'opencode.probe-workspace')) else None\n",
                "   body=json.dumps({'id':session_id,'directory':directory,'workspaceID':workspace_id}).encode()\n",
                "   self.send_response(200); self.send_header('Content-Type','application/json'); self.send_header('Content-Length',str(len(body))); self.end_headers(); self.wfile.write(body)\n",
                " server=ThreadingHTTPServer(('127.0.0.1',0),Handler)\n",
                " print('opencode server listening on http://127.0.0.1:'+str(server.server_address[1]),flush=True)\n",
                " server.serve_forever()\n",
                " raise SystemExit(0)\n",
                "print('FIXTURE_OPENCODE_ARGS='+json.dumps(sys.argv[1:],ensure_ascii=False))\n",
                "print('FIXTURE_OPENCODE_CWD='+os.getcwd())\n",
                "print('FIXTURE_OPENCODE_CONFIG_CONTENT_PRESENT='+str('OPENCODE_CONFIG_CONTENT' in os.environ))\n",
                "raise SystemExit(0)\n",
            ),
        )?;
        fs::set_permissions(&opencode, fs::Permissions::from_mode(0o755))?;
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
            .env_remove("OPENCODE_CONFIG_CONTENT")
            .env_remove("OPENCODE_WORKSPACE_ID")
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
            }
            .into(),
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
                engine: Default::default(),
                custom_command: None,
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
                engine: Default::default(),
                custom_command: None,
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
                engine: Default::default(),
                custom_command: None,
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
fn custom_cli_prepare_never_probes_and_starts_exact_reviewed_argv() -> Result<()> {
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
                        name: "custom CLI fixture".into(),
                        roots: vec![],
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                workspaces: vec![CatalogWrite {
                    value: Workspace {
                        id: workspace,
                        project_id: project,
                        task: "custom task".into(),
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
                        name: "custom".into(),
                        layout_json: serde_json::to_string(&TabLayout::default())?,
                        active_pane: None,
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            }
            .into(),
        },
    )?;
    let exe = host.bin.path().join("custom-cli");
    let capture = host.bin.path().join("custom-argv.json");
    fs::write(
        &exe,
        "#!/usr/bin/python3\nimport json,os,sys\nargv=sys.argv[1:]\nopen(argv[0],'w').write(json.dumps({'argv':argv,'cwd':os.getcwd()}))\nprint('CUSTOM_DONE')\n",
    )?;
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755))?;
    let args = vec![
        capture.to_string_lossy().into_owned(),
        "two words".into(),
        "line\nargument".into(),
        "".into(),
    ];
    let valid_request = PrepareRequest {
        tab,
        provider: "custom".into(),
        side: Direction::Right,
        target: None,
        role_override: "metadata only".into(),
        agent_instructions: "not delivered".into(),
        include_memory: false,
        memory_selection: None,
        deliver_context: false,
        engine: Default::default(),
        custom_command: Some(pigade_storage::CustomCommand {
            executable: exe.to_string_lossy().into_owned(),
            args: args.clone(),
        }),
    };
    for (label, mut invalid) in [
        ("context", valid_request.clone()),
        ("memory", valid_request.clone()),
        ("engine", valid_request.clone()),
        ("builtin command", valid_request.clone()),
    ] {
        match label {
            "context" => invalid.deliver_context = true,
            "memory" => invalid.include_memory = true,
            "engine" => invalid.engine.model = Some("unexpected-model".into()),
            "builtin command" => invalid.provider = "codex".into(),
            _ => unreachable!(),
        }
        ensure!(
            exchange(&host.socket(), Command::PrepareLaunch { request: invalid })?
                .error
                .is_some(),
            "invalid custom launch combination {label} was accepted"
        );
        ensure!(success(&host.socket(), Command::List)?.views.is_empty() && !capture.exists());
    }
    let prepared = success(
        &host.socket(),
        Command::PrepareLaunch {
            request: valid_request,
        },
    )?;
    let preview = prepared.launch_preview.context("custom preview missing")?;
    ensure!(
        prepared.views.is_empty() && !capture.exists(),
        "custom prepare executed the command"
    );
    let capabilities = serde_json::to_value(&preview.capabilities)?;
    ensure!(
        preview.provider == "custom"
            && preview.cli_version == "custom"
            && capabilities
                .as_object()
                .is_some_and(|values| values.values().all(|value| value == false))
            && preview
                .custom_command
                .as_ref()
                .context("frozen custom command missing")?
                .args
                == args
    );
    // A prepared plan is durable input to StartPrepared. Mutations that try
    // to change argv, enable generic capabilities, inject environment, or
    // claim transport support must be rejected before the attempt is admitted.
    let database = rusqlite::Connection::open(host.directory.path().join("metadata.sqlite"))?;
    database.busy_timeout(Duration::from_secs(3))?;
    let original_payload: String = database.query_row(
        "SELECT payload FROM prepared_launches WHERE id=?1",
        [preview.plan_id.to_string()],
        |row| row.get(0),
    )?;
    let mut mutations = Vec::new();
    let mut changed: serde_json::Value = serde_json::from_str(&original_payload)?;
    changed["preview"]["custom_command"]["args"][1] = "mutated-after-review".into();
    mutations.push(("argv", changed));
    let mut changed: serde_json::Value = serde_json::from_str(&original_payload)?;
    changed["preview"]["capabilities"]["startup_context"] = true.into();
    mutations.push(("capability", changed));
    let mut changed: serde_json::Value = serde_json::from_str(&original_payload)?;
    changed["launch"]["env"]["CUSTOM_INJECTION"] = "unexpected".into();
    mutations.push(("environment", changed));
    let mut changed: serde_json::Value = serde_json::from_str(&original_payload)?;
    changed["preview"]["deliver_context"] = true.into();
    mutations.push(("context", changed));
    let mut changed: serde_json::Value = serde_json::from_str(&original_payload)?;
    changed["preview"]["include_memory"] = true.into();
    mutations.push(("memory", changed));
    for (label, changed) in mutations {
        database.execute(
            "UPDATE prepared_launches SET payload=?2 WHERE id=?1",
            rusqlite::params![
                preview.plan_id.to_string(),
                serde_json::to_string(&changed)?
            ],
        )?;
        ensure!(
            exchange(
                &host.socket(),
                Command::StartPrepared {
                    plan_id: preview.plan_id
                }
            )?
            .error
            .is_some(),
            "mutated custom {label} payload was accepted"
        );
        ensure!(success(&host.socket(), Command::List)?.views.is_empty() && !capture.exists());
        let phase: String = database.query_row(
            "SELECT phase FROM prepared_launches WHERE id=?1",
            [preview.plan_id.to_string()],
            |row| row.get(0),
        )?;
        ensure!(
            phase == "prepared",
            "rejected {label} mutation consumed start admission"
        );
    }
    database.execute(
        "UPDATE prepared_launches SET payload=?2 WHERE id=?1",
        rusqlite::params![preview.plan_id.to_string(), original_payload],
    )?;
    drop(database);
    let start = success(
        &host.socket(),
        Command::StartPrepared {
            plan_id: preview.plan_id,
        },
    )?;
    let _ = start;
    host.active = Some((preview.session, preview.generation));
    let deadline = Instant::now() + STARTUP_LIMIT;
    while !capture.exists() {
        ensure!(Instant::now() < deadline, "custom CLI did not execute");
        thread::sleep(Duration::from_millis(10));
    }
    let recorded: serde_json::Value = serde_json::from_slice(&fs::read(&capture)?)?;
    ensure!(
        recorded["argv"]
            .as_array()
            .context("argv missing")?
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>()
            == args.iter().map(String::as_str).collect::<Vec<_>>()
    );
    ensure!(
        recorded["cwd"] == root.to_string_lossy().as_ref(),
        "custom CLI cwd differs from reviewed workspace"
    );
    ensure!(
        exchange(
            &host.socket(),
            Command::StartPrepared {
                plan_id: preview.plan_id
            }
        )?
        .error
        .is_some(),
        "custom attempt relaunched"
    );
    Ok(())
}

#[test]
fn prepared_resume_reuses_frozen_context_preserves_history_and_rejects_stale_binding() -> Result<()>
{
    frozen_context_resume("codex", true)
}

#[test]
fn claude_resume_reuses_frozen_context_preserves_history_and_rejects_stale_binding() -> Result<()> {
    frozen_context_resume("claude", true)
}

#[test]
fn claude_resume_without_context_omits_task_and_original_engine_options() -> Result<()> {
    frozen_context_resume("claude", false)
}

fn frozen_context_resume(provider: &str, deliver_context: bool) -> Result<()> {
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
            }
            .into(),
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
                engine: if provider == "claude" {
                    pigade_storage::EngineOptions {
                        model: Some("claude-fixture-model".into()),
                        reasoning_effort: Some("high".into()),
                    }
                } else {
                    Default::default()
                },
                custom_command: None,
                tab,
                provider: provider.into(),
                side: Direction::Right,
                target: None,
                role_override: "".into(),
                agent_instructions: "FROZEN_AGENT_INSTRUCTIONS".into(),
                include_memory: false,
                memory_selection: None,
                deliver_context,
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
    ensure!(old_frame.contains("FIXTURE_CONTEXT_CRC32=") == deliver_context);

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
    host.restart()?;
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
    ensure!(
        prepared.engine.is_empty(),
        "resume reapplied the initial model/effort"
    );
    ensure!(prepared.deliver_context == deliver_context);
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
    let expected_args = match provider {
        "codex" => vec![
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
        "claude" => {
            let mut args = vec!["--resume".to_owned(), conversation_id.to_owned()];
            if deliver_context {
                args.extend([
                    "--append-system-prompt".to_owned(),
                    "<append-system-prompt>".to_owned(),
                ]);
            }
            args
        }
        _ => unreachable!(),
    };
    ensure!(
        resume_args == expected_args,
        "resume did not receive the reviewed exact argv"
    );
    let resumed_text = resumed_view.frame.text();
    ensure!(
        resumed_text.contains(&format!("FIXTURE_RESUME_CWD={}", root.display())),
        "resume CLI ran in a different working directory"
    );
    if deliver_context {
        ensure!(
            resumed_text.contains(&format!(
                "FIXTURE_CONTEXT_CRC32={}",
                crc32fast::hash(prepared.context.text.as_bytes())
            )),
            "resumed CLI received different frozen context bytes"
        );
    } else {
        ensure!(
            !resumed_text.contains("FIXTURE_CONTEXT_CRC32="),
            "resume delivered unrequested context"
        );
    }
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
    let executable = host.bin.path().join(provider);
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

#[test]
fn opencode_1193_exact_id_resume_uses_original_cwd_without_context_delivery() -> Result<()> {
    let mut host = TestHost::new()?;
    let root = host.root().canonicalize()?;
    let launch_cwd = root.join("original-workspace-cwd");
    fs::create_dir(&launch_cwd)?;
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
                        name: "OpenCode resume fixture".into(),
                        roots: vec![],
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                workspaces: vec![CatalogWrite {
                    value: Workspace {
                        id: workspace,
                        project_id: project,
                        task: "This task must not be repeated".into(),
                        cwd: launch_cwd.clone(),
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
                        name: "OpenCode".into(),
                        layout_json: serde_json::to_string(&TabLayout::default())?,
                        active_pane: None,
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            }
            .into(),
        },
    )?;
    let frozen_context = "FROZEN_CONTEXT_MUST_NOT_BE_DELIVERED";
    success(
        &host.socket(),
        Command::SaveContext {
            document: context(project, 0, "", "", ""),
            expected_revision: 0,
        },
    )?;
    success(
        &host.socket(),
        Command::SaveContext {
            document: context(workspace, 0, frozen_context, "", ""),
            expected_revision: 0,
        },
    )?;

    let initial = success(
        &host.socket(),
        Command::PrepareLaunch {
            request: PrepareRequest {
                engine: Default::default(),
                custom_command: None,
                tab,
                provider: "opencode".into(),
                side: Direction::Right,
                target: None,
                role_override: String::new(),
                agent_instructions: String::new(),
                include_memory: false,
                memory_selection: None,
                deliver_context: false,
            },
        },
    )?
    .launch_preview
    .context("initial OpenCode preview missing")?;
    ensure!(initial.cli_version == "1.19.3");
    ensure!(!initial.deliver_context);
    ensure!(initial.context.text.contains(frozen_context));
    host.active = Some((initial.session, initial.generation));
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
                && view.frame.text().contains("FIXTURE_OPENCODE_ARGS=")
        })
    })?;
    ensure!(initial_views.len() == 1);
    let initial_view = &initial_views[0];
    let initial_text = initial_view.frame.text();
    ensure!(initial_text.contains("FIXTURE_OPENCODE_CWD="));
    let initial_frame: String = initial_view
        .frame
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    let initial_args_json = initial_frame
        .split_once("FIXTURE_OPENCODE_ARGS=")
        .map(|(_, value)| value)
        .context("OpenCode fixture omitted initial argv")?;
    let initial_args = serde_json::Deserializer::from_str(initial_args_json)
        .into_iter::<Vec<String>>()
        .next()
        .context("initial OpenCode args JSON missing")??;
    ensure!(
        initial_args.is_empty(),
        "initial context-off launch had arguments"
    );
    ensure!(
        initial_text.contains(&format!("FIXTURE_OPENCODE_CWD={}", launch_cwd.display())),
        "initial OpenCode launch used a different cwd"
    );

    let conversation_id = "ses_pigade_exact_resume_0193";
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
    .context("OpenCode binding preview missing")?
    .conversation
    .context("OpenCode exact-ID binding missing")?;
    ensure!(binding.conversation_id == conversation_id);

    success(
        &host.socket(),
        Command::SaveContext {
            document: context(workspace, 1, "EDITED_CONTEXT_MUST_NOT_BE_DELIVERED", "", ""),
            expected_revision: 1,
        },
    )?;

    fs::write(host.bin.path().join("opencode.version"), "1.19.2\n")?;
    let unsupported = exchange(
        &host.socket(),
        Command::PrepareResume {
            session: initial.session,
        },
    )?;
    ensure!(
        unsupported.error.is_some(),
        "unsupported OpenCode version produced a resume plan"
    );
    ensure!(
        success(&host.socket(), Command::List)?.views.len() == 1,
        "unsupported-version preflight created a generation"
    );
    fs::write(host.bin.path().join("opencode.version"), "1.19.3\n")?;

    fs::write(
        host.bin.path().join("opencode.probe-wrong-directory"),
        "1\n",
    )?;
    let wrong_directory = exchange(
        &host.socket(),
        Command::PrepareResume {
            session: initial.session,
        },
    )?;
    ensure!(
        wrong_directory.error.is_some(),
        "resume accepted an OpenCode session stored under another directory"
    );
    fs::remove_file(host.bin.path().join("opencode.probe-wrong-directory"))?;
    fs::write(host.bin.path().join("opencode.probe-workspace"), "1\n")?;
    let workspace_routed = exchange(
        &host.socket(),
        Command::PrepareResume {
            session: initial.session,
        },
    )?;
    ensure!(
        workspace_routed.error.is_some(),
        "resume accepted a workspace-routed OpenCode session"
    );
    fs::remove_file(host.bin.path().join("opencode.probe-workspace"))?;
    ensure!(
        success(&host.socket(), Command::List)?.views.len() == 1,
        "metadata preflight refusal created a generation"
    );

    let hold = host.bin.path().join("opencode.probe-hold");
    let entered = host.bin.path().join("opencode.probe-entered");
    fs::write(
        &hold,
        "hold metadata until independent host commands finish",
    )?;
    let socket = host.socket();
    let session = initial.session;
    let pending = thread::spawn(move || success(&socket, Command::PrepareResume { session }));
    let deadline = Instant::now() + Duration::from_secs(2);
    while !entered.exists() {
        ensure!(
            Instant::now() < deadline,
            "metadata request did not reach the gate"
        );
        thread::sleep(Duration::from_millis(5));
    }
    // These must complete before the held metadata operation is released.
    ensure!(success(&host.socket(), Command::List)?.views.len() == 1);
    let poll = success(
        &host.socket(),
        Command::PollPanels {
            panels: vec![pigade_session_host::poll::PanelTarget {
                session: initial.session,
                generation: None,
            }],
            known: vec![],
        },
    )?;
    ensure!(poll.views.len() == 1 && poll.views[0].generation == initial.generation);
    fs::remove_file(hold)?;
    let stale = pending
        .join()
        .map_err(|_| anyhow::anyhow!("resume preparation thread failed"))??
        .launch_preview
        .context("OpenCode resume preparation omitted preview")?;
    ensure!(stale.session == initial.session);
    ensure!(stale.generation != initial.generation);
    ensure!(!stale.deliver_context);
    ensure!(stale.context.content_hash == initial.context.content_hash);
    ensure!(stale.context.text.contains(frozen_context));
    ensure!(
        !stale
            .context
            .text
            .contains("EDITED_CONTEXT_MUST_NOT_BE_DELIVERED")
    );
    ensure!(stale.cwd.as_ref() == Some(&launch_cwd));
    ensure!(
        stale
            .resume
            .as_ref()
            .is_some_and(|source| source.binding == binding)
    );
    let before = success(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing before OpenCode resume")?;

    let changed_binding = "ses_pigade_rebound_exact_0193";
    let rebound = success(
        &host.socket(),
        Command::BindConversation {
            session: initial.session,
            generation: initial.generation,
            conversation_id: changed_binding.into(),
            expected_revision: binding.revision,
        },
    )?
    .launch_preview
    .context("replacement conversation binding preview missing")?
    .conversation
    .context("replacement OpenCode binding missing")?;
    ensure!(
        rebound.revision == binding.revision + 1,
        "explicit replacement binding was not recorded"
    );
    ensure!(
        exchange(
            &host.socket(),
            Command::StartPrepared {
                plan_id: stale.plan_id,
            },
        )?
        .error
        .is_some(),
        "resume with a stale conversation binding was admitted"
    );
    ensure!(
        success(&host.socket(), Command::List)?.views.len() == 1,
        "stale-binding rejection created a new generation"
    );

    let restored_binding = success(
        &host.socket(),
        Command::BindConversation {
            session: initial.session,
            generation: initial.generation,
            conversation_id: conversation_id.into(),
            expected_revision: rebound.revision,
        },
    )?
    .launch_preview
    .context("restored conversation binding preview missing")?
    .conversation
    .context("restored OpenCode binding missing")?;

    let prepared = success(
        &host.socket(),
        Command::PrepareResume {
            session: initial.session,
        },
    )?
    .launch_preview
    .context("fresh OpenCode resume preparation omitted preview")?;
    ensure!(prepared.session == initial.session);
    ensure!(prepared.generation != initial.generation);
    ensure!(!prepared.deliver_context);
    ensure!(prepared.context.content_hash == initial.context.content_hash);
    ensure!(prepared.cwd.as_ref() == Some(&launch_cwd));
    ensure!(
        prepared
            .resume
            .as_ref()
            .is_some_and(|source| source.binding == restored_binding),
        "fresh resume did not capture the restored explicit binding"
    );

    fs::write(
        host.bin.path().join("opencode.probe-wrong-directory"),
        "1\n",
    )?;
    ensure!(
        exchange(
            &host.socket(),
            Command::StartPrepared {
                plan_id: prepared.plan_id,
            },
        )?
        .error
        .is_some(),
        "StartPrepared did not recheck OpenCode session directory metadata"
    );
    fs::remove_file(host.bin.path().join("opencode.probe-wrong-directory"))?;
    ensure!(
        success(&host.socket(), Command::List)?.views.len() == 1,
        "StartPrepared metadata refusal created a generation"
    );
    let prepared = success(
        &host.socket(),
        Command::PrepareResume {
            session: initial.session,
        },
    )?
    .launch_preview
    .context("verified OpenCode resume preparation omitted preview")?;

    host.active = Some((prepared.session, prepared.generation));
    success(
        &host.socket(),
        Command::StartPrepared {
            plan_id: prepared.plan_id,
        },
    )?;
    let resumed_views = host.wait_for(|views| {
        views.iter().any(|view| {
            view.session == prepared.session
                && view.generation == prepared.generation
                && view.state == "exited"
                && view.frame.text().contains("FIXTURE_OPENCODE_ARGS=")
        })
    })?;
    ensure!(
        resumed_views.len() == 2,
        "resume created other than one generation"
    );
    let old_view = resumed_views
        .iter()
        .find(|view| view.generation == initial.generation)
        .context("original OpenCode generation disappeared")?;
    ensure!(old_view.frame.text() == initial_text);
    let resumed_view = resumed_views
        .iter()
        .find(|view| view.generation == prepared.generation)
        .context("resumed OpenCode generation missing")?;
    let resumed_frame: String = resumed_view
        .frame
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    let args_json = resumed_frame
        .split_once("FIXTURE_OPENCODE_ARGS=")
        .map(|(_, value)| value)
        .context("OpenCode fixture omitted resume argv")?;
    let args = serde_json::Deserializer::from_str(args_json)
        .into_iter::<Vec<String>>()
        .next()
        .context("OpenCode resume args JSON missing")??;
    ensure!(
        args == vec!["--session".to_owned(), conversation_id.to_owned()],
        "OpenCode resume must use only the exact session ID"
    );
    let resumed_text = resumed_view.frame.text();
    ensure!(
        resumed_text.contains(&format!("FIXTURE_OPENCODE_CWD={}", launch_cwd.display())),
        "OpenCode resumed in a different working directory"
    );
    ensure!(
        resumed_text.contains("FIXTURE_OPENCODE_CONFIG_CONTENT_PRESENT=False"),
        "resume unexpectedly received OpenCode config context environment"
    );
    ensure!(
        !resumed_text.contains(frozen_context)
            && !resumed_text.contains("EDITED_CONTEXT_MUST_NOT_BE_DELIVERED")
            && !resumed_text.contains("This task must not be repeated")
            && !resumed_text.contains("continue")
            && !resumed_text.contains("fork"),
        "resume unexpectedly delivered a prompt, task, or context"
    );
    ensure!(
        exchange(
            &host.socket(),
            Command::StartPrepared {
                plan_id: prepared.plan_id,
            },
        )?
        .error
        .is_some(),
        "OpenCode resume plan was admitted more than once"
    );
    let after = success(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing after OpenCode resume")?;
    ensure!(after.layout.revision == before.layout.revision);
    ensure!(after.layout.json == before.layout.json);
    ensure!(
        success(&host.socket(), Command::List)?.views.len() == 2,
        "duplicate OpenCode resume created an extra generation"
    );
    Ok(())
}

#[test]
fn profile_launch_keeps_frozen_source_after_profile_update_and_delete() -> Result<()> {
    let mut host = TestHost::new()?;
    let root = host.root().canonicalize()?;
    let project = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    let tab = Uuid::new_v4();
    let profile_id = Uuid::new_v4();
    let original_name = "Persistent review profile";
    let original_role = "profile architect";
    let original_instructions = "PROFILE_ORIGINAL_INSTRUCTIONS λ";
    let initial = success(
        &host.socket(),
        Command::CatalogApply {
            batch: CatalogBatch {
                projects: vec![CatalogWrite {
                    value: Project {
                        id: project,
                        canonical_path: root.clone(),
                        git_common_dir: root.clone(),
                        name: "Profile launch fixture".into(),
                        roots: vec![],
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                workspaces: vec![CatalogWrite {
                    value: Workspace {
                        id: workspace,
                        project_id: project,
                        task: "profile snapshot task".into(),
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
                        name: "Profile fixture".into(),
                        layout_json: serde_json::to_string(&TabLayout::default())?,
                        active_pane: None,
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                agent_profiles: vec![CatalogWrite {
                    value: AgentProfile {
                        id: profile_id,
                        name: original_name.into(),
                        settings: LaunchSettings {
                            engine: pigade_storage::EngineOptions {
                                model: Some("gpt-6-luna".into()),
                                reasoning_effort: Some("xhigh".into()),
                            },
                            provider: "codex".into(),
                            role: original_role.into(),
                            instructions: original_instructions.into(),
                            deliver_context: true,
                            custom_command: None,
                        },
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            }
            .into(),
        },
    )?
    .catalog
    .context("profile fixture catalog missing")?;
    let layout_before = initial.layout.clone();
    success(
        &host.socket(),
        Command::SaveContext {
            document: context(project, 0, "PROFILE_PROJECT_CONTEXT", "", ""),
            expected_revision: 0,
        },
    )?;
    success(
        &host.socket(),
        Command::SaveContext {
            document: context(workspace, 0, "PROFILE_WORKSPACE_CONTEXT", "", ""),
            expected_revision: 0,
        },
    )?;

    let stale = exchange(
        &host.socket(),
        Command::PrepareProfileLaunch {
            tab,
            profile: profile_id,
            expected_revision: 2,
            side: Direction::Right,
            target: None,
        },
    )?;
    ensure!(stale.error.is_some(), "stale profile revision was accepted");
    ensure!(stale.launch_preview.is_none());
    ensure!(
        success(&host.socket(), Command::List)?.views.is_empty(),
        "stale profile revision created a generation"
    );

    let prepared = success(
        &host.socket(),
        Command::PrepareProfileLaunch {
            tab,
            profile: profile_id,
            expected_revision: 1,
            side: Direction::Right,
            target: None,
        },
    )?;
    ensure!(prepared.views.is_empty(), "profile prepare launched a PTY");
    let preview = prepared
        .launch_preview
        .context("profile prepare omitted launch preview")?;
    let source = preview
        .profile
        .as_ref()
        .context("profile provenance missing from preview")?;
    ensure!(
        source.id == profile_id && source.name == original_name && source.revision == 1,
        "profile provenance did not match the selected revision"
    );
    ensure!(
        preview.role_override == original_role
            && preview.agent_instructions == original_instructions
            && preview.deliver_context,
        "profile settings were not copied into the prepared launch"
    );
    for expected in [
        "PROFILE_PROJECT_CONTEXT",
        "PROFILE_WORKSPACE_CONTEXT",
        original_role,
        original_instructions,
    ] {
        ensure!(
            preview.context.text.contains(expected),
            "prepared context omitted {expected:?}"
        );
    }
    ensure!(
        preview.engine.model.as_deref() == Some("gpt-6-luna")
            && preview.engine.reasoning_effort.as_deref() == Some("xhigh")
    );
    let plan_id = preview.plan_id;
    // Fault injection: even a valid option string cannot drift away from the
    // frozen argv. Reject before beginning the durable launch attempt.
    let database = rusqlite::Connection::open(host.directory.path().join("metadata.sqlite"))?;
    database.busy_timeout(Duration::from_secs(3))?;
    let original_payload: String = database.query_row(
        "SELECT payload FROM prepared_launches WHERE id=?1",
        [plan_id.to_string()],
        |row| row.get(0),
    )?;
    let mut changed: serde_json::Value = serde_json::from_str(&original_payload)?;
    changed["preview"]["engine"]["model"] = "changed-after-review".into();
    database.execute(
        "UPDATE prepared_launches SET payload=?2 WHERE id=?1",
        rusqlite::params![plan_id.to_string(), serde_json::to_string(&changed)?],
    )?;
    let refused = exchange(&host.socket(), Command::StartPrepared { plan_id })?;
    ensure!(
        refused.error.is_some(),
        "changed engine snapshot was launched"
    );
    ensure!(success(&host.socket(), Command::List)?.views.is_empty());
    let phase: String = database.query_row(
        "SELECT phase FROM prepared_launches WHERE id=?1",
        [plan_id.to_string()],
        |row| row.get(0),
    )?;
    ensure!(
        phase == "prepared",
        "invalid snapshot consumed launch admission"
    );
    database.execute(
        "UPDATE prepared_launches SET payload=?2 WHERE id=?1",
        rusqlite::params![plan_id.to_string(), original_payload],
    )?;
    drop(database);
    ensure!(success(&host.socket(), Command::List)?.views.is_empty());

    let mut replacement = AgentProfile {
        id: profile_id,
        name: "Updated profile name".into(),
        settings: LaunchSettings {
            engine: pigade_storage::EngineOptions {
                model: Some("different-model".into()),
                reasoning_effort: Some("low".into()),
            },
            provider: "codex".into(),
            role: "changed role".into(),
            instructions: "PROFILE_CHANGED_INSTRUCTIONS".into(),
            deliver_context: false,
            custom_command: None,
        },
        revision: 1,
    };
    let after_update = success(
        &host.socket(),
        Command::CatalogApply {
            batch: CatalogBatch {
                agent_profiles: vec![CatalogWrite {
                    value: replacement.clone(),
                    expected_revision: Some(1),
                }],
                ..CatalogBatch::default()
            }
            .into(),
        },
    )?
    .catalog
    .context("updated profile catalog missing")?;
    ensure!(after_update.layout == layout_before);
    ensure!(success(&host.socket(), Command::List)?.views.is_empty());

    replacement.revision = 2;
    let after_delete = success(
        &host.socket(),
        Command::CatalogApply {
            batch: CatalogBatch {
                remove_agent_profiles: vec![AgentProfileDelete {
                    id: profile_id,
                    expected_revision: replacement.revision,
                }],
                ..CatalogBatch::default()
            }
            .into(),
        },
    )?
    .catalog
    .context("deleted profile catalog missing")?;
    ensure!(after_delete.agent_profiles.as_ref().unwrap().is_empty());
    ensure!(after_delete.layout == layout_before);
    ensure!(
        success(&host.socket(), Command::List)?.views.is_empty(),
        "profile update or deletion created a CLI generation"
    );

    host.active = Some((preview.session, preview.generation));
    let started = success(&host.socket(), Command::StartPrepared { plan_id })?;
    ensure!(started.launch_phase.as_deref() == Some("delivered_unconfirmed"));
    let views = host.wait_for(|views| {
        views.iter().any(|view| {
            view.session == preview.session
                && view.generation == preview.generation
                && view.state == "exited"
                && view.frame.text().contains("FIXTURE_ARGS")
        })
    })?;
    ensure!(views.len() == 1, "profile launch created extra generations");
    let output = views[0].frame.text();
    ensure!(
        output.contains("FIXTURE_MODEL=gpt-6-luna")
            && output.contains("FIXTURE_EFFORT=model_reasoning_effort=\"xhigh\""),
        "model/effort argv did not reach the real PTY fixture"
    );
    ensure!(
        output.contains(&format!(
            "FIXTURE_CONTEXT_CRC32={}",
            crc32fast::hash(preview.context.text.as_bytes())
        )),
        "profile launch did not deliver the frozen context snapshot"
    );
    let stored = success(
        &host.socket(),
        Command::GetSessionContext {
            session: preview.session,
            generation: preview.generation,
        },
    )?
    .launch_preview
    .context("session context omitted frozen profile provenance")?;
    ensure!(stored.profile.as_ref().is_some_and(|source| {
        source.id == profile_id && source.name == original_name && source.revision == 1
    }));
    ensure!(
        stored.engine == preview.engine,
        "editing/deleting profile changed frozen engine options"
    );
    host.restart()?;
    let reopened = success(
        &host.socket(),
        Command::GetSessionContext {
            session: preview.session,
            generation: preview.generation,
        },
    )?
    .launch_preview
    .context("reopened engine snapshot missing")?;
    ensure!(
        reopened.engine == preview.engine
            && reopened
                .profile
                .as_ref()
                .is_some_and(|source| source.revision == 1)
    );
    ensure!(stored.role_override == original_role);
    ensure!(stored.agent_instructions == original_instructions);
    ensure!(stored.context.content_hash == preview.context.content_hash);
    ensure!(
        success(&host.socket(), Command::List)?.views.len() == 1,
        "profile snapshot lookup or library deletion created another generation"
    );
    Ok(())
}
