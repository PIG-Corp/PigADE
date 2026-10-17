//! Git-backed project discovery and workspace creation.
//!
//! Git is invoked with fixed argument vectors (never through a shell). The
//! metadata store remains the sole durable catalog writer.
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail, ensure};
use pigade_layout::TabLayout;
use pigade_storage::{
    Catalog, CatalogBatch, CatalogWrite, MetadataStore, Project, Selection, Tab, Workspace,
    WorkspaceMode, WorkspaceStatus,
};
use uuid::Uuid;

use crate::catalog::apply_metadata;

const MAX_TASK_BYTES: usize = 64 * 1024;
const MAX_ERROR_BYTES: usize = 16 * 1024;

/// Discover the containing Git checkout and add or select its common project.
/// Different worktrees of the same repository resolve to one project identity.
pub(crate) fn add_project(store: &mut MetadataStore, path: PathBuf) -> Result<Catalog> {
    let root = git_root(&path)?;
    let common_dir = git_common_dir(&root)?;
    let mut catalog = store.catalog()?;
    if let Some(existing) = catalog
        .projects
        .iter()
        .find(|project| project.git_common_dir == common_dir)
        .cloned()
    {
        let workspace = catalog
            .workspaces
            .iter()
            .find(|workspace| {
                workspace.project_id == existing.id && workspace.status == WorkspaceStatus::Ready
            })
            .map(|workspace| workspace.id);
        let tab = workspace.and_then(|workspace_id| {
            catalog
                .tabs
                .iter()
                .find(|tab| tab.workspace_id == workspace_id)
                .map(|tab| tab.id)
        });
        catalog.selection.project_id = Some(existing.id);
        catalog.selection.workspace_id = workspace;
        catalog.selection.tab_id = tab;
        let selection_revision = catalog.selection.revision;
        return apply_metadata(
            store,
            CatalogBatch {
                selection: Some(CatalogWrite {
                    value: catalog.selection,
                    expected_revision: Some(selection_revision),
                }),
                ..CatalogBatch::default()
            },
        );
    }

    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Project")
        .to_owned();
    let project = Project {
        id: Uuid::new_v4(),
        canonical_path: root,
        git_common_dir: common_dir,
        name,
        roots: Vec::new(),
        revision: 0,
    };
    let selection_revision = catalog.selection.revision;
    let selection = Selection {
        project_id: Some(project.id),
        workspace_id: None,
        tab_id: None,
        ..catalog.selection
    };
    apply_metadata(
        store,
        CatalogBatch {
            projects: vec![CatalogWrite {
                value: project,
                expected_revision: None,
            }],
            selection: Some(CatalogWrite {
                value: selection,
                expected_revision: Some(selection_revision),
            }),
            ..CatalogBatch::default()
        },
    )
}

/// Create a new, empty Git project directory below a user-selected parent.
/// The directory is retained if `git init` or catalog registration fails so
/// the user can inspect and recover the partial setup.
pub(crate) fn create_project(
    store: &mut MetadataStore,
    parent: PathBuf,
    name: String,
) -> Result<Catalog> {
    validate_project_name(&name)?;
    let parent = fs::canonicalize(&parent)
        .with_context(|| format!("cannot resolve project parent {}", parent.display()))?;
    ensure!(parent.is_dir(), "project parent is not a directory");
    let path = parent.join(&name);
    fs::create_dir(&path)
        .with_context(|| format!("cannot create new project directory {}", path.display()))?;
    let output = Command::new("git")
        .arg("-C")
        .arg(&path)
        .args(["init", "--initial-branch=main"])
        .output()
        .context("failed to start git init")?;
    if !output.status.success() {
        bail!(
            "git init failed ({}): {}",
            output.status,
            bounded_error(String::from_utf8_lossy(&output.stderr).trim())
        );
    }
    add_project(store, path)
}

fn validate_project_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 96,
        "project name must contain 1 to 96 UTF-8 bytes"
    );
    ensure!(
        name != "."
            && name != ".."
            && !name.contains('/')
            && !name.contains('\\')
            && !name.chars().any(char::is_control),
        "project name must be one safe directory name"
    );
    Ok(())
}

/// Create a shared-checkout workspace or a dedicated Git worktree.
/// The Preparing row is committed before `git worktree add`; failures remain
/// visible as Failed and never fall back to the shared project directory.
pub(crate) fn create_workspace(
    store: &mut MetadataStore,
    state_root: &Path,
    project_id: Uuid,
    task: String,
    worktree: bool,
) -> Result<Catalog> {
    ensure!(
        !task.trim().is_empty() && task.len() <= MAX_TASK_BYTES && !task.contains('\0'),
        "workspace task is empty or exceeds the 64 KiB bound"
    );
    let before = store.catalog()?;
    let project = before
        .projects
        .iter()
        .find(|project| project.id == project_id)
        .cloned()
        .context("project does not exist")?;
    validate_project(&project)?;

    let id = Uuid::new_v4();
    let mode = if worktree {
        WorkspaceMode::Worktree
    } else {
        WorkspaceMode::SharedCheckout
    };
    let cwd = if worktree {
        managed_worktree_path(state_root, id)?
    } else {
        project.canonical_path.clone()
    };
    if worktree {
        ensure!(!cwd.exists(), "workspace target already exists");
    } else {
        ensure!(cwd.is_dir(), "shared checkout path is not a directory");
    }
    let preparing = Workspace {
        id,
        project_id,
        task,
        cwd: cwd.clone(),
        mode,
        status: WorkspaceStatus::Preparing,
        error: None,
        context_revision: None,
        revision: 0,
    };
    apply_metadata(
        store,
        CatalogBatch {
            workspaces: vec![CatalogWrite {
                value: preparing,
                expected_revision: None,
            }],
            ..CatalogBatch::default()
        },
    )?;

    if worktree {
        let branch = format!("pigade/{id}");
        if let Err(error) = git_worktree_add(&project.canonical_path, &cwd, &branch) {
            let message = bounded_error(&format!("worktree creation failed: {error:#}"));
            return fail_preparing(store, id, message);
        }
        let validation = (|| -> Result<()> {
            ensure!(
                cwd.is_dir(),
                "git did not create the requested worktree path"
            );
            let root = git_root(&cwd)?;
            ensure!(root == cwd, "Git worktree resolved to an unexpected root");
            ensure!(
                git_common_dir(&root)? == project.git_common_dir,
                "created worktree has a different Git common directory"
            );
            Ok(())
        })();
        if let Err(error) = validation {
            let message = bounded_error(&format!("created worktree validation failed: {error:#}"));
            return fail_preparing(store, id, message);
        }
    }

    let current = store.catalog()?;
    let workspace = current
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .context("preparing workspace disappeared")?;
    let mut ready = workspace.clone();
    ready.status = WorkspaceStatus::Ready;
    ready.error = None;
    let tab = Tab {
        id: Uuid::new_v4(),
        workspace_id: id,
        name: "Development".into(),
        layout_json: serde_json::to_string(&TabLayout::default())?,
        active_pane: None,
        revision: 0,
    };
    let selection = Selection {
        project_id: Some(project_id),
        workspace_id: Some(id),
        tab_id: Some(tab.id),
        ..current.selection.clone()
    };
    match apply_metadata(
        store,
        CatalogBatch {
            workspaces: vec![CatalogWrite {
                value: ready,
                expected_revision: Some(workspace.revision),
            }],
            tabs: vec![CatalogWrite {
                value: tab,
                expected_revision: None,
            }],
            selection: Some(CatalogWrite {
                value: selection,
                expected_revision: Some(current.selection.revision),
            }),
            ..CatalogBatch::default()
        },
    ) {
        Ok(catalog) => Ok(catalog),
        Err(error) => {
            let reason = bounded_error(&format!(
                "workspace setup completed but catalog finalization failed: {error:#}"
            ));
            fail_preparing(store, id, reason)
        }
    }
}

fn fail_preparing(store: &mut MetadataStore, id: Uuid, reason: String) -> Result<Catalog> {
    let catalog = store.catalog()?;
    let workspace = catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .context("preparing workspace disappeared before failure was recorded")?;
    let mut failed = workspace.clone();
    failed.status = WorkspaceStatus::Failed;
    failed.error = Some(reason);
    apply_metadata(
        store,
        CatalogBatch {
            workspaces: vec![CatalogWrite {
                value: failed,
                expected_revision: Some(workspace.revision),
            }],
            ..CatalogBatch::default()
        },
    )
}

fn validate_project(project: &Project) -> Result<()> {
    ensure!(
        project.canonical_path.is_dir(),
        "project path is missing or is not a directory"
    );
    let root = git_root(&project.canonical_path)?;
    ensure!(
        root == project.canonical_path,
        "project path is no longer the Git root"
    );
    ensure!(
        git_common_dir(&root)? == project.git_common_dir,
        "project Git identity changed; re-add or repair the project"
    );
    Ok(())
}

fn git_root(path: &Path) -> Result<PathBuf> {
    let canonical_input = fs::canonicalize(path)
        .with_context(|| format!("cannot resolve project path {}", path.display()))?;
    ensure!(canonical_input.is_dir(), "project path is not a directory");
    let output = git_output(&canonical_input, &["rev-parse", "--show-toplevel"])?;
    let root = output_path(&output, "Git repository root")?;
    let root = fs::canonicalize(root).context("cannot resolve Git repository root")?;
    ensure!(root.is_dir(), "Git repository root is not a directory");
    Ok(root)
}

fn git_common_dir(root: &Path) -> Result<PathBuf> {
    let output = git_output(root, &["rev-parse", "--git-common-dir"])?;
    let common = output_path(&output, "Git common directory")?;
    let common = if common.is_absolute() {
        common
    } else {
        root.join(common)
    };
    fs::canonicalize(common).context("cannot resolve Git common directory")
}

fn output_path(output: &Output, label: &str) -> Result<PathBuf> {
    let text = std::str::from_utf8(&output.stdout)
        .with_context(|| format!("{label} is not UTF-8"))?
        .trim();
    ensure!(!text.is_empty() && !text.contains('\0'), "{label} is empty");
    Ok(PathBuf::from(text))
}

fn git_output(path: &Path, args: &[&str]) -> Result<Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .with_context(|| format!("failed to start git {}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git {} failed ({}): {}",
            args.join(" "),
            output.status,
            bounded_error(stderr.trim())
        );
    }
    Ok(output)
}

fn git_worktree_add(root: &Path, target: &Path, branch: &str) -> Result<()> {
    // `--quiet` keeps successful output bounded. Absolute managed target paths
    // and generated branch names are passed as individual argv entries.
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["worktree", "add", "--quiet", "-b", branch])
        .arg(target)
        .arg("HEAD")
        .output()
        .context("failed to start git worktree add")?;
    if !output.status.success() {
        bail!(
            "git worktree add failed ({}): {}",
            output.status,
            bounded_error(String::from_utf8_lossy(&output.stderr).trim())
        );
    }
    Ok(())
}

fn managed_worktree_path(state_root: &Path, id: Uuid) -> Result<PathBuf> {
    let state_root = fs::canonicalize(state_root).context("cannot resolve state root")?;
    ensure!(state_root.is_dir(), "state root is not a directory");
    let worktrees = state_root.join("worktrees");
    match fs::symlink_metadata(&worktrees) {
        Ok(metadata) => ensure!(
            metadata.file_type().is_dir(),
            "managed worktrees path is not a real directory"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(false)
                .mode(0o700)
                .create(&worktrees)
                .context("cannot create private managed worktrees directory")?;
        }
        Err(error) => return Err(error).context("cannot inspect managed worktrees directory"),
    }
    let canonical_worktrees = fs::canonicalize(&worktrees)?;
    ensure!(
        canonical_worktrees == worktrees,
        "managed worktrees directory resolves outside state root"
    );
    Ok(canonical_worktrees.join(id.to_string()))
}

fn bounded_error(error: &str) -> String {
    let mut result = String::new();
    for character in error.chars().filter(|character| *character != '\0') {
        if result.len() + character.len_utf8() > MAX_ERROR_BYTES {
            break;
        }
        result.push(character);
    }
    if result.is_empty() {
        result.push_str("Git operation failed");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tempfile::TempDir;

    fn git(path: &Path, args: &[&str]) -> Result<()> {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()?;
        ensure!(output.status.success(), "test git command failed");
        Ok(())
    }

    fn initialized_repo(parent: &Path, commit: bool) -> Result<PathBuf> {
        let path = parent.join("repo");
        fs::create_dir(&path)?;
        let output = Command::new("git")
            .args(["init", "--quiet"])
            .arg(&path)
            .output()?;
        ensure!(output.status.success(), "git init failed");
        if commit {
            git(
                &path,
                &[
                    "-c",
                    "user.name=PigADE test",
                    "-c",
                    "user.email=pigade-test@example.invalid",
                    "commit",
                    "--allow-empty",
                    "--quiet",
                    "-m",
                    "initial",
                ],
            )?;
        }
        Ok(path)
    }

    fn store(root: &TempDir) -> Result<MetadataStore> {
        MetadataStore::open(root.path().join("metadata.sqlite"))
    }

    #[test]
    fn add_project_discovers_root_deduplicates_common_dir_and_selects_existing() -> Result<()> {
        let root = TempDir::new()?;
        let repo = initialized_repo(root.path(), true)?;
        let mut store = store(&root)?;
        let first = add_project(&mut store, repo.join("."))?;
        let project = first.projects.first().context("project missing")?;
        assert_eq!(project.canonical_path, fs::canonicalize(&repo)?);
        assert_eq!(first.selection.project_id, Some(project.id));

        let branch = "existing-worktree";
        let second_path = root.path().join("existing-worktree");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                branch,
                second_path.to_str().unwrap(),
            ],
        )?;
        let second = add_project(&mut store, second_path)?;
        assert_eq!(second.projects.len(), 1);
        assert_eq!(second.projects[0].id, project.id);
        assert_eq!(second.selection.project_id, Some(project.id));
        Ok(())
    }

    #[test]
    fn worktree_workspace_is_prepared_then_ready_with_empty_development_tab() -> Result<()> {
        let root = TempDir::new()?;
        let repo = initialized_repo(root.path(), true)?;
        let state = root.path().join("state");
        fs::create_dir(&state)?;
        let mut store = store(&root)?;
        let catalog = add_project(&mut store, repo)?;
        let project_id = catalog.projects[0].id;

        let catalog = create_workspace(
            &mut store,
            &state,
            project_id,
            "Implement the feature".into(),
            true,
        )?;
        let workspace = catalog.workspaces.first().context("workspace missing")?;
        assert_eq!(workspace.status, WorkspaceStatus::Ready);
        assert_eq!(workspace.mode, WorkspaceMode::Worktree);
        assert_eq!(workspace.revision, 2);
        assert!(workspace.cwd.is_dir());
        assert_eq!(catalog.tabs.len(), 1);
        assert_eq!(catalog.tabs[0].name, "Development");
        assert_eq!(catalog.tabs[0].workspace_id, workspace.id);
        assert_eq!(catalog.selection.workspace_id, Some(workspace.id));
        assert_eq!(
            serde_json::from_str::<TabLayout>(&catalog.tabs[0].layout_json)?,
            TabLayout::default()
        );
        assert!(workspace.cwd.starts_with(state.join("worktrees")));
        Ok(())
    }

    #[test]
    fn failed_worktree_creation_stays_visible_as_failed_without_shared_fallback() -> Result<()> {
        let root = TempDir::new()?;
        let repo = initialized_repo(root.path(), false)?;
        let state = root.path().join("state");
        fs::create_dir(&state)?;
        let mut store = store(&root)?;
        let catalog = add_project(&mut store, repo.clone())?;
        let project_id = catalog.projects[0].id;

        let catalog = create_workspace(
            &mut store,
            &state,
            project_id,
            "No commit exists".into(),
            true,
        )?;
        let workspace = catalog.workspaces.first().context("workspace missing")?;
        assert_eq!(workspace.status, WorkspaceStatus::Failed);
        assert_eq!(workspace.mode, WorkspaceMode::Worktree);
        assert_ne!(workspace.cwd, fs::canonicalize(repo)?);
        assert!(
            workspace
                .error
                .as_deref()
                .is_some_and(|error| !error.is_empty())
        );
        assert!(catalog.tabs.is_empty());
        assert!(catalog.selection.workspace_id.is_none());
        Ok(())
    }

    #[test]
    fn shared_checkout_workspace_uses_validated_project_root() -> Result<()> {
        let root = TempDir::new()?;
        let repo = initialized_repo(root.path(), true)?;
        let state = root.path().join("state");
        fs::create_dir(&state)?;
        let mut store = store(&root)?;
        let catalog = add_project(&mut store, repo.clone())?;

        let catalog = create_workspace(
            &mut store,
            &state,
            catalog.projects[0].id,
            "Review".into(),
            false,
        )?;
        let workspace = catalog.workspaces.first().context("workspace missing")?;
        assert_eq!(workspace.cwd, fs::canonicalize(repo)?);
        assert_eq!(workspace.status, WorkspaceStatus::Ready);
        assert_eq!(workspace.mode, WorkspaceMode::SharedCheckout);
        assert_eq!(catalog.tabs[0].name, "Development");
        Ok(())
    }

    #[test]
    fn non_git_paths_are_rejected_without_catalog_changes() -> Result<()> {
        let root = TempDir::new()?;
        let not_git = root.path().join("plain");
        fs::create_dir(&not_git)?;
        let mut store = store(&root)?;
        assert!(add_project(&mut store, not_git).is_err());
        let catalog = store.catalog()?;
        assert!(catalog.projects.is_empty());
        Ok(())
    }

    #[test]
    fn creates_a_new_named_git_project_without_committing() -> Result<()> {
        let root = TempDir::new()?;
        let parent = root.path().join("projects");
        fs::create_dir(&parent)?;
        let mut store = store(&root)?;

        let catalog = create_project(&mut store, parent.clone(), "BridgeMind".into())?;
        let project = catalog.projects.first().context("project not registered")?;
        let path = parent.join("BridgeMind");
        assert_eq!(project.canonical_path, fs::canonicalize(&path)?);
        assert_eq!(project.name, "BridgeMind");
        assert_eq!(git_root(&path)?, project.canonical_path);
        assert_eq!(git_common_dir(&path)?, project.git_common_dir);
        assert_eq!(catalog.selection.project_id, Some(project.id));
        assert!(catalog.selection.workspace_id.is_none());
        assert!(catalog.selection.tab_id.is_none());
        assert!(
            git_output(&path, &["status", "--porcelain"])?
                .stdout
                .is_empty()
        );
        assert_eq!(
            git_output(&path, &["symbolic-ref", "--short", "HEAD"])?.stdout,
            b"main\n"
        );
        Ok(())
    }

    #[test]
    fn refuses_existing_targets_and_unsafe_names_without_overwriting() -> Result<()> {
        let root = TempDir::new()?;
        let parent = root.path().join("projects");
        fs::create_dir(&parent)?;
        let existing = parent.join("keep");
        fs::write(&existing, b"keep exactly")?;
        let mut store = store(&root)?;

        assert!(create_project(&mut store, parent.clone(), "keep".into()).is_err());
        assert_eq!(fs::read(existing)?, b"keep exactly");
        for invalid in ["", ".", "..", "a/b", "a\\b", "bad\nname"] {
            assert!(create_project(&mut store, parent.clone(), invalid.into()).is_err());
        }
        assert!(create_project(&mut store, parent, "é".repeat(49)).is_err());
        Ok(())
    }
}
