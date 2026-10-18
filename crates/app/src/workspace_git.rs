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
use std::{path::PathBuf, sync::mpsc, thread, time::Instant};
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
    pub(crate) fn submit(&self, command: Value) -> Result<(), String> {
        self.sender
            .try_send(command)
            .map_err(|_| "Очередь Git занята; запрос не отправлен".into())
    }
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
    pub(crate) fn selected(catalog: &Catalog) -> Option<Self> {
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

    pub(crate) fn current(&self, catalog: Option<&Catalog>) -> bool {
        catalog.and_then(Self::selected).is_some_and(|current| {
            current.workspace == self.workspace
                && current.revision == self.revision
                && current.project == self.project
                && current.cwd == self.cwd
        })
    }

    pub(crate) fn same_scope(&self, other: &Self) -> bool {
        self.workspace == other.workspace && self.project == other.project && self.cwd == other.cwd
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
    pub watch_stamp: Option<(Uuid, u64)>,
    pub loading: bool,
    pub error: Option<String>,
    pub selected: usize,
    pub diff: Option<DiffSheet>,
    pub last_index: Option<IndexNotice>,
    pub commit_operation: Option<protocol::WorkspaceGitCommitResult>,
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
    CommitPrepare,
    Commit {
        ticket: Uuid,
        message: Option<String>,
        inspect: bool,
        acknowledge: bool,
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
    pub(crate) fn can_prepare_batch(&self, action: protocol::GitIndexAction) -> bool {
        !self.loading
            && self.error.is_none()
            && self.diff.is_none()
            && self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.index_batch_schema == Some(1)
                    && !snapshot.truncated
                    && snapshot.index_batch_unavailable_reason.is_none()
                    && snapshot.entries.iter().any(|entry| match action {
                        protocol::GitIndexAction::Stage => {
                            entry.untracked || !matches!(entry.worktree_status, '.' | ' ')
                        }
                        protocol::GitIndexAction::Unstage => {
                            !entry.untracked && !matches!(entry.index_status, '.' | ' ')
                        }
                    })
            })
    }
    pub(crate) fn can_prepare_commit(&self) -> bool {
        !self.loading
            && self.error.is_none()
            && self.diff.is_none()
            && self.snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.commit_schema == Some(1)
                    && snapshot.commit_unavailable_reason.is_none()
                    && snapshot.index_operation_unavailable_reason.is_none()
                    && !snapshot.truncated
                    && snapshot.entries.iter().any(|entry| {
                        !entry.unmerged
                            && matches!(entry.index_status, 'A' | 'M' | 'D' | 'T' | 'R' | 'C')
                    })
            })
            && !self.last_index.as_ref().is_some_and(|notice| {
                matches!(
                    notice.state.phase,
                    IndexPhase::Applying | IndexPhase::Unknown | IndexPhase::Unavailable
                )
            })
            && self.commit_operation.as_ref().is_none_or(|operation| {
                !matches!(
                    operation.phase,
                    protocol::GitCommitPhase::Running
                        | protocol::GitCommitPhase::Unknown
                        | protocol::GitCommitPhase::Unavailable
                )
            })
    }

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
        if let Some(commit) = &self.commit_operation {
            if commit.phase == protocol::GitCommitPhase::Unknown {
                actions.push(A::GitInspectCommit);
            }
            if matches!(
                commit.phase,
                protocol::GitCommitPhase::Unknown | protocol::GitCommitPhase::Unavailable
            ) {
                actions.push(A::GitAcknowledgeCommit);
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
                && !self.commit_operation.as_ref().is_some_and(|operation| {
                    matches!(
                        operation.phase,
                        protocol::GitCommitPhase::Running
                            | protocol::GitCommitPhase::Unknown
                            | protocol::GitCommitPhase::Unavailable
                    )
                })
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
            if self.can_prepare_commit() {
                actions.push(A::GitPrepareCommit);
            }
            for action in [
                protocol::GitIndexAction::Stage,
                protocol::GitIndexAction::Unstage,
            ] {
                if self.can_prepare_batch(action) {
                    actions.push(A::GitPrepareBatch(action));
                }
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
    after_read: Option<Pending>,
    index: Option<IndexOperation>,
    commit: Option<CommitOperation>,
    commit_drafts: Vec<(Uuid, String)>,
    submitted_draft: Option<(Uuid, Uuid, String)>,
    commit_plan: Option<(Target, protocol::WorkspaceGitCommitPlan)>,
    last_commit_inspect: Option<Instant>,
}

#[derive(Clone)]
struct CommitOperation {
    target: Target,
    state: protocol::WorkspaceGitCommitResult,
}

fn valid_commit_message(message: &str) -> bool {
    !message.trim().is_empty()
        && message.len() <= 8192
        && !message
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
}

impl CommitOperation {
    fn unresolved(&self) -> bool {
        matches!(
            self.state.phase,
            protocol::GitCommitPhase::Running
                | protocol::GitCommitPhase::Unknown
                | protocol::GitCommitPhase::Unavailable
        )
    }
    fn receive(&mut self, result: Result<protocol::WorkspaceGitCommitResult, String>) -> bool {
        let can_transition = matches!(
            self.state.phase,
            protocol::GitCommitPhase::Running | protocol::GitCommitPhase::Unknown
        );
        match result {
            Ok(receipt)
                if receipt.ticket == self.state.ticket
                    && receipt.workspace_id == self.target.workspace
                    && receipt.workspace_revision == self.target.revision
                    && receipt.project_id == self.target.project
                    && receipt.cwd == self.target.cwd =>
            {
                let applied = receipt.phase == protocol::GitCommitPhase::Applied;
                self.state = receipt;
                can_transition && applied
            }
            Ok(_) => {
                self.state.phase = protocol::GitCommitPhase::Unknown;
                self.state.commit = None;
                self.state.acknowledged = false;
                self.state.message = Some(
                    "Ответ относится к другому воркспейсу или коммиту. Повтор не отправлен.".into(),
                );
                false
            }
            Err(error) => {
                self.state.phase = protocol::GitCommitPhase::Unknown;
                self.state.commit = None;
                self.state.acknowledged = false;
                self.state.message = Some(format!(
                    "Исход не подтверждён: {}. Проверьте квитанцию; коммит не повторяется.",
                    error.chars().take(512).collect::<String>()
                ));
                false
            }
        }
    }
}

impl Flow {
    pub(crate) fn unresolved_resources(&self) -> Vec<&'static str> {
        let mut resources = Vec::new();
        if self
            .commit
            .as_ref()
            .is_some_and(CommitOperation::unresolved)
        {
            resources.push("Итог отправленного Git-коммита");
        }
        if self.index.as_ref().is_some_and(IndexOperation::unresolved) {
            resources.push("Итог изменения Git-индекса");
        }
        resources
    }
    pub(crate) fn close_uncertainty(&self) -> Option<&'static str> {
        if self.commit.as_ref().is_some_and(|operation| {
            matches!(
                operation.state.phase,
                protocol::GitCommitPhase::Unknown | protocol::GitCommitPhase::Unavailable
            )
        }) || self.index.as_ref().is_some_and(|operation| {
            matches!(
                operation.state.phase,
                IndexPhase::Unknown | IndexPhase::Unavailable
            )
        }) {
            Some(
                "Исход отправленной операции Git неизвестен. Вернитесь в Git для проверки квитанции; операция не повторяется. Закрытие UI не отменяет её.",
            )
        } else {
            None
        }
    }
    pub(crate) fn request_pending(&self) -> bool {
        self.pending.is_some() || self.next.is_some() || self.after_read.is_some()
    }
    pub(crate) fn dirty_commit_workspaces(&self) -> Vec<Uuid> {
        self.commit_drafts
            .iter()
            .filter(|(_, text)| !text.trim().is_empty())
            .map(|(id, _)| *id)
            .collect()
    }
    fn commit_applied(&mut self, workspace: Uuid, ticket: Uuid) {
        if let Some((id, submitted_ticket, text)) = &self.submitted_draft
            && *id == workspace
            && *submitted_ticket == ticket
        {
            self.commit_drafts
                .retain(|(id, draft)| *id != workspace || draft != text);
            self.submitted_draft = None;
        }
    }
    pub(crate) fn commit_draft(&self, workspace: Uuid) -> String {
        self.commit_drafts
            .iter()
            .find(|(id, _)| *id == workspace)
            .map(|(_, draft)| draft.clone())
            .unwrap_or_default()
    }
    pub(crate) fn remember_commit_draft(&mut self, workspace: Uuid, draft: &str) {
        // The editor enforces this memory bound without trimming accepted text.
        // Commit start separately enforces Git's stricter 8192-byte message limit.
        if draft.len() > 32 * 1024 {
            return;
        }
        if let Some((_, value)) = self
            .commit_drafts
            .iter_mut()
            .find(|(id, _)| *id == workspace)
        {
            *value = draft.to_owned();
            return;
        }
        if self.commit_drafts.len() == 16 {
            self.commit_drafts.remove(0);
        }
        self.commit_drafts.push((workspace, draft.to_owned()));
    }
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
                Kind::Index { .. }
                | Kind::IndexAcknowledge { .. }
                | Kind::CommitPrepare
                | Kind::Commit { .. } => false,
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
            Kind::CommitPrepare => {
                json!({"command":"workspace_git_commit_prepare","workspace":self.target.workspace,"expected_revision":self.target.revision,"_ui_request":self.request})
            }
            Kind::Commit {
                ticket,
                message,
                inspect,
                acknowledge,
            } => {
                if *acknowledge {
                    json!({"command":"workspace_git_commit_acknowledge","workspace":self.target.workspace,"expected_revision":self.target.revision,"ticket":ticket,"_ui_request":self.request})
                } else {
                    json!({"command":"workspace_git_commit","workspace":self.target.workspace,"expected_revision":self.target.revision,"ticket":ticket,"message":message,"inspect":inspect,"_ui_request":self.request})
                }
            }
        }
    }
}

fn enqueue_commit_refresh(
    next: &mut Option<Pending>,
    after_read: &mut Option<Pending>,
    refresh: Pending,
) {
    if next.as_ref().is_some_and(|pending| {
        matches!(pending.kind, Kind::Snapshot)
            && pending.dialog == refresh.dialog
            && pending.target == refresh.target
    }) || after_read.as_ref().is_some_and(|pending| {
        matches!(pending.kind, Kind::Snapshot)
            && pending.dialog == refresh.dialog
            && pending.target == refresh.target
    }) {
        return;
    }
    if next.is_none() {
        *next = Some(refresh);
    } else if after_read.is_none() {
        *after_read = Some(refresh);
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
            Kind::Index { .. }
            | Kind::IndexAcknowledge { .. }
            | Kind::CommitPrepare
            | Kind::Commit { .. } => (),
        }
    }
}

impl CanvasApp {
    fn sync_git_index_notice(&mut self) {
        if let Some(sheet) = &mut self.product.git_sheet {
            sheet.last_index = self.git_flow.index.as_ref().map(IndexOperation::notice);
            sheet.commit_operation = self
                .git_flow
                .commit
                .as_ref()
                .filter(|operation| operation.target.same_scope(&sheet.target))
                .map(|operation| operation.state.clone());
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
            let active_workspace = self
                .product
                .git_sheet
                .as_ref()
                .map(|sheet| sheet.target.workspace);
            if self.git_flow.commit.as_ref().is_some_and(|operation| {
                Some(operation.target.workspace) == active_workspace && operation.unresolved()
            }) {
                self.product.notice =
                    Some("Сначала разрешите исход предыдущего коммита Git.".into());
                cx.notify();
                return;
            }
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
        if self.product.git_batch.unresolved()
            && matches!(
                action,
                ProductAction::GitApplyIndex
                    | ProductAction::GitPrepareCommit
                    | ProductAction::GitStartCommit
            )
        {
            self.product.notice = Some("Сначала проверьте исход пакетной операции Git".into());
            cx.notify();
            return;
        }
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
                    target: target.clone(),
                    snapshot: None,
                    watch_stamp: None,
                    loading: false,
                    error: None,
                    selected: 0,
                    diff: None,
                    last_index: self.git_flow.index.as_ref().map(IndexOperation::notice),
                    commit_operation: self
                        .git_flow
                        .commit
                        .as_ref()
                        .filter(|operation| operation.target.same_scope(&target))
                        .map(|operation| operation.state.clone()),
                });
                self.product.git_commit_plan = self
                    .git_flow
                    .commit_plan
                    .as_ref()
                    .filter(|(plan_target, _)| plan_target == &target)
                    .map(|(_, plan)| plan.clone());
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
            ProductAction::GitPrepareCommit => self.prepare_git_commit(cx),
            ProductAction::GitRefreshCommitPlan => self.prepare_git_commit(cx),
            ProductAction::GitStartCommit => self.start_git_commit(cx),
            ProductAction::GitInspectCommit => self.request_git_commit(false, true, cx),
            ProductAction::GitAcknowledgeCommit => self.request_git_commit(true, false, cx),
            _ => return,
        }
        cx.notify();
    }

    fn prepare_git_commit(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self.product.git_sheet.as_ref() else {
            return;
        };
        let Some(snapshot) = sheet.snapshot.as_ref() else {
            return;
        };
        if snapshot.commit_schema != Some(1)
            || snapshot.truncated
            || self.git_flow.commit.as_ref().is_some_and(|operation| {
                operation.target.same_scope(&sheet.target) && operation.unresolved()
            })
            || self
                .git_flow
                .index
                .as_ref()
                .is_some_and(IndexOperation::unresolved)
            || self.git_flow.pending.is_some()
        {
            self.product.notice = Some("Подготовка коммита сейчас недоступна: обновите снимок или сначала разрешите предыдущий исход.".into());
            cx.notify();
            return;
        }
        if !sheet.target.current(self.product.catalog.as_ref()) {
            return;
        }
        let target = sheet.target.clone();
        let review = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::WorkspaceGitCommit)
            .cloned()
            .unwrap_or_else(|| Dialog::new(DialogKind::WorkspaceGitCommit));
        let pending = Pending {
            request: Uuid::new_v4(),
            dialog: review.id,
            target: target.clone(),
            kind: Kind::CommitPrepare,
        };
        if self.git_worker.sender.try_send(pending.command()).is_err() {
            self.product.notice = Some("План коммита не запрошен: очередь Git занята.".into());
            cx.notify();
            return;
        }
        self.git_flow.commit_plan = None;
        self.product.git_commit_plan = None;
        self.product.git_commit_plan_error = None;
        let mut review = review;
        if review.fields.is_empty() {
            review
                .fields
                .push(self.git_flow.commit_draft(target.workspace).into());
        }
        self.product.dialog = Some(review);
        self.git_flow.pending = Some(pending);
        self.git_flow.next = None;
        cx.notify();
    }

    fn start_git_commit(&mut self, cx: &mut Context<Self>) {
        let Some((target, plan)) = self.git_flow.commit_plan.clone() else {
            return;
        };
        if !target.current(self.product.catalog.as_ref())
            || self
                .git_flow
                .commit
                .as_ref()
                .is_some_and(|operation| operation.target == target && operation.unresolved())
            || self
                .git_flow
                .index
                .as_ref()
                .is_some_and(IndexOperation::unresolved)
        {
            return;
        }
        let message = self
            .product
            .dialog
            .as_ref()
            .filter(|dialog| dialog.kind == DialogKind::WorkspaceGitCommit)
            .and_then(|dialog| dialog.fields.first())
            .map(|field| field.text().to_owned())
            .unwrap_or_default();
        if !valid_commit_message(&message) {
            self.product.notice=Some("Сообщение должно содержать текст, не превышать 8192 байта и не содержать управляющие символы.".into());
            cx.notify();
            return;
        }
        let state = protocol::WorkspaceGitCommitResult {
            ticket: plan.ticket,
            workspace_id: target.workspace,
            workspace_revision: target.revision,
            project_id: target.project,
            cwd: target.cwd.clone(),
            phase: protocol::GitCommitPhase::Running,
            commit: None,
            acknowledged: false,
            message: None,
        };
        let operation = CommitOperation {
            target: target.clone(),
            state: state.clone(),
        };
        let pending = Pending {
            request: Uuid::new_v4(),
            dialog: self
                .product
                .git_sheet
                .as_ref()
                .map_or_else(Uuid::new_v4, |sheet| sheet.dialog),
            target: target.clone(),
            kind: Kind::Commit {
                ticket: plan.ticket,
                message: Some(message.clone()),
                inspect: false,
                acknowledge: false,
            },
        };
        if self.git_worker.sender.try_send(pending.command()).is_err() {
            self.product.notice=Some("Коммит не отправлен: очередь Git занята. План сохранён; явный повтор остаётся под вашим контролем.".into());
            cx.notify();
            return;
        }
        self.git_flow
            .remember_commit_draft(target.workspace, &message);
        self.git_flow.submitted_draft = Some((target.workspace, plan.ticket, message));
        self.git_flow.commit = Some(operation);
        self.git_flow.last_commit_inspect = Some(Instant::now());
        self.git_flow.commit_plan = None;
        self.product.git_commit_plan = None;
        self.git_flow.pending = Some(pending);
        self.product.dialog = self.product.git_sheet.as_ref().map(|sheet| Dialog {
            kind: DialogKind::WorkspaceGit,
            id: sheet.dialog,
            fields: vec![],
            active_field: 0,
        });
        cx.notify();
    }

    fn request_git_commit(&mut self, acknowledge: bool, inspect: bool, cx: &mut Context<Self>) {
        if self.git_flow.pending.is_some() {
            return;
        }
        let Some(operation) = self.git_flow.commit.clone() else {
            return;
        };
        if acknowledge
            && !matches!(
                operation.state.phase,
                protocol::GitCommitPhase::Unknown | protocol::GitCommitPhase::Unavailable
            )
        {
            return;
        }
        let pending = Pending {
            request: Uuid::new_v4(),
            dialog: self
                .product
                .git_sheet
                .as_ref()
                .map_or_else(Uuid::new_v4, |sheet| sheet.dialog),
            target: operation.target.clone(),
            kind: Kind::Commit {
                ticket: operation.state.ticket,
                message: None,
                inspect,
                acknowledge,
            },
        };
        if self.git_worker.sender.try_send(pending.command()).is_err() {
            return;
        }
        if inspect {
            self.git_flow.last_commit_inspect = Some(Instant::now());
        }
        self.git_flow.pending = Some(pending);
        self.git_flow.next = None;
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
            Kind::Index { .. }
            | Kind::IndexAcknowledge { .. }
            | Kind::CommitPrepare
            | Kind::Commit { .. } => return,
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
                    Kind::Snapshot => {
                        sheet.loading = true;
                        sheet.watch_stamp = self
                            .product
                            .git_watch_view
                            .as_ref()
                            .filter(|view| {
                                view.workspace == sheet.target.workspace
                                    && view.revision == sheet.target.revision
                                    && view.project == sheet.target.project
                                    && view.cwd == sheet.target.cwd
                            })
                            .and_then(crate::git_watch::View::stamp);
                    }
                    Kind::Diff { path, side } => {
                        if let Some(diff) = &mut sheet.diff
                            && diff.path == *path
                            && diff.side == *side
                        {
                            diff.loading = true;
                        }
                    }
                    Kind::Index { .. }
                    | Kind::IndexAcknowledge { .. }
                    | Kind::CommitPrepare
                    | Kind::Commit { .. } => (),
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
        if self.git_flow.pending.is_none()
            && (self
                .product
                .dialog
                .as_ref()
                .is_some_and(|dialog| dialog.kind == DialogKind::WorkspaceGit)
                || self.close_flow.active())
            && let Some(operation) = self.git_flow.commit.as_ref()
            && operation.state.phase == protocol::GitCommitPhase::Running
            && (self.close_flow.active()
                || self.product.git_sheet.as_ref().is_some_and(|sheet| {
                    sheet.target.same_scope(&operation.target)
                        && sheet.target.current(self.product.catalog.as_ref())
                }))
            && self
                .git_flow
                .last_commit_inspect
                .is_none_or(|last| last.elapsed().as_secs() >= 1)
        {
            self.git_flow.last_commit_inspect = Some(Instant::now());
            self.request_git_commit(false, true, cx);
        }
        let Ok(reply) = self.git_worker.receiver.try_recv() else {
            return;
        };
        let request = match &reply {
            Ok(reply) => reply.ui_request,
            Err(error) => error.ui_request,
        };
        if self.product.git_batch.matches_request(request) {
            self.receive_git_batch(reply, cx);
            return;
        }
        if let Ok(reply) = &reply
            && let Some(snapshot) = &reply.workspace_git_snapshot
            && let Some(target) = self.product.catalog.as_ref().and_then(Target::selected)
        {
            self.product.git_batch.observe_snapshot(&target, snapshot);
        }
        if !self
            .git_flow
            .pending
            .as_ref()
            .is_some_and(|pending| Some(pending.request) == request)
        {
            return;
        }
        let pending = self.git_flow.pending.take().expect("matching Git request");
        if let Err(error) = &reply {
            self.close_flow
                .fail(format!("Операция Git не подтверждена: {}", error.message));
        }
        if matches!(pending.kind, Kind::CommitPrepare | Kind::Commit { .. }) {
            match pending.kind {
                Kind::CommitPrepare => {
                    let review_is_current = self.product.dialog.as_ref().is_some_and(|dialog| {
                        dialog.id == pending.dialog
                            && dialog.kind == DialogKind::WorkspaceGitCommit
                            && self.product.git_sheet.as_ref().is_some_and(|sheet| {
                                sheet.target == pending.target
                                    && pending.target.current(self.product.catalog.as_ref())
                            })
                    });
                    let result = reply.and_then(|reply| {
                        reply
                            .workspace_git_commit_plan
                            .ok_or_else(|| protocol::ProductError {
                                ui_request: None,
                                providers_request: false,
                                message: "Session host не вернул план коммита.".into(),
                                rejected_before_launch: false,
                            })
                    });
                    match result {
                        Ok(_) if !review_is_current => {
                            // A late response from a closed/replaced review cannot
                            // install a plan into the next dialog instance.
                        }
                        Ok(plan)
                            if plan.workspace_id == pending.target.workspace
                                && plan.workspace_revision == pending.target.revision
                                && plan.project_id == pending.target.project
                                && plan.cwd == pending.target.cwd =>
                        {
                            if let Some(draft) = self
                                .product
                                .dialog
                                .as_ref()
                                .filter(|dialog| dialog.kind == DialogKind::WorkspaceGitCommit)
                                .and_then(|dialog| dialog.fields.first())
                                .map(|field| field.text().to_owned())
                            {
                                self.git_flow
                                    .remember_commit_draft(pending.target.workspace, &draft);
                            }
                            self.git_flow.commit_plan =
                                Some((pending.target.clone(), plan.clone()));
                            self.product.git_commit_plan = Some(plan.clone());
                            self.product.git_commit_plan_error = None;
                        }
                        Ok(_) => {
                            self.git_flow.commit_plan = None;
                            self.product.git_commit_plan = None;
                            self.product.git_commit_plan_error =
                                Some("План коммита относится к другой ревизии воркспейса.".into());
                            self.product.notice =
                                Some("План коммита относится к другой ревизии воркспейса.".into());
                        }
                        Err(error) => {
                            if review_is_current {
                                self.git_flow.commit_plan = None;
                                self.product.git_commit_plan = None;
                                self.product.git_commit_plan_error = Some(error.message.clone());
                                self.product.notice = Some(format!(
                                    "Не удалось подготовить план коммита: {}",
                                    error.message
                                ));
                            }
                        }
                    }
                }
                Kind::Commit {
                    ticket,
                    acknowledge,
                    ..
                } => {
                    let receipt = reply.and_then(|reply| {
                        reply
                            .workspace_git_commit
                            .ok_or_else(|| protocol::ProductError {
                                ui_request: None,
                                providers_request: false,
                                message: "Session host не вернул квитанцию коммита.".into(),
                                rejected_before_launch: false,
                            })
                    });
                    let mut applied_transition = false;
                    if let Some(operation) = &mut self.git_flow.commit
                        && operation.target == pending.target
                        && operation.state.ticket == ticket
                    {
                        applied_transition =
                            operation.receive(receipt.map_err(|error| error.message));
                        if acknowledge && operation.state.acknowledged {
                            self.git_flow.commit = None;
                        }
                    }
                    if applied_transition {
                        self.git_flow
                            .commit_applied(pending.target.workspace, ticket);
                        self.queue_commit_snapshot_refresh(&pending);
                    }
                }
                _ => unreachable!(),
            }
            // Receipt state is global; only the active dialog gets presentation changes.
            self.sync_git_index_notice();
            if self.product.dialog.as_ref().is_some_and(|dialog| {
                dialog.kind == DialogKind::WorkspaceGit
                    && self
                        .product
                        .git_sheet
                        .as_ref()
                        .is_some_and(|sheet| sheet.target.same_scope(&pending.target))
            }) && let Some(operation) = &self.git_flow.commit
            {
                self.product.notice = Some(commit_notice(&operation.state));
            }
            cx.notify();
            self.dispatch_queued_git();
            return;
        }
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
            if let Ok(reply) = &reply
                && let Some(snapshot) = &reply.workspace_git_snapshot
                && let Some(receipt) = &snapshot.commit_operation
                && !self
                    .git_flow
                    .commit
                    .as_ref()
                    .is_some_and(CommitOperation::unresolved)
                && let Some(sheet) = &self.product.git_sheet
                && pending.current(sheet, self.product.catalog.as_ref())
            {
                let mut target = sheet.target.clone();
                target.revision = receipt.workspace_revision;
                if receipt.workspace_id == target.workspace
                    && receipt.project_id == target.project
                    && receipt.cwd == target.cwd
                {
                    self.git_flow.commit = Some(CommitOperation {
                        target,
                        state: receipt.clone(),
                    });
                }
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
                    Kind::CommitPrepare | Kind::Commit { .. } => (),
                }
            }
            self.sync_git_index_notice();
            if let Some(operation) = &self.git_flow.commit {
                self.product.notice = Some(commit_notice(&operation.state));
            }
        }
        self.dispatch_queued_git();
        cx.notify();
    }

    fn queue_commit_snapshot_refresh(&mut self, completed: &Pending) {
        let Some(sheet) = self.product.git_sheet.as_ref() else {
            return;
        };
        if !self.product.dialog.as_ref().is_some_and(|dialog| {
            dialog.id == completed.dialog && dialog.kind == DialogKind::WorkspaceGit
        }) || sheet.target != completed.target
            || !sheet.target.current(self.product.catalog.as_ref())
            || sheet.diff.is_some()
        {
            return;
        }
        let refresh = Pending {
            request: Uuid::new_v4(),
            dialog: sheet.dialog,
            target: sheet.target.clone(),
            kind: Kind::Snapshot,
        };
        enqueue_commit_refresh(
            &mut self.git_flow.next,
            &mut self.git_flow.after_read,
            refresh,
        );
    }

    fn dispatch_queued_git(&mut self) {
        while self.git_flow.pending.is_none() {
            let next = self.git_flow.next.take();
            let next = next.or_else(|| self.git_flow.after_read.take());
            let Some(next) = next else {
                return;
            };
            let valid = self.product.dialog.as_ref().is_some_and(|dialog| {
                dialog.id == next.dialog && dialog.kind == DialogKind::WorkspaceGit
            }) && self
                .product
                .git_sheet
                .as_ref()
                .is_some_and(|sheet| next.current(sheet, self.product.catalog.as_ref()));
            if !valid {
                continue;
            }
            self.dispatch_git(next);
            if self.git_flow.pending.is_some() {
                if self.git_flow.next.is_none() {
                    self.git_flow.next = self.git_flow.after_read.take();
                }
                return;
            }
        }
    }
}

fn commit_notice(receipt: &protocol::WorkspaceGitCommitResult) -> String {
    match receipt.phase {
        protocol::GitCommitPhase::Running => "Создаём коммит…".into(),
        protocol::GitCommitPhase::Applied => format!(
            "Коммит создан: {}",
            receipt
                .commit
                .as_deref()
                .map(|oid| oid.chars().take(12).collect::<String>())
                .unwrap_or_else(|| "OID отсутствует".into())
        ),
        protocol::GitCommitPhase::Failed | protocol::GitCommitPhase::Rejected => format!(
            "Коммит не создан: {}",
            receipt
                .message
                .as_deref()
                .unwrap_or("операция завершилась ошибкой")
        ),
        protocol::GitCommitPhase::Unknown => format!(
            "Исход коммита неизвестен. Проверьте квитанцию; повтор запрещён. {}",
            receipt.message.as_deref().unwrap_or("")
        ),
        protocol::GitCommitPhase::Unavailable => format!(
            "Квитанция коммита недоступна. {}",
            receipt.message.as_deref().unwrap_or("")
        ),
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
            agent_profile_roster: None,
            prompts: None,
            tab_close_schema: None,
            engine_options_schema: Some(1),
            pane_tab_drop_schema: Some(1),
            pane_workspace_drop_schema: Some(1),
            workspace_recovery_schema: Some(1),
            history_grid_schema: Some(1),
            custom_cli_schema: Some(1),
            workspace_sessions_schema: None,
            history_pan_schema: None,
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
                watch_stamp: None,
                loading: true,
                error: None,
                selected: 0,
                diff: None,
                last_index: None,
                commit_operation: None,
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
            index_batch_schema: None,
            index_batch_operation: None,
            index_batch_unavailable_reason: None,
            commit_schema: None,
            commit_operation: None,
            commit_unavailable_reason: None,
        }
    }

    #[test]
    fn commit_review_action_requires_supported_host_and_staged_entries() {
        let (_, mut sheet, _) = fixture();
        assert!(!sheet.can_prepare_commit());
        let mut status = snapshot(&sheet.target);
        status.commit_schema = Some(1);
        status.entries.push(protocol::WorkspaceGitEntry {
            path: "tracked.txt".into(),
            original_path: None,
            index_status: 'M',
            worktree_status: '.',
            untracked: false,
            unmerged: false,
        });
        sheet.snapshot = Some(status);
        sheet.loading = false;
        assert!(sheet.can_prepare_commit());
        sheet.snapshot.as_mut().unwrap().commit_unavailable_reason =
            Some("Git version unavailable".into());
        assert!(!sheet.can_prepare_commit());
        sheet.snapshot.as_mut().unwrap().commit_unavailable_reason = None;
        sheet
            .snapshot
            .as_mut()
            .unwrap()
            .index_operation_unavailable_reason = Some("Receipt unavailable".into());
        assert!(!sheet.can_prepare_commit());
        sheet
            .snapshot
            .as_mut()
            .unwrap()
            .index_operation_unavailable_reason = None;
        sheet.last_index = Some(IndexNotice {
            path: "tracked.txt".into(),
            workspace: "Task".into(),
            state: IndexState {
                ticket: protocol::GitIndexTicket {
                    id: Uuid::new_v4(),
                    action: protocol::GitIndexAction::Stage,
                },
                phase: IndexPhase::Unknown,
                message: None,
            },
        });
        assert!(!sheet.can_prepare_commit());
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

    #[test]
    fn close_retains_durable_git_work_across_transport_gaps_and_unknown_outcomes() {
        let (index, _) = index_fixture();
        let mut flow = Flow {
            index: Some(index),
            ..Default::default()
        };
        assert!(!flow.request_pending());
        assert_eq!(
            flow.unresolved_resources(),
            vec!["Итог изменения Git-индекса"]
        );
        flow.index.as_mut().unwrap().state.phase = IndexPhase::Unknown;
        assert!(flow.close_uncertainty().is_some());
        flow.index.as_mut().unwrap().state.phase = IndexPhase::Applied;
        assert!(flow.unresolved_resources().is_empty());
        assert!(flow.close_uncertainty().is_none());
        let (_, sheet, _) = fixture();
        flow.commit = Some(CommitOperation {
            target: sheet.target.clone(),
            state: protocol::WorkspaceGitCommitResult {
                ticket: Uuid::new_v4(),
                workspace_id: sheet.target.workspace,
                workspace_revision: sheet.target.revision,
                project_id: sheet.target.project,
                cwd: sheet.target.cwd.clone(),
                phase: protocol::GitCommitPhase::Running,
                commit: None,
                acknowledged: false,
                message: None,
            },
        });
        assert!(!flow.request_pending());
        assert_eq!(
            flow.unresolved_resources(),
            vec!["Итог отправленного Git-коммита"]
        );
        flow.commit.as_mut().unwrap().state.phase = protocol::GitCommitPhase::Unknown;
        assert!(flow.close_uncertainty().is_some());
        flow.commit.as_mut().unwrap().state.phase = protocol::GitCommitPhase::Applied;
        assert!(flow.unresolved_resources().is_empty());
    }

    #[test]
    fn applied_commit_clears_only_its_exact_submitted_draft() {
        let workspace = Uuid::from_u128(1);
        let ticket = Uuid::from_u128(2);
        let mut flow = Flow::default();
        flow.remember_commit_draft(workspace, "first");
        flow.submitted_draft = Some((workspace, ticket, "first".into()));
        flow.commit_applied(workspace, Uuid::nil());
        assert_eq!(flow.dirty_commit_workspaces(), vec![workspace]);
        flow.remember_commit_draft(workspace, "new unsaved text");
        flow.commit_applied(workspace, ticket);
        assert_eq!(flow.commit_draft(workspace), "new unsaved text");
        flow.submitted_draft = Some((workspace, ticket, "new unsaved text".into()));
        flow.commit_applied(workspace, ticket);
        assert!(flow.dirty_commit_workspaces().is_empty());
    }

    #[test]
    fn commit_message_validation_and_workspace_drafts_are_bounded() {
        assert!(valid_commit_message("subject\nbody\tcolumn"));
        assert!(!valid_commit_message(" \n\t"));
        assert!(!valid_commit_message(&"x".repeat(8193)));
        assert!(!valid_commit_message("subject\u{0007}"));

        let mut flow = Flow::default();
        let workspace = Uuid::new_v4();
        let oversized_for_git = format!("{}界", "x".repeat(8191));
        flow.remember_commit_draft(workspace, &oversized_for_git);
        assert_eq!(flow.commit_draft(workspace), oversized_for_git);
        for _ in 0..16 {
            flow.remember_commit_draft(Uuid::new_v4(), "draft");
        }
        assert_eq!(flow.commit_drafts.len(), 16);
    }

    #[test]
    fn commit_transport_or_mismatched_receipt_stays_unknown_without_retry() {
        let (_, sheet, _) = fixture();
        let receipt = protocol::WorkspaceGitCommitResult {
            ticket: Uuid::new_v4(),
            workspace_id: sheet.target.workspace,
            workspace_revision: sheet.target.revision,
            project_id: sheet.target.project,
            cwd: sheet.target.cwd.clone(),
            phase: protocol::GitCommitPhase::Running,
            commit: None,
            acknowledged: false,
            message: None,
        };
        let mut operation = CommitOperation {
            target: sheet.target.clone(),
            state: receipt.clone(),
        };
        operation.receive(Err("reply lost".into()));
        assert_eq!(operation.state.phase, protocol::GitCommitPhase::Unknown);
        assert!(operation.unresolved());
        assert!(
            operation
                .state
                .message
                .as_deref()
                .unwrap()
                .contains("не повторяется")
        );

        operation.state = receipt;
        let mut wrong = operation.state.clone();
        wrong.ticket = Uuid::new_v4();
        wrong.phase = protocol::GitCommitPhase::Applied;
        wrong.commit = Some("a".repeat(40));
        operation.receive(Ok(wrong));
        assert_eq!(operation.state.phase, protocol::GitCommitPhase::Unknown);
        assert!(operation.state.commit.is_none());
    }

    #[test]
    fn applied_commit_transition_is_emitted_once_for_refresh_scheduling() {
        let (_, sheet, _) = fixture();
        let running = protocol::WorkspaceGitCommitResult {
            ticket: Uuid::new_v4(),
            workspace_id: sheet.target.workspace,
            workspace_revision: sheet.target.revision,
            project_id: sheet.target.project,
            cwd: sheet.target.cwd.clone(),
            phase: protocol::GitCommitPhase::Running,
            commit: None,
            acknowledged: false,
            message: None,
        };
        let mut operation = CommitOperation {
            target: sheet.target.clone(),
            state: running,
        };
        let mut applied = operation.state.clone();
        applied.phase = protocol::GitCommitPhase::Applied;
        applied.commit = Some("a".repeat(40));
        assert!(operation.receive(Ok(applied.clone())));
        assert!(!operation.receive(Ok(applied)));

        operation.state.phase = protocol::GitCommitPhase::Unknown;
        let mut recovered = operation.state.clone();
        recovered.phase = protocol::GitCommitPhase::Applied;
        recovered.commit = Some("b".repeat(40));
        assert!(operation.receive(Ok(recovered)));
    }

    #[test]
    fn commit_snapshot_refresh_queues_after_existing_read_without_replacing_it() {
        let (_, sheet, _) = fixture();
        let existing = Pending {
            request: Uuid::new_v4(),
            dialog: sheet.dialog,
            target: sheet.target.clone(),
            kind: Kind::Diff {
                path: "file.txt".into(),
                side: protocol::GitDiffSide::Worktree,
            },
        };
        let existing_request = existing.request;
        let refresh = Pending {
            request: Uuid::new_v4(),
            dialog: sheet.dialog,
            target: sheet.target.clone(),
            kind: Kind::Snapshot,
        };
        let mut next = Some(existing);
        let mut after_read = None;
        enqueue_commit_refresh(&mut next, &mut after_read, refresh);
        assert_eq!(next.as_ref().unwrap().request, existing_request);
        assert!(matches!(after_read.as_ref().unwrap().kind, Kind::Snapshot));

        let duplicate = Pending {
            request: Uuid::new_v4(),
            dialog: sheet.dialog,
            target: sheet.target.clone(),
            kind: Kind::Snapshot,
        };
        enqueue_commit_refresh(&mut next, &mut after_read, duplicate);
        assert!(matches!(after_read.as_ref().unwrap().kind, Kind::Snapshot));
    }

    #[test]
    fn commit_start_and_inspection_have_distinct_non_retrying_payloads() {
        let (_, sheet, _) = fixture();
        let ticket = Uuid::new_v4();
        let start = Pending {
            request: Uuid::new_v4(),
            dialog: sheet.dialog,
            target: sheet.target.clone(),
            kind: Kind::Commit {
                ticket,
                message: Some("Review first".into()),
                inspect: false,
                acknowledge: false,
            },
        }
        .command();
        assert_eq!(start["command"], "workspace_git_commit");
        assert_eq!(start["message"], "Review first");
        assert_eq!(start["inspect"], false);
        let inspect = Pending {
            request: Uuid::new_v4(),
            dialog: sheet.dialog,
            target: sheet.target.clone(),
            kind: Kind::Commit {
                ticket,
                message: None,
                inspect: true,
                acknowledge: false,
            },
        }
        .command();
        assert_eq!(inspect["command"], "workspace_git_commit");
        assert!(inspect["message"].is_null());
        assert_eq!(inspect["inspect"], true);
        assert_eq!(inspect["ticket"], ticket.to_string());
    }

    #[test]
    fn unresolved_commit_blocks_index_mutation_and_exposes_only_inspection_choices() {
        let (_, mut sheet, _) = fixture();
        let mut snapshot = snapshot(&sheet.target);
        snapshot.commit_schema = Some(1);
        snapshot.entries.push(protocol::WorkspaceGitEntry {
            path: "staged.rs".into(),
            original_path: None,
            index_status: 'M',
            worktree_status: ' ',
            untracked: false,
            unmerged: false,
        });
        sheet.snapshot = Some(snapshot);
        let ticket = protocol::GitIndexTicket {
            id: Uuid::new_v4(),
            action: protocol::GitIndexAction::Stage,
        };
        sheet.diff = Some(DiffSheet {
            path: "staged.rs".into(),
            side: protocol::GitDiffSide::Worktree,
            result: Some(protocol::WorkspaceGitDiff {
                workspace_id: sheet.target.workspace,
                workspace_revision: sheet.target.revision,
                project_id: sheet.target.project,
                cwd: sheet.target.cwd.clone(),
                path: "staged.rs".into(),
                side: protocol::GitDiffSide::Worktree,
                text: "diff".into(),
                truncated: false,
                binary: false,
                index_ticket: Some(ticket),
                index_unavailable_reason: None,
            }),
            loading: false,
            error: None,
            page: 0,
            index_state: Some(IndexState {
                ticket,
                phase: IndexPhase::Ready,
                message: None,
            }),
        });
        sheet.commit_operation = Some(protocol::WorkspaceGitCommitResult {
            ticket: Uuid::new_v4(),
            workspace_id: sheet.target.workspace,
            workspace_revision: sheet.target.revision,
            project_id: sheet.target.project,
            cwd: sheet.target.cwd.clone(),
            phase: protocol::GitCommitPhase::Unknown,
            commit: None,
            acknowledged: false,
            message: None,
        });
        let actions = sheet.control_actions();
        assert!(actions.contains(&ProductAction::GitInspectCommit));
        assert!(actions.contains(&ProductAction::GitAcknowledgeCommit));
        assert!(!actions.contains(&ProductAction::GitApplyIndex));
    }
}
