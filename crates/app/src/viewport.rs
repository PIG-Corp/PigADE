//! Pure canvas scrolling and scrollbar geometry for the product tiling view.

use pigade_layout::{Axis, CanvasGeometry, Rect};
use pigade_storage::CanvasOffset;
use uuid::Uuid;

const SCROLLBAR_THICKNESS: f64 = 24.0;
const MIN_THUMB_LENGTH: f64 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Scrollbar {
    pub track: Rect,
    pub thumb: Rect,
    pub axis: Axis,
    pub max_offset: f64,
}

/// Apply a scroll offset to layout geometry while preserving every pane's size.
///
/// The layout geometry is projected at the viewport origin. The returned bounds,
/// panes, and dividers are translated together so the root can clip them to the
/// viewport. `reveal` adjusts the requested offset minimally to expose that pane.
pub(crate) fn project(
    mut geometry: CanvasGeometry,
    viewport: Rect,
    requested: CanvasOffset,
    scale: f64,
    reveal: Option<Uuid>,
) -> (CanvasGeometry, CanvasOffset) {
    let max_x = (geometry.bounds.width - viewport.width).max(0.0);
    let max_y = (geometry.bounds.height - viewport.height).max(0.0);
    let mut offset = CanvasOffset {
        x: clamp_finite(requested.x, max_x),
        y: clamp_finite(requested.y, max_y),
    };

    if let Some(target) = reveal
        && let Some(pane) = geometry.panes.iter().find(|pane| pane.pane == target)
    {
        offset.x = reveal_axis(
            pane.rect.x,
            pane.rect.x + pane.rect.width,
            viewport.x,
            viewport.width,
            offset.x,
        );
        offset.y = reveal_axis(
            pane.rect.y,
            pane.rect.y + pane.rect.height,
            viewport.y,
            viewport.height,
            offset.y,
        );
    }

    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    offset.x = align_offset(offset.x, max_x, scale);
    offset.y = align_offset(offset.y, max_y, scale);

    let dx = -offset.x;
    let dy = -offset.y;
    translate_rect(&mut geometry.bounds, dx, dy);
    for pane in &mut geometry.panes {
        translate_rect(&mut pane.rect, dx, dy);
    }
    for divider in &mut geometry.dividers {
        translate_rect(&mut divider.rect, dx, dy);
    }
    (geometry, offset)
}

fn clamp_finite(value: f64, maximum: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, maximum)
    } else {
        0.0
    }
}

fn align_offset(value: f64, maximum: f64, scale: f64) -> f64 {
    let max_aligned = (maximum * scale).floor() / scale;
    ((value * scale).round() / scale).clamp(0.0, max_aligned.max(0.0))
}

fn reveal_axis(
    pane_start: f64,
    pane_end: f64,
    viewport_start: f64,
    viewport_extent: f64,
    requested: f64,
) -> f64 {
    let pane_extent = pane_end - pane_start;
    let current_start = viewport_start + requested;
    let current_end = current_start + viewport_extent;
    if pane_extent > viewport_extent {
        let align_start = pane_start - viewport_start;
        let align_end = pane_end - viewport_start - viewport_extent;
        if (requested - align_start).abs() <= (requested - align_end).abs() {
            align_start.max(0.0)
        } else {
            align_end.max(0.0)
        }
    } else if pane_start < current_start {
        (pane_start - viewport_start).max(0.0)
    } else if pane_end > current_end {
        (pane_end - viewport_start - viewport_extent).max(0.0)
    } else {
        requested
    }
}

fn translate_rect(rect: &mut Rect, dx: f64, dy: f64) {
    rect.x += dx;
    rect.y += dy;
}

/// Compute a scrollbar in the 24 logical pixels reserved beside the viewport.
pub(crate) fn scrollbar(
    viewport: Rect,
    canvas_extent: f64,
    offset: f64,
    axis: Axis,
) -> Option<Scrollbar> {
    let (viewport_extent, track) = match axis {
        Axis::Horizontal => (
            viewport.width,
            Rect {
                x: viewport.x,
                y: viewport.y + viewport.height,
                width: viewport.width,
                height: SCROLLBAR_THICKNESS,
            },
        ),
        Axis::Vertical => (
            viewport.height,
            Rect {
                x: viewport.x + viewport.width,
                y: viewport.y,
                width: SCROLLBAR_THICKNESS,
                height: viewport.height,
            },
        ),
    };
    if !viewport_extent.is_finite()
        || viewport_extent <= 0.0
        || !canvas_extent.is_finite()
        || canvas_extent <= viewport_extent
    {
        return None;
    }

    let track_extent = viewport_extent;
    let max_offset = canvas_extent - viewport_extent;
    let thumb_extent = (track_extent * viewport_extent / canvas_extent)
        .max(MIN_THUMB_LENGTH.min(track_extent))
        .min(track_extent);
    let travel = track_extent - thumb_extent;
    let thumb_offset = clamp_finite(offset, max_offset) / max_offset * travel;
    let thumb = match axis {
        Axis::Horizontal => Rect {
            x: track.x + thumb_offset,
            y: track.y,
            width: thumb_extent,
            height: track.height,
        },
        Axis::Vertical => Rect {
            x: track.x,
            y: track.y + thumb_offset,
            width: track.width,
            height: thumb_extent,
        },
    };
    Some(Scrollbar {
        track,
        thumb,
        axis,
        max_offset,
    })
}

/// Map a pointer delta on the scrollbar thumb to the corresponding canvas offset.
pub(crate) fn dragged_offset(bar: Scrollbar, initial: CanvasOffset, delta: f64) -> CanvasOffset {
    if !delta.is_finite() {
        return initial;
    }
    let (track_extent, thumb_extent, start) = match bar.axis {
        Axis::Horizontal => (bar.track.width, bar.thumb.width, initial.x),
        Axis::Vertical => (bar.track.height, bar.thumb.height, initial.y),
    };
    let travel = track_extent - thumb_extent;
    if travel <= 0.0 || bar.max_offset <= 0.0 {
        return initial;
    }
    let moved = (start + delta / travel * bar.max_offset).clamp(0.0, bar.max_offset);
    match bar.axis {
        Axis::Horizontal => CanvasOffset {
            x: moved,
            ..initial
        },
        Axis::Vertical => CanvasOffset {
            y: moved,
            ..initial
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigade_layout::{Divider, PaneRect};

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn geometry() -> CanvasGeometry {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        CanvasGeometry {
            bounds: rect(10.0, 20.0, 500.0, 400.0),
            panes: vec![
                PaneRect {
                    pane: first,
                    rect: rect(10.0, 20.0, 245.0, 400.0),
                },
                PaneRect {
                    pane: second,
                    rect: rect(255.0, 20.0, 255.0, 400.0),
                },
            ],
            dividers: vec![Divider {
                split: Uuid::from_u128(3),
                boundary: 0,
                axis: Axis::Horizontal,
                rect: rect(245.0, 20.0, 10.0, 400.0),
            }],
        }
    }

    #[test]
    fn scrolling_translates_tree_geometry_without_resizing_panes() {
        let source = geometry();
        let viewport = rect(10.0, 20.0, 100.0, 80.0);
        let (projected, offset) = project(
            source.clone(),
            viewport,
            CanvasOffset { x: 50.0, y: 40.0 },
            1.0,
            None,
        );
        assert_eq!(offset, CanvasOffset { x: 50.0, y: 40.0 });
        assert_eq!(projected.bounds, rect(-40.0, -20.0, 500.0, 400.0));
        assert_eq!(projected.panes[0].rect.width, source.panes[0].rect.width);
        assert_eq!(projected.panes[0].rect.height, source.panes[0].rect.height);
        assert_eq!(projected.panes[0].rect, rect(-40.0, -20.0, 245.0, 400.0));
        assert_eq!(projected.dividers[0].rect, rect(195.0, -20.0, 10.0, 400.0));
    }

    #[test]
    fn offsets_clamp_to_canvas_and_align_to_device_pixels() {
        let viewport = rect(0.0, 0.0, 100.0, 80.0);
        let (_, aligned) = project(
            geometry(),
            viewport,
            CanvasOffset { x: 33.31, y: 900.0 },
            1.25,
            None,
        );
        assert!((aligned.x * 1.25 - (aligned.x * 1.25).round()).abs() < 1e-9);
        assert!((aligned.y * 1.25 - (aligned.y * 1.25).round()).abs() < 1e-9);
        assert!(aligned.x <= 400.0 && aligned.y <= 320.0);
        assert_eq!(aligned.y, 320.0);
    }

    #[test]
    fn reveal_scrolls_only_enough_to_show_a_partially_hidden_pane() {
        let pane = Uuid::from_u128(2);
        let mut geometry = geometry();
        geometry.bounds = rect(0.0, 0.0, 400.0, 200.0);
        geometry.panes = vec![PaneRect {
            pane,
            rect: rect(120.0, 20.0, 40.0, 40.0),
        }];
        let (_, offset) = project(
            geometry,
            rect(0.0, 0.0, 100.0, 100.0),
            CanvasOffset { x: 10.0, y: 0.0 },
            1.0,
            Some(pane),
        );
        assert_eq!(offset.x, 60.0);
        assert_eq!(offset.y, 0.0);
    }

    #[test]
    fn oversized_reveal_uses_nearest_edge_and_remains_stable() {
        let pane = Uuid::from_u128(4);
        let source = CanvasGeometry {
            bounds: rect(0.0, 0.0, 400.0, 100.0),
            panes: vec![PaneRect {
                pane,
                rect: rect(20.0, 0.0, 300.0, 100.0),
            }],
            dividers: Vec::new(),
        };
        let viewport = rect(0.0, 0.0, 100.0, 100.0);
        let (_, first) = project(
            source.clone(),
            viewport,
            CanvasOffset { x: 190.0, y: 0.0 },
            1.0,
            Some(pane),
        );
        let (_, second) = project(source, viewport, first, 1.0, Some(pane));
        assert_eq!(first.x, 220.0);
        assert_eq!(second, first);
    }

    #[test]
    fn scrollbar_geometry_and_drag_map_to_scroll_extent() {
        let viewport = rect(10.0, 20.0, 100.0, 60.0);
        let bar = scrollbar(viewport, 300.0, 0.0, Axis::Horizontal).unwrap();
        assert_eq!(bar.track, rect(10.0, 80.0, 100.0, 24.0));
        assert!(bar.thumb.width >= 24.0);
        assert_eq!(bar.max_offset, 200.0);
        let travel = bar.track.width - bar.thumb.width;
        let half = dragged_offset(bar, CanvasOffset::default(), travel / 2.0);
        assert!((half.x - 100.0).abs() < 1e-9);
        assert_eq!(dragged_offset(bar, half, travel * 4.0).x, 200.0);
    }

    #[test]
    fn scrollbar_is_absent_when_canvas_fits() {
        assert!(scrollbar(rect(0.0, 0.0, 100.0, 80.0), 100.0, 0.0, Axis::Horizontal).is_none());
    }
}
