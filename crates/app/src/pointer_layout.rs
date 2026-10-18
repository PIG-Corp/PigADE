//! Pure layout helpers for pointer-driven pane movement and focus persistence.

use std::collections::BTreeMap;

use pigade_layout::{Command, Direction, LayoutModel, LayoutState, PaneRect, Rect, TabLayout};
use uuid::Uuid;

/// A modifier grab begins moving only after the pointer travels more than 10px.
pub(crate) fn travelled(grab_x: f64, grab_y: f64, x: f64, y: f64) -> bool {
    [grab_x, grab_y, x, y].into_iter().all(f64::is_finite) && (x - grab_x).hypot(y - grab_y) > 10.0
}

/// Map the original grab point onto the temporary, smaller dragged pane.
/// Grabbing a header or an edge must not jump the pane's center to the cursor.
pub(crate) fn floating_rect(
    source: Rect,
    grab_x: f64,
    grab_y: f64,
    pointer_x: f64,
    pointer_y: f64,
) -> Option<Rect> {
    if !valid_rect(source)
        || ![grab_x, grab_y, pointer_x, pointer_y]
            .into_iter()
            .all(f64::is_finite)
    {
        return None;
    }
    let width = source.width * 0.85;
    let height = source.height * 0.85;
    let grip_x = ((grab_x - source.x) / source.width).clamp(0.0, 1.0);
    let grip_y = ((grab_y - source.y) / source.height).clamp(0.0, 1.0);
    let rect = Rect {
        x: pointer_x - width * grip_x,
        y: pointer_y - height * grip_y,
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

/// Select the closest non-source pane for a drop, including pointer positions
/// in gaps between panes. UUID order breaks equal-distance ties consistently.
pub(crate) fn drop_target(panes: &[PaneRect], source: Uuid, x: f64, y: f64) -> Option<PaneRect> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    panes
        .iter()
        .copied()
        .filter(|pane| pane.pane != source && valid_rect(pane.rect))
        .min_by(|first, second| {
            distance_squared(first.rect, x, y)
                .total_cmp(&distance_squared(second.rect, x, y))
                .then_with(|| first.pane.cmp(&second.pane))
        })
}

fn distance_squared(rect: Rect, x: f64, y: f64) -> f64 {
    let right = rect.x + rect.width;
    let bottom = rect.y + rect.height;
    let dx = if x < rect.x {
        rect.x - x
    } else if x > right {
        x - right
    } else {
        0.0
    };
    let dy = if y < rect.y {
        rect.y - y
    } else if y > bottom {
        y - bottom
    } else {
        0.0
    };
    dx.mul_add(dx, dy * dy)
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
    use pigade_layout::{
        Axis, Command, Direction, LayoutState, Node, PaneRect, Rect, SplitId, TabLayout,
    };
    use std::collections::BTreeMap;
    use uuid::Uuid;

    use super::{
        FocusIntent, FocusQueue, detached_layout, drop_command, drop_target, floating_rect,
        travelled,
    };

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
    fn a_grab_click_or_jitter_does_not_authorize_placement() {
        assert!(!travelled(300., 200., 300., 200.));
        assert!(!travelled(300., 200., 310., 200.));
        assert!(!travelled(300., 200., 306., 208.));
        assert!(travelled(300., 200., 310.01, 200.));
        assert!(travelled(300., 200., 308., 208.));
        assert!(!travelled(f64::NAN, 200., 307., 200.));
        assert!(!travelled(300., 200., f64::INFINITY, 200.));
    }

    #[test]
    fn drop_target_uses_nearest_rectangle_through_gutters_and_outside_canvas() {
        let panes = [
            PaneRect {
                pane: id(1),
                rect: Rect {
                    x: 0.,
                    y: 0.,
                    width: 100.,
                    height: 100.,
                },
            },
            PaneRect {
                pane: id(2),
                rect: Rect {
                    x: 120.,
                    y: 0.,
                    width: 100.,
                    height: 100.,
                },
            },
        ];
        assert_eq!(drop_target(&panes, id(1), 110., 50.).unwrap().pane, id(2));
        assert_eq!(drop_target(&panes, id(1), -20., 50.).unwrap().pane, id(2));
        assert!(drop_target(&panes, id(1), f64::NAN, 50.).is_none());
        assert!(drop_target(&[], id(1), 50., 50.).is_none());
    }

    #[test]
    fn drop_target_ignores_source_and_breaks_distance_ties_by_identity() {
        let panes = [
            PaneRect {
                pane: id(3),
                rect: Rect {
                    x: 0.,
                    y: 0.,
                    width: 10.,
                    height: 10.,
                },
            },
            PaneRect {
                pane: id(2),
                rect: Rect {
                    x: 20.,
                    y: 0.,
                    width: 10.,
                    height: 10.,
                },
            },
            PaneRect {
                pane: id(1),
                rect: Rect {
                    x: -20.,
                    y: 0.,
                    width: 10.,
                    height: 10.,
                },
            },
        ];
        assert_eq!(drop_target(&panes, id(3), 5., 5.).unwrap().pane, id(1));
    }

    #[test]
    fn floating_geometry_preserves_the_grab_point_and_eighty_five_percent() {
        let rect = floating_rect(
            Rect {
                x: 10.0,
                y: 20.0,
                width: 200.0,
                height: 100.0,
            },
            60.0,
            30.0,
            300.0,
            250.0,
        )
        .unwrap();
        assert_eq!(rect.width, 170.0);
        assert_eq!(rect.height, 85.0);
        assert_eq!(
            (rect.x + rect.width * 0.25, rect.y + rect.height * 0.1),
            (300.0, 250.0)
        );
        assert!(floating_rect(Rect { width: 0.0, ..rect }, 1., 1., 1., 1.).is_none());
        assert!(floating_rect(rect, 1., 1., f64::NAN, 1.).is_none());
        assert!(floating_rect(rect, f64::NAN, 1., 1., 1.).is_none());
    }

    #[test]
    fn floating_motion_is_translation_without_cumulative_drift() {
        let source = Rect {
            x: 10.,
            y: 20.,
            width: 200.,
            height: 100.,
        };
        // A header grab remains near the top; moving back recovers the exact
        // initial preview, independently of intermediate samples or repaints.
        let initial = floating_rect(source, 50., 22., 50., 22.).unwrap();
        let moved = floating_rect(source, 50., 22., 550., 322.).unwrap();
        assert_eq!(moved.x - initial.x, 500.);
        assert_eq!(moved.y - initial.y, 300.);
        assert_eq!(floating_rect(source, 50., 22., 50., 22.), Some(initial));
        let edge = floating_rect(source, -100., 500., 400., 300.).unwrap();
        assert_eq!(edge.x, 400.);
        assert_eq!(edge.y + edge.height, 300.);
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
        let projected = LayoutState {
            tabs: BTreeMap::from([(id(9), preview)]),
        }
        .project(
            id(9),
            Rect {
                x: 0.,
                y: 0.,
                width: 800.,
                height: 600.,
            },
            pigade_layout::Size {
                width: 100.,
                height: 80.,
            },
            0.,
        )
        .unwrap();
        assert_eq!(
            projected
                .panes
                .iter()
                .map(|pane| pane.pane)
                .collect::<Vec<_>>(),
            [id(2)]
        );
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
