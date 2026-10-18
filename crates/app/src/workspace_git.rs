//! A workspace-bound, read-only snapshot. No periodic Git scans or UI-thread I/O.
use crate::{
    CanvasApp, WakeSignal,
    product::{Dialog, DialogKind, ProductAction},
    protocol,
};
use gpui::Context;
use pigade_storage::{Catalog, WorkspaceStatus};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::mpsc, thread};
use uuid::Uuid;

pub(crate) struct Worker {
    sender: mpsc::SyncSender<Value>,
    receiver: mpsc::Receiver<Result<protocol::ProductReply, protocol::ProductError>>,
}

impl Worker {
    pub(crate) fn new(socket: PathBuf, wake: WakeSignal) -> Self {
        let (sender, requests) = mpsc::sync_channel::<Value>(1);
        let (results, receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            while let Ok(request) = requests.recv() {
                let result = protocol::product_request(&socket, request);
                if results.send(result).is_err() {
                    break;
                }
                wake.notify();
            }
        });
        Self { sender, receiver }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub workspace: Uuid,
    pub revision: u64,
    pub project: Uuid,
    pub cwd: PathBuf,
    pub name: String,
}

impl Target {
    fn selected(catalog: &Catalog) -> Option<Self> {
        let workspace = catalog.workspaces.iter().find(|workspace| {
            Some(workspace.id) == catalog.selection.workspace_id
                && workspace.status == WorkspaceStatus::Ready
        })?;
        Some(Self {
            workspace: workspace.id,
            revision: workspace.revision,
            project: workspace.project_id,
            cwd: workspace.cwd.clone(),
            name: workspace.task.clone(),
        })
    }

    fn current(&self, catalog: Option<&Catalog>) -> bool {
        catalog.and_then(Self::selected).is_some_and(|current| {
            current.workspace == self.workspace
                && current.revision == self.revision
                && current.project == self.project
                && current.cwd == self.cwd
        })
    }

    fn matches(&self, snapshot: &protocol::WorkspaceGitSnapshot) -> bool {
        snapshot.workspace_id == self.workspace
            && snapshot.workspace_revision == self.revision
            && snapshot.project_id == self.project
            && snapshot.cwd == self.cwd
    }
}

#[derive(Clone)]
pub(crate) struct GitSheet {
    pub dialog: Uuid,
    pub target: Target,
    pub snapshot: Option<protocol::WorkspaceGitSnapshot>,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
struct Pending {
    request: Uuid,
    dialog: Uuid,
    target: Target,
}

impl GitSheet {
    fn receive(
        &mut self,
        pending: &Pending,
        catalog: Option<&Catalog>,
        result: Result<protocol::WorkspaceGitSnapshot, String>,
    ) {
        if self.dialog != pending.dialog || self.target != pending.target {
            return;
        }
        self.loading = false;
        if !self.target.current(catalog) {
            self.error = Some("Воркспейс изменился. Закройте Git и откройте его снова.".into());
            return;
        }
        match result {
            Ok(snapshot) if self.target.matches(&snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Ok(_) => {
                self.error =
                    Some("Ответ Git относится к другой папке или ревизии воркспейса.".into())
            }
            Err(error) => self.error = Some(error.chars().take(512).collect()),
        }
    }
}

#[derive(Default)]
pub(crate) struct Flow {
    pending: Option<Pending>,
}

impl CanvasApp {
    pub(crate) fn workspace_git_action(&mut self, action: ProductAction, cx: &mut Context<Self>) {
        if action == ProductAction::OpenWorkspaceGit {
            let Some(target) = self.product.catalog.as_ref().and_then(Target::selected) else {
                self.product.notice =
                    Some("Выберите доступный воркспейс, чтобы открыть Git.".into());
                cx.notify();
                return;
            };
            let dialog = Dialog::new(DialogKind::WorkspaceGit);
            self.product.git_sheet = Some(GitSheet {
                dialog: dialog.id,
                target,
                snapshot: None,
                loading: false,
                error: None,
            });
            self.product.git_scroll = Default::default();
            self.product.dialog = Some(dialog);
        }
        self.refresh_workspace_git(cx);
    }

    fn refresh_workspace_git(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = &mut self.product.git_sheet else {
            return;
        };
        if !self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.kind == DialogKind::WorkspaceGit && dialog.id == sheet.dialog
        }) || sheet.loading
        {
            return;
        }
        if !sheet.target.current(self.product.catalog.as_ref()) {
            sheet.error = Some("Воркспейс изменился. Закройте Git и откройте его снова.".into());
        } else if self.git_flow.pending.is_some() {
            sheet.error = Some(
                "Предыдущий запрос Git ещё выполняется. Повторите обновление после его завершения."
                    .into(),
            );
        } else {
            let request = Uuid::new_v4();
            let command = json!({"command":"workspace_git_snapshot","workspace":sheet.target.workspace,"expected_revision":sheet.target.revision,"_ui_request":request});
            if self.git_worker.sender.try_send(command).is_ok() {
                self.git_flow.pending = Some(Pending {
                    request,
                    dialog: sheet.dialog,
                    target: sheet.target.clone(),
                });
                sheet.loading = true;
                sheet.error = None;
            } else {
                sheet.error =
                    Some("Git-запрос не принят: очередь занята. Повторите обновление.".into());
            }
        }
        cx.notify();
    }

    pub(crate) fn service_workspace_git(&mut self, cx: &mut Context<Self>) {
        let Ok(reply) = self.git_worker.receiver.try_recv() else {
            return;
        };
        let request = match &reply {
            Ok(reply) => reply.ui_request,
            Err(error) => error.ui_request,
        };
        if !self
            .git_flow
            .pending
            .as_ref()
            .is_some_and(|pending| Some(pending.request) == request)
        {
            return;
        }
        let pending = self.git_flow.pending.take().expect("matching Git request");
        // A late response never reopens a canceled sheet or changes another dialog.
        if !self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.id == pending.dialog && dialog.kind == DialogKind::WorkspaceGit
        }) {
            return;
        }
        let result = reply.map_err(|error| error.message).and_then(|reply| {
            reply
                .workspace_git_snapshot
                .ok_or_else(|| "Session host не вернул Git-статус.".into())
        });
        if let Some(sheet) = &mut self.product.git_sheet {
            sheet.receive(&pending, self.product.catalog.as_ref(), result);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Catalog, GitSheet, Pending) {
        let workspace = Uuid::new_v4();
        let project = Uuid::new_v4();
        let catalog = Catalog {
            projects: vec![],
            workspaces: vec![pigade_storage::Workspace {
                id: workspace,
                project_id: project,
                task: "Task".into(),
                cwd: PathBuf::from("/worktree"),
                mode: pigade_storage::WorkspaceMode::SharedCheckout,
                status: WorkspaceStatus::Ready,
                error: None,
                context_revision: None,
                revision: 4,
            }],
            tabs: vec![],
            selection: pigade_storage::Selection {
                workspace_id: Some(workspace),
                ..Default::default()
            },
            layout: Default::default(),
            layout_templates: None,
            template_launch_schema: None,
            pane_labels: None,
            agent_profiles: None,
            prompts: None,
            tab_close_schema: None,
        };
        let target = Target::selected(&catalog).unwrap();
        let dialog = Uuid::new_v4();
        let pending = Pending {
            request: Uuid::new_v4(),
            dialog,
            target: target.clone(),
        };
        (
            catalog,
            GitSheet {
                dialog,
                target,
                snapshot: None,
                loading: true,
                error: None,
            },
            pending,
        )
    }
    fn snapshot(target: &Target) -> protocol::WorkspaceGitSnapshot {
        protocol::WorkspaceGitSnapshot {
            workspace_id: target.workspace,
            workspace_revision: target.revision,
            project_id: target.project,
            cwd: target.cwd.clone(),
            branch: Some("main".into()),
            head: None,
            detached: false,
            entries: vec![],
            truncated: false,
        }
    }
    #[test]
    fn late_receipt_cannot_replace_another_sheet_and_changed_workspace_is_rejected() {
        let (mut catalog, mut sheet, pending) = fixture();
        let other = Pending {
            dialog: Uuid::new_v4(),
            ..pending.clone()
        };
        sheet.receive(&other, Some(&catalog), Ok(snapshot(&sheet.target)));
        assert!(sheet.loading && sheet.snapshot.is_none());
        catalog.workspaces[0].revision += 1;
        sheet.receive(&pending, Some(&catalog), Ok(snapshot(&sheet.target)));
        assert!(!sheet.loading && sheet.snapshot.is_none() && sheet.error.is_some());
    }
    #[test]
    fn matching_receipt_is_snapshot_only_and_failed_refresh_preserves_old_result() {
        let (catalog, mut sheet, pending) = fixture();
        let original = snapshot(&sheet.target);
        sheet.receive(&pending, Some(&catalog), Ok(original.clone()));
        assert_eq!(sheet.snapshot, Some(original.clone()));
        assert!(sheet.error.is_none());
        sheet.loading = true;
        sheet.receive(&pending, Some(&catalog), Err("timeout".into()));
        assert_eq!(sheet.snapshot, Some(original));
        assert!(!sheet.loading);
        assert_eq!(sheet.error.as_deref(), Some("timeout"));
    }
    #[test]
    fn same_id_with_different_cwd_or_project_is_not_accepted() {
        let (catalog, mut sheet, pending) = fixture();
        let mut wrong = snapshot(&sheet.target);
        wrong.cwd = "/another/worktree".into();
        sheet.receive(&pending, Some(&catalog), Ok(wrong));
        assert!(sheet.snapshot.is_none() && sheet.error.is_some());
        let mut wrong = snapshot(&sheet.target);
        wrong.project_id = Uuid::new_v4();
        sheet.receive(&pending, Some(&catalog), Ok(wrong));
        assert!(sheet.snapshot.is_none() && sheet.error.is_some());
    }
}
