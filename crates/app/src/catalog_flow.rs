//! Metadata edits retain the exact object and revision the user opened.
//! These batches contain no process or placement operations.
use pigade_storage::{
    Catalog, CatalogBatch, CatalogWrite, Project, Tab, Workspace, WorkspaceStatus,
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntityKind {
    Project,
    Workspace,
    Tab,
}

#[derive(Clone, Debug)]
pub(crate) enum ManagedEntity {
    Project(Project),
    Workspace(Workspace),
    Tab(Tab),
}

impl ManagedEntity {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Project(value) => &value.name,
            Self::Workspace(value) => &value.task,
            Self::Tab(value) => &value.name,
        }
    }

    pub(crate) fn rename(&self, name: &str) -> Result<CatalogBatch, String> {
        let name = name.trim();
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err("Введите название без переносов строк, до 256 байт UTF-8".into());
        }
        let mut batch = CatalogBatch::default();
        match self {
            Self::Project(value) => {
                let mut value = value.clone();
                value.name = name.into();
                batch.projects.push(CatalogWrite {
                    expected_revision: Some(value.revision),
                    value,
                });
            }
            Self::Workspace(value) => {
                let mut value = value.clone();
                value.task = name.into();
                batch.workspaces.push(CatalogWrite {
                    expected_revision: Some(value.revision),
                    value,
                });
            }
            Self::Tab(value) => {
                let mut value = value.clone();
                value.name = name.into();
                batch.tabs.push(CatalogWrite {
                    expected_revision: Some(value.revision),
                    value,
                });
            }
        }
        Ok(batch)
    }

    /// Archive is a visibility change. Restoring selects the workspace; the
    /// host revalidates its checkout before it can become Ready again.
    pub(crate) fn archive(&self, catalog: &Catalog) -> Result<CatalogBatch, String> {
        let Self::Workspace(original) = self else {
            return Err("Архив доступен для воркспейсов".into());
        };
        let mut workspace = original.clone();
        let restoring = match workspace.status {
            WorkspaceStatus::Ready => false,
            WorkspaceStatus::Archived => true,
            _ => return Err("Сначала завершите подготовку или исправьте ошибку воркспейса".into()),
        };
        workspace.status = if restoring {
            WorkspaceStatus::Ready
        } else {
            WorkspaceStatus::Archived
        };
        let mut batch = CatalogBatch {
            workspaces: vec![CatalogWrite {
                expected_revision: Some(workspace.revision),
                value: workspace,
            }],
            ..Default::default()
        };
        if restoring || catalog.selection.workspace_id == Some(original.id) {
            let destination = if restoring {
                Some(original)
            } else {
                catalog
                    .workspaces
                    .iter()
                    .filter(|value| {
                        value.id != original.id && value.status == WorkspaceStatus::Ready
                    })
                    .min_by_key(|value| value.project_id != original.project_id)
            };
            let mut selection = catalog.selection.clone();
            selection.project_id = destination
                .map(|value| value.project_id)
                .or(Some(original.project_id));
            selection.workspace_id = destination.map(|value| value.id);
            selection.tab_id = destination
                .and_then(|workspace| {
                    catalog
                        .tabs
                        .iter()
                        .find(|tab| tab.workspace_id == workspace.id)
                })
                .map(|tab| tab.id);
            batch.selection = Some(CatalogWrite {
                expected_revision: Some(selection.revision),
                value: selection,
            });
        }
        Ok(batch)
    }
}

pub(crate) fn first_active_workspace(catalog: &Catalog, project: Uuid) -> Option<&Workspace> {
    catalog
        .workspaces
        .iter()
        .find(|value| value.project_id == project && value.status != WorkspaceStatus::Archived)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_storage::{Selection, WorkspaceMode};

    fn workspace(id: Uuid, project: Uuid) -> Workspace {
        Workspace {
            id,
            project_id: project,
            task: "Task".into(),
            cwd: "/repo".into(),
            mode: WorkspaceMode::SharedCheckout,
            status: WorkspaceStatus::Ready,
            error: None,
            context_revision: None,
            revision: 7,
        }
    }
    fn catalog(workspaces: Vec<Workspace>) -> Catalog {
        Catalog {
            projects: vec![],
            workspaces,
            tabs: vec![],
            selection: Selection::default(),
            layout: Default::default(),
            layout_templates: Some(vec![]),
            template_launch_schema: Some(1),
            pane_labels: Some(vec![]),
            agent_profiles: None,
            prompts: None,
            tab_close_schema: None,
            engine_options_schema: Some(1),
        }
    }

    #[test]
    fn rename_is_a_cas_on_the_opened_object_and_preserves_placement() {
        let tab = Tab {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            name: "Old".into(),
            layout_json: "saved exact bytes".into(),
            active_pane: Some(Uuid::new_v4()),
            revision: 41,
        };
        let edit = ManagedEntity::Tab(tab.clone());
        let batch = edit.rename("  New  ").unwrap();
        assert!(batch.workspaces.is_empty() && batch.selection.is_none() && batch.layout.is_none());
        let write = &batch.tabs[0];
        assert_eq!(write.expected_revision, Some(41));
        let mut expected = tab;
        expected.name = "New".into();
        assert_eq!(write.value, expected);
        assert!(edit.rename("\n").is_err());
        assert!(edit.rename("two\nlines").is_err());
        assert!(edit.rename(&"a".repeat(257)).is_err());
    }

    #[test]
    fn archiving_selected_workspace_selects_neighbor_without_deleting_anything() {
        let project = Uuid::new_v4();
        let first = workspace(Uuid::new_v4(), project);
        let second = workspace(Uuid::new_v4(), project);
        let other = workspace(Uuid::new_v4(), Uuid::new_v4());
        let mut catalog = catalog(vec![other, first.clone(), second.clone()]);
        catalog.selection.workspace_id = Some(first.id);
        catalog.selection.revision = 9;
        let batch = ManagedEntity::Workspace(first.clone())
            .archive(&catalog)
            .unwrap();
        let mut expected = first;
        expected.status = WorkspaceStatus::Archived;
        assert_eq!(batch.workspaces[0].value, expected);
        let selection = batch.selection.unwrap();
        assert_eq!(selection.value.workspace_id, Some(second.id));
        assert_eq!(selection.expected_revision, Some(9));
        assert!(batch.layout.is_none() && batch.tabs.is_empty() && batch.projects.is_empty());
    }

    #[test]
    fn archiving_last_workspace_and_restore_have_explicit_selection() {
        let mut value = workspace(Uuid::new_v4(), Uuid::new_v4());
        let mut catalog = catalog(vec![value.clone()]);
        catalog.selection.workspace_id = Some(value.id);
        let batch = ManagedEntity::Workspace(value.clone())
            .archive(&catalog)
            .unwrap();
        let selection = batch.selection.unwrap().value;
        assert_eq!(selection.project_id, Some(value.project_id));
        assert!(selection.workspace_id.is_none() && selection.tab_id.is_none());
        value.status = WorkspaceStatus::Archived;
        let batch = ManagedEntity::Workspace(value.clone())
            .archive(&catalog)
            .unwrap();
        assert_eq!(batch.selection.unwrap().value.workspace_id, Some(value.id));
        assert_eq!(batch.workspaces[0].value.status, WorkspaceStatus::Ready);
        value.status = WorkspaceStatus::MissingPath;
        assert!(ManagedEntity::Workspace(value).archive(&catalog).is_err());
    }
}
