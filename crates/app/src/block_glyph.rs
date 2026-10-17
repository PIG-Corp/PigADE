//! Device-pixel geometry for Unicode terminal block elements.
//!
//! These characters describe cell-relative shapes. Font outlines can leave
//! gaps when terminal line height differs from the font's own metrics, so the
//! native cell painter can use this plan instead of shaping supported blocks.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct BlockRect {
    pub x_px: u16,
    pub y_px: u16,
    pub width_px: u16,
    pub height_px: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BlockGlyphPlan {
    rects: [BlockRect; 4],
    len: u8,
}

impl BlockGlyphPlan {
    pub(crate) fn rects(&self) -> &[BlockRect] {
        &self.rects[..usize::from(self.len)]
    }
}

/// Plan a Unicode block-element glyph within one terminal cell.
///
/// Rectangles use physical pixels relative to the cell origin. Edges on the
/// eighth-cell grid are rounded once, so adjoining pieces share exact edges.
/// Shade glyphs U+2591..U+2593 are left to the font because they are patterns,
/// not solid block geometry.
pub(crate) fn plan(character: char, width_px: u16, height_px: u16) -> Option<BlockGlyphPlan> {
    if width_px == 0 || height_px == 0 {
        return None;
    }

    let code = character as u32;
    let mut result = BlockGlyphPlan {
        rects: [BlockRect::default(); 4],
        len: 0,
    };

    match code {
        0x2580 => result.push(0, 0, 8, 4), // upper half
        0x2581..=0x2587 => result.push(0, 8 - (code - 0x2580) as u8, 8, 8), // lower 1/8..7/8
        0x2588 => result.push(0, 0, 8, 8), // full block
        0x2589..=0x258F => result.push(0, 0, (0x2590 - code) as u8, 8), // left 7/8..1/8
        0x2590 => result.push(4, 0, 8, 8), // right half
        0x2591..=0x2593 => return None,    // shaded patterns
        0x2594 => result.push(0, 0, 8, 1), // upper 1/8
        0x2595 => result.push(7, 0, 8, 8), // right 1/8
        0x2596..=0x259F => {
            // Bit order: upper-left, upper-right, lower-left, lower-right.
            let quadrants = match code {
                0x2596 => 0b0100, // lower-left
                0x2597 => 0b1000, // lower-right
                0x2598 => 0b0001, // upper-left
                0x2599 => 0b1101, // all but upper-right
                0x259A => 0b1001, // upper-left + lower-right
                0x259B => 0b0111, // all but lower-right
                0x259C => 0b1011, // all but lower-left
                0x259D => 0b0010, // upper-right
                0x259E => 0b0110, // upper-right + lower-left
                0x259F => 0b1110, // all but upper-left
                _ => unreachable!(),
            };
            for (bit, x, y) in [(1, 0, 0), (2, 4, 0), (4, 0, 4), (8, 4, 4)] {
                if quadrants & bit != 0 {
                    result.push(x, y, x + 4, y + 4);
                }
            }
        }
        _ => return None,
    }

    result.scale(width_px, height_px);
    Some(result)
}

impl BlockGlyphPlan {
    fn push(&mut self, x0: u8, y0: u8, x1: u8, y1: u8) {
        self.rects[usize::from(self.len)] = BlockRect {
            x_px: u16::from(x0),
            y_px: u16::from(y0),
            width_px: u16::from(x1 - x0),
            height_px: u16::from(y1 - y0),
        };
        self.len += 1;
    }

    fn scale(&mut self, width_px: u16, height_px: u16) {
        let mut scaled = [BlockRect::default(); 4];
        let mut len = 0;
        for rect in &self.rects[..usize::from(self.len)] {
            let x0 = edge(rect.x_px, width_px);
            let x1 = edge(rect.x_px + rect.width_px, width_px);
            let y0 = edge(rect.y_px, height_px);
            let y1 = edge(rect.y_px + rect.height_px, height_px);
            let rect = BlockRect {
                x_px: x0,
                y_px: y0,
                width_px: x1 - x0,
                height_px: y1 - y0,
            };
            if rect.width_px != 0 && rect.height_px != 0 {
                scaled[usize::from(len)] = rect;
                len += 1;
            }
        }
        self.rects = scaled;
        self.len = len;
    }
}

fn edge(eighths: u16, extent_px: u16) -> u16 {
    ((u32::from(eighths) * u32::from(extent_px) + 4) / 8) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(character: char, width: u16, height: u16) -> BlockRect {
        let plan = plan(character, width, height).expect("supported block glyph");
        assert_eq!(plan.rects().len(), 1);
        plan.rects()[0]
    }

    #[test]
    fn full_half_and_fractional_blocks_fill_the_expected_cell_regions() {
        assert_eq!(
            one('█', 8, 16),
            BlockRect {
                x_px: 0,
                y_px: 0,
                width_px: 8,
                height_px: 16
            }
        );
        assert_eq!(
            one('▀', 8, 16),
            BlockRect {
                x_px: 0,
                y_px: 0,
                width_px: 8,
                height_px: 8
            }
        );
        assert_eq!(
            one('▄', 8, 16),
            BlockRect {
                x_px: 0,
                y_px: 8,
                width_px: 8,
                height_px: 8
            }
        );
        assert_eq!(
            one('▁', 8, 16),
            BlockRect {
                x_px: 0,
                y_px: 14,
                width_px: 8,
                height_px: 2
            }
        );
        assert_eq!(
            one('▇', 8, 16),
            BlockRect {
                x_px: 0,
                y_px: 2,
                width_px: 8,
                height_px: 14
            }
        );
        assert_eq!(
            one('▉', 8, 16),
            BlockRect {
                x_px: 0,
                y_px: 0,
                width_px: 7,
                height_px: 16
            }
        );
        assert_eq!(
            one('▏', 8, 16),
            BlockRect {
                x_px: 0,
                y_px: 0,
                width_px: 1,
                height_px: 16
            }
        );
        assert_eq!(
            one('▐', 8, 16),
            BlockRect {
                x_px: 4,
                y_px: 0,
                width_px: 4,
                height_px: 16
            }
        );
        assert_eq!(
            one('▔', 8, 16),
            BlockRect {
                x_px: 0,
                y_px: 0,
                width_px: 8,
                height_px: 2
            }
        );
        assert_eq!(
            one('▕', 8, 16),
            BlockRect {
                x_px: 7,
                y_px: 0,
                width_px: 1,
                height_px: 16
            }
        );
    }

    #[test]
    fn quadrant_rectangles_share_edges_and_bound_the_cell() {
        let diagonal = plan('▞', 9, 23).expect("quadrant glyph"); // upper-right + lower-left
        assert_eq!(
            diagonal.rects(),
            &[
                BlockRect {
                    x_px: 5,
                    y_px: 0,
                    width_px: 4,
                    height_px: 12
                },
                BlockRect {
                    x_px: 0,
                    y_px: 12,
                    width_px: 5,
                    height_px: 11
                },
            ]
        );

        let all_but_upper_right = plan('▙', 9, 23).expect("three quadrant glyph");
        assert_eq!(all_but_upper_right.rects().len(), 3);
        assert!(
            all_but_upper_right
                .rects()
                .iter()
                .all(|rect| { rect.x_px + rect.width_px <= 9 && rect.y_px + rect.height_px <= 23 })
        );

        let all_but_lower_left = plan('▜', 9, 23).expect("upper and right quadrants");
        assert_eq!(all_but_lower_left.rects().len(), 3);
        let upper_right = plan('▝', 8, 16).expect("single upper-right quadrant");
        assert_eq!(
            upper_right.rects(),
            &[BlockRect {
                x_px: 4,
                y_px: 0,
                width_px: 4,
                height_px: 8
            }]
        );
        let upper_right_lower_left = plan('▞', 8, 16).expect("diagonal quadrants");
        assert_eq!(upper_right_lower_left.rects().len(), 2);
    }

    #[test]
    fn quadrant_codepoints_match_the_unicode_block_chart() {
        let expected = [
            ('▖', 0b0100),
            ('▗', 0b1000),
            ('▘', 0b0001),
            ('▙', 0b1101),
            ('▚', 0b1001),
            ('▛', 0b0111),
            ('▜', 0b1011),
            ('▝', 0b0010),
            ('▞', 0b0110),
            ('▟', 0b1110),
        ];
        for (character, mask) in expected {
            let plan = plan(character, 8, 8).expect("quadrant block");
            for (bit, x, y) in [(1, 2, 2), (2, 6, 2), (4, 2, 6), (8, 6, 6)] {
                let covered = plan.rects().iter().any(|rect| {
                    x >= rect.x_px
                        && x < rect.x_px + rect.width_px
                        && y >= rect.y_px
                        && y < rect.y_px + rect.height_px
                });
                assert_eq!(covered, mask & bit != 0, "{character}");
            }
        }
    }

    #[test]
    fn shade_glyphs_and_out_of_range_characters_use_font_fallback() {
        for character in ['░', '▒', '▓', 'A', ' '] {
            assert_eq!(plan(character, 8, 16), None);
        }
    }

    #[test]
    fn zero_sized_cells_are_rejected() {
        assert_eq!(plan('█', 0, 16), None);
        assert_eq!(plan('█', 8, 0), None);
    }
}
