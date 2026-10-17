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
use crate::changes::{ChangeHub, Token};

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

/// One logical panel to poll. A generation pins the panel to an exact view;
/// `None` follows the host's authoritative head for the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelTarget {
    pub session: Uuid,
    pub generation: Option<Uuid>,
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
    pub change_token: Option<Token>,
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
    changes: &ChangeHub,
) -> Result<PollResult> {
    poll_selected(views, known, changes, None)
}

/// Poll the active tab's stable panel identities; hidden PTYs remain untouched.
pub fn poll_visible<'a>(
    views: impl IntoIterator<Item = &'a Mutex<View>>,
    known: &[Known],
    changes: &ChangeHub,
    sessions: &[Uuid],
) -> Result<PollResult> {
    ensure!(sessions.len() <= MAX_VIEWS, "too many visible panels");
    let unique: HashSet<_> = sessions.iter().collect();
    ensure!(unique.len() == sessions.len(), "duplicate visible panel");
    poll_selected(views, known, changes, Some(sessions))
}

/// Poll stable panel identities in caller order, resolving unpinned panels
/// only through the supplied authoritative session-head map.
///
/// Missing heads and heads whose exact view is unavailable produce no entry;
/// this function never falls back to another generation of that session.
pub fn poll_panels<'a>(
    views: impl IntoIterator<Item = &'a Mutex<View>>,
    known: &[Known],
    changes: &ChangeHub,
    panels: &[PanelTarget],
    heads: &std::collections::BTreeMap<Uuid, Uuid>,
) -> Result<PollResult> {
    let change_token = sample_change_token(changes);
    ensure!(panels.len() <= MAX_VIEWS, "too many panel targets");
    let mut panel_sessions = HashSet::with_capacity(panels.len());
    for panel in panels {
        ensure!(!panel.session.is_nil(), "nil panel session");
        ensure!(
            panel
                .generation
                .is_none_or(|generation| !generation.is_nil()),
            "nil panel generation"
        );
        ensure!(
            panel_sessions.insert(panel.session),
            "duplicate panel target"
        );
    }
    ensure!(
        known.len() <= MAX_VIEWS,
        "too many known session watermarks"
    );
    let known_by_session_generation = validate_known(known)?;

    let mut by_pair = std::collections::HashMap::with_capacity(MAX_VIEWS);
    for view in views {
        ensure!(by_pair.len() < MAX_VIEWS, "too many host views");
        let current = view
            .lock()
            .map_err(|_| anyhow::anyhow!("session view lock poisoned"))?;
        let key = (current.session, current.generation);
        drop(current);
        ensure!(
            by_pair.insert(key, view).is_none(),
            "duplicate host session generation view"
        );
    }

    let mut selected = Vec::with_capacity(panels.len());
    let mut changed_views = Vec::with_capacity(panels.len());
    for panel in panels {
        let generation = panel
            .generation
            .or_else(|| heads.get(&panel.session).copied());
        let Some(generation) = generation else {
            continue;
        };
        let Some(view) = by_pair.get(&(panel.session, generation)) else {
            continue;
        };
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
        change_token,
    })
}

fn sample_change_token(changes: &ChangeHub) -> Option<Token> {
    match changes.token() {
        Ok(token) => Some(token),
        Err(error) => {
            changes.report_failure_once(|| {
                eprintln!("host change token unavailable; Poll remains active: {error:#}");
            });
            None
        }
    }
}

fn validate_known(known: &[Known]) -> Result<std::collections::HashMap<(Uuid, Uuid), &Known>> {
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
    Ok(known_by_session_generation)
}

fn poll_selected<'a>(
    views: impl IntoIterator<Item = &'a Mutex<View>>,
    known: &[Known],
    changes: &ChangeHub,
    sessions: Option<&[Uuid]>,
) -> Result<PollResult> {
    // Sample before taking any view locks. If publication races this walk, the
    // returned epoch is stale and the next wait immediately observes the bump.
    let change_token = sample_change_token(changes);
    ensure!(
        known.len()
            <= if sessions.is_some() {
                MAX_VIEWS
            } else {
                MAX_SELECTED
            },
        "too many known session watermarks"
    );
    let known_by_session_generation = validate_known(known)?;

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
    if let Some(sessions) = sessions {
        candidates.retain(|(session, _, _)| sessions.contains(session));
        candidates.sort_by_key(|(session, _, _)| sessions.iter().position(|id| id == session));
    }

    let limit = if sessions.is_some() {
        MAX_VIEWS
    } else {
        MAX_SELECTED
    };
    let mut selected = Vec::with_capacity(candidates.len().min(limit));
    let mut changed_views = Vec::with_capacity(limit);
    for (_, _, view) in candidates.into_iter().take(limit) {
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
        change_token,
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
        let result = poll([&current], &[watermark(&current)], &ChangeHub::new()).unwrap();
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
            let result = poll([&current], &[known], &ChangeHub::new()).unwrap();
            assert_eq!(result.views.len(), 1);
        }
    }

    #[test]
    fn selected_sessions_are_stable_and_limited_to_four_of_thirty_two() {
        let views = (1..=MAX_VIEWS as u128)
            .rev()
            .map(|id| Arc::new(view(Uuid::from_u128(id), Uuid::from_u128(100 + id))))
            .collect::<Vec<_>>();
        let result = poll(views.iter().map(AsRef::as_ref), &[], &ChangeHub::new()).unwrap();
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
        assert!(poll(views.iter(), &[], &ChangeHub::new()).is_err());

        let current = view(Uuid::from_u128(1), Uuid::from_u128(2));
        let known = watermark(&current);
        assert!(poll([&current], &[known.clone(), known], &ChangeHub::new()).is_err());
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
            &ChangeHub::new(),
        )
        .unwrap();

        assert_eq!(result.selected.len(), 2);
        assert_eq!(result.views.len(), 1);
        assert_eq!(result.views[0].generation, Uuid::from_u128(11));
    }

    #[test]
    fn poll_epoch_is_captured_before_view_iteration_to_prevent_lost_wake() {
        let current = view(Uuid::from_u128(1), Uuid::from_u128(2));
        let changes = ChangeHub::new();
        let mut yielded = false;
        let poll_views = std::iter::from_fn(|| {
            if yielded {
                None
            } else {
                yielded = true;
                changes.notify().unwrap();
                Some(&current)
            }
        });

        let result = poll(poll_views, &[], &changes).unwrap();
        let token = result.change_token.unwrap();
        assert_eq!(token.revision, 0);
        let wake = changes
            .wait(token, std::time::Duration::from_millis(100))
            .unwrap();
        assert!(wake.changed);
        assert_eq!(wake.token.revision, 1);
    }

    #[test]
    fn change_wait_then_conditional_poll_catches_up_the_latest_complete_frame() {
        let current = view(Uuid::from_u128(31), Uuid::from_u128(32));
        let changes = ChangeHub::new();
        let first = poll([&current], &[], &changes).unwrap();
        let known = first.selected[0].known();

        {
            let mut view = current.lock().unwrap();
            view.sequence += 1;
            let mut terminal = Terminal::new(Geometry::new(8, 40).unwrap(), false);
            terminal.output(b"fresh frame");
            view.frame = terminal.frame();
        }
        let committed = changes.notify().unwrap();
        let wake = changes
            .wait(
                first.change_token.unwrap(),
                std::time::Duration::from_millis(100),
            )
            .unwrap();
        assert!(wake.changed);
        assert_eq!(wake.token, committed);

        let latest = poll([&current], &[known], &changes).unwrap();
        assert_eq!(latest.views.len(), 1);
        assert_eq!(latest.views[0].sequence, 8);
        assert!(latest.views[0].frame.text().contains("fresh frame"));
        assert_eq!(latest.selected[0].sequence, 8);
    }

    #[test]
    fn unavailable_notification_token_keeps_conditional_poll_usable() {
        let changes = ChangeHub::new();
        changes.close_for_test();
        let current = view(Uuid::from_u128(41), Uuid::from_u128(42));
        let result = poll([&current], &[], &changes).unwrap();
        assert!(result.change_token.is_none());
        assert_eq!(result.views.len(), 1);
        assert_eq!(result.views[0].sequence, 7);
    }

    #[test]
    fn panel_poll_follows_authoritative_head_and_pins_explicit_generation() {
        let session = Uuid::from_u128(1);
        let old = Arc::new(view(session, Uuid::from_u128(90)));
        let new = Arc::new(view(session, Uuid::from_u128(2)));
        let old_generation = old.lock().unwrap().generation;
        let mut stale_old = watermark(&old);
        stale_old.sequence = u64::MAX;
        let following_panel = [PanelTarget {
            session,
            generation: None,
        }];
        let heads = std::collections::BTreeMap::from([(session, Uuid::from_u128(2))]);
        let result = poll_panels(
            [new.as_ref(), old.as_ref()],
            &[stale_old],
            &ChangeHub::new(),
            &following_panel,
            &heads,
        )
        .unwrap();

        assert_eq!(result.selected.len(), 1);
        assert_eq!(result.selected[0].generation, Uuid::from_u128(2));
        assert_eq!(result.views.len(), 1);
        assert_eq!(result.views[0].generation, Uuid::from_u128(2));

        let pinned = poll_panels(
            [new.as_ref(), old.as_ref()],
            &[],
            &ChangeHub::new(),
            &[PanelTarget {
                session,
                generation: Some(old_generation),
            }],
            &heads,
        )
        .unwrap();
        assert_eq!(pinned.selected.len(), 1);
        assert_eq!(pinned.selected[0].generation, Uuid::from_u128(90));
        assert_eq!(pinned.views[0].generation, Uuid::from_u128(90));
    }

    #[test]
    fn panel_order_is_preserved_and_unavailable_head_never_falls_back() {
        let first = Uuid::from_u128(100);
        let second = Uuid::from_u128(1);
        let first_view = view(first, Uuid::from_u128(10));
        let second_old = view(second, Uuid::from_u128(20));
        let panels = [
            PanelTarget {
                session: first,
                generation: None,
            },
            PanelTarget {
                session: second,
                generation: None,
            },
        ];
        let heads = std::collections::BTreeMap::from([
            (first, Uuid::from_u128(10)),
            (second, Uuid::from_u128(21)),
        ]);
        let result = poll_panels(
            [&second_old, &first_view],
            &[],
            &ChangeHub::new(),
            &panels,
            &heads,
        )
        .unwrap();
        assert_eq!(result.selected.len(), 1);
        assert_eq!(result.selected[0].session, first);
        assert_eq!(result.views.len(), 1);
        assert_eq!(result.views[0].session, first);
    }

    #[test]
    fn panel_poll_rejects_nil_duplicate_targets_and_excess_panels() {
        let changes = ChangeHub::new();
        let empty = std::collections::BTreeMap::new();
        assert!(
            poll_panels(
                [],
                &[],
                &changes,
                &[PanelTarget {
                    session: Uuid::nil(),
                    generation: None,
                }],
                &empty
            )
            .is_err()
        );
        let duplicate = PanelTarget {
            session: Uuid::from_u128(1),
            generation: None,
        };
        assert!(poll_panels([], &[], &changes, &[duplicate.clone(), duplicate], &empty).is_err());
        let too_many = (1..=MAX_VIEWS + 1)
            .map(|id| PanelTarget {
                session: Uuid::from_u128(id as u128),
                generation: None,
            })
            .collect::<Vec<_>>();
        assert!(poll_panels([], &[], &changes, &too_many, &empty).is_err());
    }

    #[test]
    fn panel_poll_rejects_duplicate_host_pairs_and_ignores_missing_heads() {
        let session = Uuid::from_u128(7);
        let generation = Uuid::from_u128(8);
        let duplicate_a = view(session, generation);
        let duplicate_b = view(session, generation);
        assert!(
            poll_panels(
                [&duplicate_a, &duplicate_b],
                &[],
                &ChangeHub::new(),
                &[PanelTarget {
                    session,
                    generation: None,
                }],
                &std::collections::BTreeMap::from([(session, generation)]),
            )
            .is_err()
        );

        let absent = poll_panels(
            [&duplicate_a],
            &[],
            &ChangeHub::new(),
            &[PanelTarget {
                session,
                generation: None,
            }],
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        assert!(absent.selected.is_empty());
        assert!(absent.views.is_empty());
    }

    #[test]
    fn panel_poll_samples_change_token_before_view_scan() {
        let session = Uuid::from_u128(8);
        let current = view(session, Uuid::from_u128(9));
        let changes = ChangeHub::new();
        let panels = [PanelTarget {
            session,
            generation: None,
        }];
        let heads = std::collections::BTreeMap::from([(session, Uuid::from_u128(9))]);
        let mut yielded = false;
        let poll_views = std::iter::from_fn(|| {
            if yielded {
                None
            } else {
                yielded = true;
                changes.notify().unwrap();
                Some(&current)
            }
        });

        let result = poll_panels(poll_views, &[], &changes, &panels, &heads).unwrap();
        let token = result.change_token.unwrap();
        assert_eq!(token.revision, 0);
        let wake = changes
            .wait(token, std::time::Duration::from_millis(100))
            .unwrap();
        assert!(wake.changed);
        assert_eq!(wake.token.revision, 1);
    }
}
