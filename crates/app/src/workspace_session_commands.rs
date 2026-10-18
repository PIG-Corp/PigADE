//! Workspace overview uses the existing product worker and native resume review.
use crate::{
    CanvasApp,
    product::{Dialog, DialogKind, ProductAction as A},
    protocol,
    workspace_sessions::Target,
};
use gpui::Context;
use serde_json::{Value, json};
use uuid::Uuid;

pub(crate) struct ResumeRequest {
    pub request: Uuid,
    pub dialog: Uuid,
    pub target: Target,
}

impl CanvasApp {
    pub(crate) fn workspace_session_action(&mut self, action: A, cx: &mut Context<Self>) {
        if let A::SelectWorkspaceSession(index) = action {
            if !self.product.busy {
                self.product.workspace_sessions.select(index);
                self.product.workspace_sessions_scroll.scroll_to_item(index);
                cx.notify();
            }
            return;
        }
        if self.product.busy || self.product.workspace_sessions.request_pending() {
            self.product.notice = Some("Дождитесь завершения текущего запроса".into());
            cx.notify();
            return;
        }
        let command: Result<Value, String> = match action {
            A::OpenWorkspaceSessions => {
                let Some(catalog) = self.product.catalog.as_ref() else {
                    return;
                };
                if catalog.workspace_sessions_schema != Some(1) {
                    self.product.notice=Some("Работающая служба не поддерживает обзор воркспейса. Неразмещённые сессии доступны через палитру; обновление службы — отдельное действие.".into());
                    cx.notify();
                    return;
                }
                let dialog = Dialog::new(DialogKind::WorkspaceSessions);
                let command = self.product.workspace_sessions.open(catalog, dialog.id);
                if command.is_ok() {
                    self.product.dialog = Some(dialog);
                    self.product
                        .workspace_sessions_scroll
                        .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                }
                command
            }
            A::RefreshWorkspaceSessions => self.product.workspace_sessions.refresh(),
            A::FirstWorkspaceSessions => self.product.workspace_sessions.first(),
            A::WorkspaceSessionsPage(next) => {
                if next {
                    self.product.workspace_sessions.next()
                } else {
                    self.product.workspace_sessions.previous()
                }
            }
            A::OpenWorkspaceSession => {
                let Some(sheet) = self.product.workspace_sessions.sheet.as_ref() else {
                    return;
                };
                if !sheet.target.current(self.product.catalog.as_ref()) || sheet.error.is_some() {
                    self.product.notice = Some("Воркспейс изменился. Обновите обзор сессий".into());
                    cx.notify();
                    return;
                }
                let Some(entry) = self.product.workspace_sessions.selected_entry() else {
                    return;
                };
                let (tab, session) = (entry.tab_id, entry.session);
                self.navigation_action(A::ActivatePane { tab, session }, cx);
                return;
            }
            A::PrepareWorkspaceSessionResume => {
                if self.product_launch_unknown {
                    self.product.notice =
                        Some("Сначала проверьте неизвестный исход предыдущего запуска".into());
                    cx.notify();
                    return;
                }
                let Some(dialog) = self
                    .product
                    .dialog
                    .as_ref()
                    .filter(|dialog| dialog.kind == DialogKind::WorkspaceSessions)
                    .map(|dialog| dialog.id)
                else {
                    return;
                };
                let Some(catalog) = self.product.catalog.as_ref() else {
                    return;
                };
                match self
                    .product
                    .workspace_sessions
                    .resume_command(catalog, dialog)
                {
                    Ok(mut command) => {
                        let request = Uuid::new_v4();
                        command["_ui_request"] = json!(request);
                        if self.try_product_command(command, cx) {
                            self.pending_launch_review = true;
                            self.pending_workspace_resume = Some(ResumeRequest {
                                request,
                                dialog,
                                target: self
                                    .product
                                    .workspace_sessions
                                    .sheet
                                    .as_ref()
                                    .expect("validated sheet")
                                    .target
                                    .clone(),
                            });
                        }
                        return;
                    }
                    Err(error) => Err(error),
                }
            }
            _ => return,
        };
        match command {
            Ok(command) => {
                let request = command
                    .get("_ui_request")
                    .and_then(Value::as_str)
                    .and_then(|id| id.parse().ok());
                if !self.try_product_command(command, cx)
                    && let Some(request) = request
                {
                    self.product.workspace_sessions.abort_unsent(request);
                }
            }
            Err(error) => self.product.notice = Some(error),
        }
        cx.notify();
    }

    /// Consume only this feature's read replies. A closed/replaced overview may
    /// leave an unused prepared plan; it never reopens a review or starts a CLI.
    pub(crate) fn receive_workspace_session_reply(
        &mut self,
        reply: &Result<protocol::ProductReply, protocol::ProductError>,
    ) -> bool {
        let id = match reply {
            Ok(reply) => reply.ui_request,
            Err(error) => error.ui_request,
        };
        let Some(request) = self
            .pending_workspace_resume
            .as_ref()
            .filter(|pending| Some(pending.request) == id)
        else {
            return false;
        };
        let valid = self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.id == request.dialog && dialog.kind == DialogKind::WorkspaceSessions
        }) && request.target.current(self.product.catalog.as_ref());
        self.pending_workspace_resume = None;
        if !valid {
            self.pending_launch_review = false;
        }
        !valid
    }
}
