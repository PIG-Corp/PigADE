//! Bounded physical-cell projections for normal terminal history.
//!
//! The host owns parsing and decoded history. This module only copies typed
//! cells into the renderer's existing cell vocabulary; it never advances the
//! terminal or performs side effects.

use crate::{Cell, Terminal, history};
use alacritty_terminal::{grid::Dimensions, index::Line, term::cell::Cell as CoreCell};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const MAX_GRID_COLUMNS: u16 = 1024;
pub const MAX_GRID_ROW_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryGridRow {
    /// Physical source width. The returned `cells` may be clipped to requested width.
    pub source_columns: u16,
    pub cells: Vec<Cell>,
    pub hyperlinks: Vec<(String, String)>,
    pub horizontal_clipped: bool,
}

fn validate_width(cols: u16) -> Result<()> {
    ensure!(
        (1..=MAX_GRID_COLUMNS).contains(&cols),
        "history grid width outside 1..=1024"
    );
    Ok(())
}

fn validate_span(column_start: u16, cols: u16) -> Result<()> {
    validate_width(cols)?;
    ensure!(
        column_start
            .checked_add(cols)
            .is_some_and(|end| end <= MAX_GRID_COLUMNS),
        "history grid span outside 0..=1024"
    );
    Ok(())
}

fn core_row(
    row: &alacritty_terminal::grid::Row<CoreCell>,
    column_start: u16,
    cols: u16,
) -> Result<HistoryGridRow> {
    validate_span(column_start, cols)?;
    let source_columns = u16::try_from(row.len())?;
    ensure!(
        source_columns > 0 && source_columns <= MAX_GRID_COLUMNS,
        "invalid history row width"
    );
    let count = usize::from(cols.min(source_columns.saturating_sub(column_start)));
    let mut cells = Vec::new();
    cells.try_reserve_exact(count)?;
    let mut hyperlinks = Vec::new();
    let mut link_indices = std::collections::HashMap::new();
    let mut byte_count = 0usize;
    for cell in row.into_iter().skip(usize::from(column_start)).take(count) {
        let extra = cell
            .zerowidth()
            .map_or(0, |chars| chars.iter().map(|ch| ch.len_utf8()).sum());
        byte_count = byte_count
            .checked_add(cell.c.len_utf8() + extra + 128)
            .ok_or_else(|| anyhow::anyhow!("history grid row byte count overflow"))?;
        ensure!(
            byte_count <= MAX_GRID_ROW_BYTES,
            "history grid row exceeds 1 MiB"
        );
        if let Some(link) = cell.hyperlink() {
            let key = (link.id().as_ptr() as usize, link.uri().as_ptr() as usize);
            if !link_indices.contains_key(&key) {
                byte_count = byte_count
                    .checked_add(link.id().len() + link.uri().len())
                    .ok_or_else(|| anyhow::anyhow!("history grid hyperlink size overflow"))?;
                ensure!(
                    byte_count <= MAX_GRID_ROW_BYTES,
                    "history grid row exceeds 1 MiB"
                );
            }
        }
        cells.push(core_cell(cell, &mut hyperlinks, &mut link_indices));
    }
    Ok(HistoryGridRow {
        source_columns,
        cells,
        hyperlinks,
        horizontal_clipped: column_start.saturating_add(cols) < source_columns,
    })
}

fn core_cell(
    cell: &CoreCell,
    hyperlinks: &mut Vec<(String, String)>,
    link_indices: &mut std::collections::HashMap<(usize, usize), u32>,
) -> Cell {
    let mut text = cell.c.to_string();
    if let Some(extra) = cell.zerowidth() {
        text.extend(extra);
    }
    let hyperlink = cell.hyperlink().map(|link| {
        let key = (link.id().as_ptr() as usize, link.uri().as_ptr() as usize);
        *link_indices.entry(key).or_insert_with(|| {
            let index = hyperlinks.len() as u32;
            hyperlinks.push((link.id().to_owned(), link.uri().to_owned()));
            index
        })
    });
    Cell {
        text,
        fg: format!("{:?}", cell.fg),
        bg: format!("{:?}", cell.bg),
        flags: cell.flags.bits(),
        underline: cell.underline_color().map(|color| format!("{color:?}")),
        hyperlink,
    }
}

/// Project one retained normal-grid row, oldest first, without changing model state.
impl Terminal {
    pub fn normal_history_grid_row(&self, row_index: usize, cols: u16) -> Result<HistoryGridRow> {
        self.normal_history_grid_span(row_index, 0, cols)
    }

    /// Project a bounded horizontal span of one retained normal-grid row.
    pub fn normal_history_grid_span(
        &self,
        row_index: usize,
        column_start: u16,
        cols: u16,
    ) -> Result<HistoryGridRow> {
        validate_span(column_start, cols)?;
        let grid = self.term.normal_grid();
        let history_size = grid.history_size();
        let row_count = history_size
            .checked_add(grid.screen_lines())
            .ok_or_else(|| anyhow::anyhow!("normal history row count overflow"))?;
        ensure!(
            row_index < row_count,
            "normal history row index outside grid"
        );
        let line = Line(i32::try_from(row_index)? - i32::try_from(history_size)?);
        core_row(&grid[line], column_start, cols)
    }

    /// Explicit dynamic colors currently governing normal hot rows.
    pub fn normal_history_palette(&self) -> Vec<(u16, [u8; 3])> {
        (0..alacritty_terminal::term::color::COUNT)
            .filter_map(|index| {
                self.term.colors()[index].map(|color| (index as u16, [color.r, color.g, color.b]))
            })
            .collect()
    }
}

/// Project a decoded cold row using the same style vocabulary as live frames.
pub fn decoded_row(row: &history::DecodedRow, cols: u16) -> Result<HistoryGridRow> {
    decoded_span(row, 0, cols)
}

/// Project a bounded horizontal span of a decoded cold row.
pub fn decoded_span(
    row: &history::DecodedRow,
    column_start: u16,
    cols: u16,
) -> Result<HistoryGridRow> {
    validate_span(column_start, cols)?;
    let source_columns = row.columns;
    ensure!(
        source_columns > 0
            && source_columns <= MAX_GRID_COLUMNS
            && row.cells.len() == usize::from(source_columns)
            && usize::from(row.occupied) <= row.cells.len(),
        "invalid decoded history row dimensions"
    );
    let count = usize::from(cols.min(source_columns.saturating_sub(column_start)));
    let mut cells = Vec::new();
    cells.try_reserve_exact(count)?;
    let mut hyperlinks = Vec::new();
    let mut link_indices = std::collections::HashMap::new();
    let mut byte_count = 0usize;
    for cell in row.cells.iter().skip(usize::from(column_start)).take(count) {
        let extra = cell.zerowidth.iter().map(|ch| ch.len_utf8()).sum::<usize>();
        byte_count = byte_count
            .checked_add(cell.c.len_utf8() + extra + 128)
            .ok_or_else(|| anyhow::anyhow!("history grid row byte count overflow"))?;
        ensure!(
            byte_count <= MAX_GRID_ROW_BYTES,
            "history grid row exceeds 1 MiB"
        );
        if let Some(index) = cell.hyperlink
            && !link_indices.contains_key(&index)
        {
            let link = row
                .hyperlinks
                .get(usize::from(index))
                .ok_or_else(|| anyhow::anyhow!("history hyperlink reference outside pool"))?;
            byte_count = byte_count
                .checked_add(link.id.len() + link.uri.len())
                .ok_or_else(|| anyhow::anyhow!("history grid hyperlink size overflow"))?;
            ensure!(
                byte_count <= MAX_GRID_ROW_BYTES,
                "history grid row exceeds 1 MiB"
            );
        }
        cells.push(decoded_cell(cell, row, &mut hyperlinks, &mut link_indices)?);
    }
    Ok(HistoryGridRow {
        source_columns,
        cells,
        hyperlinks,
        horizontal_clipped: column_start.saturating_add(cols) < source_columns,
    })
}

fn decoded_cell(
    cell: &history::DecodedCell,
    row: &history::DecodedRow,
    hyperlinks: &mut Vec<(String, String)>,
    link_indices: &mut std::collections::HashMap<u16, u32>,
) -> Result<Cell> {
    let mut text = cell.c.to_string();
    text.extend(cell.zerowidth.iter().copied());
    let hyperlink = cell
        .hyperlink
        .map(|index| {
            let link = row
                .hyperlinks
                .get(usize::from(index))
                .ok_or_else(|| anyhow::anyhow!("history hyperlink reference outside pool"))?;
            Ok::<u32, anyhow::Error>(*link_indices.entry(index).or_insert_with(|| {
                let index = hyperlinks.len() as u32;
                hyperlinks.push((link.id.clone(), link.uri.clone()));
                index
            }))
        })
        .transpose()?;
    Ok(Cell {
        text,
        fg: format!("{:?}", cell.fg),
        bg: format!("{:?}", cell.bg),
        flags: cell.flags.bits(),
        underline: cell.underline_color.map(|color| format!("{color:?}")),
        hyperlink,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

    #[test]
    fn decoded_projection_preserves_unicode_styles_links_and_clipping() -> Result<()> {
        let mut first = history::DecodedCell {
            c: '界',
            fg: Color::Spec(Rgb { r: 1, g: 2, b: 3 }),
            bg: Color::Named(NamedColor::Background),
            flags: Flags::BOLD | Flags::WIDE_CHAR,
            zerowidth: vec!['\u{301}'],
            underline_color: Some(Color::Indexed(4)),
            hyperlink: Some(0),
        };
        let second = history::DecodedCell {
            c: ' ',
            fg: first.fg,
            bg: first.bg,
            flags: Flags::WIDE_CHAR_SPACER,
            zerowidth: Vec::new(),
            underline_color: None,
            hyperlink: Some(0),
        };
        first.flags.insert(Flags::WRAPLINE);
        let row = history::DecodedRow {
            columns: 3,
            occupied: 2,
            hyperlinks: vec![history::DecodedHyperlink {
                id: "id".into(),
                uri: "https://example.invalid/".into(),
            }],
            cells: vec![
                first,
                second,
                history::DecodedCell {
                    c: 'x',
                    fg: Color::Named(NamedColor::Foreground),
                    bg: Color::Named(NamedColor::Background),
                    flags: Flags::empty(),
                    zerowidth: Vec::new(),
                    underline_color: None,
                    hyperlink: None,
                },
            ],
        };
        let projected = decoded_row(&row, 2)?;
        assert_eq!(projected.source_columns, 3);
        assert!(projected.horizontal_clipped);
        assert_eq!(projected.cells[0].text, "界\u{301}");
        assert_eq!(projected.cells[0].fg, format!("{:?}", row.cells[0].fg));
        assert_eq!(projected.cells[0].flags, row.cells[0].flags.bits());
        assert_eq!(
            projected.cells[0].underline,
            Some(format!("{:?}", row.cells[0].underline_color.unwrap()))
        );
        assert_eq!(projected.cells[0].hyperlink, Some(0));
        assert_eq!(projected.cells[1].flags, Flags::WIDE_CHAR_SPACER.bits());
        assert_eq!(
            projected.hyperlinks,
            vec![("id".into(), "https://example.invalid/".into())]
        );
        assert!(decoded_row(&row, 1025).is_err());
        let span = decoded_span(&row, 1, 2)?;
        assert_eq!(span.source_columns, 3);
        assert_eq!(span.cells, decoded_row(&row, 3)?.cells[1..]);
        assert!(!span.horizontal_clipped);
        assert!(decoded_span(&row, 3, 1)?.cells.is_empty());
        assert!(decoded_span(&row, 1023, 2).is_err());
        let clipped_wide = decoded_row(&row, 1)?;
        assert!(clipped_wide.horizontal_clipped);
        assert_eq!(clipped_wide.cells.len(), 1);
        assert!(clipped_wide.cells[0].flags & Flags::WIDE_CHAR.bits() != 0);
        Ok(())
    }

    #[test]
    fn hot_rows_match_live_frame_and_remain_normal_during_alt_screen() -> Result<()> {
        use crate::Geometry;

        let mut terminal = Terminal::new(Geometry::new(2, 8)?, false);
        terminal.output(b"\x1b]4;1;rgb:11/22/33\x07");
        terminal.output(b"\x1b[1;4;31mA\x1b]8;;https://example.invalid\x1b\\B\x1b]8;;\x1b\\");
        let row_index = terminal.normal_text_row_count() - 2;
        let frame = terminal.frame();
        let expected = frame.cells[..8].to_vec();
        let live = terminal.normal_history_grid_row(row_index, 8)?;
        assert_eq!(live.cells, expected);
        let span = terminal.normal_history_grid_span(row_index, 2, 3)?;
        assert_eq!(span.cells, expected[2..5]);
        assert_eq!(span.source_columns, 8);
        assert!(span.horizontal_clipped);
        assert!(live.cells[0].flags & Flags::BOLD.bits() != 0);
        assert!(live.cells[0].flags & Flags::UNDERLINE.bits() != 0);
        assert_eq!(live.cells[1].hyperlink, Some(0));
        assert_eq!(live.hyperlinks.len(), 1);
        assert!(live.hyperlinks[0].1.contains("example.invalid"));
        assert!(
            terminal
                .normal_history_palette()
                .contains(&(1, [0x11, 0x22, 0x33]))
        );

        terminal.output(b"\x1b[?1049h\x1b[2Jalternate");
        let before_read = terminal.checkpoint()?;
        assert_eq!(
            terminal.normal_history_grid_row(row_index, 8)?.cells,
            expected
        );
        assert_eq!(terminal.checkpoint()?, before_read);
        Ok(())
    }

    #[test]
    fn oversized_combining_text_and_link_payloads_are_rejected_before_projection() -> Result<()> {
        let mut row = history::DecodedRow {
            columns: 1,
            occupied: 1,
            hyperlinks: Vec::new(),
            cells: vec![history::DecodedCell {
                c: 'x',
                fg: Color::Named(NamedColor::Foreground),
                bg: Color::Named(NamedColor::Background),
                flags: Flags::empty(),
                zerowidth: vec!['a'; MAX_GRID_ROW_BYTES],
                underline_color: None,
                hyperlink: None,
            }],
        };
        assert!(decoded_row(&row, 1).is_err());

        row.cells[0].zerowidth.clear();
        row.cells[0].hyperlink = Some(0);
        row.hyperlinks.push(history::DecodedHyperlink {
            id: "id".into(),
            uri: "u".repeat(MAX_GRID_ROW_BYTES),
        });
        assert!(decoded_row(&row, 1).is_err());
        Ok(())
    }
}
