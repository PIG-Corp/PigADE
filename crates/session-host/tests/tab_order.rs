//! Real-host coverage for durable per-workspace tab ordering.
use anyhow::{Context, Result, bail, ensure};
use pigade_layout::{Direction, TabLayout};
use pigade_session_host::{Command, PROTOCOL, PrepareRequest, Request, Response, receive, send};
use pigade_storage::{CatalogBatch, CatalogWrite, Tab, WorkspaceStatus};
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

struct HostFixture {
    state: TempDir,
    bin: TempDir,
    child: Option<Child>,
    active: Option<(Uuid, Uuid)>,
}

impl HostFixture {
    fn new() -> Result<Self> {
        let state = tempfile::tempdir()?;
        let bin = tempfile::tempdir()?;
        let mut fixture = Self {
            state,
            bin,
            child: None,
            active: None,
        };
        fixture.write_codex_fixture()?;
        fixture.start()?;
        Ok(fixture)
    }

    fn state(&self) -> &Path {
        self.state.path()
    }
    fn socket(&self) -> PathBuf {
        self.state().join("host.sock")
    }

    fn write_codex_fixture(&self) -> Result<()> {
        let path = self.bin.path().join("codex");
        fs::write(
            &path,
            concat!(
                "#!/bin/sh\n",
                "if [ \"$1\" = --version ]; then echo 'codex-cli 0.162.1'; exit 0; fi\n",
                "printf 'TAB_ORDER_FIXTURE_READY\\n'\n",
                "exec sleep 3600\n",
            ),
        )?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        for (name, version) in [("claude", "2.1.259"), ("opencode", "fixture-1")] {
            let path = self.bin.path().join(name);
            fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n"))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        let path =
            std::env::join_paths([self.bin.path(), Path::new("/usr/bin"), Path::new("/bin")])?;
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_pigade-session-host"));
        command
            .arg("serve")
            .arg(self.state())
            .env("PATH", path)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.state().join("host.log"))?);
        self.child = Some(command.spawn()?);
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
        if let Some((session, generation)) = self.active.take() {
            let _ = exchange(
                &self.socket(),
                Command::Stop {
                    session,
                    generation,
                },
            );
            let deadline = Instant::now() + Duration::from_secs(4);
            loop {
                let stopped =
                    exchange(&self.socket(), Command::List)
                        .ok()
                        .is_some_and(|response| {
                            response.views.iter().any(|view| {
                                view.session == session
                                    && view.generation == generation
                                    && view.state == "exited"
                            })
                        });
                if stopped || Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
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
    if let Some(error) = response.error.as_deref() {
        bail!("host command failed: {error}");
    }
    Ok(response)
}

fn add_tab(socket: &Path, workspace: Uuid, name: &str) -> Result<Tab> {
    let tab = Tab {
        id: Uuid::new_v4(),
        workspace_id: workspace,
        name: name.into(),
        layout_json: serde_json::to_string(&TabLayout::default())?,
        active_pane: None,
        revision: 0,
    };
    exchange(
        socket,
        Command::CatalogApply {
            batch: Box::new(CatalogBatch {
                tabs: vec![CatalogWrite {
                    value: tab.clone(),
                    expected_revision: None,
                }],
                ..Default::default()
            }),
        },
    )?;
    Ok(tab)
}

#[test]
fn tab_order_reorders_metadata_without_touching_sessions_and_survives_reopen() -> Result<()> {
    let mut host = HostFixture::new()?;
    let parent = host.state().join("projects");
    fs::create_dir(&parent)?;
    let project_catalog = exchange(
        &host.socket(),
        Command::CreateProject {
            parent,
            name: "TabOrderProject".into(),
        },
    )?
    .catalog
    .context("project catalog missing")?;
    let project = project_catalog
        .projects
        .first()
        .context("project missing")?
        .clone();
    let initial = exchange(
        &host.socket(),
        Command::CreateWorkspace {
            project: project.id,
            task: "primary".into(),
            worktree: false,
        },
    )?
    .catalog
    .context("workspace catalog missing")?;
    let workspace = initial
        .workspaces
        .iter()
        .find(|row| row.project_id == project.id)
        .context("ready workspace missing")?
        .clone();
    ensure!(workspace.status == WorkspaceStatus::Ready);
    let first = initial
        .tabs
        .iter()
        .find(|tab| tab.workspace_id == workspace.id)
        .context("initial tab missing")?
        .clone();
    let second = add_tab(&host.socket(), workspace.id, "Second")?;
    let third = add_tab(&host.socket(), workspace.id, "Third")?;

    let other_workspace_catalog = exchange(
        &host.socket(),
        Command::CreateWorkspace {
            project: project.id,
            task: "other".into(),
            worktree: false,
        },
    )?
    .catalog
    .context("second workspace catalog missing")?;
    let other_workspace = other_workspace_catalog
        .workspaces
        .iter()
        .find(|row| row.id != workspace.id)
        .context("second workspace missing")?
        .clone();
    let other_ids: Vec<_> = other_workspace_catalog
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == other_workspace.id)
        .map(|tab| tab.id)
        .collect();
    ensure!(!other_ids.is_empty(), "second workspace tab missing");

    let prepared = exchange(
        &host.socket(),
        Command::PrepareLaunch {
            request: PrepareRequest {
                tab: first.id,
                provider: "codex".into(),
                side: Direction::Right,
                target: None,
                role_override: String::new(),
                agent_instructions: "tab order fixture".into(),
                include_memory: false,
                memory_selection: None,
                deliver_context: false,
                engine: Default::default(),
                custom_command: None,
            },
        },
    )?
    .launch_preview
    .context("launch preview missing")?;
    exchange(
        &host.socket(),
        Command::StartPrepared {
            plan_id: prepared.plan_id,
        },
    )?;
    host.active = Some((prepared.session, prepared.generation));
    let deadline = Instant::now() + Duration::from_secs(8);
    let live = loop {
        let views = exchange(&host.socket(), Command::List)?.views;
        if let Some(view) = views
            .into_iter()
            .find(|view| view.session == prepared.session)
        {
            let text: String = view
                .frame
                .cells
                .iter()
                .map(|cell| cell.text.as_str())
                .collect();
            if view.pid.is_some() && text.contains("TAB_ORDER_FIXTURE_READY") {
                break view;
            }
        }
        ensure!(
            Instant::now() < deadline,
            "fixture shell did not become live"
        );
        thread::sleep(Duration::from_millis(20));
    };
    ensure!(live.generation == prepared.generation && live.pid.is_some());
    let journal_path = host.state().join(format!("{}.journal", live.generation));
    let journal_before = fs::read(&journal_path)
        .with_context(|| format!("read session journal {}", journal_path.display()))?;
    let before = exchange(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    let before_order = before
        .tab_orders
        .as_ref()
        .context("tab order capability missing")?
        .iter()
        .find(|order| order.workspace_id == workspace.id)
        .context("workspace order missing")?
        .clone();
    let desired = vec![third.id, first.id, second.id];
    let saved = exchange(
        &host.socket(),
        Command::SetTabOrder {
            workspace: workspace.id,
            expected_workspace_revision: workspace.revision,
            expected_revision: before_order.revision,
            ordered_ids: desired.clone(),
        },
    )?
    .catalog
    .context("reordered catalog missing")?;
    let order = saved
        .tab_orders
        .as_ref()
        .context("tab order capability missing")?
        .iter()
        .find(|row| row.workspace_id == workspace.id)
        .context("reordered workspace order missing")?;
    ensure!(order.schema == 1 && order.ordered_ids == desired);
    ensure!(order.revision == before_order.revision + 1);
    ensure!(saved.selection == before.selection && saved.layout == before.layout);
    ensure!(saved.workspaces == before.workspaces && saved.projects == before.projects);
    ensure!(
        saved
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == workspace.id)
            .map(|tab| tab.id)
            .collect::<Vec<_>>()
            == desired,
        "catalog tabs are not in canonical workspace order"
    );
    ensure!(
        saved
            .tabs
            .iter()
            .filter(|tab| tab.workspace_id == other_workspace.id)
            .map(|tab| tab.id)
            .collect::<Vec<_>>()
            == other_ids,
        "other workspace order changed"
    );
    ensure!(
        fs::read(&journal_path)? == journal_before,
        "metadata reorder changed the session journal"
    );
    let no_op = exchange(
        &host.socket(),
        Command::SetTabOrder {
            workspace: workspace.id,
            expected_workspace_revision: workspace.revision,
            expected_revision: order.revision,
            ordered_ids: desired.clone(),
        },
    )?
    .catalog
    .context("no-op catalog missing")?;
    let no_op_order = no_op
        .tab_orders
        .as_ref()
        .unwrap()
        .iter()
        .find(|row| row.workspace_id == workspace.id)
        .unwrap();
    ensure!(
        no_op_order.revision == order.revision && no_op_order.ordered_ids == desired,
        "no-op reorder advanced the revision"
    );
    ensure!(no_op == saved, "no-op reorder changed metadata");
    for (expected_revision, revision, ids) in [
        (
            workspace.revision,
            order.revision,
            vec![first.id, first.id, second.id],
        ),
        (
            workspace.revision,
            order.revision,
            vec![first.id, second.id],
        ),
        (
            workspace.revision,
            order.revision,
            vec![first.id, second.id, other_ids[0]],
        ),
        (workspace.revision + 1, order.revision, desired.clone()),
        (workspace.revision, before_order.revision, desired.clone()),
    ] {
        ensure!(
            exchange_allow_error(
                &host.socket(),
                Command::SetTabOrder {
                    workspace: workspace.id,
                    expected_workspace_revision: expected_revision,
                    expected_revision: revision,
                    ordered_ids: ids,
                }
            )
            .is_err(),
            "invalid, no-op or stale order was accepted"
        );
        ensure!(
            exchange(&host.socket(), Command::Catalog)?.catalog.as_ref() == Some(&saved),
            "rejected order changed catalog"
        );
        ensure!(
            fs::read(&journal_path)? == journal_before,
            "rejected order changed the session journal"
        );
    }
    let still_live = exchange(&host.socket(), Command::List)?
        .views
        .into_iter()
        .find(|view| view.session == live.session)
        .context("session disappeared after reorder")?;
    ensure!(
        still_live.generation == live.generation
            && still_live.pid == live.pid
            && still_live.frame == live.frame
            && still_live.sequence == live.sequence,
        "metadata reorder changed session generation, PID, frame, or sequence"
    );

    let archived = {
        let mut value = workspace.clone();
        value.status = WorkspaceStatus::Archived;
        value.revision += 1;
        value
    };
    let archived_catalog = exchange(
        &host.socket(),
        Command::CatalogApply {
            batch: Box::new(CatalogBatch {
                workspaces: vec![CatalogWrite {
                    value: archived.clone(),
                    expected_revision: Some(workspace.revision),
                }],
                ..Default::default()
            }),
        },
    )?
    .catalog
    .context("archive catalog missing")?;
    ensure!(
        exchange_allow_error(
            &host.socket(),
            Command::SetTabOrder {
                workspace: workspace.id,
                expected_workspace_revision: archived.revision,
                expected_revision: order.revision,
                ordered_ids: desired.clone(),
            }
        )
        .is_err()
    );
    ensure!(
        exchange(&host.socket(), Command::Catalog)?.catalog.as_ref() == Some(&archived_catalog)
    );
    let after_archive_refusal = exchange(&host.socket(), Command::List)?.views;
    ensure!(
        after_archive_refusal.len() == 1
            && after_archive_refusal[0].generation == live.generation
            && after_archive_refusal[0].pid == live.pid
            && after_archive_refusal[0].frame == live.frame
            && fs::read(&journal_path)? == journal_before,
        "archived-workspace refusal changed the running generation or journal"
    );
    let mut failed = archived.clone();
    failed.status = WorkspaceStatus::Failed;
    failed.error = Some("fixture failure".into());
    failed.revision += 1;
    let failed_catalog = exchange(
        &host.socket(),
        Command::CatalogApply {
            batch: Box::new(CatalogBatch {
                workspaces: vec![CatalogWrite {
                    value: failed.clone(),
                    expected_revision: Some(archived.revision),
                }],
                ..Default::default()
            }),
        },
    )?
    .catalog
    .context("failed catalog missing")?;
    ensure!(
        exchange_allow_error(
            &host.socket(),
            Command::SetTabOrder {
                workspace: workspace.id,
                expected_workspace_revision: failed.revision,
                expected_revision: order.revision,
                ordered_ids: desired.clone(),
            }
        )
        .is_err()
    );
    ensure!(exchange(&host.socket(), Command::Catalog)?.catalog.as_ref() == Some(&failed_catalog));
    let after_failed_refusal = exchange(&host.socket(), Command::List)?.views;
    ensure!(
        after_failed_refusal.len() == 1
            && after_failed_refusal[0].generation == live.generation
            && after_failed_refusal[0].pid == live.pid
            && after_failed_refusal[0].frame == live.frame
            && fs::read(&journal_path)? == journal_before,
        "failed-workspace refusal changed the running generation or journal"
    );

    // Restore the workspace and exercise membership edits through their existing
    // atomic host commands: creating appends; closing removes exactly one ID.
    let current = exchange(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    let mut ready = current
        .workspaces
        .iter()
        .find(|row| row.id == workspace.id)
        .unwrap()
        .clone();
    ready.status = WorkspaceStatus::Ready;
    ready.error = None;
    ready.revision += 1;
    exchange(
        &host.socket(),
        Command::CatalogApply {
            batch: Box::new(CatalogBatch {
                workspaces: vec![CatalogWrite {
                    value: ready.clone(),
                    expected_revision: Some(ready.revision - 1),
                }],
                ..Default::default()
            }),
        },
    )?;
    let before_append = exchange(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    let appended = add_tab(&host.socket(), workspace.id, "Appended")?;
    let after_append = exchange(&host.socket(), Command::Catalog)?
        .catalog
        .context("catalog missing")?;
    let appended_order = after_append
        .tab_orders
        .as_ref()
        .unwrap()
        .iter()
        .find(|row| row.workspace_id == workspace.id)
        .unwrap();
    ensure!(appended_order.ordered_ids.last() == Some(&appended.id));
    ensure!(
        appended_order.revision
            == before_append
                .tab_orders
                .as_ref()
                .unwrap()
                .iter()
                .find(|row| row.workspace_id == workspace.id)
                .unwrap()
                .revision
                + 1
    );
    let close = exchange(
        &host.socket(),
        Command::CloseTab {
            tab: appended.id,
            expected_revision: appended.revision,
            expected_layout_revision: after_append.layout.revision,
            expected_selection_revision: after_append.selection.revision,
        },
    )?
    .catalog
    .context("close catalog missing")?;
    let closed_order = close
        .tab_orders
        .as_ref()
        .unwrap()
        .iter()
        .find(|row| row.workspace_id == workspace.id)
        .unwrap();
    ensure!(!closed_order.ordered_ids.contains(&appended.id));
    ensure!(closed_order.revision == appended_order.revision + 1);
    ensure!(close.selection.tab_id != Some(appended.id));

    // Stop only this test's owned fixture shell before host shutdown. Its
    // generation remains journaled as historical after reopening the host.
    exchange(
        &host.socket(),
        Command::Stop {
            session: live.session,
            generation: live.generation,
        },
    )?;
    host.active = None;
    host.restart()?;
    let reopened = exchange(&host.socket(), Command::Catalog)?
        .catalog
        .context("reopened catalog missing")?;
    ensure!(
        reopened
            .tab_orders
            .as_ref()
            .unwrap()
            .iter()
            .find(|row| row.workspace_id == workspace.id)
            .unwrap()
            .ordered_ids
            == closed_order.ordered_ids
    );
    let historical = exchange(
        &host.socket(),
        Command::Snapshot {
            session: live.session,
            generation: live.generation,
        },
    )?;
    if historical.error.is_some() {
        // A stopped generation can be addressed through the generation list and
        // snapshot query; preserve a hard assertion if the host's legacy Snapshot
        // shape rejects the explicit generation field.
        ensure!(false, "historical snapshot failed: {:?}", historical.error);
    }
    let final_view = historical
        .views
        .first()
        .context("historical generation missing")?;
    ensure!(
        final_view.generation == live.generation && final_view.frame == live.frame,
        "host reopen relaunched or changed the saved terminal frame"
    );
    Ok(())
}
