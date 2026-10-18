//! Workspace registry navigation. Selection still belongs to the host catalog;
//! these local preferences remember only confirmed destinations and tree folds.
use crate::{
    WakeSignal,
    product::{Dialog, DialogKind},
    workspace_nav_store::{self as store, Config, Op, Visit},
};
use pigade_storage::{Catalog, WorkspaceStatus};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};
use uuid::Uuid;

pub(crate) const PAGE: usize = 8;
const MAX_PENDING: usize = 192;

#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub workspace: Uuid,
    pub project: Uuid,
    pub name: String,
    pub detail: String,
    pub status: WorkspaceStatus,
    pub recent: bool,
}

#[derive(Default)]
pub(crate) struct Model {
    pub config: Config,
    pub rows: Vec<Row>,
    pub selected: Option<Uuid>,
    pub offset: usize,
    pub error: Option<String>,
    pub saving: bool,
    query: Option<(Uuid, u64)>,
}

impl Model {
    pub fn invalidate(&mut self) {
        self.query = None;
    }
    pub fn refresh(&mut self, catalog: Option<&Catalog>, dialog: Option<&Dialog>) {
        let Some(dialog) = dialog.filter(|d| d.kind == DialogKind::WorkspaceSwitcher) else {
            return;
        };
        let revision = dialog.fields[0].revision();
        if self.query == Some((dialog.id, revision)) {
            return;
        }
        let changed_query = self
            .query
            .is_some_and(|(id, rev)| id == dialog.id && rev != revision);
        self.query = Some((dialog.id, revision));
        self.rows = catalog
            .map(|catalog| rows(catalog, &self.config, dialog.fields[0].text()))
            .unwrap_or_default();
        if changed_query
            || !self
                .rows
                .iter()
                .any(|row| Some(row.workspace) == self.selected)
        {
            self.selected = self.rows.first().map(|row| row.workspace);
        }
        self.reveal();
    }
    pub fn open(&mut self, catalog: Option<&Catalog>) {
        self.query = None;
        self.offset = 0;
        self.selected = catalog.and_then(|catalog| {
            self.config
                .recent
                .iter()
                .find(|visit| {
                    Some(visit.workspace) != catalog.selection.workspace_id
                        && catalog
                            .workspaces
                            .iter()
                            .any(|w| w.id == visit.workspace && w.status == WorkspaceStatus::Ready)
                })
                .map(|visit| visit.workspace)
        });
    }
    pub fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = None;
            return;
        }
        let index = self
            .rows
            .iter()
            .position(|row| Some(row.workspace) == self.selected)
            .unwrap_or(0);
        let index = index.saturating_add_signed(delta).min(self.rows.len() - 1);
        self.selected = Some(self.rows[index].workspace);
        self.reveal();
    }
    fn reveal(&mut self) {
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| Some(row.workspace) == self.selected)
        {
            self.offset = index / PAGE * PAGE;
        } else {
            self.offset = 0;
        }
    }
}

pub(crate) fn target_tab(catalog: &Catalog, config: &Config, workspace: Uuid) -> Option<Uuid> {
    let valid = |id| {
        catalog
            .tabs
            .iter()
            .any(|tab| tab.id == id && tab.workspace_id == workspace)
    };
    // Selecting the current workspace must never unexpectedly reset its tab.
    catalog
        .selection
        .tab_id
        .filter(|id| catalog.selection.workspace_id == Some(workspace) && valid(*id))
        .or_else(|| {
            config
                .recent
                .iter()
                .find(|visit| visit.workspace == workspace)
                .and_then(|visit| visit.tab)
                .filter(|id| valid(*id))
        })
        .or_else(|| {
            catalog
                .tabs
                .iter()
                .find(|tab| tab.workspace_id == workspace)
                .map(|tab| tab.id)
        })
}

pub(crate) fn confirmed_visit(catalog: &Catalog) -> Option<Visit> {
    let workspace = catalog.selection.workspace_id?;
    let value = catalog.workspaces.iter().find(|value| {
        value.id == workspace
            && value.status == WorkspaceStatus::Ready
            && Some(value.project_id) == catalog.selection.project_id
    })?;
    let tab = catalog.selection.tab_id.filter(|id| {
        catalog
            .tabs
            .iter()
            .any(|tab| tab.id == *id && tab.workspace_id == value.id)
    });
    Some(Visit { workspace, tab })
}

fn rows(catalog: &Catalog, config: &Config, query: &str) -> Vec<Row> {
    let projects: HashMap<_, _> = catalog.projects.iter().map(|p| (p.id, p)).collect();
    let recent: HashMap<_, _> = config
        .recent
        .iter()
        .enumerate()
        .map(|(i, visit)| (visit.workspace, i))
        .collect();
    let tabs: HashMap<_, _> = catalog.tabs.iter().map(|tab| (tab.id, tab)).collect();
    let mut first_tabs = HashMap::new();
    for tab in &catalog.tabs {
        first_tabs.entry(tab.workspace_id).or_insert(tab);
    }
    let recent_tabs: HashMap<_, _> = config
        .recent
        .iter()
        .map(|visit| (visit.workspace, visit.tab))
        .collect();
    let mut rows = Vec::new();
    for workspace in &catalog.workspaces {
        let Some(project) = projects.get(&workspace.project_id) else {
            continue;
        };
        let name = if workspace.task.trim().is_empty() {
            "Без названия".to_owned()
        } else {
            workspace.task.clone()
        };
        let last_tab = catalog
            .selection
            .tab_id
            .filter(|_| catalog.selection.workspace_id == Some(workspace.id))
            .or_else(|| recent_tabs.get(&workspace.id).copied().flatten())
            .and_then(|id| tabs.get(&id).copied())
            .filter(|tab| tab.workspace_id == workspace.id)
            .or_else(|| first_tabs.get(&workspace.id).copied());
        let detail = format!(
            "{} · {}{}",
            project.name,
            workspace.cwd.display(),
            last_tab
                .map(|tab| format!(" · {}", tab.name))
                .unwrap_or_default()
        );
        if !crate::navigation::search_matches(query, &format!("{name} {detail}")) {
            continue;
        }
        rows.push(Row {
            workspace: workspace.id,
            project: project.id,
            name,
            detail,
            status: workspace.status,
            recent: recent.contains_key(&workspace.id),
        });
    }
    rows.sort_by_cached_key(|row| {
        (
            recent.get(&row.workspace).copied().unwrap_or(usize::MAX),
            row.status != WorkspaceStatus::Ready,
            row.detail.to_lowercase(),
            row.name.to_lowercase(),
            row.workspace,
        )
    });
    rows
}

#[derive(Clone)]
enum Request {
    Apply(Op),
    Reload,
}

pub(crate) struct Writer {
    tx: mpsc::SyncSender<Request>,
    rx: mpsc::Receiver<Result<Config, String>>,
    confirmed: Config,
    inflight: Option<Request>,
    pending: VecDeque<Request>,
    blocked: bool,
    pub initial: Config,
    pub initial_error: Option<String>,
}

impl Writer {
    pub fn new(socket: &Path, wake: WakeSignal) -> Self {
        Self::for_path(store::path_for_socket(socket), wake)
    }
    fn for_path(path: Result<PathBuf, String>, wake: WakeSignal) -> Self {
        let loaded = path
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|path| store::load_from(path));
        let initial = loaded.clone().unwrap_or_default();
        let initial_error = loaded.err();
        let (tx, requests) = mpsc::sync_channel::<Request>(1);
        let (results, rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            while let Ok(request) = requests.recv() {
                let result = path
                    .as_ref()
                    .map_err(Clone::clone)
                    .and_then(|path| match request {
                        Request::Apply(op) => store::apply_to(path, &op),
                        Request::Reload => store::load_from(path),
                    });
                if results.send(result).is_err() {
                    return;
                }
                wake.notify();
            }
        });
        Self {
            tx,
            rx,
            confirmed: initial.clone(),
            inflight: None,
            pending: VecDeque::new(),
            blocked: initial_error.is_some(),
            initial,
            initial_error,
        }
    }
    pub fn queue(&mut self, op: Op) -> Result<(), String> {
        if self.pending.len() >= MAX_PENDING {
            return Err("Очередь сохранения навигации занята".into());
        }
        // Repeated focus/selection ACKs do not produce filesystem writes.
        if !self.effective().apply(&op) {
            return Ok(());
        }
        self.pending
            .retain(|request| !matches!(request,Request::Apply(old) if same_key(old,&op)));
        self.pending.push_back(Request::Apply(op));
        Ok(())
    }
    pub fn refresh(&mut self) {
        self.blocked = false;
        if !self
            .pending
            .iter()
            .any(|request| matches!(request, Request::Reload))
        {
            self.pending.push_front(Request::Reload);
        }
    }
    pub fn pending(&self) -> bool {
        self.inflight.is_some() || !self.pending.is_empty()
    }
    pub fn effective(&self) -> Config {
        let mut value = self.confirmed.clone();
        for request in self.inflight.iter().chain(self.pending.iter()) {
            if let Request::Apply(op) = request {
                value.apply(op);
            }
        }
        value
    }
    pub fn service(&mut self) -> Option<Result<Config, String>> {
        let mut outcome = None;
        if let Ok(result) = self.rx.try_recv() {
            let completed = self.inflight.take();
            match result {
                Ok(value) => {
                    self.confirmed = value;
                    outcome = Some(Ok(self.effective()));
                }
                Err(error) => {
                    if let Some(request) = completed {
                        self.pending.push_front(request);
                    }
                    self.blocked = true;
                    outcome = Some(Err(error));
                }
            }
        }
        if !self.blocked
            && self.inflight.is_none()
            && let Some(request) = self.pending.pop_front()
        {
            match self.tx.try_send(request.clone()) {
                Ok(()) => self.inflight = Some(request),
                Err(_) => {
                    self.pending.push_front(request);
                    self.blocked = true;
                    outcome = Some(Err("Сохранение навигации недоступно".into()));
                }
            }
        }
        outcome
    }
}

fn same_key(a: &Op, b: &Op) -> bool {
    match (a, b) {
        (Op::Visit(a), Op::Visit(b)) => a.workspace == b.workspace,
        (Op::SetCollapsed { project: a, .. }, Op::SetCollapsed { project: b, .. }) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_storage::{Project, Tab, Workspace, WorkspaceMode};
    use serde_json::json;

    fn catalog() -> Catalog {
        let mut catalog: Catalog = serde_json::from_value(json!({"projects":[],"workspaces":[],"tabs":[],
            "selection":{"project_id":null,"workspace_id":null,"tab_id":null,"window":pigade_storage::WindowState::default(),"revision":1},
            "layout":{"json":"null","revision":1}})).unwrap();
        catalog.projects.push(Project {
            id: Uuid::from_u128(1),
            canonical_path: "/alpha".into(),
            git_common_dir: "/alpha/.git".into(),
            name: "Alpha".into(),
            roots: vec![],
            revision: 1,
        });
        for n in 2..=3 {
            let id = Uuid::from_u128(n);
            catalog.workspaces.push(Workspace {
                id,
                project_id: Uuid::from_u128(1),
                task: format!("Task{n}"),
                cwd: format!("/alpha/task{n}").into(),
                mode: WorkspaceMode::SharedCheckout,
                status: WorkspaceStatus::Ready,
                error: None,
                context_revision: None,
                revision: 1,
            });
            for offset in 0..2 {
                catalog.tabs.push(Tab {
                    id: Uuid::from_u128(n * 10 + offset),
                    workspace_id: id,
                    name: format!("Tab{offset}"),
                    layout_json: "null".into(),
                    active_pane: None,
                    revision: 1,
                });
            }
        }
        catalog
    }

    #[test]
    fn workspace_return_remembers_last_tab_and_never_selects_foreign_or_deleted_tab() {
        let mut catalog = catalog();
        let mut config = Config::default();
        let workspace = Uuid::from_u128(2);
        config.apply(&Op::Visit(Visit {
            workspace,
            tab: Some(Uuid::from_u128(21)),
        }));
        assert_eq!(
            target_tab(&catalog, &config, workspace),
            Some(Uuid::from_u128(21))
        );
        catalog.selection.workspace_id = Some(workspace);
        catalog.selection.tab_id = Some(Uuid::from_u128(20));
        assert_eq!(
            target_tab(&catalog, &config, workspace),
            Some(Uuid::from_u128(20))
        );
        catalog.selection.workspace_id = Some(Uuid::from_u128(3));
        config.recent[0].tab = Some(Uuid::from_u128(31));
        assert_eq!(
            target_tab(&catalog, &config, workspace),
            Some(Uuid::from_u128(20))
        );
        config.recent[0].tab = Some(Uuid::from_u128(999));
        assert_eq!(
            target_tab(&catalog, &config, workspace),
            Some(Uuid::from_u128(20))
        );
    }

    #[test]
    fn only_valid_confirmed_catalog_selection_is_recorded() {
        let mut catalog = catalog();
        assert!(confirmed_visit(&catalog).is_none());
        catalog.selection.project_id = Some(Uuid::from_u128(1));
        catalog.selection.workspace_id = Some(Uuid::from_u128(2));
        catalog.selection.tab_id = Some(Uuid::from_u128(21));
        assert_eq!(
            confirmed_visit(&catalog).unwrap().tab,
            Some(Uuid::from_u128(21))
        );
        catalog.workspaces[0].status = WorkspaceStatus::Archived;
        assert!(confirmed_visit(&catalog).is_none());
    }

    #[test]
    fn search_recent_order_and_selection_survive_reordering_with_stable_ids() {
        let catalog = catalog();
        let mut model = Model::default();
        model.config.apply(&Op::Visit(Visit {
            workspace: Uuid::from_u128(3),
            tab: Some(Uuid::from_u128(31)),
        }));
        let mut dialog = Dialog::new(DialogKind::WorkspaceSwitcher);
        model.open(Some(&catalog));
        model.refresh(Some(&catalog), Some(&dialog));
        assert_eq!(model.rows[0].workspace, Uuid::from_u128(3));
        model.selected = Some(Uuid::from_u128(2));
        model.config.apply(&Op::Visit(Visit {
            workspace: Uuid::from_u128(2),
            tab: None,
        }));
        model.invalidate();
        model.refresh(Some(&catalog), Some(&dialog));
        assert_eq!(model.selected, Some(Uuid::from_u128(2)));
        dialog.fields[0]
            .replace(None, "alpha task3", false)
            .unwrap();
        model.refresh(Some(&catalog), Some(&dialog));
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.selected, Some(Uuid::from_u128(3)));
        model.move_by(-1);
        assert_eq!(model.offset, 0);
        model.move_by(99);
        assert_eq!(model.offset, 0);
    }
}
