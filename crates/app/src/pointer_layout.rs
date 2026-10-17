//! Pure layout helpers for pointer-driven pane movement and focus persistence.

use std::collections::BTreeMap;

use pigade_layout::{Command, Direction, LayoutModel, LayoutState, PaneRect, Rect, TabLayout};
use uuid::Uuid;

/// Return a temporary rectangle centered on the pointer while a tiled pane is dragged.
pub(crate) fn floating_rect(source: Rect, pointer_x: f64, pointer_y: f64) -> Option<Rect> {
    if !valid_rect(source) || !pointer_x.is_finite() || !pointer_y.is_finite() {
        return None;
    }
    let width = source.width * 0.85;
    let height = source.height * 0.85;
    let rect = Rect {
        x: pointer_x - width / 2.0,
        y: pointer_y - height / 2.0,
        width,
        height,
    };
    valid_rect(rect).then_some(rect)
}

/// Build a Dwindle-like sibling insertion command for a pane drop.
///
/// The target's aspect ratio determines the split axis; the pointer half chooses
/// the new pane's side. A drop never means swap.
pub(crate) fn drop_command(
    pane: Uuid,
    split: Uuid,
    target: PaneRect,
    x: f64,
    y: f64,
) -> Option<Command> {
    let rect = target.rect;
    if pane == target.pane || !valid_rect(rect) || !x.is_finite() || !y.is_finite() {
        return None;
    }
    if x < rect.x || x > rect.x + rect.width || y < rect.y || y > rect.y + rect.height {
        return None;
    }
    let side = if rect.width > rect.height {
        if x < rect.x + rect.width / 2.0 {
            Direction::Left
        } else {
            Direction::Right
        }
    } else if y < rect.y + rect.height / 2.0 {
        Direction::Up
    } else {
        Direction::Down
    };
    Some(Command::Move {
        pane,
        target: target.pane,
        side,
        split,
    })
}

/// Preview removing a pane for the floating portion of a drag.
///
/// A lone pane has no tiled remainder, so this returns `None` and callers should
/// keep the original layout visible until a destination is selected.
pub(crate) fn detached_layout(base: &TabLayout, pane: Uuid) -> Option<TabLayout> {
    if !base.panes().contains(&pane) || base.panes().len() <= 1 {
        return None;
    }
    let tab_id = Uuid::nil();
    let state = LayoutState {
        tabs: BTreeMap::from([(tab_id, base.clone())]),
    };
    let model = LayoutModel::new(state).ok()?;
    let mut preview = model
        .preview(Command::ClosePane { pane })
        .ok()?
        .tabs
        .remove(&tab_id)?;
    preview.focus_mode = false;
    Some(preview)
}

fn valid_rect(rect: Rect) -> bool {
    [
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        rect.x + rect.width,
        rect.y + rect.height,
    ]
    .into_iter()
    .all(f64::is_finite)
        && rect.width > 0.0
        && rect.height > 0.0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FocusIntent {
    pub tab: Uuid,
    pub pane: Uuid,
}

/// Coalesces hover focus updates to one in-flight durable write plus latest intent.
#[derive(Debug, Default)]
pub(crate) struct FocusQueue {
    pending: Option<FocusIntent>,
    inflight: Option<(Uuid, FocusIntent)>,
}

impl FocusQueue {
    pub(crate) fn choose(&mut self, intent: FocusIntent) {
        if self.inflight.is_some_and(|(_, current)| current == intent) {
            self.pending = None;
        } else if self.pending != Some(intent) {
            self.pending = Some(intent);
        }
    }

    pub(crate) fn pending(&self) -> Option<FocusIntent> {
        self.pending
    }

    /// Start the latest queued intent only when no prior focus write is outstanding.
    pub(crate) fn start(&mut self, request: Uuid) -> Option<FocusIntent> {
        if self.inflight.is_some() {
            return None;
        }
        let intent = self.pending.take()?;
        self.inflight = Some((request, intent));
        Some(intent)
    }

    pub(crate) fn matches(&self, request: Option<Uuid>) -> bool {
        request.is_some()
            && self
                .inflight
                .is_some_and(|(active, _)| Some(active) == request)
    }

    /// Acknowledge only the matching request; retain any newer queued intent.
    pub(crate) fn complete(&mut self, request: Uuid, saved: Option<FocusIntent>) -> bool {
        if !self.matches(Some(request)) {
            return false;
        }
        self.inflight = None;
        if self.pending == saved {
            self.pending = None;
        }
        true
    }

    /// Cancel queued hover changes while keeping the in-flight request routable.
    pub(crate) fn cancel(&mut self) {
        self.pending = None;
    }

    pub(crate) fn is_saving(&self) -> bool {
        self.inflight.is_some()
    }
}

#[cfg(test)]
mod tests {
    use pigade_layout::{Axis, Command, Direction, Node, PaneRect, Rect, SplitId, TabLayout};
    use uuid::Uuid;

    use super::{FocusIntent, FocusQueue, detached_layout, drop_command, floating_rect};

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn two_panes(focus_mode: bool) -> TabLayout {
        TabLayout {
            root: Some(Node::Split {
                id: id(3),
                axis: Axis::Horizontal,
                weights: vec![0.5, 0.5],
                children: vec![Node::pane(id(1)), Node::pane(id(2))],
            }),
            focused: Some(id(1)),
            focus_mode,
        }
    }

    #[test]
    fn floating_geometry_is_centered_and_eighty_five_percent() {
        let rect = floating_rect(
            Rect {
                x: 10.0,
                y: 20.0,
                width: 200.0,
                height: 100.0,
            },
            300.0,
            250.0,
        )
        .unwrap();
        assert_eq!(rect.width, 170.0);
        assert_eq!(rect.height, 85.0);
        assert_eq!(
            (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0),
            (300.0, 250.0)
        );
        assert!(floating_rect(Rect { width: 0.0, ..rect }, 1.0, 1.0).is_none());
        assert!(floating_rect(rect, f64::NAN, 1.0).is_none());
    }

    #[test]
    fn drop_sides_follow_target_aspect_and_pointer_half_without_swapping() {
        let wide = PaneRect {
            pane: id(2),
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0,
            },
        };
        let tall = PaneRect {
            pane: id(2),
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 200.0,
            },
        };
        let command = |target, x, y| drop_command(id(1), id(4), target, x, y).unwrap();
        assert!(matches!(
            command(wide, 1.0, 50.0),
            Command::Move {
                side: Direction::Left,
                ..
            }
        ));
        assert!(matches!(
            command(wide, 199.0, 50.0),
            Command::Move {
                side: Direction::Right,
                ..
            }
        ));
        assert!(matches!(
            command(wide, 100.0, 50.0),
            Command::Move {
                side: Direction::Right,
                ..
            }
        ));
        assert!(matches!(
            command(tall, 50.0, 1.0),
            Command::Move {
                side: Direction::Up,
                ..
            }
        ));
        assert!(matches!(
            command(tall, 50.0, 199.0),
            Command::Move {
                side: Direction::Down,
                ..
            }
        ));
        assert!(matches!(
            command(tall, 50.0, 100.0),
            Command::Move {
                side: Direction::Down,
                ..
            }
        ));
        assert!(drop_command(id(1), id(4), wide, 200.1, 50.0).is_none());
        assert!(drop_command(id(1), id(4), wide, 50.0, f64::INFINITY).is_none());
        assert!(drop_command(id(2), id(4), wide, 50.0, 50.0).is_none());
    }

    #[test]
    fn detach_preview_preserves_original_and_other_pane_identity() {
        let original = two_panes(true);
        let preview = detached_layout(&original, id(1)).unwrap();
        assert_eq!(original.panes(), vec![id(1), id(2)]);
        assert_eq!(preview.panes(), vec![id(2)]);
        assert_eq!(preview.focused, Some(id(2)));
        assert!(!preview.focus_mode);
        assert!(
            detached_layout(
                &TabLayout {
                    root: Some(Node::pane(id(1))),
                    focused: Some(id(1)),
                    focus_mode: false
                },
                id(1)
            )
            .is_none()
        );
        assert!(detached_layout(&original, id(99)).is_none());
    }

    #[test]
    fn move_command_is_reversible_in_layout_history() {
        let mut model = pigade_layout::LayoutModel::new(pigade_layout::LayoutState {
            tabs: std::collections::BTreeMap::from([(id(9), two_panes(false))]),
        })
        .unwrap();
        let command = drop_command(
            id(1),
            SplitId::from_u128(5),
            PaneRect {
                pane: id(2),
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 200.0,
                    height: 100.0,
                },
            },
            20.0,
            50.0,
        )
        .unwrap();
        let before = model.state().clone();
        assert!(model.apply(command).unwrap());
        assert!(model.undo());
        assert_eq!(model.state(), &before);
    }

    #[test]
    fn focus_queue_coalesces_a_to_b_to_a_and_ignores_stale_acks() {
        let a = FocusIntent {
            tab: id(10),
            pane: id(1),
        };
        let b = FocusIntent {
            tab: id(10),
            pane: id(2),
        };
        let c = FocusIntent {
            tab: id(11),
            pane: id(3),
        };
        let req1 = id(20);
        let req2 = id(21);
        let mut queue = FocusQueue::default();
        queue.choose(a);
        assert_eq!(queue.start(req1), Some(a));
        queue.choose(b);
        queue.choose(a);
        // Returning to the in-flight target needs no redundant second write.
        assert_eq!(queue.pending(), None);
        assert!(!queue.complete(req2, Some(a)));
        assert!(queue.is_saving());
        assert!(queue.complete(req1, Some(a)));
        assert_eq!(queue.pending(), None);
        queue.choose(b);
        assert_eq!(queue.start(req2), Some(b));
        queue.choose(c);
        assert!(queue.complete(req2, Some(b)));
        assert_eq!(queue.pending(), Some(c));
        assert_eq!(queue.start(req1), Some(c));
        queue.cancel();
        assert!(queue.matches(Some(req1)));
        assert!(queue.complete(req1, Some(c)));
        assert!(!queue.is_saving());
        assert_eq!(queue.pending(), None);
    }
}
