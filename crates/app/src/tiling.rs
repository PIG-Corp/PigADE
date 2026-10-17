//! Pure resize helpers for the saved tiling tree.
use pigade_layout::{Axis, CanvasGeometry, Command, Node, TabLayout};
use uuid::Uuid;

const MIN_BOUNDARY_FRACTION: f64 = 0.1;
const MAX_BOUNDARY_FRACTION: f64 = 0.9;

/// Build a boundary resize command for a pixel delta along the split's usable
/// axis extent (the extent after divider gaps are removed). Positive deltas
/// give space to the child before the boundary.
pub(crate) fn resize_by_delta(
    layout: &TabLayout,
    split: Uuid,
    boundary: usize,
    delta: f64,
    total_extent: f64,
) -> Option<Command> {
    if !delta.is_finite() || !total_extent.is_finite() || total_extent <= 0.0 {
        return None;
    }
    let Node::Split {
        weights, children, ..
    } = find_split(layout.root.as_ref()?, split)?
    else {
        return None;
    };
    if weights.len() != children.len() || boundary >= weights.len().saturating_sub(1) {
        return None;
    }
    let leading = weights[boundary];
    let trailing = weights[boundary + 1];
    let pair = leading + trailing;
    if !leading.is_finite()
        || !trailing.is_finite()
        || !pair.is_finite()
        || leading <= 0.0
        || trailing <= 0.0
        || pair <= 0.0
    {
        return None;
    }
    let fraction = ((leading * total_extent + delta) / (pair * total_extent))
        .clamp(MIN_BOUNDARY_FRACTION, MAX_BOUNDARY_FRACTION);
    fraction.is_finite().then_some(Command::ResizeBoundary {
        split,
        boundary,
        fraction,
    })
}

/// Find the nearest ancestor split with the requested axis and return its
/// nearest boundary adjacent to the pane's child subtree.
pub(crate) fn nearest_split(layout: &TabLayout, pane: Uuid, axis: Axis) -> Option<(Uuid, usize)> {
    nearest_in_node(layout.root.as_ref()?, pane, axis)
}

pub(crate) fn parent_split(layout: &TabLayout, pane: Uuid) -> Option<Uuid> {
    fn visit(node: &Node, pane: Uuid) -> Option<Uuid> {
        let Node::Split { id, children, .. } = node else {
            return None;
        };
        children
            .iter()
            .find_map(|child| visit(child, pane))
            .or_else(|| {
                children
                    .iter()
                    .any(|child| matches!(child,Node::Pane {id} if *id == pane))
                    .then_some(*id)
            })
    }
    visit(layout.root.as_ref()?, pane)
}

/// Return a split's usable extent from its projected descendant boxes, less
/// the split's own divider gaps. Hidden or incomplete geometry returns None.
pub(crate) fn split_extent(
    geometry: &CanvasGeometry,
    layout: &TabLayout,
    split: Uuid,
) -> Option<f64> {
    let Node::Split { axis, children, .. } = find_split(layout.root.as_ref()?, split)? else {
        return None;
    };
    let mut pane_ids = Vec::new();
    for child in children {
        collect_panes(child, &mut pane_ids);
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for pane in pane_ids {
        let rect = geometry.panes.iter().find(|entry| entry.pane == pane)?.rect;
        let (start, extent) = match axis {
            Axis::Horizontal => (rect.x, rect.width),
            Axis::Vertical => (rect.y, rect.height),
        };
        if !start.is_finite() || !extent.is_finite() || extent <= 0.0 {
            return None;
        }
        min = min.min(start);
        max = max.max(start + extent);
    }
    let dividers: Vec<_> = geometry
        .dividers
        .iter()
        .filter(|divider| divider.split == split && divider.axis == *axis)
        .collect();
    if dividers.len() != children.len().checked_sub(1)? {
        return None;
    }
    let gaps: f64 = dividers
        .iter()
        .map(|divider| match axis {
            Axis::Horizontal => divider.rect.width,
            Axis::Vertical => divider.rect.height,
        })
        .sum();
    if !gaps.is_finite() || gaps < 0.0 {
        return None;
    }
    let usable = (max - min) - gaps;
    (usable.is_finite() && usable > 0.0).then_some(usable)
}

fn find_split(node: &Node, split: Uuid) -> Option<&Node> {
    match node {
        Node::Split { id, .. } if *id == split => Some(node),
        Node::Split { children, .. } => children.iter().find_map(|child| find_split(child, split)),
        Node::Pane { .. } => None,
    }
}

fn nearest_in_node(node: &Node, pane: Uuid, axis: Axis) -> Option<(Uuid, usize)> {
    match node {
        Node::Pane { .. } => None,
        Node::Split {
            id,
            axis: split_axis,
            children,
            ..
        } => {
            let child_index = children.iter().position(|child| contains(child, pane))?;
            if let Some(found) = nearest_in_node(&children[child_index], pane, axis) {
                return Some(found);
            }
            if *split_axis != axis || children.len() < 2 {
                return None;
            }
            let boundary = if child_index + 1 < children.len() {
                child_index
            } else {
                child_index - 1
            };
            Some((*id, boundary))
        }
    }
}

fn contains(node: &Node, pane: Uuid) -> bool {
    match node {
        Node::Pane { id } => *id == pane,
        Node::Split { children, .. } => children.iter().any(|child| contains(child, pane)),
    }
}

fn collect_panes(node: &Node, panes: &mut Vec<Uuid>) {
    match node {
        Node::Pane { id } => panes.push(*id),
        Node::Split { children, .. } => {
            for child in children {
                collect_panes(child, panes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_layout::{LayoutState, Rect, Size};
    use std::collections::BTreeMap;

    fn fixture() -> (TabLayout, Uuid, Uuid, Uuid, Uuid) {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let split = Uuid::from_u128(10);
        let inner = Uuid::from_u128(11);
        let layout = TabLayout {
            root: Some(Node::Split {
                id: split,
                axis: Axis::Horizontal,
                weights: vec![0.6, 0.4],
                children: vec![
                    Node::pane(a),
                    Node::Split {
                        id: inner,
                        axis: Axis::Vertical,
                        weights: vec![0.5, 0.5],
                        children: vec![Node::pane(b), Node::pane(c)],
                    },
                ],
            }),
            focused: Some(b),
            focus_mode: false,
        };
        (layout, split, inner, a, b)
    }

    #[test]
    fn resize_delta_respects_pair_and_clamps_fraction() {
        let (layout, split, _, _, _) = fixture();
        let Command::ResizeBoundary {
            split: actual_split,
            boundary,
            fraction,
        } = resize_by_delta(&layout, split, 0, 9.0, 100.0).unwrap()
        else {
            panic!("expected boundary command");
        };
        assert_eq!(actual_split, split);
        assert_eq!(boundary, 0);
        assert!((fraction - 0.69).abs() < 1e-12);
        let Command::ResizeBoundary { fraction, .. } =
            resize_by_delta(&layout, split, 0, 10_000.0, 100.0).unwrap()
        else {
            unreachable!()
        };
        assert_eq!(fraction, MAX_BOUNDARY_FRACTION);
        assert!(resize_by_delta(&layout, split, 0, f64::NAN, 100.0).is_none());
    }

    #[test]
    fn nearest_split_prefers_closest_matching_axis_ancestor() {
        let (layout, split, inner, _, b) = fixture();
        assert_eq!(nearest_split(&layout, b, Axis::Vertical), Some((inner, 0)));
        assert_eq!(
            nearest_split(&layout, b, Axis::Horizontal),
            Some((split, 0))
        );
        assert_eq!(
            nearest_split(&layout, Uuid::new_v4(), Axis::Horizontal),
            None
        );
    }

    #[test]
    fn split_extent_uses_projected_descendants_and_excludes_own_gap() {
        let (layout, split, _, a, _) = fixture();
        let tab_id = Uuid::from_u128(20);
        let state = LayoutState {
            tabs: BTreeMap::from([(tab_id, layout.clone())]),
        };
        let geometry = state
            .project(
                tab_id,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 101.0,
                    height: 80.0,
                },
                Size {
                    width: 10.0,
                    height: 10.0,
                },
                1.0,
            )
            .unwrap();
        assert!((split_extent(&geometry, &layout, split).unwrap() - 100.0).abs() < 1e-9);
        assert_eq!(split_extent(&geometry, &layout, a), None);
        let mut incomplete = geometry;
        incomplete.panes.retain(|entry| entry.pane != a);
        assert!(split_extent(&incomplete, &layout, split).is_none());
    }
}
