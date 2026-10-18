//! Workspace-bound Git reads and exact diff-ticket index operations.
//! No periodic scans, UI-thread I/O or implicit mutation retries.
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

pub(crate) const DIFF_PAGE_LINES: usize = 200;
pub(crate) const MAX_DIFF_LINES: usize = 2000;

pub(crate) fn diff_line_count(text: &str) -> usize {
    text.lines()
        .take(MAX_DIFF_LINES + 1)
        .count()
        .min(MAX_DIFF_LINES)
}

pub(crate) fn diff_page_count(text: &str) -> usize {
    diff_line_count(text).div_ceil(DIFF_PAGE_LINES).max(1)
}

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
    pub action_focus: Option<ProductAction>,
    pub dialog: Uuid,
    pub target: Target,
    pub snapshot: Option<protocol::WorkspaceGitSnapshot>,
    pub loading: bool,
    pub error: Option<String>,
    pub selected: usize,
    pub diff: Option<DiffSheet>,
    pub last_index: Option<IndexNotice>,
}

#[derive(Clone)]
pub(crate) struct DiffSheet {
    pub path: String,
    pub side: protocol::GitDiffSide,
    pub result: Option<protocol::WorkspaceGitDiff>,
    pub loading: bool,
    pub error: Option<String>,
    pub page: usize,
    pub index_state: Option<IndexState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IndexPhase {
    Ready,
    Applying,
    Applied,
    Rejected,
    Unknown,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndexState {
    pub ticket: protocol::GitIndexTicket,
    pub phase: IndexPhase,
    pub message: Option<String>,
}

#[derive(Clone)]
pub(crate) struct IndexNotice {
    pub path: String,
    pub workspace: String,
    pub state: IndexState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Snapshot,
    Diff {
        path: String,
        side: protocol::GitDiffSide,
    },
    Index {
        ticket: Uuid,
        inspect: bool,
    },
    IndexAcknowledge {
        ticket: Uuid,
    },
}

#[derive(Clone)]
struct Pending {
    request: Uuid,
    dialog: Uuid,
    target: Target,
    kind: Kind,
}

#[derive(Clone)]
struct IndexOperation {
    target: Target,
    path: String,
    side: protocol::GitDiffSide,
    state: IndexState,
}

impl IndexOperation {
    fn unresolved(&self) -> bool {
        matches!(
            self.state.phase,
            IndexPhase::Applying | IndexPhase::Unknown | IndexPhase::Unavailable
        )
    }

    fn notice(&self) -> IndexNotice {
        IndexNotice {
            path: self.path.clone(),
            workspace: self.target.name.clone(),
            state: self.state.clone(),
        }
    }

    fn receive(&mut self, result: Result<protocol::WorkspaceGitIndexResult, String>) -> bool {
        match result {
            Ok(receipt)
                if receipt.ticket == self.state.ticket.id
                    && receipt.workspace_id == self.target.workspace
                    && receipt.workspace_revision == self.target.revision
                    && receipt.project_id == self.target.project
                    && receipt.cwd == self.target.cwd
                    && (receipt.phase == protocol::GitIndexPhase::Unavailable
                        || (receipt.path == self.path
                            && receipt.side == self.side
                            && receipt.action == self.state.ticket.action)) =>
            {
                self.state.phase = match receipt.phase {
                    protocol::GitIndexPhase::Applied => IndexPhase::Applied,
                    protocol::GitIndexPhase::Rejected => IndexPhase::Rejected,
                    protocol::GitIndexPhase::Unknown => IndexPhase::Unknown,
                    protocol::GitIndexPhase::Unavailable => IndexPhase::Unavailable,
                };
                self.state.message = receipt.message;
                return receipt.acknowledged;
            }
            Ok(_) => {
                self.state.phase = IndexPhase::Unknown;
                self.state.message = Some("Ответ относится к другому действию или файлу. Проверьте результат без повтора изменения индекса.".into());
            }
            Err(error) => {
                self.state.phase = IndexPhase::Unknown;
                self.state.message = Some(format!(
                    "Исход не подтверждён: {}. Проверьте результат; действие не повторяется.",
                    error.chars().take(512).collect::<String>()
                ));
            }
        }
        false
    }
}

impl GitSheet {
    /// The same visible control order used by Tab/Shift+Tab and Enter.
    pub(crate) fn control_actions(&self) -> Vec<ProductAction> {
        use ProductAction as A;
        let mut actions = Vec::new();
        if let Some(notice) = &self.last_index {
            if notice.state.phase == IndexPhase::Unknown {
                actions.push(A::GitInspectIndex);
            }
            if matches!(
                notice.state.phase,
                IndexPhase::Unknown | IndexPhase::Unavailable
            ) {
                actions.push(A::GitDismissIndexOutcome);
            }
        }
        let applying = self
            .last_index
            .as_ref()
            .is_some_and(|notice| notice.state.phase == IndexPhase::Applying)
            || self
                .diff
                .as_ref()
                .and_then(|diff| diff.index_state.as_ref())
                .is_some_and(|state| state.phase == IndexPhase::Applying);
        if let Some(diff) = &self.diff {
            let pages = diff
                .result
                .as_ref()
                .map_or(1, |result| diff_page_count(&result.text));
            let page = diff.page.min(pages - 1);
            if !diff.loading {
                if page > 0 {
                    actions.push(A::GitDiffPage(false));
                }
                if page + 1 < pages {
                    actions.push(A::GitDiffPage(true));
                }
            }
            actions.push(A::GitBack);
            if !applying {
                actions.push(A::GitRefreshDiff);
            }
            actions.extend([A::GitCopyDiff, A::CloseDialog]);
            let can_apply = !diff.loading
                && diff.error.is_none()
                && !self.last_index.as_ref().is_some_and(|notice| {
                    matches!(
                        notice.state.phase,
                        IndexPhase::Applying | IndexPhase::Unknown | IndexPhase::Unavailable
                    )
                })
                && diff.index_state.as_ref().is_some_and(|state| {
                    state.phase == IndexPhase::Ready
                        && diff.result.as_ref().and_then(|result| result.index_ticket)
                            == Some(state.ticket)
                        && matches!(
                            (diff.side, state.ticket.action),
                            (
                                protocol::GitDiffSide::Index,
                                protocol::GitIndexAction::Unstage
                            ) | (
                                protocol::GitDiffSide::Worktree | protocol::GitDiffSide::Untracked,
                                protocol::GitIndexAction::Stage
                            )
                        )
                });
            if can_apply {
                actions.push(A::GitApplyIndex);
            }
        } else {
            if !applying {
                actions.push(A::RefreshWorkspaceGit);
            }
            actions.push(A::CloseDialog);
        }
        actions
    }

    pub(crate) fn cycle_action(&mut self, reverse: bool) {
        let actions = self.control_actions();
        if actions.is_empty() {
            self.action_focus = None;
            return;
        }
        let current = self
            .action_focus
            .as_ref()
            .and_then(|selected| actions.iter().position(|action| action == selected));
        let index = match current {
            None if reverse => actions.len() - 1,
            None => 0,
            Some(index) => (index + if reverse { actions.len() - 1 } else { 1 }) % actions.len(),
        };
        self.action_focus = Some(actions[index].clone());
    }

    pub(crate) fn focused_action(&self) -> Option<ProductAction> {
        self.action_focus
            .as_ref()
            .filter(|selected| self.control_actions().contains(selected))
            .cloned()
    }

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
                let count = snapshot
                    .entries
                    .iter()
                    .map(|entry| {
                        if entry.unmerged || entry.untracked {
                            1
                        } else {
                            usize::from(entry.index_status != '.')
                                + usize::from(entry.worktree_status != '.')
                        }
                    })
                    .sum::<usize>();
                self.selected = self.selected.min(count.saturating_sub(1));
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
    index: Option<IndexOperation>,
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
                Kind::Index { .. } | Kind::IndexAcknowledge { .. } => false,
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
            Kind::Index { ticket, inspect } => {
                json!({"command":"workspace_git_index","workspace":self.target.workspace,"expected_revision":self.target.revision,"ticket":ticket,"inspect":inspect,"_ui_request":self.request})
            }
            Kind::IndexAcknowledge { ticket } => {
                json!({"command":"workspace_git_index_acknowledge","workspace":self.target.workspace,"expected_revision":self.target.revision,"ticket":ticket,"_ui_request":self.request})
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
                diff.index_state = value.index_ticket.as_ref().map(|ticket| IndexState {
                    ticket: *ticket,
                    phase: IndexPhase::Ready,
                    message: None,
                });
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
            Kind::Index { .. } | Kind::IndexAcknowledge { .. } => (),
        }
    }
}

impl CanvasApp {
    fn sync_git_index_notice(&mut self) {
        if let Some(sheet) = &mut self.product.git_sheet {
            sheet.last_index = self.git_flow.index.as_ref().map(IndexOperation::notice);
            if let Some(operation) = &self.git_flow.index
                && sheet.target == operation.target
                && let Some(diff) = &mut sheet.diff
                && diff.path == operation.path
                && diff.side == operation.side
                && diff
                    .index_state
                    .as_ref()
                    .is_some_and(|state| state.ticket == operation.state.ticket)
            {
                diff.index_state = Some(operation.state.clone());
            }
        }
    }

    fn request_git_index(&mut self, inspect: bool, acknowledge: bool, cx: &mut Context<Self>) {
        if self.git_flow.pending.is_some() {
            self.product.notice = Some(
                "Дождитесь текущего ответа Git. Действие с индексом не отправлено повторно.".into(),
            );
            cx.notify();
            return;
        }
        let operation = if inspect {
            let Some(operation) = self.git_flow.index.clone() else {
                return;
            };
            if acknowledge
                && !matches!(
                    operation.state.phase,
                    IndexPhase::Unknown | IndexPhase::Unavailable
                )
            {
                return;
            }
            operation
        } else {
            if self
                .git_flow
                .index
                .as_ref()
                .is_some_and(IndexOperation::unresolved)
            {
                self.product.notice =
                    Some("Сначала проверьте результат предыдущего изменения индекса.".into());
                cx.notify();
                return;
            }
            let Some(sheet) = &self.product.git_sheet else {
                return;
            };
            if !self.product.dialog.as_ref().is_some_and(|dialog| {
                dialog.id == sheet.dialog && dialog.kind == DialogKind::WorkspaceGit
            }) || !sheet.target.current(self.product.catalog.as_ref())
            {
                return;
            }
            let Some(diff) = &sheet.diff else { return };
            let Some(state) = &diff.index_state else {
                return;
            };
            let Some(result) = &diff.result else { return };
            if diff.loading
                || diff.error.is_some()
                || state.phase != IndexPhase::Ready
                || result.index_ticket.as_ref() != Some(&state.ticket)
                || !matches!(
                    (diff.side, state.ticket.action),
                    (
                        protocol::GitDiffSide::Index,
                        protocol::GitIndexAction::Unstage
                    ) | (
                        protocol::GitDiffSide::Worktree | protocol::GitDiffSide::Untracked,
                        protocol::GitIndexAction::Stage
                    )
                )
            {
                return;
            }
            IndexOperation {
                target: sheet.target.clone(),
                path: diff.path.clone(),
                side: diff.side,
                state: state.clone(),
            }
        };
        let dialog = self
            .product
            .git_sheet
            .as_ref()
            .map_or_else(Uuid::new_v4, |sheet| sheet.dialog);
        let pending = Pending {
            request: Uuid::new_v4(),
            dialog,
            target: operation.target.clone(),
            kind: if acknowledge {
                Kind::IndexAcknowledge {
                    ticket: operation.state.ticket.id,
                }
            } else {
                Kind::Index {
                    ticket: operation.state.ticket.id,
                    inspect,
                }
            },
        };
        // No write intents enter the replaceable read queue. The state changes
        // only after admission; the request worker never retries this command.
        if self.git_worker.sender.try_send(pending.command()).is_err() {
            self.product.notice =
                Some("Действие с индексом не отправлено: очередь Git недоступна.".into());
            cx.notify();
            return;
        }
        let mut operation = operation;
        if !inspect {
            operation.state.phase = IndexPhase::Applying;
        }
        operation.state.message = None;
        self.git_flow.index = Some(operation);
        self.git_flow.pending = Some(pending);
        self.git_flow.next = None;
        self.sync_git_index_notice();
        cx.notify();
    }

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
                    action_focus: None,
                    dialog: dialog.id,
                    target,
                    snapshot: None,
                    loading: false,
                    error: None,
                    selected: 0,
                    diff: None,
                    last_index: self.git_flow.index.as_ref().map(IndexOperation::notice),
                });
                self.product.git_scroll = Default::default();
                self.product.dialog = Some(dialog);
                self.request_git(Kind::Snapshot, cx);
            }
            ProductAction::RefreshWorkspaceGit => {
                if self
                    .git_flow
                    .index
                    .as_ref()
                    .is_some_and(IndexOperation::unresolved)
                {
                    self.request_git_index(true, false, cx);
                    return;
                }
                if let Some(sheet) = &mut self.product.git_sheet {
                    sheet.diff = None;
                }
                self.request_git(Kind::Snapshot, cx);
            }
            ProductAction::GitSelect(index) => {
                let count = self.product.git_entry_count();
                if let Some(sheet) = &mut self.product.git_sheet {
                    sheet.action_focus = None;
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
                sheet.action_focus = None;
                sheet.loading = false;
                sheet.diff = Some(DiffSheet {
                    path: path.clone(),
                    side,
                    result: None,
                    loading: false,
                    error: None,
                    page: 0,
                    index_state: None,
                });
                self.product.git_scroll = Default::default();
                self.request_git(Kind::Diff { path, side }, cx);
            }
            ProductAction::GitBack => {
                if let Some(sheet) = &mut self.product.git_sheet {
                    sheet.action_focus = None;
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
                if self
                    .git_flow
                    .index
                    .as_ref()
                    .is_some_and(IndexOperation::unresolved)
                {
                    self.request_git_index(true, false, cx);
                    return;
                }
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
                    let last = diff
                        .result
                        .as_ref()
                        .map_or(0, |result| diff_page_count(&result.text) - 1);
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
            ProductAction::GitApplyIndex => {
                self.request_git_index(false, false, cx);
            }
            ProductAction::GitInspectIndex => {
                self.request_git_index(true, false, cx);
            }
            ProductAction::GitDismissIndexOutcome => {
                self.request_git_index(true, true, cx);
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
            Kind::Index { .. } | Kind::IndexAcknowledge { .. } => return,
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
                    Kind::Index { .. } | Kind::IndexAcknowledge { .. } => (),
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
        if matches!(
            pending.kind,
            Kind::Index { .. } | Kind::IndexAcknowledge { .. }
        ) {
            let result = reply.map_err(|error| error.message).and_then(|reply| {
                reply.workspace_git_index.ok_or_else(|| {
                    "Session host не вернул сохранённый результат изменения индекса.".into()
                })
            });
            let mut acknowledged = false;
            if let Some(operation) = &mut self.git_flow.index
                && operation.target == pending.target
                && match pending.kind {
                    Kind::Index { ticket, .. } | Kind::IndexAcknowledge { ticket } => {
                        ticket == operation.state.ticket.id
                    }
                    _ => false,
                }
            {
                let explicit_missing_ack = matches!(pending.kind, Kind::IndexAcknowledge { .. })
                    && result
                        .as_ref()
                        .is_ok_and(|receipt| receipt.phase == protocol::GitIndexPhase::Unavailable);
                acknowledged = operation.receive(result);
                // An explicit acknowledgement can also dismiss an absent
                // receipt. This never says that the original index write failed.
                acknowledged |=
                    explicit_missing_ack && operation.state.phase == IndexPhase::Unavailable;
            }
            let refresh_source = self.git_flow.index.as_ref().is_some_and(|operation| {
                (acknowledged || operation.state.phase == IndexPhase::Applied)
                    && self.product.dialog.as_ref().is_some_and(|dialog| {
                        dialog.id == pending.dialog && dialog.kind == DialogKind::WorkspaceGit
                    })
                    && self.product.git_sheet.as_ref().is_some_and(|sheet| {
                        sheet.target.current(self.product.catalog.as_ref())
                            && (acknowledged
                                || (sheet.target == operation.target
                                    && sheet.diff.as_ref().is_some_and(|diff| {
                                        diff.path == operation.path
                                            && diff.side == operation.side
                                            && diff.index_state.as_ref().is_some_and(|state| {
                                                state.ticket == operation.state.ticket
                                            })
                                    })))
                    })
            });
            self.sync_git_index_notice();
            if acknowledged {
                self.git_flow.index = None;
                if let Some(sheet) = &mut self.product.git_sheet {
                    sheet.last_index = None;
                    if let Some(diff) = &mut sheet.diff {
                        diff.index_state = None;
                    }
                }
                self.product.notice =
                    Some("Сохранённый исход просмотрен. Старое действие не повторяется.".into());
            }
            if refresh_source {
                if let Some(sheet) = &mut self.product.git_sheet {
                    sheet.action_focus = None;
                    sheet.diff = None;
                }
                self.git_flow.next = None;
                self.request_git(Kind::Snapshot, cx);
            }
            // A closed or replaced dialog stays closed. The global receipt
            // continues to exist and is shown when Git is opened again.
        } else {
            if let Ok(reply) = &reply
                && let Some(snapshot) = &reply.workspace_git_snapshot
                && let Some(receipt) = &snapshot.index_operation
                && !self
                    .git_flow
                    .index
                    .as_ref()
                    .is_some_and(IndexOperation::unresolved)
                && let Some(sheet) = &self.product.git_sheet
                && pending.current(sheet, self.product.catalog.as_ref())
            {
                let mut target = sheet.target.clone();
                target.revision = receipt.workspace_revision;
                self.git_flow.index = Some(IndexOperation {
                    target,
                    path: receipt.path.clone(),
                    side: receipt.side,
                    state: IndexState {
                        ticket: protocol::GitIndexTicket {
                            id: receipt.ticket,
                            action: receipt.action,
                        },
                        phase: IndexPhase::Unknown,
                        message: receipt.message.clone(),
                    },
                });
            }
            if self.product.dialog.as_ref().is_some_and(|dialog| {
                dialog.id == pending.dialog && dialog.kind == DialogKind::WorkspaceGit
            }) && let Some(sheet) = &mut self.product.git_sheet
            {
                match &pending.kind {
                    Kind::Snapshot if sheet.diff.is_none() => sheet.receive(&pending,self.product.catalog.as_ref(),reply.map_err(|error|error.message).and_then(|reply|reply.workspace_git_snapshot.ok_or_else(||"Session host не вернул Git-статус.".into()))),
                    Kind::Snapshot => (),
                    Kind::Diff {..} => sheet.receive_diff(&pending,self.product.catalog.as_ref(),reply.map_err(|error|error.message).and_then(|reply|reply.workspace_git_diff.ok_or_else(||"Session host не вернул diff. Возможно, требуется новая версия host.".into()))),
                    Kind::Index {..} | Kind::IndexAcknowledge {..} => (),
                }
            }
            self.sync_git_index_notice();
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
                action_focus: None,
                dialog,
                target,
                snapshot: None,
                loading: true,
                error: None,
                selected: 0,
                diff: None,
                last_index: None,
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
            index_operation: None,
            index_operation_unavailable_reason: None,
        }
    }
    fn index_fixture() -> (IndexOperation, protocol::WorkspaceGitIndexResult) {
        let (_, sheet, _) = fixture();
        let ticket = protocol::GitIndexTicket {
            id: Uuid::new_v4(),
            action: protocol::GitIndexAction::Stage,
        };
        let operation = IndexOperation {
            target: sheet.target.clone(),
            path: "src/main.rs".into(),
            side: protocol::GitDiffSide::Worktree,
            state: IndexState {
                ticket,
                phase: IndexPhase::Applying,
                message: None,
            },
        };
        let receipt = protocol::WorkspaceGitIndexResult {
            ticket: ticket.id,
            workspace_id: sheet.target.workspace,
            workspace_revision: sheet.target.revision,
            project_id: sheet.target.project,
            cwd: sheet.target.cwd,
            path: operation.path.clone(),
            side: operation.side,
            action: ticket.action,
            phase: protocol::GitIndexPhase::Applied,
            acknowledged: false,
            message: None,
        };
        (operation, receipt)
    }

    #[test]
    fn tab_and_shift_tab_choose_visible_git_controls_without_opening_a_file() {
        let (_, mut sheet, _) = fixture();
        assert_eq!(sheet.focused_action(), None);
        sheet.cycle_action(false);
        assert_eq!(
            sheet.focused_action(),
            Some(ProductAction::RefreshWorkspaceGit)
        );
        sheet.cycle_action(false);
        assert_eq!(sheet.focused_action(), Some(ProductAction::CloseDialog));
        sheet.cycle_action(false);
        assert_eq!(
            sheet.focused_action(),
            Some(ProductAction::RefreshWorkspaceGit)
        );
        sheet.cycle_action(true);
        assert_eq!(sheet.focused_action(), Some(ProductAction::CloseDialog));
    }

    #[test]
    fn hidden_or_blocked_index_control_cannot_remain_the_enter_target() {
        let (_, mut sheet, _, mut result) = diff_fixture();
        let (mut operation, _) = index_fixture();
        operation.state.phase = IndexPhase::Ready;
        result.index_ticket = Some(operation.state.ticket);
        let diff = sheet.diff.as_mut().unwrap();
        diff.loading = false;
        diff.result = Some(result);
        diff.index_state = Some(operation.state.clone());
        sheet.action_focus = Some(ProductAction::GitApplyIndex);
        assert_eq!(sheet.focused_action(), Some(ProductAction::GitApplyIndex));
        sheet.diff.as_mut().unwrap().loading = true;
        assert_eq!(sheet.focused_action(), None);
        sheet.diff.as_mut().unwrap().loading = false;
        operation.state.phase = IndexPhase::Unknown;
        sheet.last_index = Some(operation.notice());
        assert_eq!(sheet.focused_action(), None);
        assert_eq!(
            &sheet.control_actions()[..2],
            &[
                ProductAction::GitInspectIndex,
                ProductAction::GitDismissIndexOutcome
            ]
        );
        assert!(
            !sheet
                .control_actions()
                .contains(&ProductAction::GitApplyIndex)
        );
        sheet.cycle_action(false);
        assert_eq!(sheet.focused_action(), Some(ProductAction::GitInspectIndex));
    }

    #[test]
    fn git_tab_cycle_excludes_disabled_diff_page_controls() {
        for lines in [0, 1, 200, 201, 400, 401] {
            let (_, mut sheet, _, mut result) = diff_fixture();
            result.text = "line\n".repeat(lines);
            let diff = sheet.diff.as_mut().unwrap();
            diff.loading = false;
            diff.result = Some(result);
            let pages = lines.div_ceil(200).max(1);
            for page in 0..pages {
                sheet.diff.as_mut().unwrap().page = page;
                let actions = sheet.control_actions();
                assert_eq!(
                    actions.contains(&ProductAction::GitDiffPage(false)),
                    page > 0
                );
                assert_eq!(
                    actions.contains(&ProductAction::GitDiffPage(true)),
                    page + 1 < pages
                );
            }
            sheet.diff.as_mut().unwrap().loading = true;
            assert!(
                !sheet
                    .control_actions()
                    .iter()
                    .any(|action| matches!(action, ProductAction::GitDiffPage(_)))
            );
            sheet.action_focus = Some(ProductAction::GitDiffPage(true));
            assert_eq!(sheet.focused_action(), None);
        }
    }

    #[test]
    fn exact_index_receipt_updates_its_operation_without_any_open_dialog() {
        let (mut operation, receipt) = index_fixture();
        assert!(!operation.receive(Ok(receipt)));
        assert_eq!(operation.state.phase, IndexPhase::Applied);
        assert!(!operation.unresolved());
        assert_eq!(operation.notice().path, "src/main.rs");
    }

    #[test]
    fn changed_index_receipt_scope_never_claims_success_or_acknowledgement() {
        for mismatch in 0..8 {
            let (mut operation, mut receipt) = index_fixture();
            receipt.acknowledged = true;
            match mismatch {
                0 => receipt.ticket = Uuid::new_v4(),
                1 => receipt.workspace_id = Uuid::new_v4(),
                2 => receipt.workspace_revision += 1,
                3 => receipt.project_id = Uuid::new_v4(),
                4 => receipt.cwd = "/different/root".into(),
                5 => receipt.path = "another.rs".into(),
                6 => receipt.side = protocol::GitDiffSide::Index,
                _ => receipt.action = protocol::GitIndexAction::Unstage,
            }
            assert!(!operation.receive(Ok(receipt)));
            assert_eq!(operation.state.phase, IndexPhase::Unknown);
            assert!(operation.unresolved());
        }
    }

    #[test]
    fn uncertain_index_reply_keeps_the_original_ticket_for_read_only_inspection() {
        let (mut operation, _) = index_fixture();
        let ticket = operation.state.ticket;
        assert!(!operation.receive(Err("socket closed after dispatch".into())));
        assert!(operation.unresolved());
        assert_eq!(operation.state.ticket, ticket);
        let pending = Pending {
            request: Uuid::new_v4(),
            dialog: Uuid::new_v4(),
            target: operation.target.clone(),
            kind: Kind::Index {
                ticket: ticket.id,
                inspect: true,
            },
        };
        assert_eq!(pending.command()["inspect"], true);
        assert_eq!(pending.command()["ticket"], ticket.id.to_string());
        assert_eq!(operation.state.phase, IndexPhase::Unknown);
    }

    #[test]
    fn unavailable_receipt_preserves_frozen_file_and_requires_explicit_acknowledgement() {
        let (mut operation, mut receipt) = index_fixture();
        receipt.phase = protocol::GitIndexPhase::Unavailable;
        receipt.path = "(unknown)".into();
        receipt.side = protocol::GitDiffSide::Worktree;
        receipt.message = Some("receipt unavailable".into());
        assert!(!operation.receive(Ok(receipt)));
        assert_eq!(operation.path, "src/main.rs");
        assert_eq!(operation.state.phase, IndexPhase::Unavailable);
        assert!(operation.unresolved());
    }

    #[test]
    fn durable_acknowledgement_does_not_turn_unknown_into_an_applied_result() {
        let (mut operation, mut receipt) = index_fixture();
        receipt.phase = protocol::GitIndexPhase::Unknown;
        receipt.acknowledged = true;
        assert!(operation.receive(Ok(receipt)));
        assert_eq!(operation.state.phase, IndexPhase::Unknown);
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
            index_state: None,
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
            index_ticket: None,
            index_unavailable_reason: None,
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
