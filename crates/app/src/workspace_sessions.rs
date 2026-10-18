//! Read-only, bounded session recovery overview for the selected workspace.
//!
//! This model owns request scope and paging state only. It never starts a CLI;
//! resume is prepared through the existing host launch review path.

use crate::protocol::{self, ProductError, ProductReply, WorkspaceResumeState};
use pigade_storage::{Catalog, WorkspaceStatus};
use serde_json::{Value, json};
use std::path::PathBuf;
use uuid::Uuid;

const MAX_CURSOR_HISTORY: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub workspace: Uuid,
    pub revision: u64,
    pub project: Uuid,
    pub cwd: PathBuf,
}

impl Target {
    pub fn selected(catalog: &Catalog) -> Option<Self> {
        if catalog.workspace_sessions_schema != Some(1) {
            return None;
        }
        let workspace = catalog.workspaces.iter().find(|workspace| {
            Some(workspace.id) == catalog.selection.workspace_id
                && workspace.status == WorkspaceStatus::Ready
        })?;
        Some(Self {
            workspace: workspace.id,
            revision: workspace.revision,
            project: workspace.project_id,
            cwd: workspace.cwd.clone(),
        })
    }

    pub fn current(&self, catalog: Option<&Catalog>) -> bool {
        catalog.and_then(Self::selected).as_ref() == Some(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sheet {
    pub dialog: Uuid,
    pub target: Target,
    pub page: Option<protocol::WorkspaceSessions>,
    pub error: Option<String>,
    pub selected: usize,
    pub selected_session: Option<Uuid>,
    pub previous_cursors: Vec<Option<Uuid>>,
}

#[derive(Clone, Debug)]
struct Pending {
    id: Uuid,
    cursor: Option<Uuid>,
    restore_cursors: Option<Vec<Option<Uuid>>>,
}

#[derive(Default)]
pub struct Model {
    pub sheet: Option<Sheet>,
    pending: Option<Pending>,
}

impl Model {
    pub fn close(&mut self, dialog: Uuid) -> bool {
        if self
            .sheet
            .as_ref()
            .is_none_or(|sheet| sheet.dialog != dialog)
        {
            return false;
        }
        self.sheet = None;
        self.pending = None;
        true
    }

    pub fn open(&mut self, catalog: &Catalog, dialog: Uuid) -> Result<Value, String> {
        if self.pending.is_some() {
            return Err("Дождитесь ответа на запрос сессий".into());
        }
        if dialog.is_nil() {
            return Err("Недопустимый идентификатор окна сессий".into());
        }
        let target = Target::selected(catalog)
            .ok_or_else(|| "Сессии воркспейса недоступны для выбранной записи".to_owned())?;
        self.sheet = Some(Sheet {
            dialog,
            target,
            page: None,
            error: None,
            selected: 0,
            selected_session: None,
            previous_cursors: Vec::new(),
        });
        self.pending = None;
        self.request_page(None)
    }

    pub fn refresh(&mut self) -> Result<Value, String> {
        let cursor = self
            .sheet
            .as_ref()
            .and_then(|sheet| sheet.page.as_ref())
            .and_then(|page| page.after);
        self.request_page(cursor)
    }

    /// Request the first page and reset backward history, restoring it if unsent/failed.
    pub fn first(&mut self) -> Result<Value, String> {
        if self.pending.is_some() {
            return Err("Запрос сессий уже выполняется".into());
        }
        let before = self
            .sheet
            .as_ref()
            .ok_or_else(|| "Окно сессий закрыто".to_owned())?
            .previous_cursors
            .clone();
        self.sheet.as_mut().unwrap().previous_cursors.clear();
        self.request_page_with_restore(None, Some(before))
    }

    pub fn next(&mut self) -> Result<Value, String> {
        if self.pending.is_some() {
            return Err("Запрос сессий уже выполняется".into());
        }
        let sheet = self
            .sheet
            .as_ref()
            .ok_or_else(|| "Окно сессий закрыто".to_owned())?;
        let page = sheet
            .page
            .as_ref()
            .ok_or_else(|| "Страница сессий ещё не загружена".to_owned())?;
        let cursor = page
            .next_after
            .ok_or_else(|| "Следующей страницы нет".to_owned())?;
        let before = sheet.previous_cursors.clone();
        let current_cursor = page.after;
        let cursors = &mut self.sheet.as_mut().unwrap().previous_cursors;
        cursors.push(current_cursor);
        if cursors.len() > MAX_CURSOR_HISTORY {
            cursors.remove(0);
        }
        self.request_page_with_restore(Some(cursor), Some(before))
    }

    pub fn previous(&mut self) -> Result<Value, String> {
        if self.pending.is_some() {
            return Err("Запрос сессий уже выполняется".into());
        }
        let cursor = self
            .sheet
            .as_ref()
            .ok_or_else(|| "Окно сессий закрыто".to_owned())?
            .previous_cursors
            .last()
            .copied()
            .ok_or_else(|| "Предыдущей страницы нет".to_owned())?;
        let before = self.sheet.as_ref().unwrap().previous_cursors.clone();
        self.sheet.as_mut().unwrap().previous_cursors.pop();
        self.request_page_with_restore(cursor, Some(before))
    }

    fn request_page(&mut self, cursor: Option<Uuid>) -> Result<Value, String> {
        self.request_page_with_restore(cursor, None)
    }

    fn request_page_with_restore(
        &mut self,
        cursor: Option<Uuid>,
        restore_cursors: Option<Vec<Option<Uuid>>>,
    ) -> Result<Value, String> {
        if self.pending.is_some() {
            return Err("Запрос сессий уже выполняется".into());
        }
        let sheet = self
            .sheet
            .as_ref()
            .ok_or_else(|| "Окно сессий закрыто".to_owned())?;
        let request = Uuid::new_v4();
        let value = json!({
            "command": "workspace_sessions",
            "workspace": sheet.target.workspace,
            "expected_revision": sheet.target.revision,
            "after": cursor,
            "_ui_request": request,
        });
        self.pending = Some(Pending {
            id: request,
            cursor,
            restore_cursors,
        });
        Ok(value)
    }

    pub fn matches_request(&self, id: Uuid) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
    }

    pub fn request_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Select a visible row by its current page index, matching keyboard selection behavior.
    pub fn select(&mut self, index: usize) -> Option<Uuid> {
        let sheet = self.sheet.as_mut()?;
        let page = sheet.page.as_ref()?;
        if page.entries.is_empty() {
            sheet.selected = 0;
            sheet.selected_session = None;
            return None;
        }
        sheet.selected = index.min(page.entries.len() - 1);
        sheet.selected_session = page.entries.get(sheet.selected).map(|entry| entry.session);
        sheet.selected_session
    }

    /// Cancels only an unsent request and restores the pre-request page cursor state.
    pub fn abort_unsent(&mut self, request: Uuid) -> bool {
        let Some(pending) = self.pending.as_ref() else {
            return false;
        };
        if pending.id != request {
            return false;
        }
        let restore_cursors = pending.restore_cursors.clone();
        self.pending = None;
        if let (Some(sheet), Some(cursors)) = (self.sheet.as_mut(), restore_cursors) {
            sheet.previous_cursors = cursors;
        }
        true
    }

    pub fn receive(
        &mut self,
        result: Result<ProductReply, ProductError>,
        live_dialog: Option<Uuid>,
        catalog: Option<&Catalog>,
    ) -> bool {
        let incoming_id = match &result {
            Ok(reply) => reply.ui_request,
            Err(error) => error.ui_request,
        };
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| Some(pending.id) != incoming_id)
        {
            return false;
        }
        let Some(pending) = self.pending.take() else {
            return false;
        };
        let Some(sheet) = self.sheet.as_mut() else {
            return false;
        };
        if live_dialog != Some(sheet.dialog) || !sheet.target.current(catalog) {
            if let Some(cursors) = pending.restore_cursors.as_ref() {
                sheet.previous_cursors = cursors.clone();
            }
            return false;
        }
        let restore_cursors = pending.restore_cursors.clone();
        match result {
            Ok(reply) if reply.ui_request == Some(pending.id) => {
                let Some(page) = reply.workspace_sessions else {
                    if let Some(cursors) = restore_cursors {
                        sheet.previous_cursors = cursors;
                    }
                    sheet.error = Some("Session host не вернул страницу сессий воркспейса".into());
                    return true;
                };
                if page.after != pending.cursor
                    || page.workspace_id != sheet.target.workspace
                    || page.workspace_revision != sheet.target.revision
                    || page.project_id != sheet.target.project
                    || page.cwd != sheet.target.cwd
                {
                    if let Some(cursors) = restore_cursors {
                        sheet.previous_cursors = cursors;
                    }
                    sheet.error = Some("Ответ относится к другой области сессий".into());
                    return true;
                }
                let selected = sheet.selected_session;
                sheet.selected = selected
                    .and_then(|id| page.entries.iter().position(|entry| entry.session == id))
                    .unwrap_or(0);
                sheet.selected_session =
                    page.entries.get(sheet.selected).map(|entry| entry.session);
                sheet.page = Some(page);
                sheet.error = None;
            }
            Ok(_) => return false,
            Err(error) if error.ui_request == Some(pending.id) => {
                if let Some(cursors) = restore_cursors {
                    sheet.previous_cursors = cursors;
                }
                sheet.error = Some(error.message.chars().take(1024).collect());
            }
            Err(_) => return false,
        }
        true
    }

    pub fn move_selection(&mut self, delta: isize) -> Option<Uuid> {
        let sheet = self.sheet.as_mut()?;
        let page = sheet.page.as_ref()?;
        if page.entries.is_empty() {
            return None;
        }
        sheet.selected = sheet
            .selected
            .saturating_add_signed(delta)
            .min(page.entries.len() - 1);
        sheet.selected_session = page.entries.get(sheet.selected).map(|entry| entry.session);
        sheet.selected_session
    }

    pub fn selected_entry(&self) -> Option<&protocol::WorkspaceSessionEntry> {
        let sheet = self.sheet.as_ref()?;
        sheet.page.as_ref()?.entries.get(sheet.selected)
    }

    /// Returns a host preparation request for the exact selected native conversation.
    pub fn resume_command(&self, catalog: &Catalog, dialog: Uuid) -> Result<Value, String> {
        let sheet = self
            .sheet
            .as_ref()
            .ok_or_else(|| "Окно сессий закрыто".to_owned())?;
        if sheet.dialog != dialog
            || !sheet.target.current(Some(catalog))
            || self.pending.is_some()
            || sheet.error.is_some()
        {
            return Err("Состояние воркспейса изменилось; обновите список сессий".into());
        }
        let entry = self
            .selected_entry()
            .ok_or_else(|| "Выберите сессию".to_owned())?;
        let conversation = entry
            .conversation
            .as_ref()
            .ok_or_else(|| "У сессии нет подтверждённой привязки беседы".to_owned())?;
        let head = entry
            .head
            .ok_or_else(|| "У сессии нет текущего поколения".to_owned())?;
        if entry.resume_state != WorkspaceResumeState::CheckAvailable
            || entry.selected_generation != Some(head)
        {
            return Err("Продолжение доступно только для текущей беседы и поколения".into());
        }
        Ok(json!({
            "command": "prepare_workspace_resume",
            "workspace": sheet.target.workspace,
            "expected_revision": sheet.target.revision,
            "session": entry.session,
            "expected_head": head,
            "expected_binding_revision": conversation.revision,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_storage::{Project, Workspace, WorkspaceMode};

    fn catalog(schema: Option<u16>) -> Catalog {
        let project = Uuid::from_u128(1);
        let workspace = Uuid::from_u128(2);
        let mut catalog: Catalog = serde_json::from_value(json!({
            "projects": [], "workspaces": [], "tabs": [],
            "selection": {"project_id": project, "workspace_id": workspace, "tab_id": null,
                "window": pigade_storage::WindowState::default(), "revision": 1},
            "layout": {"json": "null", "revision": 1}
        }))
        .unwrap();
        catalog.projects.push(Project {
            id: project,
            canonical_path: "/repo".into(),
            git_common_dir: "/repo/.git".into(),
            name: "Repo".into(),
            roots: vec![],
            revision: 1,
        });
        catalog.workspaces.push(Workspace {
            id: workspace,
            project_id: project,
            task: "task".into(),
            cwd: "/repo".into(),
            mode: WorkspaceMode::SharedCheckout,
            status: WorkspaceStatus::Ready,
            error: None,
            context_revision: None,
            revision: 7,
        });
        catalog.workspace_sessions_schema = schema;
        catalog
    }

    fn entry(state: WorkspaceResumeState) -> protocol::WorkspaceSessionEntry {
        let session = Uuid::from_u128(10);
        let tab = Uuid::from_u128(11);
        let generation = Uuid::from_u128(12);
        protocol::WorkspaceSessionEntry {
            session,
            tab_id: tab,
            tab_name: "Main".into(),
            selected_generation: Some(generation),
            head: Some(generation),
            title: "Conversation".into(),
            provider: Some("codex".into()),
            state: "exited".into(),
            head_state: "exited".into(),
            head_pid: None,
            conversation: Some(protocol::WorkspaceConversation {
                provider: "codex".into(),
                conversation_id: "thread-id".into(),
                revision: 4,
                source_generation: Uuid::from_u128(13),
            }),
            resume_state: state,
        }
    }

    fn page(entries: Vec<protocol::WorkspaceSessionEntry>) -> protocol::WorkspaceSessions {
        protocol::WorkspaceSessions {
            schema: 1,
            workspace_id: Uuid::from_u128(2),
            workspace_revision: 7,
            project_id: Uuid::from_u128(1),
            cwd: "/repo".into(),
            after: None,
            entries,
            next_after: None,
        }
    }

    fn page_at(after: Option<Uuid>, first_session: u128) -> protocol::WorkspaceSessions {
        let entries: Vec<_> = (0..32)
            .map(|offset| {
                let mut row = entry(WorkspaceResumeState::Unknown);
                row.session = Uuid::from_u128(first_session + offset);
                row
            })
            .collect();
        let next_after = entries.last().map(|row| row.session);
        protocol::WorkspaceSessions {
            after,
            entries,
            next_after,
            ..page(Vec::new())
        }
    }

    fn receive_page(
        model: &mut Model,
        catalog: &Catalog,
        dialog: Uuid,
        request: Value,
        page: protocol::WorkspaceSessions,
    ) -> bool {
        let request_id = request["_ui_request"].as_str().unwrap().parse().unwrap();
        model.receive(
            Ok(ProductReply {
                ui_request: Some(request_id),
                workspace_sessions: Some(page),
                ..Default::default()
            }),
            Some(dialog),
            Some(catalog),
        )
    }

    #[test]
    fn capability_and_selected_workspace_scope_gate_open() {
        let mut model = Model::default();
        assert!(model.open(&catalog(None), Uuid::from_u128(20)).is_err());
        let request = model.open(&catalog(Some(1)), Uuid::from_u128(20)).unwrap();
        assert_eq!(request["command"], "workspace_sessions");
        assert_eq!(request["workspace"], Uuid::from_u128(2).to_string());
        assert_eq!(request["expected_revision"], 7);
        assert!(model.matches_request(request["_ui_request"].as_str().unwrap().parse().unwrap()));
    }

    #[test]
    fn resume_freezes_current_head_and_binding_and_refuses_unknown_or_pinned_rows() {
        let catalog = catalog(Some(1));
        let mut model = Model::default();
        let request = model.open(&catalog, Uuid::from_u128(20)).unwrap();
        let id = request["_ui_request"].as_str().unwrap().parse().unwrap();
        let page = protocol::WorkspaceSessions {
            schema: 1,
            workspace_id: Uuid::from_u128(2),
            workspace_revision: 7,
            project_id: Uuid::from_u128(1),
            cwd: "/repo".into(),
            after: None,
            entries: vec![entry(WorkspaceResumeState::CheckAvailable)],
            next_after: None,
        };
        assert!(model.receive(
            Ok(ProductReply {
                ui_request: Some(id),
                workspace_sessions: Some(page),
                ..Default::default()
            }),
            Some(Uuid::from_u128(20)),
            Some(&catalog)
        ));
        let request = model.resume_command(&catalog, Uuid::from_u128(20)).unwrap();
        assert_eq!(request["command"], "prepare_workspace_resume");
        assert_eq!(request["session"], Uuid::from_u128(10).to_string());
        assert_eq!(request["expected_head"], Uuid::from_u128(12).to_string());
        assert_eq!(request["expected_binding_revision"], 4);
        for state in [
            WorkspaceResumeState::Unknown,
            WorkspaceResumeState::PinnedHistory,
        ] {
            model.sheet.as_mut().unwrap().page.as_mut().unwrap().entries[0] = entry(state);
            assert!(model.resume_command(&catalog, Uuid::from_u128(20)).is_err());
        }
    }

    #[test]
    fn failed_pager_request_restores_cursor_history_and_stale_dialog_result_is_discarded() {
        let catalog = catalog(Some(1));
        let mut model = Model::default();
        model.open(&catalog, Uuid::from_u128(20)).unwrap();
        let first_request = model.pending.as_ref().unwrap().id;
        model.sheet.as_mut().unwrap().page = Some(protocol::WorkspaceSessions {
            schema: 1,
            workspace_id: Uuid::from_u128(2),
            workspace_revision: 7,
            project_id: Uuid::from_u128(1),
            cwd: "/repo".into(),
            after: None,
            entries: vec![entry(WorkspaceResumeState::Unknown)],
            next_after: Some(Uuid::from_u128(10)),
        });
        model.pending = None;
        let request = model.next().unwrap();
        assert_eq!(model.sheet.as_ref().unwrap().previous_cursors.len(), 1);
        assert!(model.next().is_err());
        assert_eq!(model.sheet.as_ref().unwrap().previous_cursors.len(), 1);
        let id = request["_ui_request"].as_str().unwrap().parse().unwrap();
        assert!(!model.receive(
            Ok(ProductReply {
                ui_request: Some(Uuid::from_u128(99)),
                ..Default::default()
            }),
            Some(Uuid::from_u128(20)),
            Some(&catalog)
        ));
        assert!(model.matches_request(id));
        assert!(!model.receive(
            Ok(ProductReply {
                ui_request: Some(id),
                ..Default::default()
            }),
            Some(Uuid::from_u128(21)),
            Some(&catalog)
        ));
        assert!(model.sheet.as_ref().unwrap().previous_cursors.is_empty());
        assert!(!model.matches_request(first_request));
        assert!(model.sheet.as_ref().unwrap().previous_cursors.is_empty());
    }

    #[test]
    fn closed_dialog_reply_cannot_consume_replacement_dialog_request() {
        let catalog = catalog(Some(1));
        let mut model = Model::default();
        let old_request = model.open(&catalog, Uuid::from_u128(20)).unwrap();
        let old_id = old_request["_ui_request"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(model.close(Uuid::from_u128(20)));
        let new_request = model.open(&catalog, Uuid::from_u128(21)).unwrap();
        let new_id = new_request["_ui_request"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(!receive_page(
            &mut model,
            &catalog,
            Uuid::from_u128(21),
            json!({"_ui_request": old_id}),
            page(vec![entry(WorkspaceResumeState::CheckAvailable)])
        ));
        assert!(model.matches_request(new_id));
        assert!(!model.receive(
            Ok(ProductReply {
                ui_request: Some(old_id),
                ..Default::default()
            }),
            Some(Uuid::from_u128(21)),
            Some(&catalog)
        ));
        assert!(model.matches_request(new_id));
    }

    #[test]
    fn wrong_scope_reply_keeps_old_page_and_blocks_resume_until_refresh() {
        let catalog = catalog(Some(1));
        let dialog = Uuid::from_u128(20);
        let mut model = Model::default();
        let open = model.open(&catalog, dialog).unwrap();
        assert!(receive_page(
            &mut model,
            &catalog,
            dialog,
            open,
            page(vec![entry(WorkspaceResumeState::CheckAvailable)])
        ));
        for scope_error in 0..3 {
            let request = model.refresh().unwrap();
            let mut wrong_scope = page(vec![entry(WorkspaceResumeState::CheckAvailable)]);
            match scope_error {
                0 => wrong_scope.workspace_revision += 1,
                1 => wrong_scope.project_id = Uuid::from_u128(99),
                _ => wrong_scope.cwd = "/other".into(),
            }
            assert!(model.receive(
                Ok(ProductReply {
                    ui_request: Some(request["_ui_request"].as_str().unwrap().parse().unwrap()),
                    workspace_sessions: Some(wrong_scope),
                    ..Default::default()
                }),
                Some(dialog),
                Some(&catalog)
            ));
            assert_eq!(
                model.sheet.as_ref().unwrap().page.as_ref().unwrap().entries[0].session,
                Uuid::from_u128(10)
            );
            assert!(model.sheet.as_ref().unwrap().error.is_some());
            assert!(model.resume_command(&catalog, dialog).is_err());
            let refresh = model.refresh().unwrap();
            assert!(receive_page(
                &mut model,
                &catalog,
                dialog,
                refresh,
                page(vec![entry(WorkspaceResumeState::CheckAvailable)])
            ));
            assert!(model.sheet.as_ref().unwrap().error.is_none());
        }
    }

    #[test]
    fn refresh_keeps_selection_by_session_uuid() {
        let catalog = catalog(Some(1));
        let dialog = Uuid::from_u128(20);
        let mut model = Model::default();
        let open = model.open(&catalog, dialog).unwrap();
        let mut second = entry(WorkspaceResumeState::CheckAvailable);
        second.session = Uuid::from_u128(14);
        assert!(receive_page(
            &mut model,
            &catalog,
            dialog,
            open,
            page(vec![
                entry(WorkspaceResumeState::CheckAvailable),
                second.clone()
            ])
        ));
        assert_eq!(model.select(1), Some(Uuid::from_u128(14)));
        let refresh = model.refresh().unwrap();
        second.title = "Renamed after refresh".into();
        assert!(receive_page(
            &mut model,
            &catalog,
            dialog,
            refresh,
            page(vec![entry(WorkspaceResumeState::CheckAvailable), second])
        ));
        assert_eq!(model.selected_entry().unwrap().session, Uuid::from_u128(14));
        assert_eq!(
            model.selected_entry().unwrap().title,
            "Renamed after refresh"
        );
    }

    #[test]
    fn forward_paging_exceeds_history_window_and_first_abort_restores_exact_stack() {
        let catalog = catalog(Some(1));
        let dialog = Uuid::from_u128(20);
        let mut model = Model::default();
        let open = model.open(&catalog, dialog).unwrap();
        assert!(receive_page(
            &mut model,
            &catalog,
            dialog,
            open,
            page_at(None, 1_000)
        ));

        for page_index in 1..=40u128 {
            let request = model.next().unwrap();
            let cursor = Uuid::from_u128(1_000 + (page_index - 1) * 32 + 31);
            assert_eq!(request["after"], cursor.to_string());
            let history_len = model.sheet.as_ref().unwrap().previous_cursors.len();
            assert!(history_len <= MAX_CURSOR_HISTORY);
            assert!(model.next().is_err());
            assert_eq!(
                model.sheet.as_ref().unwrap().previous_cursors.len(),
                history_len
            );
            assert!(receive_page(
                &mut model,
                &catalog,
                dialog,
                request,
                page_at(Some(cursor), 1_000 + page_index * 32)
            ));
        }
        assert_eq!(
            model.sheet.as_ref().unwrap().previous_cursors.len(),
            MAX_CURSOR_HISTORY
        );

        let before_first = model.sheet.as_ref().unwrap().previous_cursors.clone();
        let first = model.first().unwrap();
        assert!(first["after"].is_null());
        let first_id = first["_ui_request"].as_str().unwrap().parse().unwrap();
        assert!(model.sheet.as_ref().unwrap().previous_cursors.is_empty());
        assert!(model.first().is_err());
        assert!(model.sheet.as_ref().unwrap().previous_cursors.is_empty());
        assert!(model.abort_unsent(first_id));
        assert_eq!(model.sheet.as_ref().unwrap().previous_cursors, before_first);

        let first = model.first().unwrap();
        assert!(model.sheet.as_ref().unwrap().previous_cursors.is_empty());
        assert!(receive_page(
            &mut model,
            &catalog,
            dialog,
            first,
            page_at(None, 1_000)
        ));
        assert!(model.sheet.as_ref().unwrap().previous_cursors.is_empty());
        assert_eq!(
            model.selected_entry().unwrap().session,
            Uuid::from_u128(1_000)
        );
    }
}
