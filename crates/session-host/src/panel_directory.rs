//! Bounded search over currently placed logical panes.
use std::collections::HashMap;

use anyhow::{Result, ensure};
use pigade_storage::{LaunchState, WorkspaceStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Host, catalog};

const PAGE_SIZE: usize = 64;

fn localized_state_alias(state: &str) -> &'static str {
    match state {
        "running" => "работает живой",
        "exited" => "завершён",
        "history_unavailable" | "history" => "история",
        "degraded" => "ошибка",
        "launch_outcome_unknown" => "исход неизвестен",
        _ => "",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelDirectory {
    pub query: String,
    pub entries: Vec<PanelEntry>,
    pub next_after: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelEntry {
    pub session: Uuid,
    pub tab_id: Uuid,
    pub workspace_id: Uuid,
    pub project_id: Uuid,
    pub generation: Option<Uuid>,
    pub title: String,
    pub provider: Option<String>,
    pub role: String,
    pub deliver_context: bool,
    pub state: String,
}

pub(crate) fn search(
    host: &mut Host,
    query: String,
    after: Option<Uuid>,
) -> Result<PanelDirectory> {
    ensure!(query.len() <= 256, "panel search query exceeds 256 bytes");
    ensure!(
        query.chars().all(|ch| !ch.is_control()),
        "panel query contains control characters"
    );
    if let Some(cursor) = after {
        ensure!(!cursor.is_nil(), "invalid panel search cursor");
    }
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    ensure!(terms.len() <= 32, "panel search has too many terms");

    let current = catalog::read_metadata(&host.metadata)?;
    let model = catalog::load_layout(&current)?;
    // Build per-request indexes once. Catalog and live-session capacities are
    // bounded; repeated linear searches made the old path quadratic.
    let tab_by_id: HashMap<_, _> = current.tabs.iter().map(|tab| (tab.id, tab)).collect();
    let workspace_by_id: HashMap<_, _> = current
        .workspaces
        .iter()
        .map(|workspace| (workspace.id, workspace))
        .collect();
    let project_by_id: HashMap<_, _> = current
        .projects
        .iter()
        .map(|project| (project.id, project))
        .collect();
    let label_by_session: HashMap<_, _> = current
        .pane_labels
        .as_ref()
        .into_iter()
        .flatten()
        .map(|label| (label.session, label.name.as_str()))
        .collect();
    let mut live_by_generation = HashMap::with_capacity(host.sessions.len());
    let mut live_by_session = HashMap::with_capacity(host.sessions.len());
    for live in &host.sessions {
        let view = live.view.lock().expect("session view lock");
        let title = view.frame.title.clone();
        let state = view.state.clone();
        live_by_generation.insert(
            (view.session, view.generation),
            (title.clone(), state.clone()),
        );
        live_by_session
            .entry(view.session)
            .or_insert((view.generation, (title, state)));
    }
    // LayoutState::panes is a BTreeSet, so results naturally follow the stable
    // UUID cursor order. Skip prior pages before any per-pane lookup.
    let mut entries = Vec::with_capacity(PAGE_SIZE + 1);
    let mut has_more = false;
    for session in model.state().panes() {
        if after.is_some_and(|cursor| session <= cursor) {
            continue;
        }
        let Some(tab_id) = model.state().pane_tab(session) else {
            continue;
        };
        let Some(tab) = tab_by_id.get(&tab_id).copied() else {
            continue;
        };
        let Some(workspace) = workspace_by_id.get(&tab.workspace_id).copied() else {
            continue;
        };
        if workspace.status != WorkspaceStatus::Ready {
            continue;
        }
        let Some(project) = project_by_id.get(&workspace.project_id).copied() else {
            continue;
        };
        let generation = current
            .selection
            .window
            .panel_generations
            .get(&session)
            .copied()
            .or_else(|| host.generation_heads.get(&session).copied());
        let launch_metadata = generation
            .map(|generation| host.metadata.panel_launch_metadata(session, generation))
            .transpose()?
            .flatten();
        let provider = launch_metadata.as_ref().and_then(|m| m.0.clone());
        let role = launch_metadata
            .as_ref()
            .map(|m| m.1.clone())
            .unwrap_or_default();
        let deliver_context = launch_metadata.as_ref().is_some_and(|m| m.2);
        let view = generation
            .and_then(|generation| live_by_generation.get(&(session, generation)))
            .or_else(|| {
                generation
                    .is_none()
                    .then(|| live_by_session.get(&session).map(|(_, metadata)| metadata))
                    .flatten()
            });
        let persisted_state = if view.is_none() {
            generation
                .map(|generation| {
                    host.metadata
                        .launch_state_for_generation(session, generation)
                })
                .transpose()?
                .flatten()
                .map(|state| match state {
                    LaunchState::Exited => "history_unavailable",
                    LaunchState::Unknown
                    | LaunchState::IntentCommitted
                    | LaunchState::HelperRegistered
                    | LaunchState::ExecPermitted
                    | LaunchState::Running => "launch_outcome_unknown",
                })
        } else {
            None
        };
        let title = label_by_session
            .get(&session)
            .filter(|name| !name.is_empty())
            .map(|name| (*name).to_string())
            .or_else(|| {
                view.map(|(title, _)| title.trim().to_string())
                    .filter(|title| !title.is_empty())
            })
            .or_else(|| provider.clone())
            .unwrap_or_else(|| format!("Pane {}", &session.to_string()[..8]));
        let state = view
            .map(|(_, state)| state.clone())
            .unwrap_or_else(|| persisted_state.unwrap_or("history_unavailable").to_string());
        let fields = [
            title.as_str(),
            provider.as_deref().unwrap_or_default(),
            role.as_str(),
            state.as_str(),
            localized_state_alias(&state),
            project.name.as_str(),
            workspace.task.as_str(),
            tab.name.as_str(),
        ]
        .join("\n")
        .to_lowercase();
        if terms.iter().any(|term| !fields.contains(term)) {
            continue;
        }
        if entries.len() == PAGE_SIZE {
            has_more = true;
            break;
        }
        entries.push(PanelEntry {
            session,
            tab_id,
            workspace_id: workspace.id,
            project_id: project.id,
            generation,
            title,
            provider,
            role,
            deliver_context,
            state,
        });
    }
    let next_after = has_more.then(|| entries.last().expect("nonempty next page").session);
    Ok(PanelDirectory {
        query,
        entries,
        next_after,
    })
}
