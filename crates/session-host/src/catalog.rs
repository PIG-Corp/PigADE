//! Validated placement operations over the sole durable metadata writer.
//! No command in this module launches, stops, or writes input to a process.
use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use pigade_layout::{Command, LayoutHistory, LayoutModel, LayoutState, TabLayout};
use pigade_storage::{
    Catalog, CatalogBatch, CatalogWrite, LayoutDocument, MetadataStore, RevisionConflict,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LayoutAction {
    Apply { command: Command },
    Undo,
    Redo,
}

/// Import schema-2 per-tab placement only when no authoritative history exists.
/// Original projections remain untouched and no historical undo is invented.
pub(crate) fn migrate_legacy_layout(store: &mut MetadataStore) -> Result<Option<String>> {
    let catalog = store.catalog()?;
    if catalog.layout.json.trim() != "null" || catalog.tabs.is_empty() {
        return Ok(None);
    }
    let mut state = LayoutState::default();
    for tab in &catalog.tabs {
        let projection: TabLayout = serde_json::from_str(&tab.layout_json).with_context(|| {
            format!("legacy tab {} has an unknown or malformed layout; original preserved; restore or repair its saved projection before importing", tab.id)
        })?;
        ensure!(
            tab.active_pane == projection.focused,
            "legacy tab {} active pane differs from its saved focus; original preserved; repair the saved projection before importing",
            tab.id
        );
        state.tabs.insert(tab.id, projection);
    }
    let model = LayoutModel::new(state)
        .context("legacy layout violates pane/weight/focus invariants; original preserved; restore or repair the saved projections before importing")?;
    ensure_membership(&catalog, &model)?;
    let write = history_write(&model, &catalog.layout)
        .context("legacy layout cannot be read back safely; original preserved; retain the migration backup and repair the saved projections before importing")?;
    store.apply_catalog(CatalogBatch {
        layout: Some(write),
        ..CatalogBatch::default()
    })?;
    Ok(Some(
        "legacy layout imported; previous schema stored no undo history".into(),
    ))
}

/// Validate restored placement before returning a product manifest to the UI.
pub(crate) fn read_metadata(store: &MetadataStore) -> Result<Catalog> {
    let catalog = store.catalog()?;
    load_layout(&catalog)?;
    Ok(catalog)
}

/// UI metadata writes may rename tabs, but placement has one validated entry point.
pub(crate) fn apply_metadata(
    store: &mut MetadataStore,
    mut batch: CatalogBatch,
) -> Result<Catalog> {
    ensure!(
        batch.layout.is_none(),
        "raw layout history writes are not allowed"
    );
    let current = store.catalog()?;
    for write in &batch.workspaces {
        if let Some(previous) = current
            .workspaces
            .iter()
            .find(|workspace| workspace.id == write.value.id)
            && previous.status == pigade_storage::WorkspaceStatus::Archived
            && write.value.status == pigade_storage::WorkspaceStatus::Ready
        {
            ensure!(
                write.value.cwd == previous.cwd && write.value.mode == previous.mode,
                "cannot restore workspace after changing its checkout path or mode"
            );
            let project = current
                .projects
                .iter()
                .find(|project| project.id == previous.project_id)
                .context("archived workspace project is missing")?;
            crate::projects::validate_workspace_restore(project, &previous.cwd)?;
        }
    }
    let mut model = load_layout(&current)?;
    let mut added = false;
    for write in &mut batch.tabs {
        if let Some(existing) = current.tabs.iter().find(|tab| tab.id == write.value.id) {
            ensure!(
                write.value.layout_json == existing.layout_json
                    && write.value.active_pane == existing.active_pane,
                "tab placement must be changed through layout commands"
            );
        } else {
            ensure!(
                write.value.active_pane.is_none(),
                "new tab cannot supply an active pane"
            );
            let empty = write.value.layout_json.trim();
            let canonical_empty = serde_json::to_string(&TabLayout::default())?;
            ensure!(
                empty == "null" || empty == "{}" || empty == canonical_empty,
                "new tab must start with an empty layout"
            );
            model.add_tab_metadata(write.value.id)?;
            write.value.layout_json = canonical_empty;
            added = true;
        }
    }
    if added {
        batch.layout = Some(history_write(&model, &current.layout)?);
    }
    store.apply_catalog(batch)
}

pub(crate) fn rename_pane(
    store: &mut MetadataStore,
    session: Uuid,
    name: String,
    expected_revision: Option<u64>,
) -> Result<Catalog> {
    ensure!(!session.is_nil(), "invalid pane identity");
    let current = read_metadata(store)?;
    let model = load_layout(&current)?;
    let tab_id = model
        .state()
        .pane_tab(session)
        .context("pane is no longer placed in a tab")?;
    let tab = current
        .tabs
        .iter()
        .find(|tab| tab.id == tab_id)
        .context("pane tab is missing")?;
    let workspace = current
        .workspaces
        .iter()
        .find(|workspace| workspace.id == tab.workspace_id)
        .context("pane workspace is missing")?;
    ensure!(
        workspace.status == pigade_storage::WorkspaceStatus::Ready,
        "pane workspace is unavailable"
    );
    let previous = current
        .pane_labels
        .as_ref()
        .and_then(|labels| labels.iter().find(|label| label.session == session));
    ensure!(
        previous.map(|label| label.revision) == expected_revision,
        "pane label revision conflict"
    );
    let revision = expected_revision.unwrap_or(0);
    store.save_pane_label(CatalogWrite {
        value: pigade_storage::PaneLabel {
            session,
            name,
            revision,
        },
        expected_revision,
    })
}

/// Closing placement metadata cannot stop a process or remove its history.
/// Rebase every placement snapshot before the same durable catalog transaction.
pub(crate) fn close_tab(
    store: &mut MetadataStore,
    tab_id: Uuid,
    expected_revision: u64,
    expected_layout_revision: u64,
    expected_selection_revision: u64,
) -> Result<Catalog> {
    let current = read_metadata(store)?;
    ensure_revision(&current.layout, expected_layout_revision)?;
    ensure!(
        current.selection.revision == expected_selection_revision,
        "selection changed; review the tab again"
    );
    let tab = current
        .tabs
        .iter()
        .find(|tab| tab.id == tab_id)
        .context("tab is already closed or unavailable")?;
    ensure!(
        tab.revision == expected_revision,
        "tab changed; review it again"
    );
    ensure!(
        current
            .workspaces
            .iter()
            .any(|workspace| workspace.id == tab.workspace_id
                && workspace.status == pigade_storage::WorkspaceStatus::Ready),
        "tab workspace is unavailable"
    );
    let mut before = None;
    for page_number in 0..=32 {
        let page = store.template_runs(before)?;
        ensure!(
            !page.entries.iter().any(|run| run.tab_id == tab_id),
            "finish or cancel the tab's configured launch before closing it"
        );
        before = page.next_before;
        if before.is_none() {
            break;
        }
        ensure!(page_number < 32, "active template run page bound exceeded");
    }
    let mut model = load_layout(&current)?;
    model.remove_tab_metadata(tab_id)?;
    let mut remaining = current.clone();
    remaining.tabs.retain(|tab| tab.id != tab_id);
    ensure_membership(&remaining, &model)?;
    let mut selection = current.selection.clone();
    selection.window.canvas_offsets.remove(&tab_id);
    if selection.tab_id == Some(tab_id) {
        let peers: Vec<_> = current
            .tabs
            .iter()
            .filter(|peer| peer.workspace_id == tab.workspace_id)
            .collect();
        let index = peers
            .iter()
            .position(|peer| peer.id == tab_id)
            .context("tab is not in its workspace")?;
        selection.tab_id = peers
            .iter()
            .skip(index + 1)
            .chain(peers[..index].iter().rev())
            .next()
            .map(|peer| peer.id);
    }
    let batch = CatalogBatch {
        layout: Some(history_write(&model, &current.layout)?),
        selection: Some(CatalogWrite {
            value: selection,
            expected_revision: Some(expected_selection_revision),
        }),
        ..Default::default()
    };
    store.apply_catalog_closing_tab(
        batch,
        pigade_storage::TabClose {
            id: tab_id,
            expected_revision,
        },
    )
}

pub(crate) fn activate_pane(
    store: &mut MetadataStore,
    tab_id: Uuid,
    session: Uuid,
    expected_layout_revision: u64,
) -> Result<Catalog> {
    ensure!(
        !tab_id.is_nil() && !session.is_nil(),
        "invalid pane activation identity"
    );
    let current = read_metadata(store)?;
    ensure_revision(&current.layout, expected_layout_revision)?;
    let mut model = load_layout(&current)?;
    ensure!(
        model.state().pane_tab(session) == Some(tab_id),
        "pane moved or is not in the requested tab"
    );
    let tab = current
        .tabs
        .iter()
        .find(|tab| tab.id == tab_id)
        .context("pane tab is missing")?;
    let workspace = current
        .workspaces
        .iter()
        .find(|workspace| workspace.id == tab.workspace_id)
        .context("pane workspace is missing")?;
    ensure!(
        workspace.status == pigade_storage::WorkspaceStatus::Ready,
        "pane workspace is unavailable"
    );
    let project = current
        .projects
        .iter()
        .find(|project| project.id == workspace.project_id)
        .context("pane project is missing")?;
    let focus_changed = model.apply(Command::Focus { pane: session })?;
    let mut batch = CatalogBatch::default();
    if focus_changed {
        let mut focused_tab = tab.clone();
        focused_tab.active_pane = Some(session);
        focused_tab.layout_json = serde_json::to_string(&model.state().tabs[&tab_id])?;
        batch.tabs.push(CatalogWrite {
            value: focused_tab,
            expected_revision: Some(tab.revision),
        });
        batch.layout = Some(history_write(&model, &current.layout)?);
    }
    let mut selection = current.selection.clone();
    let selection_revision = selection.revision;
    selection.project_id = Some(project.id);
    selection.workspace_id = Some(workspace.id);
    selection.tab_id = Some(tab_id);
    batch.selection = Some(CatalogWrite {
        value: selection,
        expected_revision: Some(selection_revision),
    });
    store.apply_catalog(batch)
}

/// All changed per-tab projections and the complete undo/redo document share one
/// SQLite transaction. A stale revision or failed write publishes nothing.
pub(crate) fn layout_edit(
    store: &mut MetadataStore,
    expected_revision: u64,
    action: LayoutAction,
) -> Result<Catalog> {
    let current = store.catalog()?;
    ensure_revision(&current.layout, expected_revision)?;
    let mut model = load_layout(&current)?;
    let changed = match action {
        LayoutAction::Apply { command } => {
            ensure!(
                !matches!(command, Command::AddTab { .. }),
                "create tabs through catalog metadata"
            );
            model.apply(command)?
        }
        LayoutAction::Undo => model.undo(),
        LayoutAction::Redo => model.redo(),
    };
    if !changed {
        return Ok(current);
    }
    ensure_membership(&current, &model)?;
    let mut batch = CatalogBatch {
        layout: Some(history_write(&model, &current.layout)?),
        ..CatalogBatch::default()
    };
    for tab in &current.tabs {
        let layout = &model.state().tabs[&tab.id];
        let projection = serde_json::to_string(layout)?;
        if projection != tab.layout_json || layout.focused != tab.active_pane {
            let mut updated = tab.clone();
            updated.layout_json = projection;
            updated.active_pane = layout.focused;
            batch.tabs.push(CatalogWrite {
                value: updated,
                expected_revision: Some(tab.revision),
            });
        }
    }
    store.apply_catalog(batch)
}

/// Finish a template run and its final normal layout command in one durable
/// transaction. A geometrically identical template still commits the run CAS.
pub(crate) fn layout_edit_with_run(
    store: &mut MetadataStore,
    expected_revision: u64,
    command: Command,
    run: pigade_storage::StoredTemplateRun,
    expected_run_revision: u64,
) -> Result<pigade_storage::StoredTemplateRun> {
    let current = store.catalog()?;
    ensure_revision(&current.layout, expected_revision)?;
    let mut model = load_layout(&current)?;
    let changed = model.apply(command)?;
    let mut batch = CatalogBatch::default();
    if changed {
        ensure_membership(&current, &model)?;
        batch.layout = Some(history_write(&model, &current.layout)?);
        for tab in &current.tabs {
            let layout = &model.state().tabs[&tab.id];
            let projection = serde_json::to_string(layout)?;
            if projection != tab.layout_json || layout.focused != tab.active_pane {
                let mut updated = tab.clone();
                updated.layout_json = projection;
                updated.active_pane = layout.focused;
                batch.tabs.push(CatalogWrite {
                    value: updated,
                    expected_revision: Some(tab.revision),
                });
            }
        }
    }
    let run_id = run.id;
    store.apply_catalog_with_template_run(
        batch,
        CatalogWrite {
            value: run,
            expected_revision: Some(expected_run_revision),
        },
    )?;
    store
        .template_run(run_id)?
        .context("template run disappeared after atomic commit")
}

pub(crate) fn history_write(
    model: &LayoutModel,
    previous: &LayoutDocument,
) -> Result<CatalogWrite<LayoutDocument>> {
    let history = model.export_history();
    let json = serde_json::to_string(&history)?;
    ensure!(
        json.len() <= 2 * 1024 * 1024,
        "layout history exceeds2MiB bound"
    );
    // Keep serde's recursion limit enabled. A logically valid depth-64 tree can
    // exceed that limit once wrapped in history/enums/arrays. Reject it before
    // commit if our own reader cannot roundtrip the exact candidate.
    let decoded: LayoutHistory = serde_json::from_str(&json)
        .context("candidate layout exceeds saved-history reader limits")?;
    let roundtrip = LayoutModel::from_history(decoded)?;
    ensure!(
        roundtrip.export_history() == history,
        "candidate layout cannot roundtrip exactly"
    );
    Ok(CatalogWrite {
        value: LayoutDocument {
            json,
            revision: previous.revision,
        },
        expected_revision: Some(previous.revision),
    })
}

fn ensure_revision(document: &LayoutDocument, expected: u64) -> Result<()> {
    if document.revision != expected {
        return Err(RevisionConflict {
            entity: "layout",
            id: Uuid::nil(),
            expected,
        }
        .into());
    }
    Ok(())
}

/// Unknown, invalid or inconsistent saved documents remain untouched. A null
/// document is only the pristine catalog, never permission to reset existing tabs.
pub(crate) fn load_layout(catalog: &Catalog) -> Result<LayoutModel> {
    let model = if catalog.layout.json.trim() == "null" {
        ensure!(
            catalog.tabs.is_empty(),
            "catalog tabs exist without authoritative layout history"
        );
        LayoutModel::default()
    } else {
        let history: LayoutHistory = serde_json::from_str(&catalog.layout.json)
            .context("invalid saved layout history; original preserved")?;
        LayoutModel::from_history(history)
            .context("invalid saved layout history chain; original preserved")?
    };
    ensure_membership(catalog, &model)?;
    for tab in &catalog.tabs {
        let projection: TabLayout = serde_json::from_str(&tab.layout_json)
            .context("invalid saved tab projection; original preserved")?;
        ensure!(
            projection == model.state().tabs[&tab.id] && tab.active_pane == projection.focused,
            "saved tab projection differs from authoritative layout history"
        );
    }
    Ok(model)
}

pub(crate) fn ensure_membership(catalog: &Catalog, model: &LayoutModel) -> Result<()> {
    let catalog_tabs: BTreeSet<_> = catalog.tabs.iter().map(|tab| tab.id).collect();
    let layout_tabs: BTreeSet<_> = model.state().tabs.keys().copied().collect();
    ensure!(
        catalog_tabs == layout_tabs,
        "layout history and catalog tab membership differ"
    );
    Ok(())
}

/// Window viewport persistence is independent of placement/undo and never
/// changes the selected project, tab or terminal process. Read the current
/// selection on the sole host writer so unrelated UI fields are preserved.
pub(crate) fn save_canvas_offset(
    store: &mut MetadataStore,
    tab: Uuid,
    offset: pigade_storage::CanvasOffset,
) -> Result<pigade_storage::Selection> {
    offset.validate()?;
    let current = read_metadata(store)?;
    ensure!(
        current.tabs.iter().any(|value| value.id == tab),
        "unknown canvas tab"
    );
    let mut selection = current.selection;
    let revision = selection.revision;
    if selection.window.canvas_offsets.get(&tab) == Some(&offset) {
        return Ok(selection);
    }
    selection.window.canvas_offsets.insert(tab, offset);
    let saved = store.apply_catalog(CatalogBatch {
        selection: Some(CatalogWrite {
            value: selection,
            expected_revision: Some(revision),
        }),
        ..CatalogBatch::default()
    })?;
    Ok(saved.selection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_layout::Direction;
    use pigade_storage::{CanvasOffset, Project, Tab, Workspace, WorkspaceMode, WorkspaceStatus};

    fn id(number: u128) -> Uuid {
        Uuid::from_u128(number)
    }
    fn new<T>(value: T) -> CatalogWrite<T> {
        CatalogWrite {
            value,
            expected_revision: None,
        }
    }
    fn tab(number: u128, workspace: u128) -> Tab {
        Tab {
            id: id(number),
            workspace_id: id(workspace),
            name: format!("Вкладка {number}"),
            layout_json: "null".into(),
            active_pane: None,
            revision: 0,
        }
    }
    fn seed(store: &mut MetadataStore) -> Result<Catalog> {
        let project = Project {
            id: id(1),
            canonical_path: "/pigade-test/project".into(),
            git_common_dir: "/pigade-test/project/.git".into(),
            name: "PigADE".into(),
            roots: Vec::new(),
            revision: 0,
        };
        let workspace = |number| Workspace {
            id: id(number),
            project_id: id(1),
            task: format!("Задача {number}"),
            cwd: format!("/pigade-test/worktree-{number}").into(),
            mode: WorkspaceMode::Worktree,
            status: WorkspaceStatus::Ready,
            error: None,
            context_revision: None,
            revision: 0,
        };
        apply_metadata(
            store,
            CatalogBatch {
                projects: vec![new(project)],
                workspaces: vec![new(workspace(2)), new(workspace(3))],
                tabs: vec![new(tab(4, 2)), new(tab(5, 3))],
                ..CatalogBatch::default()
            },
        )
    }

    #[test]
    fn missing_archived_workspace_cannot_be_restored_or_mutated() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let repo = directory.path().join("repo");
        std::fs::create_dir(&repo)?;
        let git = |path: &std::path::Path, args: &[&str]| -> Result<()> {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .output()?;
            anyhow::ensure!(output.status.success(), "test git command failed");
            Ok(())
        };
        let init = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&repo)
            .output()?;
        anyhow::ensure!(init.status.success(), "git init failed");
        git(
            &repo,
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
        let state = directory.path().join("state");
        std::fs::create_dir(&state)?;
        let mut store = MetadataStore::open(directory.path().join("state.sqlite"))?;
        let catalog = crate::projects::add_project(&mut store, repo.clone())?;
        let catalog = crate::projects::create_workspace(
            &mut store,
            &state,
            catalog.projects[0].id,
            "Task".into(),
            true,
        )?;
        let mut workspace = catalog.workspaces[0].clone();
        workspace.status = WorkspaceStatus::Archived;
        let archived = apply_metadata(
            &mut store,
            CatalogBatch {
                workspaces: vec![CatalogWrite {
                    value: workspace.clone(),
                    expected_revision: Some(workspace.revision),
                }],
                ..CatalogBatch::default()
            },
        )?;
        workspace = archived.workspaces[0].clone();
        let archived_cwd = workspace.cwd.clone();
        let sibling = directory.path().join("sibling");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "sibling",
                sibling.to_str().unwrap(),
            ],
        )?;
        workspace.status = WorkspaceStatus::Ready;
        workspace.cwd = sibling;
        assert!(
            apply_metadata(
                &mut store,
                CatalogBatch {
                    workspaces: vec![CatalogWrite {
                        value: workspace.clone(),
                        expected_revision: Some(workspace.revision)
                    }],
                    ..CatalogBatch::default()
                }
            )
            .is_err()
        );
        let after = store.catalog()?;
        assert_eq!(after.workspaces[0].status, WorkspaceStatus::Archived);
        assert_eq!(after.workspaces[0].revision, workspace.revision);
        assert_eq!(after.workspaces[0].cwd, archived_cwd);
        Ok(())
    }

    #[test]
    fn template_layout_edit_is_atomic_and_keeps_pane_identities() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("state.sqlite"))?;
        let initial = seed(&mut store)?;
        let tab_id = id(4);
        let mut current = layout_edit(
            &mut store,
            initial.layout.revision,
            LayoutAction::Apply {
                command: pigade_layout::Command::Insert {
                    tab: tab_id,
                    pane: id(20),
                    target: None,
                    side: Direction::Right,
                    split: id(30),
                },
            },
        )?;
        current = layout_edit(
            &mut store,
            current.layout.revision,
            LayoutAction::Apply {
                command: pigade_layout::Command::Insert {
                    tab: tab_id,
                    pane: id(21),
                    target: Some(id(20)),
                    side: Direction::Down,
                    split: id(31),
                },
            },
        )?;
        let before: TabLayout = serde_json::from_str(
            &current
                .tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .unwrap()
                .layout_json,
        )?;
        let original_panes = before.panes();
        let template = pigade_layout::GeometryTemplate {
            schema: 1,
            root: Some(pigade_layout::TemplateNode::Split {
                axis: pigade_layout::Axis::Horizontal,
                weights: vec![0.5, 0.5],
                children: vec![
                    pigade_layout::TemplateNode::Pane,
                    pigade_layout::TemplateNode::Pane,
                ],
            }),
        };
        current = layout_edit(
            &mut store,
            current.layout.revision,
            LayoutAction::Apply {
                command: pigade_layout::Command::ApplyTemplate {
                    tab: tab_id,
                    template,
                },
            },
        )?;
        let after: TabLayout = serde_json::from_str(
            &current
                .tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .unwrap()
                .layout_json,
        )?;
        assert_eq!(after.panes(), original_panes);
        assert_eq!(current.tabs.len(), 2);
        assert_eq!(
            current
                .tabs
                .iter()
                .map(|tab| tab.id)
                .collect::<std::collections::BTreeSet<_>>(),
            [id(4), id(5)].into()
        );
        Ok(())
    }
    fn edit(store: &mut MetadataStore, action: LayoutAction) -> Result<Catalog> {
        layout_edit(store, store.catalog()?.layout.revision, action)
    }
    fn insert(
        store: &mut MetadataStore,
        pane: u128,
        target: Option<u128>,
        split: u128,
    ) -> Result<Catalog> {
        edit(
            store,
            LayoutAction::Apply {
                command: Command::Insert {
                    tab: id(4),
                    pane: id(pane),
                    target: target.map(id),
                    side: Direction::Right,
                    split: id(split),
                },
            },
        )
    }
    fn find_tab(catalog: &Catalog, number: u128) -> &Tab {
        catalog
            .tabs
            .iter()
            .find(|tab| tab.id == id(number))
            .unwrap()
    }

    #[test]
    fn close_tab_commits_selection_and_history_rebase_without_resurrection() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        seed(&mut store)?;
        insert(&mut store, 10, None, 100)?;
        let current = apply_metadata(
            &mut store,
            CatalogBatch {
                tabs: vec![new(tab(6, 2))],
                ..Default::default()
            },
        )?;
        let mut selection = current.selection.clone();
        selection.project_id = Some(id(1));
        selection.workspace_id = Some(id(2));
        selection.tab_id = Some(id(4));
        selection
            .window
            .canvas_offsets
            .insert(id(4), CanvasOffset { x: 12., y: 30. });
        selection.window.panel_generations.insert(id(10), id(99));
        let current = apply_metadata(
            &mut store,
            CatalogBatch {
                selection: Some(CatalogWrite {
                    value: selection,
                    expected_revision: Some(current.selection.revision),
                }),
                ..Default::default()
            },
        )?;
        let tab_revision = find_tab(&current, 4).revision;
        assert!(
            close_tab(
                &mut store,
                id(4),
                tab_revision,
                current.layout.revision + 1,
                current.selection.revision
            )
            .is_err()
        );
        assert!(
            close_tab(
                &mut store,
                id(4),
                tab_revision + 1,
                current.layout.revision,
                current.selection.revision
            )
            .is_err()
        );
        assert!(
            close_tab(
                &mut store,
                id(4),
                tab_revision,
                current.layout.revision,
                current.selection.revision + 1
            )
            .is_err()
        );
        assert_eq!(read_metadata(&store)?, current);
        let closed = close_tab(
            &mut store,
            id(4),
            tab_revision,
            current.layout.revision,
            current.selection.revision,
        )?;
        assert_eq!(closed.selection.tab_id, Some(id(6)));
        assert!(!closed.selection.window.canvas_offsets.contains_key(&id(4)));
        assert_eq!(
            closed.selection.window.panel_generations.get(&id(10)),
            Some(&id(99))
        );
        assert_eq!(find_tab(&closed, 5), find_tab(&current, 5));
        let mut model = load_layout(&closed)?;
        while model.undo() {
            assert!(!model.state().tabs.contains_key(&id(4)));
        }
        while model.redo() {
            assert!(!model.state().tabs.contains_key(&id(4)));
        }
        drop(store);
        let mut store = MetadataStore::open(&path)?;
        assert_eq!(read_metadata(&store)?, closed);
        let last = close_tab(
            &mut store,
            id(6),
            find_tab(&closed, 6).revision,
            closed.layout.revision,
            closed.selection.revision,
        )?;
        assert_eq!(last.selection.tab_id, None);
        assert_eq!(last.selection.workspace_id, Some(id(2)));
        assert_eq!(find_tab(&last, 5), find_tab(&current, 5));
        Ok(())
    }

    #[test]
    fn close_tab_refuses_active_template_run_and_preserves_all_metadata() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("state.sqlite"))?;
        let current = seed(&mut store)?;
        let run = store.save_template_run(new(pigade_storage::StoredTemplateRun {
            id: id(88),
            workspace_id: id(2),
            tab_id: id(4),
            template_id: None,
            name: "Pending".into(),
            slots: 1,
            status: "running".into(),
            payload: "{}".into(),
            revision: 0,
        }))?;
        assert!(
            close_tab(
                &mut store,
                id(4),
                find_tab(&current, 4).revision,
                current.layout.revision,
                current.selection.revision
            )
            .is_err()
        );
        assert_eq!(read_metadata(&store)?, current);
        let mut canceled = run.clone();
        canceled.status = "cancelled".into();
        store.save_template_run(CatalogWrite {
            value: canceled,
            expected_revision: Some(run.revision),
        })?;
        let closed = close_tab(
            &mut store,
            id(4),
            find_tab(&current, 4).revision,
            current.layout.revision,
            current.selection.revision,
        )?;
        assert!(closed.tabs.iter().all(|tab| tab.id != id(4)));
        Ok(())
    }

    #[test]
    fn activate_pane_updates_tab_projection_with_authoritative_focus() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        seed(&mut store)?;
        insert(&mut store, 10, None, 100)?;
        insert(&mut store, 11, Some(10), 101)?;
        edit(
            &mut store,
            LayoutAction::Apply {
                command: Command::Focus { pane: id(10) },
            },
        )?;
        let before = store.catalog()?;
        let committed = activate_pane(&mut store, id(4), id(11), before.layout.revision)?;
        let validated = read_metadata(&store)?;
        let tab = find_tab(&validated, 4);
        let projection: pigade_layout::TabLayout = serde_json::from_str(&tab.layout_json)?;
        let model = load_layout(&validated)?;
        assert_eq!(tab.active_pane, Some(id(11)));
        assert_eq!(projection, model.state().tabs[&id(4)]);
        assert_eq!(projection.focused, Some(id(11)));
        assert_eq!(validated, committed);

        drop(store);
        let reopened = MetadataStore::open(&path)?;
        let restored = read_metadata(&reopened)?;
        let tab = find_tab(&restored, 4);
        let projection: pigade_layout::TabLayout = serde_json::from_str(&tab.layout_json)?;
        let model = load_layout(&restored)?;
        assert_eq!(tab.active_pane, Some(id(11)));
        assert_eq!(projection, model.state().tabs[&id(4)]);
        assert_eq!(projection.focused, Some(id(11)));
        Ok(())
    }

    #[test]
    fn metadata_canonicalizes_empty_tabs_and_disallows_placement_bypass() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("state.sqlite"))?;
        let initial = seed(&mut store)?;
        let model = load_layout(&initial)?;
        assert_eq!(model.undo_len(), 0);
        assert_eq!(model.state().tabs.len(), 2);
        assert!(initial.tabs.iter().all(|tab|tab.layout_json==serde_json::to_string(&TabLayout::default()).unwrap()));
        let mut bad = find_tab(&initial, 4).clone();
        bad.active_pane = Some(id(10));
        assert!(
            apply_metadata(
                &mut store,
                CatalogBatch {
                    tabs: vec![CatalogWrite {
                        value: bad,
                        expected_revision: Some(1)
                    }],
                    ..CatalogBatch::default()
                }
            )
            .is_err()
        );
        assert_eq!(store.catalog()?, initial);
        assert!(
            apply_metadata(
                &mut store,
                CatalogBatch {
                    layout: Some(CatalogWrite {
                        value: initial.layout.clone(),
                        expected_revision: Some(initial.layout.revision)
                    }),
                    ..CatalogBatch::default()
                }
            )
            .is_err()
        );
        assert_eq!(store.catalog()?, initial);
        let mut renamed = find_tab(&initial, 4).clone();
        renamed.name = "Ревью".into();
        let after = apply_metadata(
            &mut store,
            CatalogBatch {
                tabs: vec![CatalogWrite {
                    value: renamed,
                    expected_revision: Some(1),
                }],
                ..CatalogBatch::default()
            },
        )?;
        assert_eq!(after.layout, initial.layout);
        assert_eq!(find_tab(&after, 4).name, "Ревью");
        assert!(
            edit(
                &mut store,
                LayoutAction::Apply {
                    command: Command::AddTab { tab: id(99) }
                }
            )
            .is_err()
        );
        assert_eq!(store.catalog()?, after);
        Ok(())
    }

    #[test]
    fn cross_workspace_transfer_and_stale_cas_preserve_both_sides() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("state.sqlite"))?;
        seed(&mut store)?;
        let before = insert(&mut store, 10, None, 100)?;
        let after = edit(
            &mut store,
            LayoutAction::Apply {
                command: Command::Transfer {
                    pane: id(10),
                    to_tab: id(5),
                    target: None,
                    side: Direction::Down,
                    split: id(101),
                },
            },
        )?;
        assert_eq!(find_tab(&after, 4).active_pane, None);
        assert_eq!(find_tab(&after, 5).active_pane, Some(id(10)));
        assert_eq!(
            find_tab(&after, 4).revision,
            find_tab(&before, 4).revision + 1
        );
        assert_eq!(
            find_tab(&after, 5).revision,
            find_tab(&before, 5).revision + 1
        );
        assert_eq!(load_layout(&after)?.state().pane_tab(id(10)), Some(id(5)));
        assert!(layout_edit(&mut store, before.layout.revision, LayoutAction::Undo).is_err());
        assert_eq!(store.catalog()?, after);
        let reverted = edit(&mut store, LayoutAction::Undo)?;
        assert_eq!(
            load_layout(&reverted)?.state(),
            load_layout(&before)?.state()
        );
        let mut stale = find_tab(&reverted, 4).clone();
        stale.revision -= 1;
        let failed = apply_metadata(
            &mut store,
            CatalogBatch {
                tabs: vec![
                    new(tab(6, 2)),
                    CatalogWrite {
                        expected_revision: Some(stale.revision),
                        value: stale,
                    },
                ],
                ..CatalogBatch::default()
            },
        );
        assert!(failed.is_err());
        assert_eq!(store.catalog()?, reverted);
        Ok(())
    }

    #[test]
    fn persisted_fifty_undo_redo_survive_reopen_and_new_catalog_tab() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        seed(&mut store)?;
        insert(&mut store, 10, None, 100)?;
        let before = insert(&mut store, 11, Some(10), 101)?;
        for _ in 0..50 {
            edit(
                &mut store,
                LayoutAction::Apply {
                    command: Command::Rotate { split: id(101) },
                },
            )?;
        }
        let committed = apply_metadata(
            &mut store,
            CatalogBatch {
                tabs: vec![new(tab(6, 2))],
                ..CatalogBatch::default()
            },
        )?;
        drop(store);
        let mut store = MetadataStore::open(&path)?;
        assert_eq!(store.catalog()?, committed);
        for _ in 0..50 {
            edit(&mut store, LayoutAction::Undo)?;
        }
        let undone = store.catalog()?;
        let model = load_layout(&undone)?;
        assert_eq!(model.redo_len(), 50);
        assert_eq!(
            model.state().tabs[&id(4)],
            load_layout(&before)?.state().tabs[&id(4)]
        );
        assert_eq!(model.state().tabs[&id(6)], TabLayout::default());
        drop(store);
        let mut store = MetadataStore::open(&path)?;
        for _ in 0..50 {
            edit(&mut store, LayoutAction::Redo)?;
        }
        assert_eq!(
            load_layout(&store.catalog()?)?.state(),
            load_layout(&committed)?.state()
        );
        assert_eq!(load_layout(&store.catalog()?)?.redo_len(), 0);
        Ok(())
    }

    #[test]
    fn inconsistent_or_unknown_saved_layout_is_reported_without_overwrite() -> Result<()> {
        for corrupt_history in [true, false] {
            let directory = tempfile::tempdir()?;
            let mut store = MetadataStore::open(directory.path().join("state.sqlite"))?;
            let initial = seed(&mut store)?;
            let batch = if corrupt_history {
                CatalogBatch {
                    layout: Some(CatalogWrite {
                        value: LayoutDocument {
                            json: "{\"schema\":99}".into(),
                            revision: initial.layout.revision,
                        },
                        expected_revision: Some(initial.layout.revision),
                    }),
                    ..CatalogBatch::default()
                }
            } else {
                let mut tab = find_tab(&initial, 4).clone();
                tab.layout_json = "null".into();
                CatalogBatch {
                    tabs: vec![CatalogWrite {
                        expected_revision: Some(tab.revision),
                        value: tab,
                    }],
                    ..CatalogBatch::default()
                }
            };
            let corrupted = store.apply_catalog(batch)?;
            assert!(read_metadata(&store).is_err());
            assert!(edit(&mut store, LayoutAction::Undo).is_err());
            assert!(apply_metadata(&mut store, CatalogBatch::default()).is_err());
            assert_eq!(store.catalog()?, corrupted);
        }
        Ok(())
    }

    #[test]
    fn deep_valid_candidate_is_rejected_before_durable_write() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("state.sqlite"))?;
        seed(&mut store)?;
        insert(&mut store, 10, None, 100)?;
        let mut rejected = false;
        for depth in 0..64 {
            let before = read_metadata(&store)?;
            let command = Command::Insert {
                tab: id(4),
                pane: id(1000 + depth),
                target: Some(id(10)),
                side: Direction::Right,
                split: id(2000 + depth),
            };
            let mut candidate = load_layout(&before)?;
            // The pure tree is valid even when its wrapped JSON is too deep.
            candidate.apply(command.clone())?;
            match layout_edit(
                &mut store,
                before.layout.revision,
                LayoutAction::Apply { command },
            ) {
                Ok(saved) => {
                    assert_eq!(read_metadata(&store)?, saved);
                    load_layout(&saved)?.state().validate()?;
                }
                Err(error) => {
                    assert!(error.to_string().contains("reader limits"));
                    assert_eq!(store.catalog()?, before);
                    assert_eq!(read_metadata(&store)?, before);
                    rejected = true;
                    break;
                }
            }
        }
        assert!(
            rejected,
            "history wrapper must hit serde's enabled recursion bound"
        );
        Ok(())
    }
    #[test]
    fn fractional_resizes_roundtrip_the_committed_history_exactly() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut store = MetadataStore::open(directory.path().join("state.sqlite"))?;
        seed(&mut store)?;
        insert(&mut store, 10, None, 100)?;
        insert(&mut store, 11, Some(10), 101)?;
        for numerator in 1..=40 {
            let before = read_metadata(&store)?;
            let command = Command::ResizeBoundary {
                split: id(101),
                boundary: 0,
                fraction: f64::from(numerator) / 43.0,
            };
            let mut expected = load_layout(&before)?;
            expected.apply(command.clone())?;
            let committed = layout_edit(
                &mut store,
                before.layout.revision,
                LayoutAction::Apply { command },
            )?;
            assert_eq!(
                load_layout(&committed)?.export_history(),
                expected.export_history()
            );
        }
        Ok(())
    }
    fn downgrade_fixture_to_schema_two(path: &std::path::Path) -> Result<()> {
        let connection = rusqlite::Connection::open(path)?;
        // Construct an actual v2 schema rather than relabeling a current DB.
        connection.execute_batch(
            "PRAGMA synchronous=FULL; BEGIN IMMEDIATE;
            DROP TRIGGER conversation_bindings_no_update;
            DROP TRIGGER conversation_bindings_no_delete;
            DROP TABLE conversation_bindings;
            DROP INDEX prepared_launch_generation_lookup;
            DROP TABLE prepared_launches;
            DROP TABLE context_heads;
            DROP TABLE context_revisions;
            DROP TABLE layout_revisions;
            DROP TABLE template_runs;
            DROP TABLE layout_templates;
            DROP TABLE prompt_records; DROP TABLE agent_profiles; DROP TABLE pane_labels;
            DROP INDEX launch_session_generation_ordinal;
            ALTER TABLE launch_intents DROP COLUMN generation_ordinal;
            PRAGMA user_version=2; COMMIT;",
        )?;
        assert_eq!(
            connection.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            2
        );
        Ok(())
    }

    #[test]
    fn actual_schema_two_layout_import_preserves_exact_placement_without_invented_undo()
    -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        seed(&mut store)?;
        insert(&mut store, 10, None, 100)?;
        insert(&mut store, 11, Some(10), 101)?;
        edit(
            &mut store,
            LayoutAction::Apply {
                command: Command::Resize {
                    split: id(101),
                    weights: vec![2.0, 7.0],
                },
            },
        )?;
        edit(
            &mut store,
            LayoutAction::Apply {
                command: Command::Focus { pane: id(10) },
            },
        )?;
        let original = edit(
            &mut store,
            LayoutAction::Apply {
                command: Command::ToggleFocusMode { tab: id(4) },
            },
        )?;
        let original_state = load_layout(&original)?.state().clone();
        drop(store);
        downgrade_fixture_to_schema_two(&path)?;
        let mut store = MetadataStore::open(&path)?;
        let backup = store
            .migration_backup_path()
            .context("schema2 backup required")?;
        let backup_reader = rusqlite::Connection::open(backup)?;
        assert_eq!(
            backup_reader.pragma_query_value::<u32, _>(None, "user_version", |row| row.get(0))?,
            2
        );
        assert_eq!(
            backup_reader.query_row::<usize, _, _>(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='layout_revisions'",
                [],
                |row| row.get(0)
            )?,
            0
        );
        let upgraded = store.catalog()?;
        assert_eq!(upgraded.layout, LayoutDocument::default());
        assert!(read_metadata(&store).is_err());
        let launches = store.list_launches()?;
        let notice =
            migrate_legacy_layout(&mut store)?.context("expected visible migration notice")?;
        assert_eq!(
            notice,
            "legacy layout imported; previous schema stored no undo history"
        );
        let imported = read_metadata(&store)?;
        assert_eq!(imported.tabs, original.tabs);
        assert_eq!(imported.projects, original.projects);
        assert_eq!(imported.workspaces, original.workspaces);
        assert_eq!(imported.selection, original.selection);
        let model = load_layout(&imported)?;
        assert_eq!(model.state(), &original_state);
        assert_eq!((model.undo_len(), model.redo_len()), (0, 0));
        assert_eq!(store.list_launches()?, launches);
        assert!(migrate_legacy_layout(&mut store)?.is_none());
        assert_eq!(store.catalog()?, imported);
        drop(store);
        assert_eq!(read_metadata(&MetadataStore::open(&path)?)?, imported);
        Ok(())
    }

    #[test]
    fn malformed_schema_two_projection_is_actionable_and_preserved() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        let initial = seed(&mut store)?;
        let mut tab = find_tab(&initial, 4).clone();
        tab.layout_json = "{\"unknown_legacy_tree\":[]}".into();
        store.apply_catalog(CatalogBatch {
            tabs: vec![CatalogWrite {
                expected_revision: Some(tab.revision),
                value: tab,
            }],
            ..CatalogBatch::default()
        })?;
        drop(store);
        downgrade_fixture_to_schema_two(&path)?;
        let mut store = MetadataStore::open(&path)?;
        let before = store.catalog()?;
        let launches = store.list_launches()?;
        let error = migrate_legacy_layout(&mut store).unwrap_err();
        assert!(error.to_string().contains("unknown or malformed layout"));
        assert!(error.to_string().contains("original preserved"));
        assert_eq!(store.catalog()?, before);
        assert_eq!(store.list_launches()?, launches);
        assert!(read_metadata(&store).is_err());
        drop(store);
        let mut reopened = MetadataStore::open(&path)?;
        assert!(migrate_legacy_layout(&mut reopened).is_err());
        assert_eq!(reopened.catalog()?, before);
        Ok(())
    }

    #[test]
    fn canvas_offset_save_preserves_layout_and_selection_and_reopens() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let mut store = MetadataStore::open(&path)?;
        let before = seed(&mut store)?;
        let tab = before.tabs[0].id;
        let before_model = load_layout(&before)?;
        let offset = CanvasOffset {
            x: 120.5,
            y: 480.25,
        };

        let saved = save_canvas_offset(&mut store, tab, offset)?;
        let after = store.catalog()?;
        let mut expected_window = before.selection.window.clone();
        expected_window.canvas_offsets.insert(tab, offset);
        assert_eq!(saved.window, expected_window);
        assert_eq!(saved.project_id, before.selection.project_id);
        assert_eq!(saved.workspace_id, before.selection.workspace_id);
        assert_eq!(saved.tab_id, before.selection.tab_id);
        assert_eq!(after.selection, saved);
        assert_eq!(after.layout, before.layout);
        assert_eq!(after.tabs, before.tabs);
        let after_model = load_layout(&after)?;
        assert_eq!(after_model.undo_len(), before_model.undo_len());
        assert_eq!(after_model.redo_len(), before_model.redo_len());

        let unchanged = save_canvas_offset(&mut store, tab, offset)?;
        assert_eq!(unchanged.revision, saved.revision);
        assert_eq!(store.catalog()?, after);
        drop(store);

        let mut reopened = MetadataStore::open(&path)?;
        let restored = store_catalog(&reopened)?;
        assert_eq!(
            restored.selection.window.canvas_offsets.get(&tab),
            Some(&offset)
        );
        assert_eq!(restored.layout, before.layout);
        assert_eq!(restored.tabs, before.tabs);
        assert!(save_canvas_offset(&mut reopened, id(999), offset).is_err());
        assert!(save_canvas_offset(&mut reopened, tab, CanvasOffset { x: -1.0, y: 0.0 }).is_err());
        assert_eq!(store_catalog(&reopened)?, restored);
        Ok(())
    }

    fn store_catalog(store: &MetadataStore) -> Result<Catalog> {
        read_metadata(store)
    }
}
