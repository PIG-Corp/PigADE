//! Correlate the two catalog writes needed to open a project's first workspace.
use pigade_storage::{Catalog, WorkspaceStatus};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Project,
    Workspace(Uuid),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SetupFlow {
    request: Uuid,
    stage: Stage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    Ignore,
    Workspace { project: Uuid, request: Uuid },
    Ready,
}

impl SetupFlow {
    pub(crate) fn new(request: Uuid) -> Self {
        Self {
            request,
            stage: Stage::Project,
        }
    }

    pub(crate) fn matches(&self, request: Option<Uuid>) -> bool {
        request == Some(self.request)
    }

    pub(crate) fn accept(
        &mut self,
        request: Option<Uuid>,
        catalog: &Catalog,
    ) -> Result<Next, String> {
        if !self.matches(request) {
            return Ok(Next::Ignore);
        }
        let project = catalog
            .selection
            .project_id
            .ok_or("Проект не выбран после добавления")?;
        if !catalog.projects.iter().any(|value| value.id == project) {
            return Err("Добавленный проект отсутствует в каталоге".into());
        }
        if let Stage::Workspace(expected) = self.stage
            && project != expected
        {
            return Err("Ответ относится к другому проекту; повторите выбор проекта".into());
        }
        let ready = catalog
            .selection
            .workspace_id
            .and_then(|workspace| {
                catalog.workspaces.iter().find(|value| {
                    value.id == workspace
                        && value.project_id == project
                        && value.status == WorkspaceStatus::Ready
                })
            })
            .is_some_and(|workspace| {
                catalog.selection.tab_id.is_some_and(|tab| {
                    catalog
                        .tabs
                        .iter()
                        .any(|value| value.id == tab && value.workspace_id == workspace.id)
                })
            });
        if ready {
            return Ok(Next::Ready);
        }
        if matches!(self.stage, Stage::Workspace(_)) {
            return Err(
                "Основной воркспейс не готов. Откройте его в списке, чтобы увидеть причину.".into(),
            );
        }
        self.stage = Stage::Workspace(project);
        self.request = Uuid::new_v4();
        Ok(Next::Workspace {
            project,
            request: self.request,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_storage::{LayoutDocument, Project, Selection};

    fn catalog(project: Uuid) -> Catalog {
        Catalog {
            projects: vec![Project {
                id: project,
                canonical_path: "/project".into(),
                git_common_dir: "/project/.git".into(),
                name: "Project".into(),
                roots: vec![],
                revision: 0,
            }],
            workspaces: vec![],
            tabs: vec![],
            selection: Selection {
                project_id: Some(project),
                ..Default::default()
            },
            layout: LayoutDocument::default(),
            layout_templates: Some(vec![]),
            template_launch_schema: Some(1),
            pane_labels: Some(vec![]),
        }
    }

    #[test]
    fn only_matching_reply_advances_once_and_wrong_project_cannot_complete() {
        let request = Uuid::new_v4();
        let project = Uuid::new_v4();
        let mut flow = SetupFlow::new(request);
        let catalog = catalog(project);
        assert_eq!(
            flow.accept(Some(Uuid::new_v4()), &catalog).unwrap(),
            Next::Ignore
        );
        let Next::Workspace {
            project: actual,
            request: next,
        } = flow.accept(Some(request), &catalog).unwrap()
        else {
            panic!("missing initial workspace")
        };
        assert_eq!(actual, project);
        assert_eq!(flow.accept(Some(request), &catalog).unwrap(), Next::Ignore);
        assert!(flow.accept(Some(next), &catalog).is_err());
        let other = super::tests::catalog(Uuid::new_v4());
        assert!(flow.accept(Some(next), &other).is_err());
    }

    #[test]
    fn a_ready_workspace_is_reused_and_first_workspace_completes_exact_request() {
        use pigade_storage::{Tab, Workspace, WorkspaceMode};
        let request = Uuid::new_v4();
        let project = Uuid::new_v4();
        let mut flow = SetupFlow::new(request);
        let mut catalog = catalog(project);
        let Next::Workspace { request: next, .. } = flow.accept(Some(request), &catalog).unwrap()
        else {
            panic!("missing setup")
        };
        let workspace = Uuid::new_v4();
        let tab = Uuid::new_v4();
        catalog.workspaces.push(Workspace {
            id: workspace,
            project_id: project,
            task: "Основной".into(),
            cwd: "/project".into(),
            mode: WorkspaceMode::SharedCheckout,
            status: WorkspaceStatus::Ready,
            error: None,
            context_revision: None,
            revision: 0,
        });
        catalog.tabs.push(Tab {
            id: tab,
            workspace_id: workspace,
            name: "Development".into(),
            layout_json: "null".into(),
            active_pane: None,
            revision: 0,
        });
        catalog.selection.workspace_id = Some(workspace);
        catalog.selection.tab_id = Some(tab);
        assert_eq!(flow.accept(Some(next), &catalog).unwrap(), Next::Ready);
        assert_eq!(
            SetupFlow::new(request)
                .accept(Some(request), &catalog)
                .unwrap(),
            Next::Ready
        );
    }
}
