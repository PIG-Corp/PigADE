//! Explicit launch-template runs freeze a whole plan before the first process.
use anyhow::{Context, Result, bail, ensure};
use pigade_layout::{Axis, GeometryTemplate, TabLayout, TemplateNode};
use pigade_session_host::{Command, Launch, PROTOCOL, Request, Response, receive, send};
use pigade_storage::{
    CatalogBatch, CatalogWrite, LaunchSettings, LayoutTemplate, Project, Tab, Workspace,
    WorkspaceMode, WorkspaceStatus,
};
use rusqlite::Connection;
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

const WAIT: Duration = Duration::from_secs(8);

fn exchange(socket: &Path, command: Command) -> Result<Response> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(4)))?;
    stream.set_write_timeout(Some(Duration::from_secs(4)))?;
    send(
        &mut stream,
        &Request {
            protocol: PROTOCOL,
            command,
        },
    )?;
    let response: Response = receive(&mut stream)?;
    ensure!(response.protocol == PROTOCOL, "protocol mismatch");
    if let Some(error) = response.error.as_deref() {
        bail!("host: {error}");
    }
    Ok(response)
}

struct Fixture {
    state: TempDir,
    bin: TempDir,
    host: Option<Child>,
    sessions: Vec<(Uuid, Uuid)>,
}

impl Fixture {
    fn new() -> Result<Self> {
        let mut value = Self {
            state: tempfile::tempdir()?,
            bin: tempfile::tempdir()?,
            host: None,
            sessions: Vec::new(),
        };
        let cli = value.bin.path().join("codex");
        fs::write(
            &cli,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.162.1'; exit 0; fi\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
        )?;
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755))?;
        value.start()?;
        Ok(value)
    }
    fn socket(&self) -> PathBuf {
        self.state.path().join("host.sock")
    }
    fn start(&mut self) -> Result<()> {
        let path =
            std::env::join_paths([self.bin.path(), Path::new("/usr/bin"), Path::new("/bin")])?;
        let child = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"))
            .arg("serve")
            .arg(self.state.path())
            .env("PATH", path)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.state.path().join("host.log"))?)
            .spawn()?;
        self.host = Some(child);
        let until = Instant::now() + WAIT;
        loop {
            if self
                .host
                .as_mut()
                .context("host missing")?
                .try_wait()?
                .is_some()
            {
                bail!(
                    "fixture host exited: {}",
                    fs::read_to_string(self.state.path().join("host.log"))?
                );
            }
            if exchange(&self.socket(), Command::Catalog).is_ok() {
                return Ok(());
            }
            ensure!(Instant::now() < until, "fixture host did not start");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn restart(&mut self) -> Result<()> {
        self.stop()?;
        self.start()
    }
    fn stop(&mut self) -> Result<()> {
        if let Some(mut host) = self.host.take() {
            host.kill()?;
            host.wait()?;
        }
        Ok(())
    }
    fn setup_catalog(&self) -> Result<(Uuid, Uuid, Uuid, Uuid)> {
        let project = Uuid::new_v4();
        let workspace = Uuid::new_v4();
        let tab = Uuid::new_v4();
        let other_tab = Uuid::new_v4();
        let root = self.state.path().canonicalize()?;
        exchange(
            &self.socket(),
            Command::CatalogApply {
                batch: CatalogBatch {
                    projects: vec![CatalogWrite {
                        value: Project {
                            id: project,
                            canonical_path: root.clone(),
                            git_common_dir: root.clone(),
                            name: "Template fixture".into(),
                            roots: vec![],
                            revision: 0,
                        },
                        expected_revision: None,
                    }],
                    workspaces: vec![CatalogWrite {
                        value: Workspace {
                            id: workspace,
                            project_id: project,
                            task: "fixture task".into(),
                            cwd: root.clone(),
                            mode: WorkspaceMode::SharedCheckout,
                            status: WorkspaceStatus::Ready,
                            error: None,
                            context_revision: None,
                            revision: 0,
                        },
                        expected_revision: None,
                    }],
                    tabs: vec![tab, other_tab]
                        .into_iter()
                        .map(|id| CatalogWrite {
                            value: Tab {
                                id,
                                workspace_id: workspace,
                                name: "Fixture".into(),
                                layout_json: serde_json::to_string(&TabLayout::default()).unwrap(),
                                active_pane: None,
                                revision: 0,
                            },
                            expected_revision: None,
                        })
                        .collect(),
                    ..CatalogBatch::default()
                }
                .into(),
            },
        )?;
        Ok((project, workspace, tab, other_tab))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for (session, generation) in &self.sessions {
            let _ = exchange(
                &self.socket(),
                Command::Stop {
                    session: *session,
                    generation: *generation,
                },
            );
        }
        if let Some(mut child) = self.host.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn run_freezes_slots_then_advances_once_and_commits_template_with_undo() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let (_project, workspace, original_tab, other_tab) = fixture.setup_catalog()?;
    // User focus/navigation and viewport persistence advance selection CAS
    // independently of layout revisions. A template run must preserve and
    // advance that live selection revision when it creates its new tab.
    exchange(
        &fixture.socket(),
        Command::SetCanvasOffset {
            tab: original_tab,
            offset: pigade_storage::CanvasOffset { x: 1.0, y: 2.0 },
        },
    )?;
    let current = exchange(&fixture.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    ensure!(
        current.selection.revision > 0,
        "fixture selection was not advanced"
    );
    let geometry = GeometryTemplate {
        schema: 1,
        root: Some(TemplateNode::Split {
            axis: Axis::Vertical,
            weights: vec![0.68, 0.32],
            children: vec![TemplateNode::Pane, TemplateNode::Pane],
        }),
    };
    let launches = (0..2)
        .map(|_| LaunchSettings {
            engine: pigade_storage::EngineOptions {
                model: Some("gpt-6-luna".into()),
                reasoning_effort: Some("high".into()),
            },
            provider: "codex".into(),
            role: String::new(),
            instructions: String::new(),
            deliver_context: false,
            custom_command: None,
        })
        .collect::<Vec<_>>();
    let added = exchange(
        &fixture.socket(),
        Command::CatalogApply {
            batch: CatalogBatch {
                layout_templates: vec![CatalogWrite {
                    value: LayoutTemplate {
                        id: Uuid::new_v4(),
                        name: "Two panes".into(),
                        geometry: geometry.clone(),
                        launches: Some(launches.clone()),
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            }
            .into(),
        },
    )?;
    let template = added
        .catalog
        .context("catalog missing")?
        .layout_templates
        .unwrap()
        .remove(0);
    let rejected = exchange(
        &fixture.socket(),
        Command::PrepareTemplateRun {
            request: pigade_session_host::TemplateRunRequest {
                operation: Uuid::new_v4(),
                workspace,
                name: "Wrong count".into(),
                source: pigade_session_host::TemplateRunSource::Custom {
                    geometry: geometry.clone(),
                    launches: launches[..1].to_vec(),
                },
            },
        },
    );
    ensure!(rejected.is_err(), "mismatched launch count was accepted");
    let rejected = exchange(
        &fixture.socket(),
        Command::PrepareTemplateRun {
            request: pigade_session_host::TemplateRunRequest {
                operation: Uuid::new_v4(),
                workspace,
                name: "Bad schema".into(),
                source: pigade_session_host::TemplateRunSource::Custom {
                    geometry: GeometryTemplate {
                        schema: 2,
                        root: geometry.root.clone(),
                    },
                    launches: launches.clone(),
                },
            },
        },
    );
    ensure!(
        rejected.is_err(),
        "unsupported geometry schema was accepted"
    );
    ensure!(
        exchange(&fixture.socket(), Command::List)?.views.is_empty(),
        "rejected preparation launched a CLI"
    );
    let prepared = exchange(
        &fixture.socket(),
        Command::PrepareTemplateRun {
            request: pigade_session_host::TemplateRunRequest {
                operation: Uuid::new_v4(),
                workspace,
                name: template.name.clone(),
                source: pigade_session_host::TemplateRunSource::Saved {
                    id: template.id,
                    revision: template.revision,
                },
            },
        },
    )?;
    let mut run = prepared.template_run.context("run preview missing")?;
    ensure!(
        run.slots.len() == 2
            && run.slots.iter().all(|slot| slot.phase == "prepared"
                && slot.preview.engine.model.as_deref() == Some("gpt-6-luna")
                && slot.preview.engine.reasoning_effort.as_deref() == Some("high")),
        "all slots were not frozen"
    );
    let before = exchange(&fixture.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    ensure!(before.tabs.len() == 2 && before.tabs.iter().all(|tab| tab.layout_json == serde_json::to_string(&TabLayout::default()).unwrap()), "preparation mutated tab placement");
    ensure!(
        exchange(&fixture.socket(), Command::List)?.views.is_empty(),
        "preparation launched a CLI"
    );
    ensure!(
        exchange(
            &fixture.socket(),
            Command::StartPrepared {
                plan_id: run.slots[0].preview.plan_id,
            },
        )
        .is_err(),
        "generic StartPrepared bypassed template-run ownership"
    );
    let reserved = &run.slots[0].preview;
    ensure!(
        exchange(
            &fixture.socket(),
            Command::Launch {
                spec: Launch {
                    session: reserved.session,
                    generation: reserved.generation,
                    operation: reserved.plan_id,
                    executable: reserved
                        .executable
                        .clone()
                        .context("frozen executable missing")?,
                    args: Vec::new(),
                    env: Default::default(),
                    cwd: reserved.cwd.clone().context("frozen cwd missing")?,
                    geometry: pigade_terminal::Geometry::new(36, 120)?,
                }
            }
        )
        .is_err(),
        "raw Launch bypassed template-run ownership"
    );
    ensure!(
        exchange(
            &fixture.socket(),
            Command::LaunchInTab {
                tab: run.tab_id,
                provider: reserved.provider.clone(),
                session: reserved.session,
                generation: reserved.generation,
                operation: reserved.plan_id,
                side: reserved.side,
                target: reserved.target,
            }
        )
        .is_err(),
        "LaunchInTab bypassed template-run ownership"
    );
    fixture.restart()?;
    run = exchange(&fixture.socket(), Command::GetTemplateRun { run: run.id })?
        .template_run
        .context("prepared run did not survive host reopen")?;
    ensure!(
        run.status == "prepared" && exchange(&fixture.socket(), Command::List)?.views.is_empty(),
        "host reopen auto-advanced a prepared run"
    );

    run = exchange(
        &fixture.socket(),
        Command::AdvanceTemplateRun {
            run: run.id,
            expected_revision: run.revision,
        },
    )?
    .template_run
    .context("first advance missing run")?;
    ensure!(
        run.status == "running" && run.slots.iter().filter(|slot| slot.placed).count() == 1,
        "first advance did not place exactly one process"
    );
    ensure!(
        exchange(&fixture.socket(), Command::List)?.views.len() == 1,
        "one advance launched more than one CLI"
    );
    ensure!(
        exchange(
            &fixture.socket(),
            Command::StartPrepared {
                plan_id: run.slots[1].preview.plan_id,
            },
        )
        .is_err(),
        "generic StartPrepared bypassed ordered slot progression"
    );
    fixture.sessions.extend(
        run.slots
            .iter()
            .map(|slot| (slot.preview.session, slot.preview.generation)),
    );
    run = exchange(
        &fixture.socket(),
        Command::AdvanceTemplateRun {
            run: run.id,
            expected_revision: run.revision,
        },
    )?
    .template_run
    .context("second advance missing run")?;
    ensure!(
        run.status == "complete" && run.slots.iter().all(|slot| slot.placed),
        "final advance did not complete all slots"
    );
    ensure!(
        exchange(&fixture.socket(), Command::List)?.views.len() == 2,
        "second advance did not launch exactly one additional CLI"
    );

    let catalog = exchange(&fixture.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    let run_tab = catalog
        .tabs
        .iter()
        .find(|tab| tab.id == run.tab_id)
        .context("run tab missing")?;
    let layout: TabLayout = serde_json::from_str(&run_tab.layout_json)?;
    ensure!(
        GeometryTemplate::from_tab(&layout)? == geometry,
        "final geometry differs from requested template"
    );
    ensure!(
        catalog
            .tabs
            .iter()
            .find(|tab| tab.id == original_tab)
            .unwrap()
            .layout_json
            == serde_json::to_string(&TabLayout::default()).unwrap(),
        "original tab changed"
    );
    ensure!(
        catalog
            .tabs
            .iter()
            .find(|tab| tab.id == other_tab)
            .unwrap()
            .layout_json
            == serde_json::to_string(&TabLayout::default()).unwrap(),
        "other tab changed"
    );
    let ids_before = layout.panes();
    exchange(
        &fixture.socket(),
        Command::LayoutEdit {
            expected_revision: catalog.layout.revision,
            action: pigade_session_host::LayoutAction::Undo,
        },
    )?;
    let undone = exchange(&fixture.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    let undone_layout: TabLayout = serde_json::from_str(
        &undone
            .tabs
            .iter()
            .find(|tab| tab.id == run.tab_id)
            .unwrap()
            .layout_json,
    )?;
    ensure!(
        undone_layout.panes() == ids_before,
        "undo changed process pane IDs"
    );
    exchange(
        &fixture.socket(),
        Command::LayoutEdit {
            expected_revision: undone.layout.revision,
            action: pigade_session_host::LayoutAction::Redo,
        },
    )?;
    let redone = exchange(&fixture.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    let redone_layout: TabLayout = serde_json::from_str(
        &redone
            .tabs
            .iter()
            .find(|tab| tab.id == run.tab_id)
            .unwrap()
            .layout_json,
    )?;
    ensure!(
        GeometryTemplate::from_tab(&redone_layout)? == geometry
            && redone_layout.panes() == ids_before,
        "redo failed to restore exact template placement"
    );
    ensure!(
        exchange(&fixture.socket(), Command::List)?.views.len() == 2,
        "layout undo/redo changed running processes"
    );
    let cancelled = exchange(
        &fixture.socket(),
        Command::PrepareTemplateRun {
            request: pigade_session_host::TemplateRunRequest {
                operation: Uuid::new_v4(),
                workspace,
                name: "Cancelled run".into(),
                source: pigade_session_host::TemplateRunSource::Saved {
                    id: template.id,
                    revision: template.revision,
                },
            },
        },
    )?
    .template_run
    .context("second run missing")?;
    let cancelled = exchange(
        &fixture.socket(),
        Command::CancelTemplateRun {
            run: cancelled.id,
            expected_revision: cancelled.revision,
        },
    )?
    .template_run
    .context("cancel reply missing")?;
    ensure!(
        cancelled.status == "cancelled",
        "run cancellation was not saved"
    );
    ensure!(
        exchange(
            &fixture.socket(),
            Command::StartPrepared {
                plan_id: cancelled.slots[0].preview.plan_id,
            },
        )
        .is_err(),
        "cancelled template slot was launchable through generic StartPrepared"
    );
    ensure!(
        exchange(&fixture.socket(), Command::List)?.views.len() == 2,
        "cancelling an unadvanced run changed process count"
    );

    let interrupted = exchange(
        &fixture.socket(),
        Command::PrepareTemplateRun {
            request: pigade_session_host::TemplateRunRequest {
                operation: Uuid::new_v4(),
                workspace,
                name: "Interrupted run".into(),
                source: pigade_session_host::TemplateRunSource::Saved {
                    id: template.id,
                    revision: template.revision,
                },
            },
        },
    )?
    .template_run
    .context("interrupted run missing")?;
    let interrupted_plan = interrupted.slots[0].preview.plan_id;
    let interrupted_session = interrupted.slots[0].preview.session;
    fixture.stop()?;
    {
        let connection = Connection::open(fixture.state.path().join("metadata.sqlite"))?;
        let changed = connection.execute(
            "UPDATE prepared_launches SET phase='attempting' WHERE id=?1",
            [interrupted_plan.to_string()],
        )?;
        ensure!(
            changed == 1,
            "could not establish the durable attempt crash boundary"
        );
    }
    fixture.start()?;
    let reopened = exchange(
        &fixture.socket(),
        Command::GetTemplateRun {
            run: interrupted.id,
        },
    )?
    .template_run
    .context("attempting run missing after reopen")?;
    ensure!(
        !reopened.can_advance && reopened.error.is_some(),
        "attempting launch did not remain locked after reopen"
    );
    ensure!(
        exchange(
            &fixture.socket(),
            Command::AdvanceTemplateRun {
                run: interrupted.id,
                expected_revision: reopened.revision
            }
        )
        .is_err(),
        "attempting launch was retried after reopen"
    );
    let sessions = exchange(&fixture.socket(), Command::List)?.views;
    ensure!(
        sessions.len() == 2
            && sessions
                .iter()
                .all(|view| view.session != interrupted_session),
        "unknown crash boundary created another generation/process"
    );
    Ok(())
}

#[test]
fn pane_directory_rename_search_and_cross_workspace_activation_are_durable() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let (project, _first_workspace, first_tab, _) = fixture.setup_catalog()?;
    let second_workspace = Uuid::new_v4();
    let second_tab = Uuid::new_v4();
    let cwd = fixture.state.path().canonicalize()?;
    exchange(
        &fixture.socket(),
        Command::CatalogApply {
            batch: CatalogBatch {
                workspaces: vec![CatalogWrite {
                    value: Workspace {
                        id: second_workspace,
                        project_id: project,
                        task: "second task".into(),
                        cwd: cwd.clone(),
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
                        id: second_tab,
                        workspace_id: second_workspace,
                        name: "Second tab".into(),
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

    let launch = |tab, role: &str| Command::PrepareLaunch {
        request: pigade_session_host::PrepareRequest {
            engine: Default::default(),
            custom_command: None,
            tab,
            provider: "codex".into(),
            side: pigade_layout::Direction::Right,
            target: None,
            role_override: role.into(),
            agent_instructions: String::new(),
            include_memory: false,
            memory_selection: None,
            deliver_context: false,
        },
    };
    let first_preview = exchange(&fixture.socket(), launch(first_tab, "reviewer"))?
        .launch_preview
        .context("first launch preview missing")?;
    exchange(
        &fixture.socket(),
        Command::StartPrepared {
            plan_id: first_preview.plan_id,
        },
    )?;
    let second_preview = exchange(&fixture.socket(), launch(second_tab, "builder"))?
        .launch_preview
        .context("second launch preview missing")?;
    exchange(
        &fixture.socket(),
        Command::StartPrepared {
            plan_id: second_preview.plan_id,
        },
    )?;
    fixture.sessions.extend([
        (first_preview.session, first_preview.generation),
        (second_preview.session, second_preview.generation),
    ]);

    let renamed = exchange(
        &fixture.socket(),
        Command::RenamePane {
            session: first_preview.session,
            name: "Reviewer desk".into(),
            expected_revision: None,
        },
    )?;
    let catalog = renamed.catalog.context("rename catalog missing")?;
    let label = catalog
        .pane_labels
        .as_ref()
        .unwrap()
        .iter()
        .find(|label| label.session == first_preview.session)
        .context("renamed pane label missing")?;
    ensure!(
        label.name == "Reviewer desk" && label.revision == 1,
        "pane rename did not persist"
    );
    ensure!(
        exchange(
            &fixture.socket(),
            Command::RenamePane {
                session: first_preview.session,
                name: "stale rename".into(),
                expected_revision: None,
            },
        )
        .is_err(),
        "stale pane label revision was accepted"
    );
    let directory = exchange(
        &fixture.socket(),
        Command::SearchPanels {
            query: "reviewer desk codex reviewer".into(),
            after: None,
        },
    )?
    .panel_directory
    .context("panel directory missing")?;
    ensure!(
        directory.entries.len() == 1
            && directory.entries[0].session == first_preview.session
            && directory.entries[0].provider.as_deref() == Some("codex")
            && directory.entries[0].role == "reviewer",
        "directory query did not match alias/provider/role on current head"
    );
    let first_page = exchange(
        &fixture.socket(),
        Command::SearchPanels {
            query: String::new(),
            after: None,
        },
    )?
    .panel_directory
    .context("first panel page missing")?;
    ensure!(
        first_page.entries.len() == 2
            && first_page.entries[0].session < first_page.entries[1].session,
        "panel directory order is not stable by session ID"
    );
    let second_page = exchange(
        &fixture.socket(),
        Command::SearchPanels {
            query: String::new(),
            after: Some(first_page.entries[0].session),
        },
    )?
    .panel_directory
    .context("cursor panel page missing")?;
    ensure!(
        second_page.entries.len() == 1
            && second_page.entries[0].session == first_page.entries[1].session,
        "exclusive panel cursor skipped or repeated an entry"
    );
    ensure!(
        exchange(
            &fixture.socket(),
            Command::SearchPanels {
                query: "работает".into(),
                after: None,
            },
        )?
        .panel_directory
        .is_some_and(|result| result.entries.len() == 2),
        "localized live-state alias was not searchable"
    );
    let before = exchange(&fixture.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    let mut selection = before.selection.clone();
    let selection_revision = selection.revision;
    selection
        .window
        .panel_generations
        .insert(first_preview.session, first_preview.generation);
    exchange(
        &fixture.socket(),
        Command::CatalogApply {
            batch: CatalogBatch {
                selection: Some(CatalogWrite {
                    value: selection,
                    expected_revision: Some(selection_revision),
                }),
                ..CatalogBatch::default()
            }
            .into(),
        },
    )?;
    let before = exchange(&fixture.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    let undo_before =
        pigade_layout::LayoutModel::from_history(serde_json::from_str(&before.layout.json)?)?
            .undo_len();
    let activated = exchange(
        &fixture.socket(),
        Command::ActivatePane {
            tab: second_tab,
            session: second_preview.session,
            expected_layout_revision: before.layout.revision,
        },
    )?
    .catalog
    .context("activation catalog missing")?;
    ensure!(
        activated.selection.project_id == Some(project)
            && activated.selection.workspace_id == Some(second_workspace)
            && activated.selection.tab_id == Some(second_tab)
            && activated
                .selection
                .window
                .panel_generations
                .get(&first_preview.session)
                == Some(&first_preview.generation)
            && activated
                .tabs
                .iter()
                .find(|tab| tab.id == second_tab)
                .unwrap()
                .active_pane
                == Some(second_preview.session),
        "pane activation did not atomically select and focus the cross-workspace pane"
    );
    let undo_after =
        pigade_layout::LayoutModel::from_history(serde_json::from_str(&activated.layout.json)?)?
            .undo_len();
    ensure!(
        undo_after == undo_before,
        "focus consumed a layout undo slot"
    );
    ensure!(
        exchange(&fixture.socket(), Command::List)?
            .views
            .iter()
            .any(|view| {
                view.session == first_preview.session
                    && view.generation == first_preview.generation
                    && view.pid.is_some()
            })
            && exchange(&fixture.socket(), Command::List)?
                .views
                .iter()
                .any(|view| {
                    view.session == second_preview.session
                        && view.generation == second_preview.generation
                        && view.pid.is_some()
                }),
        "activation changed live process identities"
    );
    fixture.restart()?;
    let reopened = exchange(&fixture.socket(), Command::Catalog)?
        .catalog
        .context("reopened catalog missing")?;
    ensure!(
        reopened
            .pane_labels
            .as_ref()
            .unwrap()
            .iter()
            .any(|label| label.session == first_preview.session && label.name == "Reviewer desk")
            && reopened.selection.tab_id == Some(second_tab),
        "pane alias or selection did not survive host reopen"
    );
    Ok(())
}
