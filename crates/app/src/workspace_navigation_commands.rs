//! UI navigation and confirmed catalog receipts; never starts a CLI.
use super::*;
use product::{Dialog, DialogKind, ProductAction as A};
use workspace_nav_store::Op;

impl CanvasApp {
    fn queue_workspace_preference(&mut self, op: Op) {
        match self.workspace_nav_writer.queue(op) {
            Ok(()) => {
                self.product.workspace_navigation.config = self.workspace_nav_writer.effective();
                self.product.workspace_navigation.invalidate();
            }
            Err(error) => self.product.workspace_navigation.error = Some(error),
        }
    }

    pub(super) fn workspace_catalog_changed(&mut self) {
        let visit = self
            .product
            .catalog
            .as_ref()
            .and_then(workspace_navigation::confirmed_visit);
        if visit != self.workspace_last_confirmed {
            self.workspace_last_confirmed = visit.clone();
            if let Some(visit) = visit {
                self.queue_workspace_preference(Op::Visit(visit));
            }
        }
        self.product.workspace_navigation.invalidate();
    }

    pub(super) fn service_workspace_navigation(&mut self, cx: &mut Context<Self>) {
        if let Some(result) = self.workspace_nav_writer.service() {
            match result {
                Ok(config) => {
                    self.product.workspace_navigation.config = config;
                    self.product.workspace_navigation.error = None;
                    self.product.workspace_navigation.invalidate();
                }
                Err(error) => {
                    self.close_flow
                        .fail(format!("Навигация не сохранена: {error}"));
                    self.product.workspace_navigation.error = Some(error);
                }
            }
            cx.notify();
        }
        self.product.workspace_navigation.saving = self.workspace_nav_writer.pending();
        self.product
            .workspace_navigation
            .refresh(self.product.catalog.as_ref(), self.product.dialog.as_ref());
    }

    pub(super) fn workspace_navigation_action(
        &mut self,
        action: A,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            A::OpenWorkspaceSwitcher => {
                self.prefix = false;
                self.product
                    .workspace_navigation
                    .open(self.product.catalog.as_ref());
                self.product.dialog = Some(Dialog::new(DialogKind::WorkspaceSwitcher));
            }
            A::ToggleProjectFold(project) => {
                if self
                    .product
                    .catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.projects.iter().any(|p| p.id == project))
                {
                    let collapsed = !self
                        .product
                        .workspace_navigation
                        .config
                        .collapsed_projects
                        .contains(&project);
                    self.queue_workspace_preference(Op::SetCollapsed { project, collapsed });
                }
            }
            A::RefreshWorkspaceNavigation => {
                self.workspace_nav_writer.refresh();
                self.product.workspace_navigation.invalidate();
                if !self.product.busy
                    && let Some(dialog) = self
                        .product
                        .dialog
                        .as_ref()
                        .filter(|dialog| dialog.kind == DialogKind::WorkspaceSwitcher)
                {
                    let dialog_id = dialog.id;
                    let request = uuid::Uuid::new_v4();
                    if self
                        .try_product_command(json!({"command":"catalog","_ui_request":request}), cx)
                    {
                        self.workspace_nav_reload = Some((request, dialog_id));
                    }
                }
            }
            A::WorkspacePage(delta) => {
                self.product
                    .workspace_navigation
                    .move_by(delta as isize * workspace_navigation::PAGE as isize);
            }
            A::ChooseWorkspace(id) => {
                let allowed = self
                    .product
                    .dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.kind == DialogKind::WorkspaceSwitcher);
                if !allowed || self.product.busy {
                    return;
                }
                self.product
                    .workspace_navigation
                    .refresh(self.product.catalog.as_ref(), self.product.dialog.as_ref());
                let row = self
                    .product
                    .workspace_navigation
                    .rows
                    .iter()
                    .find(|row| row.workspace == id)
                    .cloned();
                let workspace = self
                    .product
                    .catalog
                    .as_ref()
                    .and_then(|catalog| {
                        catalog.workspaces.iter().find(|w| {
                            w.id == id && row.as_ref().is_some_and(|r| r.project == w.project_id)
                        })
                    })
                    .cloned();
                match workspace {
                    Some(workspace)
                        if workspace.status == pigade_storage::WorkspaceStatus::Ready =>
                    {
                        self.product_action(A::SelectWorkspace(id), window, cx);
                    }
                    Some(_) => self.product_action(A::ManageWorkspace(id), window, cx),
                    None => {
                        self.product.notice = Some(
                            "Воркспейс изменился или больше недоступен. Обновите каталог".into(),
                        )
                    }
                }
            }
            _ => return,
        }
        self.service_workspace_navigation(cx);
        cx.notify();
    }
}
