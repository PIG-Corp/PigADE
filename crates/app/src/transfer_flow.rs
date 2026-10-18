//! Pure catalog filtering and validation for the legacy transfer-to-tab action.

use pigade_layout::{Command, LayoutModel, LayoutState, TabLayout};
use pigade_storage::{Catalog, Tab, Workspace, WorkspaceStatus};
use uuid::Uuid;

pub(crate) const PAGE_SIZE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Source {
    pub(crate) tab: Uuid,
    pub(crate) pane: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) tab: Uuid,
    /// Project, workspace task, and tab name in that order.
    pub(crate) title: String,
    /// Empty for a blank destination, otherwise pane count and focus-mode state.
    pub(crate) subtitle: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Page {
    pub(crate) items: Vec<Target>,
    pub(crate) has_more: bool,
    pub(crate) offset: usize,
}

pub(crate) fn capture(catalog: &Catalog, pane: Uuid) -> Result<Source, String> {
    let tab_id = catalog
        .selection
        .tab_id
        .ok_or_else(|| "select a tab before transferring a pane".to_owned())?;
    let tab = catalog
        .tabs
        .iter()
        .find(|tab| tab.id == tab_id)
        .ok_or_else(|| "selected tab is missing".to_owned())?;
    let workspace = ready_workspace(catalog, tab.workspace_id)?;
    if catalog.selection.workspace_id != Some(workspace.id)
        || catalog.selection.project_id != Some(workspace.project_id)
    {
        return Err("selected workspace does not own the selected tab".into());
    }
    let layout = valid_tab_layout(tab)?;
    if !layout.panes().contains(&pane) {
        return Err("pane is not in selected tab".into());
    }
    Ok(Source { tab: tab_id, pane })
}

pub(crate) fn targets(catalog: &Catalog, source: &Source, query: &str, offset: usize) -> Page {
    if capture(catalog, source.pane).ok() != Some(*source) {
        return Page {
            items: Vec::new(),
            has_more: false,
            offset,
        };
    }
    let needle = query.trim().to_lowercase();
    let mut matches = Vec::new();
    for tab in &catalog.tabs {
        if tab.id == source.tab {
            continue;
        }
        let Some(workspace) = catalog
            .workspaces
            .iter()
            .find(|value| value.id == tab.workspace_id)
        else {
            continue;
        };
        if workspace.status != WorkspaceStatus::Ready {
            continue;
        }
        let Some(project) = catalog
            .projects
            .iter()
            .find(|value| value.id == workspace.project_id)
        else {
            continue;
        };
        let Ok(layout) = valid_tab_layout(tab) else {
            continue;
        };
        let searchable =
            format!("{}\n{}\n{}", project.name, workspace.task, tab.name).to_lowercase();
        if !needle.is_empty() && !searchable.contains(&needle) {
            continue;
        }
        let pane_count = layout.panes().len();
        let subtitle = if pane_count == 0 {
            String::new()
        } else if layout.focus_mode {
            format!("Панелей: {pane_count} · режим фокуса")
        } else {
            format!("Панелей: {pane_count}")
        };
        matches.push(Target {
            tab: tab.id,
            title: format!("{} / {} / {}", project.name, workspace.task, tab.name),
            subtitle,
        });
    }

    let has_more = matches.len() > offset.saturating_add(PAGE_SIZE);
    let items = matches.into_iter().skip(offset).take(PAGE_SIZE).collect();
    Page {
        items,
        has_more,
        offset,
    }
}

pub(crate) fn command(
    catalog: &Catalog,
    source: &Source,
    to_tab: Uuid,
    split: Uuid,
) -> Result<Command, String> {
    if catalog.selection.tab_id != Some(source.tab) {
        return Err("selected source tab changed".into());
    }
    let from = catalog
        .tabs
        .iter()
        .find(|tab| tab.id == source.tab)
        .ok_or_else(|| "source tab is missing".to_owned())?;
    let to = catalog
        .tabs
        .iter()
        .find(|tab| tab.id == to_tab)
        .ok_or_else(|| "destination tab is missing".to_owned())?;
    let source_workspace = ready_workspace(catalog, from.workspace_id)?;
    ready_workspace(catalog, to.workspace_id)?;
    if catalog.selection.workspace_id != Some(source_workspace.id)
        || catalog.selection.project_id != Some(source_workspace.project_id)
    {
        return Err("selected workspace does not own the source tab".into());
    }
    let from_layout = valid_tab_layout(from)?;
    if !from_layout.panes().contains(&source.pane) {
        return Err("source pane moved to another tab".into());
    }
    let to_layout = valid_tab_layout(to)?;
    let state = LayoutState {
        tabs: [(source.tab, from_layout), (to_tab, to_layout)]
            .into_iter()
            .collect(),
    };
    let (command, _) =
        crate::pointer_layout::tab_transfer_preview(&state, source.tab, source.pane, to_tab, split)
            .ok_or_else(|| "pane transfer preview is invalid".to_owned())?;
    Ok(command)
}

fn ready_workspace(catalog: &Catalog, id: Uuid) -> Result<&Workspace, String> {
    let workspace = catalog
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .ok_or_else(|| "tab workspace is missing".to_owned())?;
    if workspace.status != WorkspaceStatus::Ready {
        return Err("tab workspace is not ready".into());
    }
    catalog
        .projects
        .iter()
        .any(|project| project.id == workspace.project_id)
        .then_some(workspace)
        .ok_or_else(|| "tab project is missing".to_owned())
}

fn valid_tab_layout(tab: &Tab) -> Result<TabLayout, String> {
    let layout: TabLayout = serde_json::from_str(&tab.layout_json)
        .map_err(|error| format!("invalid tab layout: {error}"))?;
    let state = LayoutState {
        tabs: [(tab.id, layout.clone())].into_iter().collect(),
    };
    LayoutModel::new(state).map_err(|error| format!("invalid tab layout tree: {error}"))?;
    if tab.active_pane != layout.focused {
        return Err("tab focus projection is inconsistent".into());
    }
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_layout::{Axis, Node};
    use pigade_storage::{LayoutDocument, Project, Selection, WorkspaceMode};
    use std::path::PathBuf;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn project(id: Uuid, name: &str) -> Project {
        Project {
            id,
            canonical_path: PathBuf::new(),
            git_common_dir: PathBuf::new(),
            name: name.into(),
            roots: Vec::new(),
            revision: 0,
        }
    }

    fn workspace(id: Uuid, project_id: Uuid, task: &str, status: WorkspaceStatus) -> Workspace {
        Workspace {
            id,
            project_id,
            task: task.into(),
            cwd: PathBuf::new(),
            mode: WorkspaceMode::SharedCheckout,
            status,
            error: None,
            context_revision: None,
            revision: 0,
        }
    }

    fn layout(panes: &[Uuid], focus: Option<Uuid>, focus_mode: bool) -> TabLayout {
        let root = match panes {
            [] => None,
            [pane] => Some(Node::pane(*pane)),
            panes => Some(Node::Split {
                id: id(900 + panes[0].as_u128()),
                axis: Axis::Horizontal,
                weights: vec![1. / panes.len() as f64; panes.len()],
                children: panes.iter().copied().map(Node::pane).collect(),
            }),
        };
        TabLayout {
            root,
            focused: focus,
            focus_mode,
        }
    }

    fn tab(id: Uuid, workspace_id: Uuid, name: &str, layout: &TabLayout) -> Tab {
        Tab {
            id,
            workspace_id,
            name: name.into(),
            layout_json: serde_json::to_string(layout).unwrap(),
            active_pane: layout.focused,
            revision: 0,
        }
    }

    fn catalog() -> Catalog {
        Catalog {
            projects: vec![project(id(1), "Проект")],
            workspaces: vec![workspace(
                id(2),
                id(1),
                "Разработка",
                WorkspaceStatus::Ready,
            )],
            tabs: vec![
                tab(
                    id(10),
                    id(2),
                    "Исходная",
                    &layout(&[id(20), id(21)], Some(id(20)), false),
                ),
                tab(id(11), id(2), "Пустая", &layout(&[], None, false)),
                tab(
                    id(12),
                    id(2),
                    "Обзор",
                    &layout(&[id(30), id(31)], Some(id(31)), true),
                ),
            ],
            selection: Selection {
                project_id: Some(id(1)),
                workspace_id: Some(id(2)),
                tab_id: Some(id(10)),
                ..Default::default()
            },
            layout: LayoutDocument {
                json: "null".into(),
                revision: 0,
            },
            layout_templates: None,
            template_launch_schema: None,
            pane_labels: None,
            agent_profiles: None,
            agent_profile_roster: None,
            prompts: None,
            tab_close_schema: None,
            engine_options_schema: Some(1),
            pane_tab_drop_schema: Some(1),
            pane_workspace_drop_schema: Some(1),
            history_grid_schema: Some(1),
            custom_cli_schema: Some(1),
        }
    }

    #[test]
    fn capture_requires_selected_tab_pane_and_ready_workspace() {
        let value = catalog();
        assert_eq!(
            capture(&value, id(20)).unwrap(),
            Source {
                tab: id(10),
                pane: id(20)
            }
        );
        assert!(capture(&value, id(30)).is_err());

        let mut no_selection = value.clone();
        no_selection.selection.tab_id = None;
        assert!(capture(&no_selection, id(20)).is_err());

        let mut wrong_workspace = value.clone();
        wrong_workspace.selection.workspace_id = Some(id(99));
        assert!(capture(&wrong_workspace, id(20)).is_err());

        let mut archived = value;
        archived.workspaces[0].status = WorkspaceStatus::Archived;
        assert!(capture(&archived, id(20)).is_err());
    }

    #[test]
    fn stale_source_has_no_actionable_targets() {
        let mut value = catalog();
        let source = capture(&value, id(20)).unwrap();
        value.selection.tab_id = Some(id(11));
        assert!(targets(&value, &source, "", 0).items.is_empty());
        value.selection.tab_id = Some(source.tab);
        value.workspaces[0].status = WorkspaceStatus::Archived;
        assert!(targets(&value, &source, "", 0).items.is_empty());
    }

    #[test]
    fn targets_are_stable_searchable_and_paged_at_sixty_four() {
        let mut value = catalog();
        assert_eq!(
            targets(
                &value,
                &Source {
                    tab: id(10),
                    pane: id(20)
                },
                "ПРОЕКТ",
                0
            )
            .items
            .len(),
            2
        );
        assert_eq!(
            targets(
                &value,
                &Source {
                    tab: id(10),
                    pane: id(20)
                },
                "РАЗРАБОТКА",
                0
            )
            .items
            .len(),
            2
        );
        assert!(
            targets(
                &value,
                &Source {
                    tab: id(10),
                    pane: id(20)
                },
                "обзор",
                0
            )
            .items
            .len()
                == 1
        );
        let page = targets(
            &value,
            &Source {
                tab: id(10),
                pane: id(20),
            },
            "",
            0,
        );
        assert_eq!(page.items[0].tab, id(11));
        assert_eq!(page.items[0].subtitle, "");
        assert_eq!(page.items[1].tab, id(12));
        assert!(page.items[1].subtitle.contains("режим фокуса"));

        for index in 0..65 {
            let tab_id = id(100 + index);
            value.tabs.push(tab(
                tab_id,
                id(2),
                &format!("Tab {index}"),
                &layout(&[], None, false),
            ));
        }
        let first = targets(
            &value,
            &Source {
                tab: id(10),
                pane: id(20),
            },
            "",
            0,
        );
        let second = targets(
            &value,
            &Source {
                tab: id(10),
                pane: id(20),
            },
            "",
            64,
        );
        assert_eq!(first.items.len(), PAGE_SIZE);
        assert!(first.has_more);
        assert_eq!(second.items.len(), 3);
        assert!(!second.has_more);
        assert_eq!(second.offset, 64);
    }

    #[test]
    fn targets_exclude_archived_missing_and_malformed_records() {
        let mut value = catalog();
        value
            .workspaces
            .push(workspace(id(3), id(1), "Архив", WorkspaceStatus::Archived));
        value
            .tabs
            .push(tab(id(13), id(3), "Архивная", &layout(&[], None, false)));
        value.tabs.push(tab(
            id(14),
            id(99),
            "Без workspace",
            &layout(&[], None, false),
        ));
        value.tabs.push(Tab {
            id: id(15),
            workspace_id: id(2),
            name: "Malformed".into(),
            layout_json: "{".into(),
            active_pane: None,
            revision: 0,
        });
        value.workspaces.push(workspace(
            id(4),
            id(99),
            "Без проекта",
            WorkspaceStatus::Ready,
        ));
        value.tabs.push(tab(
            id(16),
            id(4),
            "Broken project",
            &layout(&[], None, false),
        ));
        let page = targets(
            &value,
            &Source {
                tab: id(10),
                pane: id(20),
            },
            "",
            0,
        );
        assert_eq!(
            page.items
                .iter()
                .map(|target| target.tab)
                .collect::<Vec<_>>(),
            [id(11), id(12)]
        );
    }

    #[test]
    fn command_supports_empty_populated_focus_and_cross_workspace_destinations() {
        let mut value = catalog();
        let original = value.clone();
        let source = Source {
            tab: id(10),
            pane: id(20),
        };
        let empty = command(&value, &source, id(11), id(50)).unwrap();
        assert!(matches!(
            empty,
            Command::Transfer {
                target: None,
                side: pigade_layout::Direction::Right,
                ..
            }
        ));
        let populated = command(&value, &source, id(12), id(51)).unwrap();
        assert!(
            matches!(populated, Command::Transfer { target: Some(target), side: pigade_layout::Direction::Right, .. } if target == id(31))
        );

        value.projects.push(project(id(60), "Другой проект"));
        value.workspaces.push(workspace(
            id(61),
            id(60),
            "Другой воркспейс",
            WorkspaceStatus::Ready,
        ));
        value.tabs.push(tab(
            id(62),
            id(61),
            "Цель",
            &layout(&[id(63)], Some(id(63)), false),
        ));
        let cross_workspace = command(&value, &source, id(62), id(52)).unwrap();
        assert!(
            matches!(cross_workspace, Command::Transfer { to_tab, target: Some(target), .. } if to_tab == id(62) && target == id(63))
        );
        assert_eq!(original, catalog());

        let mut model = LayoutModel::new(LayoutState {
            tabs: [
                (id(10), layout(&[id(20), id(21)], Some(id(20)), false)),
                (id(11), layout(&[], None, false)),
            ]
            .into_iter()
            .collect(),
        })
        .unwrap();
        let before = model.state().clone();
        let command = command(&value, &source, id(11), id(53)).unwrap();
        assert!(model.apply(command).unwrap());
        assert_eq!(model.state().pane_tab(id(20)), Some(id(11)));
        assert!(model.undo());
        assert_eq!(model.state(), &before);
    }

    #[test]
    fn command_rejects_stale_selection_moved_pane_unready_and_malformed_trees() {
        let value = catalog();
        let source = Source {
            tab: id(10),
            pane: id(20),
        };
        let mut wrong_selection = value.clone();
        wrong_selection.selection.tab_id = Some(id(11));
        assert!(command(&wrong_selection, &source, id(12), id(50)).is_err());

        let mut moved = value.clone();
        moved.tabs[0] = tab(
            id(10),
            id(2),
            "Исходная",
            &layout(&[id(21)], Some(id(21)), false),
        );
        assert!(command(&moved, &source, id(12), id(50)).is_err());

        let mut unready = value.clone();
        unready.workspaces[0].status = WorkspaceStatus::MissingPath;
        assert!(command(&unready, &source, id(12), id(50)).is_err());

        let mut malformed = value;
        malformed.tabs[2].layout_json = "{".into();
        assert!(command(&malformed, &source, id(12), id(50)).is_err());
    }
}
