//! Bounded, device-pixel geometry for terminal cell decorations.
//!
//! This module mirrors `alacritty_terminal::term::cell::Flags`; keep the wire
//! bits in sync with that source. It does not choose decoration colors or parse
//! ANSI. Returned geometry is relative to the cell origin, and patterned strokes
//! always start their phase at that origin.

use crate::protocol::Cell;

const UNDERLINE: u16 = 1 << 3;
const WIDE_CHAR: u16 = 1 << 5;
const WIDE_CHAR_SPACER: u16 = 1 << 6;
const HIDDEN: u16 = 1 << 8;
const STRIKEOUT: u16 = 1 << 9;
const LEADING_WIDE_CHAR_SPACER: u16 = 1 << 10;
const DOUBLE_UNDERLINE: u16 = 1 << 11;
const UNDERCURL: u16 = 1 << 12;
const DOTTED_UNDERLINE: u16 = 1 << 13;
const DASHED_UNDERLINE: u16 = 1 << 14;
const DECORATIONS: u16 =
    UNDERLINE | STRIKEOUT | DOUBLE_UNDERLINE | UNDERCURL | DOTTED_UNDERLINE | DASHED_UNDERLINE;

/// Reject absurd dimensions before generating any stroke segments.
pub(crate) const MAX_CELL_DIMENSION_PX: u16 = 512;
pub(crate) const MAX_CLIP_WIDTH_PX: u16 = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StrokePattern {
    Solid,
    Dotted,
    Dashed,
    Curl,
}

/// A one-pixel horizontal stroke recipe in integer physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StrokeLine {
    pub y_px: u16,
    pub width_px: u16,
    /// Full cell height used to validate that the line remains in bounds.
    pub cell_height_px: u16,
    pub pattern: StrokePattern,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StrokeRect {
    pub x_px: u16,
    pub y_px: u16,
    pub width_px: u16,
    pub height_px: u16,
}

/// Fixed-size cell style result. Double underline uses the two line slots;
/// patterned lines are expanded lazily by `StrokeLine::segments`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CellStylePlan<'a> {
    pub glyph_visible: bool,
    /// Effective span after clipping to the visible part of the pane.
    pub width_px: u16,
    pub underline: [Option<StrokeLine>; 2],
    /// Raw host metadata only; callers select/fallback the actual color.
    pub underline_color: Option<&'a str>,
    pub strike: Option<StrokeLine>,
}

impl StrokeLine {
    /// Lazily emit one-pixel rectangles; output count is bounded by width_px
    /// (at most `MAX_CLIP_WIDTH_PX` for plans returned by `plan`).
    pub(crate) fn segments(self) -> StrokeSegments {
        StrokeSegments {
            line: self,
            next_x: 0,
            emitted_solid: false,
        }
    }
}

pub(crate) struct StrokeSegments {
    line: StrokeLine,
    next_x: u16,
    emitted_solid: bool,
}

impl Iterator for StrokeSegments {
    type Item = StrokeRect;

    fn next(&mut self) -> Option<Self::Item> {
        let line = self.line;
        if line.width_px == 0 || line.cell_height_px == 0 {
            return None;
        }
        let (x_px, y_px, width_px) = match line.pattern {
            StrokePattern::Solid => {
                if self.emitted_solid {
                    return None;
                }
                self.emitted_solid = true;
                (0, line.y_px, line.width_px)
            }
            StrokePattern::Dotted => {
                if self.next_x >= line.width_px {
                    return None;
                }
                let x = self.next_x;
                self.next_x = self.next_x.saturating_add(2);
                (x, line.y_px, 1)
            }
            StrokePattern::Dashed => {
                if self.next_x >= line.width_px {
                    return None;
                }
                let x = self.next_x;
                self.next_x = self.next_x.saturating_add(5);
                (x, line.y_px, (line.width_px - x).min(3))
            }
            StrokePattern::Curl => {
                if self.next_x >= line.width_px {
                    return None;
                }
                let x = self.next_x;
                self.next_x += 1;
                let y = if x % 4 == 1 || x % 4 == 2 {
                    line.y_px.saturating_sub(1)
                } else {
                    line.y_px
                };
                (x, y, 1)
            }
        };
        Some(StrokeRect {
            x_px,
            y_px,
            width_px,
            height_px: 1,
        })
    }
}

/// Plan styles using mirrored host cell flags and pane-clipped physical width.
/// `span_cols` must be one or two (the latter for a leading wide cell); malformed
/// dimensions suppress decorations while keeping the caller's background and
/// glyph policy independent. A zero clip width simply clips all decorations.
pub(crate) fn plan(
    cell: &Cell,
    cell_width_px: u16,
    cell_height_px: u16,
    span_cols: u16,
    clip_width_px: u16,
) -> CellStylePlan<'_> {
    let hidden = cell.flags & HIDDEN != 0;
    let spacer = cell.flags & (WIDE_CHAR_SPACER | LEADING_WIDE_CHAR_SPACER) != 0;
    let mut result = CellStylePlan {
        glyph_visible: !hidden && !spacer,
        width_px: 0,
        underline: [None, None],
        underline_color: None,
        strike: None,
    };
    if hidden || spacer {
        return result;
    }
    if cell_width_px == 0
        || cell_width_px > MAX_CELL_DIMENSION_PX
        || cell_height_px == 0
        || cell_height_px > MAX_CELL_DIMENSION_PX
        || !matches!(span_cols, 1 | 2)
        || (span_cols == 2) != (cell.flags & WIDE_CHAR != 0)
        || clip_width_px > MAX_CLIP_WIDTH_PX
    {
        return result;
    }

    let width_px = cell_width_px.saturating_mul(span_cols).min(clip_width_px);
    if width_px == 0 {
        return result;
    }
    result.width_px = width_px;

    if cell.flags & STRIKEOUT != 0 {
        result.strike = Some(StrokeLine {
            y_px: cell_height_px / 2,
            width_px,
            cell_height_px,
            pattern: StrokePattern::Solid,
        });
    }

    let kind = if cell.flags & UNDERCURL != 0 {
        Some(StrokePattern::Curl)
    } else if cell.flags & DOTTED_UNDERLINE != 0 {
        Some(StrokePattern::Dotted)
    } else if cell.flags & DASHED_UNDERLINE != 0 {
        Some(StrokePattern::Dashed)
    } else if cell.flags & (DOUBLE_UNDERLINE | UNDERLINE) != 0 {
        Some(StrokePattern::Solid)
    } else {
        None
    };
    if let Some(pattern) = kind {
        let y = match pattern {
            StrokePattern::Curl => cell_height_px.saturating_sub(2),
            _ => cell_height_px - 1,
        };
        result.underline[0] = Some(StrokeLine {
            y_px: y,
            width_px,
            cell_height_px,
            pattern,
        });
        if pattern == StrokePattern::Solid
            && cell.flags & DOUBLE_UNDERLINE != 0
            && cell_height_px >= 3
        {
            result.underline[1] = Some(StrokeLine {
                y_px: cell_height_px - 3,
                width_px,
                cell_height_px,
                pattern: StrokePattern::Solid,
            });
        }
        result.underline_color = cell.underline.as_deref();
    }

    result
}

/// Whether a foreground pass is needed for this host cell. Spaces need the
/// pass when they carry a decoration; hidden text and wide continuations do not.
pub(crate) fn needs_foreground(cell: &Cell) -> bool {
    let hidden_or_spacer = cell.flags & (HIDDEN | WIDE_CHAR_SPACER | LEADING_WIDE_CHAR_SPACER) != 0;
    !hidden_or_spacer
        && ((!cell.text.is_empty() && cell.text != " ") || cell.flags & DECORATIONS != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Cell;

    fn cell(flags: u16, underline: Option<&str>) -> Cell {
        Cell {
            text: "x".into(),
            fg: "Named(Foreground)".into(),
            bg: "Named(Background)".into(),
            flags,
            underline: underline.map(str::to_owned),
            hyperlink: None,
        }
    }

    #[test]
    fn hidden_suppresses_glyph_and_decorations_but_keeps_background() {
        let cell = cell(
            HIDDEN | UNDERLINE | STRIKEOUT,
            Some("Rgb { r: 1, g: 2, b: 3 }"),
        );
        let plan = plan(&cell, 9, 23, 1, 9);
        assert!(!plan.glyph_visible);
        assert_eq!(plan.underline, [None, None]);
        assert_eq!(plan.underline_color, None);
        assert_eq!(plan.strike, None);
    }

    #[test]
    fn wide_cell_geometry_clips_without_allocating() {
        let cell = cell(WIDE_CHAR | UNDERLINE, None);
        let plan = plan(&cell, 18, 46, 2, 29);
        assert!(plan.glyph_visible);
        assert_eq!(plan.width_px, 29);
        let line = plan.underline[0].unwrap();
        assert_eq!((line.width_px, line.y_px), (29, 45));
        assert_eq!(line.segments().count(), 1);
    }

    #[test]
    fn span_must_match_host_wide_flag() {
        let wide = cell(WIDE_CHAR | UNDERLINE, None);
        let wide_mismatch = plan(&wide, 9, 23, 1, 9);
        assert!(wide_mismatch.glyph_visible);
        assert_eq!(wide_mismatch.underline, [None, None]);

        let ordinary = cell(UNDERLINE, None);
        let ordinary_mismatch = plan(&ordinary, 9, 23, 2, 18);
        assert!(ordinary_mismatch.glyph_visible);
        assert_eq!(ordinary_mismatch.underline, [None, None]);
    }

    #[test]
    fn wide_spacers_do_not_duplicate_glyph_or_decoration() {
        for flags in [
            WIDE_CHAR_SPACER | UNDERLINE,
            LEADING_WIDE_CHAR_SPACER | STRIKEOUT,
        ] {
            let cell = cell(flags, Some("custom"));
            let plan = plan(&cell, 9, 23, 1, 9);
            assert!(!plan.glyph_visible);
            assert_eq!(plan.underline, [None, None]);
            assert_eq!(plan.strike, None);
        }
    }

    #[test]
    fn underline_flags_select_expected_patterns_and_preserve_raw_color() {
        for (flag, pattern) in [
            (UNDERLINE, StrokePattern::Solid),
            (DOUBLE_UNDERLINE, StrokePattern::Solid),
            (UNDERCURL, StrokePattern::Curl),
            (DOTTED_UNDERLINE, StrokePattern::Dotted),
            (DASHED_UNDERLINE, StrokePattern::Dashed),
        ] {
            let cell = cell(flag, Some("host underline metadata"));
            let plan = plan(&cell, 9, 23, 1, 9);
            assert_eq!(plan.underline[0].unwrap().pattern, pattern);
            assert_eq!(plan.underline_color, Some("host underline metadata"));
        }
        let cell = cell(0, Some("ignored without underline flag"));
        assert_eq!(plan(&cell, 9, 23, 1, 9).underline_color, None);
    }

    #[test]
    fn double_underline_has_two_lines_with_one_pixel_gap_and_strike_midline() {
        let cell = cell(DOUBLE_UNDERLINE | STRIKEOUT, None);
        let plan = plan(&cell, 18, 46, 1, 18);
        let first = plan.underline[0].unwrap();
        let second = plan.underline[1].unwrap();
        assert_eq!((first.y_px, second.y_px), (45, 43));
        assert_eq!(plan.strike.unwrap().y_px, 23);
        assert_eq!(plan.underline[1].unwrap().pattern, StrokePattern::Solid);
    }

    #[test]
    fn patterned_segments_are_origin_stable_and_bounded_to_geometry() {
        for pattern in [
            StrokePattern::Dotted,
            StrokePattern::Dashed,
            StrokePattern::Curl,
        ] {
            let line = StrokeLine {
                y_px: 20,
                width_px: 31,
                cell_height_px: 23,
                pattern,
            };
            let segments: Vec<_> = line.segments().collect();
            assert_eq!(segments, line.segments().collect::<Vec<_>>());
            assert!(segments.len() <= 31);
            assert!(segments.iter().all(|segment| {
                segment.x_px + segment.width_px <= line.width_px
                    && segment.y_px < line.cell_height_px
                    && segment.height_px == 1
            }));
            assert!(segments.windows(2).all(|pair| pair[0].x_px < pair[1].x_px));
        }
    }

    #[test]
    fn excessive_or_malformed_geometry_suppresses_only_decorations() {
        for (width, height, span, clip) in [
            (0, 23, 1, 9),
            (513, 23, 1, 9),
            (9, 0, 1, 9),
            (9, 513, 1, 9),
            (9, 23, 0, 9),
            (9, 23, 3, 9),
            (9, 23, 1, 1025),
        ] {
            let cell = cell(UNDERLINE | STRIKEOUT, None);
            let plan = plan(&cell, width, height, span, clip);
            assert!(plan.glyph_visible);
            assert_eq!(plan.width_px, 0);
            assert_eq!(plan.underline, [None, None]);
            assert_eq!(plan.strike, None);
        }
    }

    #[test]
    fn tiny_double_underline_omits_overlapping_second_line() {
        let cell = cell(DOUBLE_UNDERLINE, None);
        let plan = plan(&cell, 2, 2, 1, 2);
        assert!(plan.underline[0].is_some());
        assert_eq!(plan.underline[1], None);
    }

    #[test]
    fn foreground_pass_keeps_decorated_spaces_and_skips_hidden_or_spacer_cells() {
        let mut decorated_space = cell(UNDERLINE, Some("underline"));
        decorated_space.text = " ".into();
        assert!(needs_foreground(&decorated_space));
        let mut struck_space = cell(STRIKEOUT, None);
        struck_space.text = " ".into();
        assert!(needs_foreground(&struck_space));
        assert!(needs_foreground(&cell(0, None)));
        let mut plain_space = cell(0, Some("unused"));
        plain_space.text = " ".into();
        assert!(!needs_foreground(&plain_space));
        assert!(!needs_foreground(&cell(HIDDEN | UNDERLINE, None)));
        assert!(!needs_foreground(&cell(WIDE_CHAR_SPACER | UNDERLINE, None)));
        assert!(!needs_foreground(&cell(LEADING_WIDE_CHAR_SPACER, None)));
    }
}
