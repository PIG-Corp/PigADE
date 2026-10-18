//! A workspace-bound, read-only snapshot. No periodic Git scans or UI-thread I/O.
use crate::{
    CanvasApp, WakeSignal,
    product::{Dialog, DialogKind, ProductAction},
    protocol,
};
use gpui::{ClipboardItem, Context};
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
    pub selected: usize,
    pub diff: Option<DiffSheet>,
}

#[derive(Clone)]
pub(crate) struct DiffSheet {
    pub path: String,
    pub side: protocol::GitDiffSide,
    pub result: Option<protocol::WorkspaceGitDiff>,
    pub loading: bool,
    pub error: Option<String>,
    pub page: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Snapshot,
    Diff {
        path: String,
        side: protocol::GitDiffSide,
    },
}

#[derive(Clone)]
struct Pending {
    request: Uuid,
    dialog: Uuid,
    target: Target,
    kind: Kind,
}

impl GitSheet {
    fn receive(
        &mut self,
        pending: &Pending,
        catalog: Option<&Catalog>,
        result: Result<protocol::WorkspaceGitSnapshot, String>,
    ) {
        if self.dialog != pending.dialog
            || self.target != pending.target
            || pending.kind != Kind::Snapshot
        {
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
    next: Option<Pending>,
}

impl Pending {
    fn current(&self, sheet: &GitSheet, catalog: Option<&Catalog>) -> bool {
        self.dialog == sheet.dialog
            && self.target == sheet.target
            && self.target.current(catalog)
            && match &self.kind {
                Kind::Snapshot => sheet.diff.is_none(),
                Kind::Diff { path, side } => sheet
                    .diff
                    .as_ref()
                    .is_some_and(|diff| diff.path == *path && diff.side == *side),
            }
    }
    fn command(&self) -> Value {
        match &self.kind {
            Kind::Snapshot => {
                json!({"command":"workspace_git_snapshot","workspace":self.target.workspace,"expected_revision":self.target.revision,"_ui_request":self.request})
            }
            Kind::Diff { path, side } => {
                json!({"command":"workspace_git_diff","workspace":self.target.workspace,"expected_revision":self.target.revision,"path":path,"side":side,"_ui_request":self.request})
            }
        }
    }
}

impl GitSheet {
    fn receive_diff(
        &mut self,
        pending: &Pending,
        catalog: Option<&Catalog>,
        result: Result<protocol::WorkspaceGitDiff, String>,
    ) {
        if self.dialog != pending.dialog || self.target != pending.target {
            return;
        }
        let Kind::Diff { path, side } = &pending.kind else {
            return;
        };
        let Some(diff) = &mut self.diff else {
            return;
        };
        if diff.path != *path || diff.side != *side {
            return;
        }
        diff.loading = false;
        if !self.target.current(catalog) {
            diff.error = Some("Воркспейс изменился. Закройте Git и откройте его снова.".into());
            return;
        }
        match result {
            Ok(value)
                if value.workspace_id == self.target.workspace
                    && value.workspace_revision == self.target.revision
                    && value.project_id == self.target.project
                    && value.cwd == self.target.cwd
                    && value.path == *path
                    && value.side == *side =>
            {
                diff.result = Some(value);
                diff.error = None;
                diff.page = 0;
            }
            Ok(_) => {
                diff.error = Some("Ответ diff относится к другой папке, ревизии или файлу.".into())
            }
            Err(error) => diff.error = Some(error.chars().take(512).collect()),
        }
    }
    fn fail(&mut self, kind: &Kind, message: &str) {
        match kind {
            Kind::Snapshot => {
                self.loading = false;
                self.error = Some(message.into());
            }
            Kind::Diff { path, side } => {
                if let Some(diff) = &mut self.diff
                    && diff.path == *path
                    && diff.side == *side
                {
                    diff.loading = false;
                    diff.error = Some(message.into());
                }
            }
        }
    }
}

impl CanvasApp {
    pub(crate) fn workspace_git_action(&mut self, action: ProductAction, cx: &mut Context<Self>) {
        match action {
            ProductAction::OpenWorkspaceGit => {
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
                    selected: 0,
                    diff: None,
                });
                self.product.git_scroll = Default::default();
                self.product.dialog = Some(dialog);
                self.request_git(Kind::Snapshot, cx);
            }
            ProductAction::RefreshWorkspaceGit => {
                if let Some(sheet) = &mut self.product.git_sheet {
                    sheet.diff = None;
                }
                self.request_git(Kind::Snapshot, cx);
            }
            ProductAction::GitSelect(index) => {
                let count = self.product.git_entry_count();
                if let Some(sheet) = &mut self.product.git_sheet {
                    sheet.selected = index.min(count.saturating_sub(1));
                }
                self.product
                    .git_scroll
                    .scroll_to_item(self.product.git_selected_scroll_index());
            }
            ProductAction::GitOpen(path, side) => {
                let Some(sheet) = &mut self.product.git_sheet else {
                    return;
                };
                sheet.loading = false;
                sheet.diff = Some(DiffSheet {
                    path: path.clone(),
                    side,
                    result: None,
                    loading: false,
                    error: None,
                    page: 0,
                });
                self.product.git_scroll = Default::default();
                self.request_git(Kind::Diff { path, side }, cx);
            }
            ProductAction::GitBack => {
                if let Some(sheet) = &mut self.product.git_sheet {
                    sheet.diff = None;
                    sheet.loading = false;
                }
                self.git_flow.next = None;
                self.product.git_scroll = Default::default();
                self.product
                    .git_scroll
                    .scroll_to_item(self.product.git_selected_scroll_index());
            }
            ProductAction::GitRefreshDiff => {
                if let Some(diff) = self
                    .product
                    .git_sheet
                    .as_ref()
                    .and_then(|sheet| sheet.diff.as_ref())
                {
                    self.request_git(
                        Kind::Diff {
                            path: diff.path.clone(),
                            side: diff.side,
                        },
                        cx,
                    );
                }
            }
            ProductAction::GitCopyDiff => {
                if let Some(result) = self
                    .product
                    .git_sheet
                    .as_ref()
                    .and_then(|sheet| sheet.diff.as_ref())
                    .and_then(|diff| diff.result.as_ref())
                    && !result.text.is_empty()
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(result.text.clone()));
                }
            }
            ProductAction::GitDiffPage(next) => {
                if let Some(diff) = self
                    .product
                    .git_sheet
                    .as_mut()
                    .and_then(|sheet| sheet.diff.as_mut())
                {
                    let last = diff.result.as_ref().map_or(0, |result| {
                        result.text.lines().count().saturating_sub(1) / 200
                    });
                    diff.page = if next {
                        (diff.page + 1).min(last)
                    } else {
                        diff.page.saturating_sub(1)
                    };
                    self.product
                        .git_scroll
                        .set_offset(gpui::point(gpui::px(0.), gpui::px(0.)));
                }
            }
            _ => return,
        }
        cx.notify();
    }

    fn request_git(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let Some(sheet) = &mut self.product.git_sheet else {
            return;
        };
        if !self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.kind == DialogKind::WorkspaceGit && dialog.id == sheet.dialog
        }) {
            return;
        }
        if !sheet.target.current(self.product.catalog.as_ref()) {
            sheet.fail(
                &kind,
                "Воркспейс изменился. Закройте Git и откройте его снова.",
            );
            cx.notify();
            return;
        }
        match &kind {
            Kind::Snapshot => {
                sheet.loading = true;
                sheet.error = None;
            }
            Kind::Diff { path, side } => {
                let Some(diff) = &mut sheet.diff else {
                    return;
                };
                if diff.path != *path || diff.side != *side {
                    return;
                }
                diff.loading = true;
                diff.error = None;
            }
        }
        let pending = Pending {
            request: Uuid::new_v4(),
            dialog: sheet.dialog,
            target: sheet.target.clone(),
            kind,
        };
        if self.git_flow.pending.is_some() {
            self.git_flow.next = Some(pending);
        } else {
            self.dispatch_git(pending);
        }
        cx.notify();
    }

    fn dispatch_git(&mut self, pending: Pending) {
        if self.git_worker.sender.try_send(pending.command()).is_ok() {
            if let Some(sheet) = &mut self.product.git_sheet {
                match &pending.kind {
                    Kind::Snapshot => sheet.loading = true,
                    Kind::Diff { path, side } => {
                        if let Some(diff) = &mut sheet.diff
                            && diff.path == *path
                            && diff.side == *side
                        {
                            diff.loading = true;
                        }
                    }
                }
            }
            self.git_flow.pending = Some(pending);
        } else if let Some(sheet) = &mut self.product.git_sheet {
            sheet.fail(
                &pending.kind,
                "Git-запрос не принят: очередь занята или недоступна. Повторите обновление.",
            );
        }
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
        if self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.id == pending.dialog && dialog.kind == DialogKind::WorkspaceGit
        }) && let Some(sheet) = &mut self.product.git_sheet
        {
            match &pending.kind {
                    Kind::Snapshot if sheet.diff.is_none() => sheet.receive(&pending,self.product.catalog.as_ref(),reply.map_err(|error|error.message).and_then(|reply|reply.workspace_git_snapshot.ok_or_else(||"Session host не вернул Git-статус.".into()))),
                    Kind::Snapshot => (),
                    Kind::Diff {..} => sheet.receive_diff(&pending,self.product.catalog.as_ref(),reply.map_err(|error|error.message).and_then(|reply|reply.workspace_git_diff.ok_or_else(||"Session host не вернул diff. Возможно, требуется новая версия host.".into()))),
                }
        }
        if let Some(next) = self.git_flow.next.take()
            && self.product.dialog.as_ref().is_some_and(|dialog| {
                dialog.id == next.dialog && dialog.kind == DialogKind::WorkspaceGit
            })
            && self
                .product
                .git_sheet
                .as_ref()
                .is_some_and(|sheet| next.current(sheet, self.product.catalog.as_ref()))
        {
            self.dispatch_git(next);
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
            engine_options_schema: Some(1),
            pane_tab_drop_schema: Some(1),
        };
        let target = Target::selected(&catalog).unwrap();
        let dialog = Uuid::new_v4();
        let pending = Pending {
            request: Uuid::new_v4(),
            dialog,
            target: target.clone(),
            kind: Kind::Snapshot,
        };
        (
            catalog,
            GitSheet {
                dialog,
                target,
                snapshot: None,
                loading: true,
                error: None,
                selected: 0,
                diff: None,
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
    fn diff_fixture() -> (Catalog, GitSheet, Pending, protocol::WorkspaceGitDiff) {
        let (catalog, mut sheet, mut pending) = fixture();
        pending.kind = Kind::Diff {
            path: "src/main.rs".into(),
            side: protocol::GitDiffSide::Worktree,
        };
        sheet.diff = Some(DiffSheet {
            path: "src/main.rs".into(),
            side: protocol::GitDiffSide::Worktree,
            result: None,
            loading: true,
            error: None,
            page: 0,
        });
        let result = protocol::WorkspaceGitDiff {
            workspace_id: sheet.target.workspace,
            workspace_revision: sheet.target.revision,
            project_id: sheet.target.project,
            cwd: sheet.target.cwd.clone(),
            path: "src/main.rs".into(),
            side: protocol::GitDiffSide::Worktree,
            text: "@@ -1 +1 @@\n-old\n+new\n".into(),
            truncated: false,
            binary: false,
        };
        (catalog, sheet, pending, result)
    }
    #[test]
    fn diff_receipts_bind_dialog_path_side_and_preserve_old_snapshot_on_refresh_error() {
        let (catalog, mut sheet, pending, result) = diff_fixture();
        assert!(pending.current(&sheet, Some(&catalog)));
        sheet.receive_diff(&pending, Some(&catalog), Ok(result.clone()));
        assert_eq!(sheet.diff.as_ref().unwrap().result, Some(result.clone()));
        sheet.receive_diff(
            &pending,
            Some(&catalog),
            Err("changed while reading".into()),
        );
        let diff = sheet.diff.as_ref().unwrap();
        assert_eq!(diff.result, Some(result));
        assert!(diff.error.is_some());
        sheet.diff.as_mut().unwrap().path = "another.rs".into();
        assert!(!pending.current(&sheet, Some(&catalog)));
        sheet.diff.as_mut().unwrap().loading = true;
        sheet.receive_diff(&pending, Some(&catalog), Err("late old file".into()));
        assert!(sheet.diff.as_ref().unwrap().loading);
        sheet.diff = None;
        sheet.receive_diff(&pending, Some(&catalog), Err("canceled".into()));
        assert!(sheet.diff.is_none());
    }
    #[test]
    fn diff_wrong_repository_or_workspace_revision_never_replaces_current_view() {
        let (mut catalog, mut sheet, pending, mut result) = diff_fixture();
        result.project_id = Uuid::new_v4();
        sheet.receive_diff(&pending, Some(&catalog), Ok(result.clone()));
        assert!(sheet.diff.as_ref().unwrap().result.is_none());
        catalog.workspaces[0].revision += 1;
        assert!(!pending.current(&sheet, Some(&catalog)));
        sheet.receive_diff(&pending, Some(&catalog), Ok(result));
        assert!(sheet.diff.as_ref().unwrap().result.is_none());
    }
}
