//! Pure cursor geometry and visibility decisions for the app painter.

use crate::{
    input::SHOW_CURSOR,
    protocol::{CursorPresentationShape, Frame},
};
use std::time::Duration;

const WIDE_CHAR: u16 = 0x0020;
const WIDE_CHAR_SPACER: u16 = 0x0040;
const LEADING_WIDE_CHAR_SPACER: u16 = 0x0400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorPlan {
    pub row: u16,
    pub column: u16,
    pub span: u16,
    pub shape: CursorPresentationShape,
    pub blinking: bool,
}

/// Plan a cursor only when its cell is present in both the frame and viewport.
pub(crate) fn plan(
    frame: &Frame,
    visible_rows: u16,
    visible_cols: u16,
    focused: bool,
) -> Option<CursorPlan> {
    let row = u16::try_from(frame.cursor.0).ok()?;
    let mut column = u16::try_from(frame.cursor.1).ok()?;
    let cols = frame.geometry.cols;
    if row >= visible_rows
        || row >= frame.geometry.rows
        || column >= visible_cols
        || column >= cols
        || cols == 0
    {
        return None;
    }

    let cell_at = |col: u16| {
        let index = usize::from(row)
            .checked_mul(usize::from(cols))?
            .checked_add(usize::from(col))?;
        frame.cells.get(index)
    };
    let mut cell = cell_at(column)?;
    if cell.flags & LEADING_WIDE_CHAR_SPACER != 0 {
        return None;
    }
    if cell.flags & WIDE_CHAR_SPACER != 0 {
        if column == 0 {
            return None;
        }
        let previous = cell_at(column - 1)?;
        if previous.flags & WIDE_CHAR == 0 {
            return None;
        }
        column -= 1;
        cell = previous;
    }

    let (mut shape, mut blinking) = match frame.cursor_presentation {
        Some(presentation) => {
            if presentation.shape == CursorPresentationShape::Hidden {
                return None;
            }
            (presentation.shape, presentation.blinking)
        }
        None => {
            if frame.modes & SHOW_CURSOR == 0 {
                return None;
            }
            // Older hosts did not provide typed presentation. Underline is the
            // stable fallback; cursor_style is debug text and is not a protocol.
            (CursorPresentationShape::Underline, false)
        }
    };
    if !focused {
        shape = CursorPresentationShape::HollowBlock;
        blinking = false;
    }

    let span = if cell.flags & WIDE_CHAR != 0 {
        visible_cols
            .saturating_sub(column)
            .min(cols.saturating_sub(column))
            .min(2)
    } else {
        1
    };
    (span != 0).then_some(CursorPlan {
        row,
        column,
        span,
        shape,
        blinking,
    })
}

/// Paint a block cursor as cell backgrounds before glyphs, preserving glyph
/// visibility instead of painting an opaque rectangle over already drawn text.
pub(crate) fn cell_colors(
    plan: Option<CursorPlan>,
    (row, column): (usize, usize),
    (foreground, background): (u32, u32),
    cursor_color: u32,
) -> (u32, u32) {
    if plan.is_some_and(|p| {
        p.shape == CursorPresentationShape::Block
            && row == usize::from(p.row)
            && (usize::from(p.column)..usize::from(p.column + p.span)).contains(&column)
    }) {
        let text_color = if background != cursor_color {
            background
        } else if ((cursor_color >> 16) & 255) * 299
            + ((cursor_color >> 8) & 255) * 587
            + (cursor_color & 255) * 114
            >= 128000
        {
            0
        } else {
            0xffffff
        };
        (text_color, cursor_color)
    } else {
        (foreground, background)
    }
}

/// A 500ms on / 500ms off cycle. Reduced-motion preference disables blinking.
pub(crate) fn blink_visible(elapsed: Duration, blinking: bool, reduced_motion: bool) -> bool {
    if reduced_motion || !blinking {
        return true;
    }
    elapsed.as_millis() % 1000 < 500
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Cell, CursorPresentation};

    fn frame(row: i32, column: usize) -> Frame {
        Frame {
            geometry: crate::protocol::Geometry { rows: 2, cols: 3 },
            cells: (0..6)
                .map(|_| Cell {
                    text: " ".into(),
                    fg: "Named(White)".into(),
                    bg: "Named(Black)".into(),
                    flags: 0,
                    underline: None,
                    hyperlink: None,
                })
                .collect(),
            hyperlinks: vec![],
            cursor: (row, column),
            cursor_style: "ignored legacy debug text".into(),
            cursor_presentation: Some(CursorPresentation {
                shape: CursorPresentationShape::Block,
                blinking: true,
            }),
            modes: SHOW_CURSOR,
            colors: String::new(),
            palette_overrides: vec![],
            history_lines: 0,
            sync_pending_bytes: 0,
            title: String::new(),
        }
    }

    #[test]
    fn all_typed_shapes_are_preserved_and_hidden_is_absent() {
        for shape in [
            CursorPresentationShape::Block,
            CursorPresentationShape::Underline,
            CursorPresentationShape::Beam,
            CursorPresentationShape::HollowBlock,
        ] {
            let mut f = frame(0, 0);
            f.cursor_presentation = Some(CursorPresentation {
                shape,
                blinking: true,
            });
            assert_eq!(plan(&f, 2, 3, true).unwrap().shape, shape);
            assert!(plan(&f, 2, 3, true).unwrap().blinking);
        }
        let mut f = frame(0, 0);
        f.cursor_presentation = Some(CursorPresentation {
            shape: CursorPresentationShape::Hidden,
            blinking: true,
        });
        assert_eq!(plan(&f, 2, 3, true), None);
    }

    #[test]
    fn legacy_visibility_uses_mode_and_underline_fallback() {
        let mut f = frame(0, 0);
        f.cursor_presentation = None;
        let planned = plan(&f, 2, 3, true).unwrap();
        assert_eq!(planned.shape, CursorPresentationShape::Underline);
        assert!(!planned.blinking);
        f.modes &= !SHOW_CURSOR;
        assert_eq!(plan(&f, 2, 3, true), None);
    }

    #[test]
    fn negative_and_out_of_bounds_cursor_is_absent() {
        assert_eq!(plan(&frame(-1, 0), 2, 3, true), None);
        assert_eq!(plan(&frame(2, 0), 2, 3, true), None);
        assert_eq!(plan(&frame(0, 3), 2, 3, true), None);
        assert_eq!(plan(&frame(0, 2), 2, 2, true), None);
    }

    #[test]
    fn wide_second_half_normalizes_and_span_clips_to_viewport() {
        let mut f = frame(0, 1);
        f.cells[0].flags = WIDE_CHAR;
        f.cells[1].flags = WIDE_CHAR_SPACER;
        let normalized = plan(&f, 2, 3, true).unwrap();
        assert_eq!((normalized.column, normalized.span), (0, 2));

        f.cursor = (0, 0);
        let clipped = plan(&f, 2, 1, true).unwrap();
        assert_eq!((clipped.column, clipped.span), (0, 1));

        // A malformed/truncated wide marker at the last acknowledged frame
        // column cannot claim cells beyond that frame, even in a wider viewport.
        f.cursor = (0, 2);
        f.cells[2].flags = WIDE_CHAR;
        let frame_clipped = plan(&f, 2, 8, true).unwrap();
        assert_eq!((frame_clipped.column, frame_clipped.span), (2, 1));

        f.cursor = (0, 1);
        f.cells[0].flags = LEADING_WIDE_CHAR_SPACER;
        assert_eq!(plan(&f, 2, 3, true), None);
    }

    #[test]
    fn unfocused_cursor_is_hollow_and_static() {
        let plan = plan(&frame(0, 0), 2, 3, false).unwrap();
        assert_eq!(plan.shape, CursorPresentationShape::HollowBlock);
        assert!(!plan.blinking);
    }

    #[test]
    fn blink_cycle_has_stable_boundaries_and_reduced_motion_is_static() {
        assert!(blink_visible(Duration::from_millis(0), true, false));
        assert!(blink_visible(Duration::from_millis(499), true, false));
        assert!(!blink_visible(Duration::from_millis(500), true, false));
        assert!(!blink_visible(Duration::from_millis(999), true, false));
        assert!(blink_visible(Duration::from_millis(1000), true, false));
        assert!(blink_visible(Duration::from_millis(999), false, false));
        assert!(blink_visible(Duration::from_millis(999), true, true));
    }
}

#[cfg(test)]
mod paint_tests {
    use super::*;
    #[test]
    fn block_background_covers_wide_span_and_keeps_a_legible_glyph() {
        let p = CursorPlan {
            row: 1,
            column: 2,
            span: 2,
            shape: CursorPresentationShape::Block,
            blinking: false,
        };
        for column in [2, 3] {
            assert_eq!(
                cell_colors(Some(p), (1, column), (0xf0f0f0, 0x112233), 0x7dafff),
                (0x112233, 0x7dafff)
            );
        }
        for cell in [(0, 2), (1, 1), (1, 4)] {
            assert_eq!(
                cell_colors(Some(p), cell, (0xf0f0f0, 0x112233), 0x7dafff),
                (0xf0f0f0, 0x112233)
            );
        }
        assert_eq!(
            cell_colors(Some(p), (1, 2), (0xeeeeee, 0), 0),
            (0xffffff, 0)
        );
        assert_eq!(
            cell_colors(Some(p), (1, 2), (0x111111, 0xffffff), 0xffffff),
            (0, 0xffffff)
        );
    }
    #[test]
    fn stroke_and_hidden_cursors_do_not_replace_glyph_colors() {
        for shape in [
            CursorPresentationShape::Beam,
            CursorPresentationShape::Underline,
            CursorPresentationShape::HollowBlock,
            CursorPresentationShape::Hidden,
        ] {
            let p = CursorPlan {
                row: 0,
                column: 0,
                span: 1,
                shape,
                blinking: false,
            };
            assert_eq!(cell_colors(Some(p), (0, 0), (1, 2), 3), (1, 2));
        }
        assert_eq!(cell_colors(None, (0, 0), (1, 2), 3), (1, 2));
    }
}
