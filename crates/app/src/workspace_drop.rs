//! Pure preview and stale-guard checks for cross-workspace pane drops.

use crate::{pointer_layout, workspace_nav_store::Config, workspace_navigation};
use pigade_layout::{Command, LayoutState, TabLayout};
use pigade_storage::{Catalog, Tab, Workspace, WorkspaceDropGuard, WorkspaceStatus};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(crate) struct Preview {
    pub(crate) from_tab: Uuid,
    pub(crate) to_tab: Uuid,
    pub(crate) workspace: Uuid,
    /// Global placement revision at preview time, for the caller's commit policy.
    pub(crate) layout_revision: u64,
    pub(crate) name: String,
    pub(crate) command: Command,
    pub(crate) layout: TabLayout,
    pub(crate) guard: WorkspaceDropGuard,
}

/// Build an exact transfer preview into the destination workspace's remembered
/// tab (or its first tab). The selected source remains the only valid source.
pub(crate) fn preview(
    catalog: &Catalog,
    config: &Config,
    from_tab: Uuid,
    pane: Uuid,
    destination_workspace: Uuid,
    split: Uuid,
) -> Option<Preview> {
    if catalog.pane_workspace_drop_schema != Some(1) {
        return None;
    }
    let source = selected_source(catalog, from_tab)?;
    if source.id == destination_workspace {
        return None;
    }
    let destination = ready_workspace(catalog, destination_workspace)?;
    let destination_project = project_exists(catalog, destination.project_id)?;
    let target = workspace_navigation::target_tab(catalog, config, destination_workspace)?;
    build_preview(
        catalog,
        source,
        destination,
        destination_project,
        (from_tab, pane, target, split),
    )
}

/// Validate a cached preview without resolving a replacement destination tab.
/// Focus-only catalog changes may advance the global layout revision and are
/// intentionally not part of this model guard.
pub(crate) fn matches(catalog: &Catalog, preview: &Preview) -> bool {
    if !guard_matches(catalog, preview.from_tab, preview.to_tab, &preview.guard)
        || preview.workspace != preview.guard.destination_workspace
    {
        return false;
    }
    let Some(split) = split_id(&preview.command) else {
        return false;
    };
    let Some((command, layout)) = transfer(
        catalog,
        preview.from_tab,
        preview.to_tab,
        pane_id(&preview.command),
        split,
    ) else {
        return false;
    };
    command == preview.command && layout == preview.layout
}

/// Check the catalog guard for a frozen source and destination tab. In
/// particular, this deliberately does not look up a replacement destination.
pub(crate) fn guard_matches(
    catalog: &Catalog,
    from_tab: Uuid,
    to_tab: Uuid,
    guard: &WorkspaceDropGuard,
) -> bool {
    if catalog.pane_workspace_drop_schema != Some(1)
        || catalog.selection.workspace_id != Some(guard.source_workspace)
        || catalog.selection.project_id
            != catalog
                .workspaces
                .iter()
                .find(|workspace| workspace.id == guard.source_workspace)
                .map(|workspace| workspace.project_id)
        || catalog.selection.tab_id != Some(from_tab)
    {
        return false;
    }
    let Some(source) = ready_workspace(catalog, guard.source_workspace) else {
        return false;
    };
    let Some(destination) = ready_workspace(catalog, guard.destination_workspace) else {
        return false;
    };
    if source.revision != guard.source_revision
        || destination.revision != guard.destination_revision
        || source.id == destination.id
        || project_exists(catalog, source.project_id).is_none()
        || project_exists(catalog, destination.project_id).is_none()
    {
        return false;
    }
    let Some(source_tab) = catalog.tabs.iter().find(|tab| tab.id == from_tab) else {
        return false;
    };
    let Some(destination_tab) = catalog.tabs.iter().find(|tab| tab.id == to_tab) else {
        return false;
    };
    if source_tab.workspace_id != source.id
        || destination_tab.workspace_id != destination.id
        || destination_tab.revision != guard.destination_tab_revision
    {
        return false;
    }
    true
}

fn selected_source(catalog: &Catalog, from_tab: Uuid) -> Option<&Workspace> {
    let workspace_id = catalog.selection.workspace_id?;
    let workspace = ready_workspace(catalog, workspace_id)?;
    if catalog.selection.project_id != Some(workspace.project_id)
        || catalog.selection.tab_id != Some(from_tab)
    {
        return None;
    }
    let tab = catalog.tabs.iter().find(|tab| tab.id == from_tab)?;
    (tab.workspace_id == workspace_id && project_exists(catalog, workspace.project_id).is_some())
        .then_some(workspace)
}

fn ready_workspace(catalog: &Catalog, id: Uuid) -> Option<&Workspace> {
    catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id && workspace.status == WorkspaceStatus::Ready)
}

fn project_exists(catalog: &Catalog, id: Uuid) -> Option<&pigade_storage::Project> {
    catalog.projects.iter().find(|project| project.id == id)
}

fn build_preview(
    catalog: &Catalog,
    source: &Workspace,
    destination: &Workspace,
    destination_project: &pigade_storage::Project,
    transfer_ids: (Uuid, Uuid, Uuid, Uuid),
) -> Option<Preview> {
    let (from_tab, pane, to_tab, split) = transfer_ids;
    let destination_tab = catalog.tabs.iter().find(|tab| tab.id == to_tab)?;
    if destination_tab.workspace_id != destination.id {
        return None;
    }
    let (command, layout) = transfer(catalog, from_tab, to_tab, pane, split)?;
    Some(Preview {
        from_tab,
        to_tab,
        workspace: destination.id,
        layout_revision: catalog.layout.revision,
        name: safe_label(&format!(
            "{} · {} · {}",
            destination_project.name, destination.task, destination_tab.name
        )),
        command,
        layout,
        guard: WorkspaceDropGuard {
            source_workspace: source.id,
            source_revision: source.revision,
            destination_workspace: destination.id,
            destination_revision: destination.revision,
            destination_tab_revision: destination_tab.revision,
        },
    })
}

fn transfer(
    catalog: &Catalog,
    from_tab: Uuid,
    to_tab: Uuid,
    pane: Uuid,
    split: Uuid,
) -> Option<(Command, TabLayout)> {
    let source = catalog.tabs.iter().find(|tab| tab.id == from_tab)?;
    let destination = catalog.tabs.iter().find(|tab| tab.id == to_tab)?;
    let state = LayoutState {
        tabs: [(from_tab, decode(source)?), (to_tab, decode(destination)?)]
            .into_iter()
            .collect(),
    };
    pointer_layout::tab_transfer_preview(&state, from_tab, pane, to_tab, split)
}

fn decode(tab: &Tab) -> Option<TabLayout> {
    serde_json::from_str(&tab.layout_json).ok()
}

fn safe_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(512)
        .collect()
}

fn pane_id(command: &Command) -> Uuid {
    match command {
        Command::Transfer { pane, .. } => *pane,
        _ => Uuid::nil(),
    }
}

fn split_id(command: &Command) -> Option<Uuid> {
    match command {
        Command::Transfer { split, .. } => Some(*split),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_layout::{Axis, Direction, Node};
    use pigade_storage::{LayoutDocument, Project, Selection, Tab, WorkspaceMode};
    use std::path::PathBuf;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn layout(panes: &[u128], focused: Option<u128>) -> TabLayout {
        let root = match panes {
            [] => None,
            [pane] => Some(Node::pane(id(*pane))),
            panes => Some(Node::Split {
                id: id(900),
                axis: Axis::Horizontal,
                weights: vec![1.0 / panes.len() as f64; panes.len()],
                children: panes.iter().map(|pane| Node::pane(id(*pane))).collect(),
            }),
        };
        TabLayout {
            root,
            focused: focused.map(id),
            focus_mode: false,
        }
    }

    fn catalog() -> Catalog {
        let source_project = id(1);
        let destination_project = id(2);
        let source_workspace = id(10);
        let destination_workspace = id(20);
        let source_tab = id(100);
        let destination_first = id(200);
        let destination_second = id(201);
        let tab = |tab_id, workspace_id, name: &str, layout: TabLayout, revision| Tab {
            id: tab_id,
            workspace_id,
            name: name.into(),
            layout_json: serde_json::to_string(&layout).unwrap(),
            active_pane: layout.focused,
            revision,
        };
        Catalog {
            projects: vec![
                Project {
                    id: source_project,
                    canonical_path: PathBuf::from("/source"),
                    git_common_dir: PathBuf::from("/source/.git"),
                    name: "Source".into(),
                    roots: vec![],
                    revision: 1,
                },
                Project {
                    id: destination_project,
                    canonical_path: PathBuf::from("/destination"),
                    git_common_dir: PathBuf::from("/destination/.git"),
                    name: "Destination".into(),
                    roots: vec![],
                    revision: 1,
                },
            ],
            workspaces: vec![
                Workspace {
                    id: source_workspace,
                    project_id: source_project,
                    task: "Build".into(),
                    cwd: PathBuf::from("/source"),
                    mode: WorkspaceMode::SharedCheckout,
                    status: WorkspaceStatus::Ready,
                    error: None,
                    context_revision: None,
                    revision: 4,
                },
                Workspace {
                    id: destination_workspace,
                    project_id: destination_project,
                    task: "Review".into(),
                    cwd: PathBuf::from("/destination"),
                    mode: WorkspaceMode::Worktree,
                    status: WorkspaceStatus::Ready,
                    error: None,
                    context_revision: None,
                    revision: 7,
                },
            ],
            tabs: vec![
                tab(
                    source_tab,
                    source_workspace,
                    "Source tab",
                    layout(&[301, 302], Some(301)),
                    3,
                ),
                tab(
                    destination_first,
                    destination_workspace,
                    "First",
                    layout(&[401], Some(401)),
                    2,
                ),
                tab(
                    destination_second,
                    destination_workspace,
                    "Remembered",
                    layout(&[], None),
                    5,
                ),
            ],
            selection: Selection {
                project_id: Some(source_project),
                workspace_id: Some(source_workspace),
                tab_id: Some(source_tab),
                ..Selection::default()
            },
            layout: LayoutDocument {
                json: "{}".into(),
                revision: 11,
            },
            layout_templates: None,
            template_launch_schema: None,
            pane_labels: None,
            agent_profiles: None,
            agent_profile_roster: None,
            prompts: None,
            tab_close_schema: None,
            engine_options_schema: None,
            pane_tab_drop_schema: Some(1),
            pane_workspace_drop_schema: Some(1),
            history_grid_schema: None,
            custom_cli_schema: None,
        }
    }

    #[test]
    fn preview_uses_remembered_membership_then_falls_back_to_first_tab() {
        let catalog = catalog();
        let mut config = Config::default();
        config.recent.push(crate::workspace_nav_store::Visit {
            workspace: id(20),
            tab: Some(id(201)),
        });
        let cached = preview(&catalog, &config, id(100), id(301), id(20), id(777)).unwrap();
        assert_eq!(cached.to_tab, id(201));
        assert_eq!(cached.name, "Destination · Review · Remembered");
        config.recent[0].tab = Some(id(999));
        assert_eq!(
            preview(&catalog, &config, id(100), id(301), id(20), id(777))
                .unwrap()
                .to_tab,
            id(200)
        );
    }

    #[test]
    fn gates_require_capability_ready_workspaces_projects_and_exact_selection() {
        let mut catalog = catalog();
        let config = Config::default();
        let args = (id(100), id(301), id(20), id(777));
        assert!(preview(&catalog, &config, args.0, args.1, args.2, args.3).is_some());
        catalog.pane_workspace_drop_schema = None;
        assert!(preview(&catalog, &config, args.0, args.1, args.2, args.3).is_none());
        catalog.pane_workspace_drop_schema = Some(1);
        catalog.workspaces[1].status = WorkspaceStatus::Preparing;
        assert!(preview(&catalog, &config, args.0, args.1, args.2, args.3).is_none());
        catalog.workspaces[1].status = WorkspaceStatus::Ready;
        catalog.projects.pop();
        assert!(preview(&catalog, &config, args.0, args.1, args.2, args.3).is_none());
        catalog.projects.push(Project {
            id: id(2),
            canonical_path: PathBuf::from("/d"),
            git_common_dir: PathBuf::from("/d/.git"),
            name: "D".into(),
            roots: vec![],
            revision: 1,
        });
        catalog.selection.tab_id = Some(id(200));
        assert!(preview(&catalog, &config, args.0, args.1, args.2, args.3).is_none());
    }

    #[test]
    fn cross_project_preview_matches_real_move_for_empty_and_occupied_targets() {
        for (target_tab, expected_target) in [(id(201), None), (id(200), Some(id(401)))] {
            let mut catalog = catalog();
            let config = Config {
                recent: vec![crate::workspace_nav_store::Visit {
                    workspace: id(20),
                    tab: Some(target_tab),
                }],
                ..Config::default()
            };
            let cached = preview(&catalog, &config, id(100), id(301), id(20), id(777)).unwrap();
            assert!(
                matches!(&cached.command, Command::Transfer { pane, to_tab, target, side: Direction::Right, split } if *pane == id(301) && *to_tab == target_tab && *target == expected_target && *split == id(777))
            );
            assert_eq!(
                cached.layout.panes(),
                if target_tab == id(201) {
                    vec![id(301)]
                } else {
                    vec![id(401), id(301)]
                }
            );
            assert!(matches(&catalog, &cached));
            let source_before = catalog.tabs[0].layout_json.clone();
            catalog.layout.revision += 1;
            assert!(
                matches(&catalog, &cached),
                "global focus/layout revision is not this guard"
            );
            assert_eq!(catalog.tabs[0].layout_json, source_before);
        }
    }

    #[test]
    fn stale_source_destination_and_target_metadata_are_rejected() {
        let config = Config {
            recent: vec![crate::workspace_nav_store::Visit {
                workspace: id(20),
                tab: Some(id(201)),
            }],
            ..Config::default()
        };
        let cached = preview(&catalog(), &config, id(100), id(301), id(20), id(777)).unwrap();
        let mut changed = catalog();
        changed.workspaces[0].revision += 1;
        assert!(!matches(&changed, &cached));
        let mut changed = catalog();
        changed.workspaces[1].revision += 1;
        assert!(!matches(&changed, &cached));
        let mut changed = catalog();
        changed.tabs[2].revision += 1;
        assert!(!matches(&changed, &cached));
        let mut changed = catalog();
        changed.selection.tab_id = Some(id(200));
        assert!(!matches(&changed, &cached));
        let mut changed = catalog();
        changed.tabs[0].layout_json = serde_json::to_string(&layout(&[302], Some(302))).unwrap();
        assert!(!matches(&changed, &cached));
        let mut changed = catalog();
        changed.tabs.swap(1, 2);
        assert!(matches(&changed, &cached));
        let mut changed = catalog();
        changed.tabs.retain(|tab| tab.id != id(201));
        assert!(!matches(&changed, &cached));
    }

    #[test]
    fn missing_tabs_invalid_trees_and_non_source_panes_are_rejected() {
        let mut current = catalog();
        let config = Config::default();
        assert!(preview(&current, &config, id(100), id(999), id(20), id(777)).is_none());
        current.tabs.retain(|tab| tab.id != id(201));
        assert!(preview(&current, &config, id(100), id(301), id(20), id(777)).is_some());
        let mut remembered = Config::default();
        remembered.recent.push(crate::workspace_nav_store::Visit {
            workspace: id(20),
            tab: Some(id(201)),
        });
        assert_eq!(
            preview(&current, &remembered, id(100), id(301), id(20), id(777))
                .unwrap()
                .to_tab,
            id(200),
            "a deleted remembered tab falls back during initial preview"
        );
        current.tabs.retain(|tab| tab.workspace_id != id(20));
        assert!(preview(&current, &remembered, id(100), id(301), id(20), id(777)).is_none());
        let mut invalid = catalog();
        invalid.tabs[0].layout_json = "{".into();
        assert!(preview(&invalid, &config, id(100), id(301), id(20), id(777)).is_none());
        invalid = catalog();
        invalid.tabs[2].layout_json = "{".into();
        assert!(preview(&invalid, &remembered, id(100), id(301), id(20), id(777)).is_none());
    }
}
