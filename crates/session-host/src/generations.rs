//! A logical pane selects one generation; historical models never become live.
use anyhow::{Result, ensure};
use pigade_storage::{CatalogBatch, CatalogWrite, LaunchState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Host;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationInfo {
    pub generation: Uuid,
    pub ordinal: u64,
    pub state: String,
    pub has_frame: bool,
    pub sequence: Option<u64>,
    pub pid: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationList {
    pub session: Uuid,
    pub head: Option<Uuid>,
    pub pinned: Option<Uuid>,
    pub entries: Vec<GenerationInfo>,
    pub next_before: Option<u64>,
}

pub(crate) fn list(host: &mut Host, session: Uuid, before: Option<u64>) -> Result<GenerationList> {
    ensure!(!session.is_nil(), "invalid generation owner");
    host.reconcile_exits()?;
    let mut records = host.metadata.generation_records(session, before, 33)?;
    let more = records.len() > 32;
    records.truncate(32);
    let next_before = more.then(|| records.last().expect("nonempty page").ordinal);
    let mut entries: Vec<_> = records
        .into_iter()
        .map(|record| {
            let model = host.sessions.iter().find_map(|model| {
                let view = model.view.lock().expect("view lock");
                (view.session == session && view.generation == record.generation)
                    .then(|| (view.state.clone(), view.sequence, view.pid))
            });
            GenerationInfo {
                generation: record.generation,
                ordinal: record.ordinal,
                state: model
                    .as_ref()
                    .map(|m| m.0.clone())
                    .unwrap_or_else(|| match record.state {
                        LaunchState::Exited => "history_unavailable".into(),
                        _ => "launch_outcome_unknown".into(),
                    }),
                has_frame: model.is_some(),
                sequence: model.as_ref().map(|m| m.1),
                pid: model.and_then(|m| m.2),
            }
        })
        .collect();
    // Imported journals have no durable launch chronology. Offer explicit
    // selection, without guessing a default head from a UUID or filename.
    if entries.is_empty() && before.is_none() && !host.generation_heads.contains_key(&session) {
        for model in &host.sessions {
            let view = model.view.lock().expect("view lock");
            if view.session == session {
                entries.push(GenerationInfo {
                    generation: view.generation,
                    ordinal: 0,
                    state: view.state.clone(),
                    has_frame: true,
                    sequence: Some(view.sequence),
                    pid: view.pid,
                });
            }
        }
        ensure!(entries.len() <= 32, "too many imported generation models");
        entries.sort_by_key(|entry| entry.generation);
    }
    let selection = host.metadata.catalog()?.selection;
    Ok(GenerationList {
        session,
        head: host.generation_heads.get(&session).copied(),
        pinned: selection.window.panel_generations.get(&session).copied(),
        entries,
        next_before,
    })
}

pub(crate) fn select(
    host: &mut Host,
    session: Uuid,
    generation: Option<Uuid>,
) -> Result<pigade_storage::Selection> {
    ensure!(!session.is_nil(), "invalid generation owner");
    if let Some(generation) = generation {
        ensure!(
            host.sessions.iter().any(|model| {
                let view = model.view.lock().expect("view lock");
                view.session == session && view.generation == generation
            }),
            "Модель этого поколения недоступна; другое поколение не будет подставлено"
        );
    } else {
        ensure!(
            host.generation_heads.contains_key(&session),
            "Порядок поколений неизвестен; выберите конкретную историю"
        );
    }
    let mut selection = host.metadata.catalog()?.selection;
    let expected_revision = selection.revision;
    if let Some(generation) = generation {
        selection
            .window
            .panel_generations
            .insert(session, generation);
    } else {
        selection.window.panel_generations.remove(&session);
    }
    let saved = host.metadata.apply_catalog(CatalogBatch {
        selection: Some(CatalogWrite {
            value: selection,
            expected_revision: Some(expected_revision),
        }),
        ..Default::default()
    })?;
    Ok(saved.selection)
}
