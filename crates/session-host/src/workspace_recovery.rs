//! Explicit, read-only recovery for interrupted workspace preparation.
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use pigade_layout::TabLayout;
use pigade_storage::{
    Catalog, CatalogBatch, CatalogWrite, MetadataStore, Project, Tab, Workspace, WorkspaceMode,
    WorkspaceStatus,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use uuid::Uuid;

const GIT_OUTPUT_LIMIT: usize = 256 * 1024;
const GIT_DEADLINE: Duration = Duration::from_secs(3);
const ERROR_LIMIT: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    workspace: Workspace,
    project: Project,
    state_root: PathBuf,
}

pub(crate) struct Completion {
    target: Target,
    result: std::result::Result<(), String>,
}

struct Task {
    job: crate::ipc_server::Job,
    target: Target,
}

pub(crate) struct Reader {
    sender: mpsc::SyncSender<Task>,
    active: Arc<AtomicBool>,
}

pub(crate) struct Activity(Arc<AtomicBool>);

impl Drop for Activity {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Reader {
    pub(crate) fn new(actor: mpsc::SyncSender<crate::ipc_server::Job>) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Task>(1);
        let active = Arc::new(AtomicBool::new(false));
        thread::Builder::new()
            .name("pigade-workspace-recovery".into())
            .spawn(move || {
                while let Ok(mut task) = receiver.recv() {
                    task.job.workspace_recovery = Some(verify(task.target));
                    if actor.send(task.job).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self { sender, active })
    }

    pub(crate) fn busy(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn submit(&self, mut job: crate::ipc_server::Job, target: Target) {
        if self
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let _ = job.reply.try_send(crate::ipc_server::error_response(
                "Проверка рабочей папки уже выполняется. Повторите попытку после завершения.",
                None,
            ));
            return;
        }
        job._workspace_recovery_activity = Some(Activity(self.active.clone()));
        if let Err(error) = self.sender.try_send(Task { job, target }) {
            let task = match error {
                mpsc::TrySendError::Full(task) | mpsc::TrySendError::Disconnected(task) => task,
            };
            let _ = task.job.reply.try_send(crate::ipc_server::error_response(
                "Проверка рабочей папки недоступна. Повторите попытку.",
                None,
            ));
        }
    }
}

/// Freeze the exact eligible metadata records before off-thread inspection.
pub(crate) fn capture(
    store: &MetadataStore,
    state_root: &Path,
    workspace_id: Uuid,
    expected_revision: u64,
) -> Result<Target> {
    let catalog = store.catalog()?;
    let workspace = catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .context("workspace does not exist")?;
    ensure!(
        workspace.revision == expected_revision,
        "workspace changed; refresh and retry"
    );
    ensure!(
        matches!(
            workspace.status,
            WorkspaceStatus::Preparing | WorkspaceStatus::Failed | WorkspaceStatus::MissingPath
        ),
        "only preparing or failed workspaces can be recovered"
    );
    let project = catalog
        .projects
        .iter()
        .find(|project| project.id == workspace.project_id)
        .context("workspace project is missing")?;
    Ok(Target {
        workspace: workspace.clone(),
        project: project.clone(),
        state_root: state_root.to_path_buf(),
    })
}

/// Inspect a frozen target without holding the metadata writer thread.
pub(crate) fn verify(target: Target) -> Completion {
    let result = validate(&target.workspace, &target.project, &target.state_root)
        .map_err(|error| safe_error(&format!("workspace recovery failed: {error:#}")));
    Completion { target, result }
}

/// Recheck exact metadata identity before publishing the Ready/Failed status.
pub(crate) fn finish(
    store: &mut MetadataStore,
    workspace_id: Uuid,
    expected_revision: u64,
    completion: Completion,
) -> Result<Catalog> {
    let target = completion.target;
    ensure!(
        target.workspace.id == workspace_id && target.workspace.revision == expected_revision,
        "workspace recovery completion belongs to another request"
    );
    let catalog = store.catalog()?;
    let current_workspace = catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .context("workspace changed during recovery")?;
    ensure!(
        current_workspace == &target.workspace,
        "workspace changed during recovery; refresh and retry"
    );
    let current_project = catalog
        .projects
        .iter()
        .find(|project| project.id == target.project.id)
        .context("workspace project changed during recovery")?;
    ensure!(
        current_project == &target.project,
        "workspace project changed during recovery; refresh and retry"
    );

    if let Err(message) = completion.result {
        let mut failed = target.workspace;
        failed.status = WorkspaceStatus::Failed;
        failed.error = Some(message);
        return store.apply_catalog(CatalogBatch {
            workspaces: vec![CatalogWrite {
                value: failed,
                expected_revision: Some(expected_revision),
            }],
            ..CatalogBatch::default()
        });
    }

    let mut ready = target.workspace;
    ready.status = WorkspaceStatus::Ready;
    ready.error = None;
    let mut batch = CatalogBatch {
        workspaces: vec![CatalogWrite {
            value: ready,
            expected_revision: Some(expected_revision),
        }],
        ..CatalogBatch::default()
    };
    if !catalog
        .tabs
        .iter()
        .any(|tab| tab.workspace_id == workspace_id)
    {
        batch.tabs.push(CatalogWrite {
            value: Tab {
                id: Uuid::new_v4(),
                workspace_id,
                name: "Development".into(),
                layout_json: serde_json::to_string(&TabLayout::default())?,
                active_pane: None,
                revision: 0,
            },
            expected_revision: None,
        });
    }
    crate::catalog::apply_metadata(store, batch)
}

/// Synchronous wrapper retained for focused unit tests.
pub(crate) fn recover(
    store: &mut MetadataStore,
    state_root: &Path,
    workspace_id: Uuid,
    expected_revision: u64,
) -> Result<Catalog> {
    let target = capture(store, state_root, workspace_id, expected_revision)?;
    finish(store, workspace_id, expected_revision, verify(target))
}

fn validate(workspace: &Workspace, project: &Project, state_root: &Path) -> Result<()> {
    let deadline = Instant::now() + GIT_DEADLINE;
    ensure!(
        project.canonical_path.is_dir(),
        "project directory is missing"
    );
    let project_root =
        fs::canonicalize(&project.canonical_path).context("cannot resolve project directory")?;
    ensure!(
        project_root == project.canonical_path,
        "project root is not canonical"
    );
    ensure!(
        crate::workspace_git::git_root_bounded(&project_root, deadline)? == project_root,
        "project path no longer identifies its Git root"
    );
    let common = crate::workspace_git::git_common_dir_bounded(&project_root, deadline)?;
    ensure!(
        common == project.git_common_dir,
        "project Git identity changed"
    );

    match workspace.mode {
        WorkspaceMode::SharedCheckout => {
            ensure!(
                workspace.cwd == project_root,
                "shared checkout path changed"
            );
            ensure!(
                workspace.cwd.is_dir(),
                "shared checkout directory is missing"
            );
            ensure!(
                fs::canonicalize(&workspace.cwd)? == project_root,
                "shared checkout resolves outside the project root"
            );
            ensure!(
                crate::workspace_git::git_root_bounded(&workspace.cwd, deadline)? == project_root,
                "shared checkout no longer identifies the project root"
            );
            ensure!(
                crate::workspace_git::git_common_dir_bounded(&workspace.cwd, deadline)? == common,
                "shared checkout Git identity changed"
            );
        }
        WorkspaceMode::Worktree => {
            let state_root = fs::canonicalize(state_root).context("cannot resolve state root")?;
            ensure!(state_root.is_dir(), "state root is not a directory");
            let worktrees = state_root.join("worktrees");
            ensure_real_dir(&worktrees, "managed worktree directory")?;
            let expected = worktrees.join(workspace.id.to_string());
            ensure!(
                workspace.cwd == expected,
                "workspace path is not its managed path"
            );
            ensure_real_dir(&expected, "managed workspace directory")?;
            let root = crate::workspace_git::git_root_bounded(&expected, deadline)?;
            ensure!(
                root == expected,
                "workspace Git root does not match its managed path"
            );
            ensure!(
                crate::workspace_git::git_common_dir_bounded(&root, deadline)? == common,
                "workspace belongs to a different Git repository"
            );
            ensure_linked_worktree_pointer(&root, &common)?;
            ensure_registered_worktree(&project_root, &expected, deadline)?;
        }
    }
    Ok(())
}

fn ensure_real_dir(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).with_context(|| format!("{label} is missing"))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "{label} is not a real directory"
    );
    Ok(())
}

fn ensure_linked_worktree_pointer(root: &Path, common: &Path) -> Result<()> {
    let pointer = root.join(".git");
    let metadata = fs::symlink_metadata(&pointer).context("workspace Git pointer is missing")?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "workspace is not a linked worktree"
    );
    let text = read_pointer_file(&pointer, "workspace Git pointer")?;
    let target = text
        .strip_prefix("gitdir: ")
        .map(strip_line_ending)
        .context("workspace Git pointer has invalid content")?;
    let target = PathBuf::from(target);
    let target = if target.is_absolute() {
        target
    } else {
        root.join(target)
    };
    let git_dir = fs::canonicalize(target).context("workspace Git pointer target is missing")?;
    let expected_parent = fs::canonicalize(common.join("worktrees"))
        .context("Git linked-worktree directory is missing")?;
    ensure!(
        git_dir.parent() == Some(expected_parent.as_path()),
        "workspace Git pointer is not registered under this repository"
    );
    ensure_real_dir(&git_dir, "linked-worktree Git directory")?;
    let back_reference = read_pointer_file(
        &git_dir.join("gitdir"),
        "worktree registration back-reference",
    )?;
    let back_reference = PathBuf::from(strip_line_ending(&back_reference));
    let back_reference = if back_reference.is_absolute() {
        back_reference
    } else {
        git_dir.join(back_reference)
    };
    ensure!(
        fs::canonicalize(back_reference)? == pointer,
        "worktree registration does not point back to this workspace"
    );
    let commondir = read_pointer_file(
        &git_dir.join("commondir"),
        "linked-worktree common-directory pointer",
    )?;
    let commondir = PathBuf::from(strip_line_ending(&commondir));
    let commondir = if commondir.is_absolute() {
        commondir
    } else {
        git_dir.join(commondir)
    };
    ensure!(
        fs::canonicalize(commondir)? == common,
        "linked-worktree pointer resolves to another repository"
    );
    Ok(())
}

fn read_pointer_file(path: &Path, label: &str) -> Result<String> {
    const POINTER_LIMIT: u64 = 16 * 1024;
    let metadata = fs::symlink_metadata(path).with_context(|| format!("{label} is missing"))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= POINTER_LIMIT,
        "{label} is not a bounded regular file"
    );
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("cannot open {label}"))?;
    let opened = file.metadata()?;
    ensure!(
        opened.is_file() && opened.len() <= POINTER_LIMIT,
        "{label} changed during inspection"
    );
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.take(POINTER_LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= POINTER_LIMIT,
        "{label} exceeded its size bound"
    );
    String::from_utf8(bytes).with_context(|| format!("{label} is not UTF-8"))
}

fn strip_line_ending(value: &str) -> &str {
    value
        .strip_suffix("\r\n")
        .or_else(|| value.strip_suffix('\n'))
        .unwrap_or(value)
}

fn ensure_registered_worktree(
    project_root: &Path,
    expected: &Path,
    deadline: Instant,
) -> Result<()> {
    let (bytes, incomplete) = crate::workspace_git::run_git_capture(
        project_root,
        &["worktree", "list", "--porcelain", "-z"],
        GIT_OUTPUT_LIMIT,
        deadline,
        "git worktree list",
    )?;
    ensure!(!incomplete, "Git worktree listing exceeded its bound");
    let expected = expected.as_os_str().as_bytes();
    let mut found = false;
    for field in bytes.split(|byte| *byte == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            ensure!(
                !found || path != expected,
                "duplicate managed worktree registration"
            );
            found |= path == expected;
        }
    }
    ensure!(
        found,
        "managed workspace is not registered with its Git repository"
    );
    Ok(())
}

fn safe_error(message: &str) -> String {
    let cleaned: String = message
        .chars()
        .filter(|ch| !ch.is_control() || *ch == ' ')
        .collect();
    if cleaned.len() <= ERROR_LIMIT {
        cleaned
    } else {
        let mut end = ERROR_LIMIT;
        while !cleaned.is_char_boundary(end) {
            end -= 1;
        }
        cleaned[..end].to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn git(path: &Path, args: &[&str]) -> Result<()> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()?;
        ensure!(output.status.success(), "git fixture command failed");
        Ok(())
    }

    fn interrupted(worktree: bool) -> Result<(TempDir, MetadataStore, PathBuf, Uuid, u64)> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("repo");
        fs::create_dir(&repo)?;
        git(&repo, &["init", "--quiet"])?;
        git(&repo, &["config", "user.name", "Recovery Test"])?;
        git(&repo, &["config", "user.email", "recovery@example.invalid"])?;
        fs::write(repo.join("tracked.txt"), "base\n")?;
        git(&repo, &["add", "tracked.txt"])?;
        git(&repo, &["commit", "--quiet", "-m", "base"])?;
        let state = temp.path().join("state");
        fs::create_dir(&state)?;
        let store_path = temp.path().join("metadata.sqlite");
        let mut store = MetadataStore::open(store_path)?;
        let project_catalog = crate::projects::add_project(&mut store, repo.clone())?;
        let project = &project_catalog.projects[0];
        let id = Uuid::new_v4();
        let cwd = if worktree {
            let parent = state.join("worktrees");
            fs::create_dir(&parent)?;
            let cwd = parent.join(id.to_string());
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "--detach",
                    cwd.to_str().unwrap(),
                ],
            )?;
            cwd
        } else {
            repo
        };
        let saved = store.apply_catalog(CatalogBatch {
            workspaces: vec![CatalogWrite {
                value: pigade_storage::Workspace {
                    id,
                    project_id: project.id,
                    task: "recover".into(),
                    cwd,
                    mode: if worktree {
                        WorkspaceMode::Worktree
                    } else {
                        WorkspaceMode::SharedCheckout
                    },
                    status: WorkspaceStatus::Preparing,
                    error: None,
                    context_revision: None,
                    revision: 0,
                },
                expected_revision: None,
            }],
            ..CatalogBatch::default()
        })?;
        let revision = saved.workspaces[0].revision;
        Ok((temp, store, state, id, revision))
    }

    #[test]
    fn recovers_interrupted_managed_worktree_without_touching_dirty_files_or_selection()
    -> Result<()> {
        let (temp, mut store, state, id, revision) = interrupted(true)?;
        let before = store.catalog()?;
        let dirty = before
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .unwrap()
            .cwd
            .join("user.txt");
        fs::write(&dirty, "keep exactly\n")?;
        let recovered = recover(&mut store, &state, id, revision)?;
        assert_eq!(recovered.workspaces[0].status, WorkspaceStatus::Ready);
        assert_eq!(recovered.selection, before.selection);
        assert_eq!(fs::read_to_string(dirty)?, "keep exactly\n");
        assert_eq!(
            recovered
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == id)
                .count(),
            1
        );
        drop(store);
        let mut store = MetadataStore::open(temp.path().join("metadata.sqlite"))?;
        let reopened = store.catalog()?;
        assert_eq!(reopened.tabs.len(), 1);
        assert!(recover(&mut store, &state, id, recovered.workspaces[0].revision).is_err());
        assert_eq!(store.catalog()?.tabs.len(), 1);
        Ok(())
    }

    #[test]
    fn missing_managed_worktree_is_recorded_as_failed_without_removing_metadata() -> Result<()> {
        let (_temp, mut store, state, id, revision) = interrupted(true)?;
        let before = store.catalog()?;
        let cwd = before
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .unwrap()
            .cwd
            .clone();
        let mut missing_path = before.workspaces[0].clone();
        missing_path.status = WorkspaceStatus::MissingPath;
        missing_path.error = Some("directory unavailable".into());
        let before = store.apply_catalog(CatalogBatch {
            workspaces: vec![CatalogWrite {
                value: missing_path,
                expected_revision: Some(revision),
            }],
            ..CatalogBatch::default()
        })?;
        fs::remove_dir_all(&cwd)?;
        let failed = recover(&mut store, &state, id, before.workspaces[0].revision)?;
        let workspace = failed
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .unwrap();
        assert_eq!(workspace.status, WorkspaceStatus::Failed);
        assert!(
            workspace
                .error
                .as_deref()
                .is_some_and(|error| error.contains("managed workspace directory"))
        );
        assert!(failed.tabs.is_empty());
        assert_eq!(fs::read_dir(state.join("worktrees"))?.count(), 0);
        Ok(())
    }

    #[test]
    fn recovers_shared_checkout_and_rejects_stale_revision() -> Result<()> {
        let (_temp, mut store, state, id, revision) = interrupted(false)?;
        assert!(recover(&mut store, &state, id, revision + 1).is_err());
        let recovered = recover(&mut store, &state, id, revision)?;
        assert_eq!(recovered.workspaces[0].status, WorkspaceStatus::Ready);
        let mut archived = recovered.workspaces[0].clone();
        archived.status = WorkspaceStatus::Archived;
        let archived_catalog = store.apply_catalog(CatalogBatch {
            workspaces: vec![CatalogWrite {
                value: archived,
                expected_revision: Some(recovered.workspaces[0].revision),
            }],
            ..CatalogBatch::default()
        })?;
        let archived_revision = archived_catalog.workspaces[0].revision;
        assert!(recover(&mut store, &state, id, archived_revision).is_err());
        let after = store.catalog()?;
        assert_eq!(after.workspaces[0].status, WorkspaceStatus::Archived);
        assert_eq!(after.workspaces[0].revision, archived_revision);
        Ok(())
    }

    #[test]
    fn existing_tabs_are_preserved_without_adding_a_duplicate() -> Result<()> {
        let (_temp, mut store, state, id, revision) = interrupted(false)?;
        let pane = Uuid::new_v4();
        let existing_tab = Tab {
            id: Uuid::new_v4(),
            workspace_id: id,
            name: "Review (renamed)".into(),
            layout_json: serde_json::to_string(&TabLayout::default())?,
            active_pane: None,
            revision: 0,
        };
        let with_tab = crate::catalog::apply_metadata(
            &mut store,
            CatalogBatch {
                tabs: vec![CatalogWrite {
                    value: existing_tab.clone(),
                    expected_revision: None,
                }],
                ..CatalogBatch::default()
            },
        )?;
        let with_pane = crate::catalog::layout_edit(
            &mut store,
            with_tab.layout.revision,
            crate::catalog::LayoutAction::Apply {
                command: pigade_layout::Command::Insert {
                    tab: existing_tab.id,
                    pane,
                    target: None,
                    side: pigade_layout::Direction::Right,
                    split: Uuid::new_v4(),
                },
            },
        )?;
        let recovered = recover(&mut store, &state, id, revision)?;
        assert_eq!(recovered.tabs.len(), 1);
        assert_eq!(recovered.tabs[0].id, existing_tab.id);
        assert_eq!(recovered.tabs[0].name, existing_tab.name);
        assert_eq!(recovered.tabs[0].layout_json, with_pane.tabs[0].layout_json);
        assert_eq!(recovered.tabs[0].active_pane, Some(pane));
        assert_eq!(recovered.selection, with_pane.selection);
        assert_eq!(recovered.layout.revision, with_pane.layout.revision);
        Ok(())
    }

    #[test]
    fn foreign_workspace_path_and_symlink_are_recorded_as_failed() -> Result<()> {
        let (_temp, mut store, state, id, revision) = interrupted(true)?;
        let current = store.catalog()?;
        let mut foreign = current.workspaces[0].clone();
        foreign.cwd = current.projects[0].canonical_path.clone();
        let foreign = store.apply_catalog(CatalogBatch {
            workspaces: vec![CatalogWrite {
                value: foreign,
                expected_revision: Some(revision),
            }],
            ..CatalogBatch::default()
        })?;
        let failed = recover(&mut store, &state, id, foreign.workspaces[0].revision)?;
        assert_eq!(failed.workspaces[0].status, WorkspaceStatus::Failed);
        assert!(failed.tabs.is_empty());

        let (_temp, mut store, state, id, revision) = interrupted(true)?;
        let current = store.catalog()?;
        let cwd = current.workspaces[0].cwd.clone();
        let target = current.projects[0].canonical_path.clone();
        fs::remove_dir_all(&cwd)?;
        std::os::unix::fs::symlink(target, &cwd)?;
        let failed = recover(&mut store, &state, id, revision)?;
        assert_eq!(failed.workspaces[0].status, WorkspaceStatus::Failed);
        assert!(fs::symlink_metadata(cwd)?.file_type().is_symlink());
        assert!(failed.tabs.is_empty());
        Ok(())
    }

    #[test]
    fn copied_worktree_pointer_must_have_a_matching_registration_back_reference() -> Result<()> {
        let (_temp, _store, state, id, _revision) = interrupted(true)?;
        let current = fs::read_dir(state.join("worktrees"))?
            .next()
            .context("managed worktree missing")??
            .path();
        let repo = state.parent().unwrap().join("repo");
        let foreign = state.parent().unwrap().join("foreign-worktree");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                foreign.to_str().unwrap(),
            ],
        )?;
        fs::copy(foreign.join(".git"), current.join(".git"))?;
        let common = crate::projects::git_common_dir(&repo)?;
        assert!(ensure_linked_worktree_pointer(&current, &common).is_err());
        assert_eq!(
            current.file_name().unwrap().to_str().unwrap(),
            id.to_string()
        );
        Ok(())
    }

    #[test]
    fn finish_refuses_workspace_or_project_changes_after_capture_without_writing() -> Result<()> {
        let (_temp, mut store, state, id, revision) = interrupted(false)?;
        let completion = verify(capture(&store, &state, id, revision)?);
        let catalog = store.catalog()?;
        let mut changed_workspace = catalog.workspaces[0].clone();
        changed_workspace.task.push_str(" updated");
        let changed = store.apply_catalog(CatalogBatch {
            workspaces: vec![CatalogWrite {
                value: changed_workspace,
                expected_revision: Some(revision),
            }],
            ..CatalogBatch::default()
        })?;
        assert!(finish(&mut store, id, revision, completion).is_err());
        let after = store.catalog()?;
        assert_eq!(after.workspaces[0], changed.workspaces[0]);
        assert_eq!(after.tabs.len(), catalog.tabs.len());

        let (_temp, mut store, state, id, revision) = interrupted(false)?;
        let completion = verify(capture(&store, &state, id, revision)?);
        let catalog = store.catalog()?;
        let mut changed_project = catalog.projects[0].clone();
        changed_project.name.push_str(" updated");
        let changed = store.apply_catalog(CatalogBatch {
            projects: vec![CatalogWrite {
                value: changed_project,
                expected_revision: Some(catalog.projects[0].revision),
            }],
            ..CatalogBatch::default()
        })?;
        assert!(finish(&mut store, id, revision, completion).is_err());
        let after = store.catalog()?;
        assert_eq!(after.projects[0], changed.projects[0]);
        assert_eq!(after.workspaces[0].status, WorkspaceStatus::Preparing);
        assert_eq!(after.workspaces[0].revision, revision);
        assert_eq!(after.tabs.len(), catalog.tabs.len());
        Ok(())
    }
}
