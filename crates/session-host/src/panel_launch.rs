//! Launch a CLI for a tab and place its pane as one ordered host operation.
//! Process launch and layout history remain separate durable systems: if the
//! second commit fails, the live generation is retained and reported as unplaced.
use std::{
    env,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use pigade_layout::{Command as LayoutCommand, Direction, LayoutHistory, LayoutModel};
use pigade_storage::{Catalog, WorkspaceStatus};
use pigade_terminal::Geometry;
use uuid::Uuid;

use crate::{Host, Launch, catalog};

#[derive(Debug)]
struct LaunchPreflightRejected;

impl std::fmt::Display for LaunchPreflightRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("launch rejected during preflight before spawn")
    }
}

impl std::error::Error for LaunchPreflightRejected {}

pub(crate) fn rejected_before_spawn(error: &anyhow::Error) -> bool {
    error.downcast_ref::<LaunchPreflightRejected>().is_some()
}

fn mark_preflight<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| error.context(LaunchPreflightRejected))
}

#[allow(clippy::too_many_arguments)] // Mirrors the stable LaunchInTab command fields.
pub(crate) fn launch_in_tab(
    host: &mut Host,
    tab_id: Uuid,
    provider: String,
    session: Uuid,
    generation: Uuid,
    operation: Uuid,
    side: Direction,
    target: Option<Uuid>,
) -> Result<Catalog> {
    let (executable, cwd, geometry, layout_revision, placement) = mark_preflight((|| {
        let executable = resolve_provider(&provider)?;
        let catalog_before = catalog::read_metadata(&host.metadata)?;
        let tab = catalog_before
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .context("target tab does not exist")?;
        let workspace = catalog_before
            .workspaces
            .iter()
            .find(|workspace| workspace.id == tab.workspace_id)
            .context("target tab has no workspace")?;
        ensure!(
            workspace.status == WorkspaceStatus::Ready,
            "target workspace is not ready"
        );
        ensure!(
            workspace.cwd.is_dir(),
            "target workspace directory is missing"
        );

        let layout_history: LayoutHistory = serde_json::from_str(&catalog_before.layout.json)
            .context("target placement history is unavailable")?;
        let model = LayoutModel::from_history(layout_history)
            .context("target placement history is invalid")?;
        let placement = LayoutCommand::Insert {
            tab: tab_id,
            pane: session,
            target,
            side,
            split: Uuid::new_v4(),
        };
        model
            .preview(placement.clone())
            .context("new pane cannot be placed in the selected tab")?;
        let geometry = Geometry::new(36, 120)?;
        Ok((
            executable,
            workspace.cwd.clone(),
            geometry,
            catalog_before.layout.revision,
            placement,
        ))
    })())?;

    let views = host
        .launch(Launch {
            session,
            generation,
            operation,
            executable,
            args: Vec::new(),
            cwd,
            geometry,
        })
        .context("launch was not confirmed; placement was not attempted; reconcile before retry")?;
    ensure!(
        views
            .iter()
            .any(|view| view.session == session && view.generation == generation),
        "launch returned no matching generation; placement was not attempted"
    );

    catalog::layout_edit(
        &mut host.metadata,
        layout_revision,
        crate::LayoutAction::Apply { command: placement },
    )
    .context(
        "process started successfully but placement failed; process and history remain live, reconcile before retry",
    )
}

fn resolve_provider(provider: &str) -> Result<PathBuf> {
    match provider {
        "shell" => {
            if let Some(shell) = env::var_os("SHELL")
                && let Some(path) = resolve_executable(Path::new(&shell))
            {
                return Ok(path);
            }
            find_on_path("bash")
                .or_else(|| find_on_path("sh"))
                .context("no executable shell found from SHELL, PATH bash, or PATH sh")
        }
        "claude" | "codex" | "opencode" => {
            find_on_path(provider).with_context(|| format!("provider {provider} is not on PATH"))
        }
        _ => bail!("unsupported CLI provider"),
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|directory| resolve_executable(&directory.join(name)))
}

fn resolve_executable(path: &Path) -> Option<PathBuf> {
    let canonical = path.canonicalize().ok()?;
    let metadata = canonical.metadata().ok()?;
    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then_some(canonical)
}

#[cfg(test)]
mod tests {
    use super::{mark_preflight, rejected_before_spawn, resolve_provider};
    use anyhow::{Context, Result, ensure};
    use pigade_layout::{Command as LayoutCommand, LayoutHistory, LayoutModel, TabLayout};
    use pigade_storage::{
        CatalogBatch, CatalogWrite, Project, Tab, Workspace, WorkspaceMode, WorkspaceStatus,
    };
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Child, Command as ProcessCommand, Stdio},
        thread,
        time::{Duration, Instant},
    };
    use uuid::Uuid;

    struct TestHost {
        _directory: tempfile::TempDir,
        root: PathBuf,
        socket: PathBuf,
        child: Child,
        launched: Option<(Uuid, Uuid)>,
    }

    impl TestHost {
        fn new() -> Result<Self> {
            let directory = tempfile::tempdir()?;
            let root = directory.path().canonicalize()?;
            let socket = root.join("host.sock");
            let binary = host_binary()?;
            let child = ProcessCommand::new(binary)
                .arg("serve")
                .arg(&root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(fs::File::create(root.join("host.log"))?)
                .spawn()?;
            let mut host = Self {
                _directory: directory,
                root,
                socket,
                child,
                launched: None,
            };
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                if let Some(status) = host.child.try_wait()? {
                    anyhow::bail!(
                        "host exited {status}: {}",
                        fs::read_to_string(host.root.join("host.log")).unwrap_or_default()
                    );
                }
                if call(&host.socket, crate::Command::List).is_ok() {
                    return Ok(host);
                }
                ensure!(Instant::now() < deadline, "host did not expose its socket");
                thread::sleep(Duration::from_millis(10));
            }
        }

        fn call(&self, command: crate::Command) -> Result<crate::Response> {
            call(&self.socket, command)
        }
    }

    impl Drop for TestHost {
        fn drop(&mut self) {
            if let Some((session, generation)) = self.launched {
                let _ = self.call(crate::Command::Stop {
                    session,
                    generation,
                });
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn host_binary() -> Result<PathBuf> {
        if let Some(binary) = option_env!("CARGO_BIN_EXE_pigade-session-host") {
            return Ok(PathBuf::from(binary));
        }
        let test_binary = std::env::current_exe()?;
        let binary = test_binary
            .parent()
            .and_then(Path::parent)
            .context("test executable is outside the Cargo target layout")?
            .join("pigade-session-host");
        ensure!(
            binary.is_file(),
            "host executable missing at {}",
            binary.display()
        );
        Ok(binary)
    }

    fn call(socket: &Path, command: crate::Command) -> Result<crate::Response> {
        let response = crate::request(socket, command)?;
        if let Some(error) = &response.error {
            anyhow::bail!("host: {error}");
        }
        Ok(response)
    }

    #[test]
    fn provider_preflight_is_marked_but_launch_uncertainty_is_not() {
        let preflight_error =
            mark_preflight(resolve_provider("unsupported-provider").map(|_| ())).unwrap_err();
        assert!(rejected_before_spawn(&preflight_error));

        let launch_error = anyhow::anyhow!(
            "launch was not confirmed; placement was not attempted; reconcile before retry"
        );
        assert!(!rejected_before_spawn(&launch_error));
    }

    #[test]
    fn one_shell_generation_stays_in_layout_through_move_and_focus() -> Result<()> {
        let mut host = TestHost::new()?;
        let root = host.root.clone();
        let project = Uuid::new_v4();
        let workspace = Uuid::new_v4();
        let tab = Uuid::new_v4();
        let other_tab = Uuid::new_v4();
        let pane = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let operation = Uuid::new_v4();
        host.call(crate::Command::CatalogApply {
            batch: CatalogBatch {
                projects: vec![CatalogWrite {
                    value: Project {
                        id: project,
                        canonical_path: root.clone(),
                        git_common_dir: root.clone(),
                        name: "panel-launch-test".into(),
                        roots: Vec::new(),
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                workspaces: vec![CatalogWrite {
                    value: Workspace {
                        id: workspace,
                        project_id: project,
                        task: "test".into(),
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
                        name: "Development".into(),
                        layout_json: serde_json::to_string(&TabLayout::default())?,
                        active_pane: None,
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            },
        })?;

        let placed = host.call(crate::Command::LaunchInTab {
            tab,
            provider: "shell".into(),
            session: pane,
            generation,
            operation,
            side: pigade_layout::Direction::Right,
            target: None,
        })?;
        host.launched = Some((pane, generation));
        let placed = placed.catalog.context("launch placement catalog missing")?;
        let history: LayoutHistory = serde_json::from_str(&placed.layout.json)?;
        let model = LayoutModel::from_history(history)?;
        assert_eq!(model.state().pane_tab(pane), Some(tab));
        let initial = host.call(crate::Command::List)?;
        assert_eq!(initial.views.len(), 1);
        assert_eq!(initial.views[0].session, pane);
        assert_eq!(initial.views[0].generation, generation);
        let pid = initial.views[0].pid;
        assert!(pid.is_some());
        assert_eq!(
            fs::read_dir(&host.root)?
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "journal"))
                .count(),
            1,
            "one launch must create one generation journal"
        );
        assert_eq!(
            host.call(crate::Command::Catalog)?
                .catalog
                .unwrap()
                .tabs
                .len(),
            1
        );

        host.call(crate::Command::CatalogApply {
            batch: CatalogBatch {
                tabs: vec![CatalogWrite {
                    value: Tab {
                        id: other_tab,
                        workspace_id: workspace,
                        name: "Other".into(),
                        layout_json: serde_json::to_string(&TabLayout::default())?,
                        active_pane: None,
                        revision: 0,
                    },
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            },
        })?;
        let placeholder = Uuid::new_v4();
        let catalog = host
            .call(crate::Command::Catalog)?
            .catalog
            .context("catalog missing")?;
        let catalog = host
            .call(crate::Command::LayoutEdit {
                expected_revision: catalog.layout.revision,
                action: crate::LayoutAction::Apply {
                    command: LayoutCommand::Insert {
                        tab: other_tab,
                        pane: placeholder,
                        target: None,
                        side: pigade_layout::Direction::Right,
                        split: Uuid::new_v4(),
                    },
                },
            })?
            .catalog
            .context("insert catalog missing")?;
        let catalog = host
            .call(crate::Command::LayoutEdit {
                expected_revision: catalog.layout.revision,
                action: crate::LayoutAction::Apply {
                    command: LayoutCommand::Move {
                        pane,
                        target: placeholder,
                        side: pigade_layout::Direction::Right,
                        split: Uuid::new_v4(),
                    },
                },
            })?
            .catalog
            .context("move catalog missing")?;
        let catalog = host
            .call(crate::Command::LayoutEdit {
                expected_revision: catalog.layout.revision,
                action: crate::LayoutAction::Apply {
                    command: LayoutCommand::Focus { pane },
                },
            })?
            .catalog
            .context("focus catalog missing")?;
        let history: LayoutHistory = serde_json::from_str(&catalog.layout.json)?;
        let model = LayoutModel::from_history(history)?;
        assert_eq!(model.state().pane_tab(pane), Some(other_tab));
        assert_eq!(model.state().tabs[&other_tab].focused, Some(pane));
        let after_layout = host.call(crate::Command::List)?;
        assert_eq!(after_layout.views.len(), 1);
        assert_eq!(after_layout.views[0].session, pane);
        assert_eq!(after_layout.views[0].generation, generation);
        assert_eq!(after_layout.views[0].pid, pid);
        let catalog = host
            .call(crate::Command::Catalog)?
            .catalog
            .context("catalog missing")?;
        assert_eq!(
            catalog
                .tabs
                .iter()
                .filter(|tab| tab.active_pane == Some(pane))
                .count(),
            1
        );

        // End the real PTY shell and wait for its actor to observe exit.
        host.call(crate::Command::Input {
            session: pane,
            generation,
            bytes: b"exit\n".to_vec(),
        })?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let views = host.call(crate::Command::List)?.views;
            if views.first().is_some_and(|view| view.state == "exited") {
                break;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "shell did not exit within five seconds"
            );
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }
}
