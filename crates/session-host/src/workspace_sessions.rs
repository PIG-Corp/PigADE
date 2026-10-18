//! Explicit metadata overview of sessions placed in one ready workspace.
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use pigade_layout::{LayoutHistory, LayoutModel};
use pigade_storage::{LaunchState, WorkspaceStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Host, catalog, prepared};

const PAGE_SIZE: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSessions {
    pub schema: u16,
    pub workspace_id: Uuid,
    pub workspace_revision: u64,
    pub project_id: Uuid,
    pub cwd: PathBuf,
    pub after: Option<Uuid>,
    pub entries: Vec<WorkspaceSessionEntry>,
    pub next_after: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSessionEntry {
    pub session: Uuid,
    pub tab_id: Uuid,
    pub tab_name: String,
    pub selected_generation: Option<Uuid>,
    pub head: Option<Uuid>,
    pub title: String,
    pub provider: Option<String>,
    pub state: String,
    pub head_state: String,
    pub head_pid: Option<u32>,
    pub conversation: Option<WorkspaceConversation>,
    pub resume_state: WorkspaceResumeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConversation {
    pub provider: String,
    pub conversation_id: String,
    pub revision: u64,
    pub source_generation: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceResumeState {
    CheckAvailable,
    NeedsBinding,
    Running,
    Unknown,
    PinnedHistory,
    Unsupported,
    Unavailable,
}

pub(crate) fn list(
    host: &mut Host,
    workspace: Uuid,
    expected_revision: u64,
    after: Option<Uuid>,
) -> Result<WorkspaceSessions> {
    host.reconcile_exits()?;
    if let Some(cursor) = after {
        ensure!(!cursor.is_nil(), "invalid workspace session cursor");
    }
    let current = catalog::read_metadata(&host.metadata)?;
    let workspace_row = current
        .workspaces
        .iter()
        .find(|item| item.id == workspace)
        .context("workspace does not exist")?;
    ensure!(
        workspace_row.status == WorkspaceStatus::Ready,
        "session overview requires a Ready workspace"
    );
    ensure!(
        workspace_row.revision == expected_revision,
        "workspace changed; refresh session overview"
    );
    let project = current
        .projects
        .iter()
        .find(|item| item.id == workspace_row.project_id)
        .context("workspace project does not exist")?;
    let model =
        LayoutModel::from_history(serde_json::from_str::<LayoutHistory>(&current.layout.json)?)?;
    let tabs: std::collections::HashMap<_, _> = current
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == workspace)
        .map(|tab| (tab.id, tab))
        .collect();
    let labels: std::collections::HashMap<_, _> = current
        .pane_labels
        .as_ref()
        .into_iter()
        .flatten()
        .map(|label| (label.session, label.name.as_str()))
        .collect();

    // Traverse placements once; repeated global pane_tab scans are quadratic
    // for a large catalog, and unrelated workspaces need no per-pane DB reads.
    let mut placements = std::collections::BTreeMap::new();
    for (tab_id, layout) in &model.state().tabs {
        if tabs.contains_key(tab_id) {
            for session in layout.panes() {
                ensure!(
                    placements.insert(session, *tab_id).is_none(),
                    "duplicate workspace session placement"
                );
            }
        }
    }
    let mut live_by_generation = std::collections::HashMap::with_capacity(host.sessions.len());
    for session in &host.sessions {
        let view = session.view.lock().expect("session view lock");
        live_by_generation.insert((view.session, view.generation), session);
    }
    let mut entries = Vec::with_capacity(PAGE_SIZE + 1);
    let mut has_more = false;
    for (session, tab_id) in placements {
        if after.is_some_and(|cursor| session <= cursor) {
            continue;
        }

        let Some(tab) = tabs.get(&tab_id).copied() else {
            continue;
        };
        if entries.len() == PAGE_SIZE {
            has_more = true;
            break;
        }
        let head = host.generation_heads.get(&session).copied();
        let selected_generation = current
            .selection
            .window
            .panel_generations
            .get(&session)
            .copied()
            .or(head);
        let selected =
            generation_metadata(host, &live_by_generation, session, selected_generation)?;
        let head_meta = generation_metadata(host, &live_by_generation, session, head)?;
        let launch = head
            .map(|generation| host.metadata.panel_launch_metadata(session, generation))
            .transpose()?
            .flatten();
        let provider = launch.as_ref().and_then(|metadata| metadata.0.clone());
        let current_binding = host.metadata.conversation_binding(session)?;
        let conversation = current_binding
            .as_ref()
            .map(|binding| WorkspaceConversation {
                provider: binding.provider.clone(),
                conversation_id: binding.conversation_id.clone(),
                revision: binding.revision,
                source_generation: binding.source_generation,
            });
        let title = labels
            .get(&session)
            .filter(|name| !name.is_empty())
            .map(|name| (*name).to_owned())
            .or_else(|| {
                selected
                    .as_ref()
                    .and_then(|meta| meta.title.clone())
                    .filter(|s| !s.trim().is_empty())
            })
            .or_else(|| provider.clone())
            .unwrap_or_else(|| format!("Pane {}", &session.to_string()[..8]));
        let state = selected
            .as_ref()
            .map(|meta| meta.state.clone())
            .unwrap_or_else(|| "unavailable".into());
        let head_state = head_meta
            .as_ref()
            .map(|meta| {
                if meta.durable_state == Some(LaunchState::Exited) {
                    "exited".into()
                } else {
                    meta.state.clone()
                }
            })
            .unwrap_or_else(|| "unavailable".into());
        let head_pid = head_meta.as_ref().and_then(|meta| meta.pid);
        let resume_state = classify_resume(
            selected_generation,
            head,
            head_meta.as_ref(),
            launch.as_ref().and_then(|metadata| metadata.0.as_deref()),
            current_binding.as_ref(),
        );
        entries.push(WorkspaceSessionEntry {
            session,
            tab_id,
            tab_name: tab.name.clone(),
            selected_generation,
            head,
            title,
            provider,
            state,
            head_state,
            head_pid,
            conversation,
            resume_state,
        });
    }
    let next_after = has_more.then(|| entries.last().expect("next page is nonempty").session);
    Ok(WorkspaceSessions {
        schema: 1,
        workspace_id: workspace,
        workspace_revision: workspace_row.revision,
        project_id: project.id,
        cwd: workspace_row.cwd.clone(),
        after,
        entries,
        next_after,
    })
}

pub(crate) fn prepare_resume(
    host: &mut Host,
    workspace: Uuid,
    expected_revision: u64,
    session: Uuid,
    expected_head: Uuid,
    expected_binding_revision: u64,
) -> Result<crate::LaunchPreview> {
    crate::panel_launch::mark_preflight(validate_resume_scope(
        host,
        workspace,
        expected_revision,
        session,
        expected_head,
        expected_binding_revision,
    ))?;
    prepared::prepare_resume(host, session)
}

pub(crate) fn validate_resume_scope(
    host: &mut Host,
    workspace: Uuid,
    expected_revision: u64,
    session: Uuid,
    expected_head: Uuid,
    expected_binding_revision: u64,
) -> Result<()> {
    ensure!(
        !workspace.is_nil()
            && !session.is_nil()
            && !expected_head.is_nil()
            && expected_revision > 0
            && expected_binding_revision > 0,
        "invalid workspace resume identity"
    );
    host.reconcile_exits()?;
    let current = catalog::read_metadata(&host.metadata)?;
    let workspace_row = current
        .workspaces
        .iter()
        .find(|item| item.id == workspace)
        .context("workspace does not exist")?;
    ensure!(
        workspace_row.status == WorkspaceStatus::Ready,
        "resume requires a Ready workspace"
    );
    ensure!(
        workspace_row.revision == expected_revision,
        "workspace changed; refresh before continuing"
    );
    let model =
        LayoutModel::from_history(serde_json::from_str::<LayoutHistory>(&current.layout.json)?)?;
    let tab_id = model
        .state()
        .pane_tab(session)
        .context("session is no longer placed")?;
    ensure!(
        current
            .tabs
            .iter()
            .any(|tab| tab.id == tab_id && tab.workspace_id == workspace),
        "session is no longer placed in this workspace"
    );
    ensure!(
        host.generation_heads.get(&session).copied() == Some(expected_head),
        "session head changed; refresh before continuing"
    );
    let pinned = current
        .selection
        .window
        .panel_generations
        .get(&session)
        .copied();
    ensure!(
        pinned.is_none() || pinned == Some(expected_head),
        "selected generation is pinned to history"
    );
    let binding = host.metadata.conversation_binding(session)?;
    ensure!(
        binding.as_ref().map_or(0, |value| value.revision) == expected_binding_revision,
        "conversation binding changed; refresh before continuing"
    );
    Ok(())
}

struct GenerationMetadata {
    state: String,
    title: Option<String>,
    pid: Option<u32>,
    durable_state: Option<LaunchState>,
}

fn generation_metadata(
    host: &Host,
    live_by_generation: &std::collections::HashMap<(Uuid, Uuid), &crate::Session>,
    session: Uuid,
    generation: Option<Uuid>,
) -> Result<Option<GenerationMetadata>> {
    let Some(generation) = generation else {
        return Ok(None);
    };
    let durable_state = host
        .metadata
        .launch_state_for_generation(session, generation)?;
    if let Some(live) = live_by_generation.get(&(session, generation)) {
        let view = live.view.lock().expect("session view lock");
        return Ok(Some(GenerationMetadata {
            state: if durable_state
                .is_some_and(|state| !matches!(state, LaunchState::Exited | LaunchState::Running))
                || durable_state == Some(LaunchState::Running) && view.pid.is_none()
            {
                "launch_outcome_unknown".into()
            } else {
                view.state.clone()
            },
            title: Some(overview_title(&view.frame.title)),
            pid: matches!(view.state.as_str(), "running" | "degraded")
                .then_some(view.pid)
                .flatten()
                .filter(|pid| *pid > 0),
            durable_state,
        }));
    }
    Ok(durable_state.map(|state| GenerationMetadata {
        state: launch_state_name(state).to_owned(),
        title: None,
        pid: None,
        durable_state: Some(state),
    }))
}

fn overview_title(title: &str) -> String {
    const LIMIT: usize = 1024;
    if title.len() <= LIMIT {
        return title.to_owned();
    }
    let mut end = LIMIT - '…'.len_utf8();
    while !title.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &title[..end])
}

fn launch_state_name(state: LaunchState) -> &'static str {
    match state {
        LaunchState::Exited => "exited",
        LaunchState::Unknown
        | LaunchState::IntentCommitted
        | LaunchState::HelperRegistered
        | LaunchState::ExecPermitted => "launch_outcome_unknown",
        // Without a verified live process, the durable Running marker has an
        // unknown outcome and cannot certify that resume is safe.
        LaunchState::Running => "launch_outcome_unknown",
    }
}

fn classify_resume(
    selected: Option<Uuid>,
    head: Option<Uuid>,
    meta: Option<&GenerationMetadata>,
    provider: Option<&str>,
    binding: Option<&pigade_storage::ConversationBinding>,
) -> WorkspaceResumeState {
    if selected != head {
        return WorkspaceResumeState::PinnedHistory;
    }
    if meta.is_some_and(|value| {
        value.pid.is_some() && matches!(value.state.as_str(), "running" | "degraded")
    }) {
        return WorkspaceResumeState::Running;
    }
    let Some(meta) = meta else {
        return WorkspaceResumeState::Unavailable;
    };
    if meta.durable_state.is_none() {
        return WorkspaceResumeState::Unavailable;
    }
    if meta.durable_state != Some(LaunchState::Exited) {
        return WorkspaceResumeState::Unknown;
    }
    if !matches!(provider, Some("claude" | "codex" | "opencode")) {
        return if provider.is_some() {
            WorkspaceResumeState::Unsupported
        } else {
            WorkspaceResumeState::Unavailable
        };
    }
    let Some(binding) = binding else {
        return WorkspaceResumeState::NeedsBinding;
    };
    if binding.provider != provider.unwrap_or_default() {
        return WorkspaceResumeState::Unavailable;
    }
    if meta.durable_state == Some(LaunchState::Exited) {
        WorkspaceResumeState::CheckAvailable
    } else {
        WorkspaceResumeState::Unavailable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(state: &str, pid: Option<u32>) -> GenerationMetadata {
        GenerationMetadata {
            state: state.into(),
            title: None,
            pid,
            durable_state: match state {
                "exited" => Some(LaunchState::Exited),
                "running" | "degraded" => Some(LaunchState::Running),
                "history_unavailable" => None,
                _ => Some(LaunchState::Unknown),
            },
        }
    }

    #[test]
    fn resume_state_requires_live_pid_or_durable_exited_generation() {
        let head = Uuid::new_v4();
        let binding = pigade_storage::ConversationBinding {
            session_id: Uuid::new_v4(),
            source_generation: head,
            plan_id: Uuid::new_v4(),
            context_revision: Uuid::new_v4(),
            provider: "codex".into(),
            conversation_id: "thread_123".into(),
            revision: 1,
            bound_at_unix_ms: 1,
            author: "user".into(),
        };
        assert_eq!(
            classify_resume(
                Some(head),
                Some(head),
                Some(&meta("degraded", Some(42))),
                Some("codex"),
                Some(&binding)
            ),
            WorkspaceResumeState::Running
        );
        assert_eq!(
            classify_resume(
                Some(head),
                Some(head),
                Some(&meta("launch_outcome_unknown", None)),
                Some("codex"),
                Some(&binding)
            ),
            WorkspaceResumeState::Unknown
        );
        assert_eq!(
            classify_resume(
                Some(head),
                Some(head),
                Some(&meta("history_unavailable", None)),
                Some("codex"),
                Some(&binding)
            ),
            WorkspaceResumeState::Unavailable
        );
        assert_eq!(
            classify_resume(
                Some(head),
                Some(head),
                Some(&meta("exited", None)),
                Some("codex"),
                Some(&binding)
            ),
            WorkspaceResumeState::CheckAvailable
        );
    }

    #[test]
    fn overview_title_is_bounded_on_utf8_boundary_with_visible_clipping() {
        let title = "界".repeat(22_000);
        let display = overview_title(&title);
        assert!(display.len() <= 1024);
        assert!(display.ends_with('…'));
        assert!(title.starts_with(display.trim_end_matches('…')));
        assert_eq!(overview_title("Codex"), "Codex");
    }

    #[test]
    fn pinned_generation_and_unsupported_providers_fail_closed() {
        let head = Uuid::new_v4();
        let pinned = Uuid::new_v4();
        assert_eq!(
            classify_resume(
                Some(pinned),
                Some(head),
                Some(&meta("exited", None)),
                Some("codex"),
                None
            ),
            WorkspaceResumeState::PinnedHistory
        );
        assert_eq!(
            classify_resume(
                Some(head),
                Some(head),
                Some(&meta("exited", None)),
                Some("shell"),
                None
            ),
            WorkspaceResumeState::Unsupported
        );
        assert_eq!(
            classify_resume(
                Some(head),
                Some(head),
                Some(&meta("exited", None)),
                Some("codex"),
                None
            ),
            WorkspaceResumeState::NeedsBinding
        );
    }
}
