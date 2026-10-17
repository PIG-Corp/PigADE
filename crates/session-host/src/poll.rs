//! Bounded metadata polling for clients that already hold terminal frames.
//!
//! The host keeps a terminal frame private unless its complete visible
//! watermark has changed. This avoids serializing full cell grids on idle
//! polling while still surfacing lifecycle and process metadata changes.

use std::collections::HashSet;
use std::sync::Mutex;

use anyhow::{Result, ensure};
use pigade_terminal::Geometry;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::View;

pub const MAX_VIEWS: usize = 32;
pub const MAX_SELECTED: usize = 4;
const MAX_KNOWN_STATE_BYTES: usize = 128;
const MAX_KNOWN_ERROR_BYTES: usize = 4096;

/// A client watermark for one session's currently displayed view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Known {
    pub session: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub state: String,
    pub pid: Option<u32>,
    pub storage_error: Option<String>,
}

/// Small current-session metadata, independent of the terminal cell grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub state: String,
    pub pid: Option<u32>,
    pub storage_error: Option<String>,
    pub geometry: Geometry,
    pub title: String,
}

impl SessionInfo {
    pub fn known(&self) -> Known {
        Known {
            session: self.session,
            generation: self.generation,
            sequence: self.sequence,
            state: self.state.clone(),
            pid: self.pid,
            storage_error: self.storage_error.clone(),
        }
    }

    fn watermark_matches(&self, known: &Known) -> bool {
        self.session == known.session
            && self.generation == known.generation
            && self.sequence == known.sequence
            && self.state == known.state
            && self.pid == known.pid
            && self.storage_error == known.storage_error
    }
}

/// The selected sessions and only the views whose client watermarks are stale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollResult {
    pub selected: Vec<SessionInfo>,
    pub views: Vec<View>,
}

/// Poll up to 32 host views, selecting the four lowest `(session, generation)` pairs.
///
/// The first pass reads only metadata and sorts candidate references. The
/// second pass locks only selected views and clones a full `View` only when
/// its generation, sequence, state, PID, or storage error differs from the
/// matching client watermark.
pub fn poll<'a>(
    views: impl IntoIterator<Item = &'a Mutex<View>>,
    known: &[Known],
) -> Result<PollResult> {
    ensure!(
        known.len() <= MAX_SELECTED,
        "too many known session watermarks"
    );
    let mut known_by_session_generation = std::collections::HashMap::with_capacity(known.len());
    for watermark in known {
        ensure!(
            watermark.state.len() <= MAX_KNOWN_STATE_BYTES,
            "known session state is too large"
        );
        ensure!(
            watermark
                .storage_error
                .as_ref()
                .is_none_or(|error| error.len() <= MAX_KNOWN_ERROR_BYTES),
            "known storage error is too large"
        );
        ensure!(
            known_by_session_generation
                .insert((watermark.session, watermark.generation), watermark)
                .is_none(),
            "duplicate known session generation watermark"
        );
    }

    let mut candidates = Vec::with_capacity(MAX_VIEWS);
    let mut session_generations = HashSet::with_capacity(MAX_VIEWS);
    for view in views {
        ensure!(candidates.len() < MAX_VIEWS, "too many host views");
        let current = view
            .lock()
            .map_err(|_| anyhow::anyhow!("session view lock poisoned"))?;
        let key = (current.session, current.generation);
        drop(current);
        ensure!(
            session_generations.insert(key),
            "duplicate host session generation view"
        );
        candidates.push((key.0, key.1, view));
    }
    candidates.sort_unstable_by_key(|(session, generation, _)| (*session, *generation));

    let mut selected = Vec::with_capacity(candidates.len().min(MAX_SELECTED));
    let mut changed_views = Vec::with_capacity(MAX_SELECTED);
    for (_, _, view) in candidates.into_iter().take(MAX_SELECTED) {
        let current = view
            .lock()
            .map_err(|_| anyhow::anyhow!("session view lock poisoned"))?;
        let info = SessionInfo {
            session: current.session,
            generation: current.generation,
            sequence: current.sequence,
            state: current.state.clone(),
            pid: current.pid,
            storage_error: current.storage_error.clone(),
            geometry: current.frame.geometry,
            title: current.frame.title.clone(),
        };
        let unchanged = known_by_session_generation
            .get(&(info.session, info.generation))
            .is_some_and(|known| info.watermark_matches(known));
        if !unchanged {
            changed_views.push(current.clone());
        }
        selected.push(info);
    }

    Ok(PollResult {
        selected,
        views: changed_views,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use pigade_terminal::Terminal;

    use super::*;

    fn view(session: Uuid, generation: Uuid) -> Mutex<View> {
        Mutex::new(View {
            session,
            generation,
            sequence: 7,
            state: "running".into(),
            recovery_path: "live".into(),
            pid: Some(1234),
            storage_error: None,
            frame: Terminal::new(Geometry::new(8, 40).unwrap(), false).frame(),
            history: None,
            recovery_journal_bytes_read: 0,
            metrics: Default::default(),
            actor_diagnostic: None,
        })
    }

    fn watermark(view: &Mutex<View>) -> Known {
        let view = view.lock().unwrap();
        Known {
            session: view.session,
            generation: view.generation,
            sequence: view.sequence,
            state: view.state.clone(),
            pid: view.pid,
            storage_error: view.storage_error.clone(),
        }
    }

    #[test]
    fn unchanged_watermark_returns_metadata_without_cloning_view() {
        let session = Uuid::from_u128(10);
        let current = view(session, Uuid::from_u128(20));
        let result = poll([&current], &[watermark(&current)]).unwrap();
        assert_eq!(result.selected.len(), 1);
        assert!(result.views.is_empty());
        assert_eq!(result.selected[0].geometry, Geometry::new(8, 40).unwrap());
    }

    #[test]
    fn sequence_state_pid_error_and_generation_changes_return_a_view() {
        for mutate in [
            Box::new(|view: &mut View| view.sequence += 1) as Box<dyn Fn(&mut View)>,
            Box::new(|view: &mut View| view.state = "degraded".into()),
            Box::new(|view: &mut View| view.pid = None),
            Box::new(|view: &mut View| view.storage_error = Some("disk error".into())),
            Box::new(|view: &mut View| view.generation = Uuid::from_u128(21)),
        ] {
            let current = view(Uuid::from_u128(10), Uuid::from_u128(20));
            let known = watermark(&current);
            mutate(&mut current.lock().unwrap());
            let result = poll([&current], &[known]).unwrap();
            assert_eq!(result.views.len(), 1);
        }
    }

    #[test]
    fn selected_sessions_are_stable_and_limited_to_four_of_thirty_two() {
        let views = (1..=MAX_VIEWS as u128)
            .rev()
            .map(|id| Arc::new(view(Uuid::from_u128(id), Uuid::from_u128(100 + id))))
            .collect::<Vec<_>>();
        let result = poll(views.iter().map(AsRef::as_ref), &[]).unwrap();
        assert_eq!(result.selected.len(), MAX_SELECTED);
        assert_eq!(
            result
                .selected
                .iter()
                .map(|info| info.session.as_u128())
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(result.views.len(), MAX_SELECTED);
    }

    #[test]
    fn rejects_oversized_input_and_duplicate_watermarks() {
        let views = (1..=MAX_VIEWS + 1)
            .map(|id| view(Uuid::from_u128(id as u128), Uuid::from_u128(100)))
            .collect::<Vec<_>>();
        assert!(poll(views.iter(), &[]).is_err());

        let current = view(Uuid::from_u128(1), Uuid::from_u128(2));
        let known = watermark(&current);
        assert!(poll([&current], &[known.clone(), known]).is_err());
    }

    #[test]
    fn accepts_multiple_generations_for_one_session() {
        let session = Uuid::from_u128(1);
        let old_generation = Arc::new(view(session, Uuid::from_u128(10)));
        let new_generation = Arc::new(view(session, Uuid::from_u128(11)));
        let old_watermark = watermark(&old_generation);
        let result = poll(
            [old_generation.as_ref(), new_generation.as_ref()],
            &[old_watermark],
        )
        .unwrap();

        assert_eq!(result.selected.len(), 2);
        assert_eq!(result.views.len(), 1);
        assert_eq!(result.views[0].generation, Uuid::from_u128(11));
    }
}
