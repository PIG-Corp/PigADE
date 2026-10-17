//! Bounded plain-text projections for typed cold rows and the live normal grid.
//!
//! The terminal host owns parsing and cells. This module only projects those
//! typed cells to UTF-8 text for history paging and search.

use crate::{Terminal, history};
use alacritty_terminal::{
    grid::{Dimensions, Grid},
    index::Line,
    term::cell::{Cell, Flags},
};
use anyhow::{Result, ensure};

pub const MAX_CHUNK_BYTES: usize = 64 * 1024;
const MAX_ROW_TEXT_BYTES: u64 = 16 * 1024 * 1024;

/// One UTF-8-safe slice of a projected row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextChunk {
    pub text: String,
    pub total_bytes: u64,
    pub offset: u64,
    pub next_offset: Option<u64>,
    pub wrap: bool,
}

fn chunk_from_chars<F, I>(chars: F, wrap: bool, offset: u64, max_bytes: usize) -> Result<TextChunk>
where
    F: Fn() -> I,
    I: Iterator<Item = char>,
{
    ensure!(
        (1..=MAX_CHUNK_BYTES).contains(&max_bytes),
        "text chunk size outside allowed range"
    );

    let mut total_bytes = 0u64;
    for character in chars() {
        total_bytes = total_bytes
            .checked_add(character.len_utf8() as u64)
            .ok_or_else(|| anyhow::anyhow!("row text byte count overflow"))?;
        ensure!(total_bytes <= MAX_ROW_TEXT_BYTES, "row text exceeds 16 MiB");
    }
    ensure!(offset <= total_bytes, "text chunk offset outside row");

    let mut position = 0u64;
    let mut text = String::new();
    text.try_reserve(max_bytes)?;
    let mut chunk_end = offset;
    for character in chars() {
        let character_bytes = character.len_utf8() as u64;
        let end = position + character_bytes;
        if position < offset {
            ensure!(end <= offset, "text chunk offset is not a UTF-8 boundary");
        } else if text.len() + character_bytes as usize <= max_bytes {
            text.push(character);
            chunk_end = end;
        } else if text.is_empty() {
            anyhow::bail!("text chunk size cannot fit the next UTF-8 scalar");
        } else {
            break;
        }
        position = end;
    }

    Ok(TextChunk {
        text,
        total_bytes,
        offset,
        next_offset: (chunk_end < total_bytes).then_some(chunk_end),
        wrap,
    })
}

/// Project a typed cold row into one bounded, UTF-8-safe text slice.
pub fn decoded_chunk(
    row: &history::DecodedRow,
    offset: u64,
    max_bytes: usize,
) -> Result<TextChunk> {
    let columns = usize::from(row.columns);
    ensure!(
        (1..=history::MAX_COLUMNS).contains(&columns)
            && row.cells.len() == columns
            && usize::from(row.occupied) <= columns,
        "invalid decoded row dimensions"
    );
    let wrap = row
        .cells
        .last()
        .is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE));
    let count = if wrap {
        columns
    } else {
        usize::from(row.occupied)
    };
    chunk_from_chars(
        || {
            row.cells[..count]
                .iter()
                .filter(|cell| {
                    !cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                        && !cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                })
                .flat_map(|cell| std::iter::once(cell.c).chain(cell.zerowidth.iter().copied()))
        },
        wrap,
        offset,
        max_bytes,
    )
}

fn live_row_chars(
    row: &alacritty_terminal::grid::Row<Cell>,
    count: usize,
) -> impl Iterator<Item = char> + '_ {
    row.into_iter()
        .take(count)
        .filter(|cell| {
            !cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                && !cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
        })
        .flat_map(|cell| {
            std::iter::once(cell.c).chain(cell.zerowidth().unwrap_or(&[]).iter().copied())
        })
}

fn row_count(grid: &Grid<Cell>) -> usize {
    grid.history_size() + grid.screen_lines()
}

impl Terminal {
    /// Number of retained normal-grid rows, oldest hot scrollback first.
    pub fn normal_text_row_count(&self) -> usize {
        row_count(self.term.normal_grid())
    }

    /// Project one retained normal-grid row. Index zero is the oldest hot row;
    /// the visible normal screen follows scrollback, including in alt screen.
    pub fn normal_text_chunk(
        &self,
        row_index: usize,
        offset: u64,
        max_bytes: usize,
    ) -> Result<TextChunk> {
        let grid = self.term.normal_grid();
        let history_size = grid.history_size();
        ensure!(
            row_index < row_count(grid),
            "normal text row index outside grid"
        );
        let line = Line(row_index as i32 - history_size as i32);
        let row = &grid[line];
        let wrap = row
            .last()
            .is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE));
        let count = if wrap { row.len() } else { row.occupied() };
        chunk_from_chars(|| live_row_chars(row, count), wrap, offset, max_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Geometry;
    use alacritty_terminal::vte::ansi::Color;

    fn decoded(text: &str, columns: usize, wrap: bool) -> history::DecodedRow {
        let mut cells = text
            .chars()
            .map(|c| history::DecodedCell {
                c,
                fg: Color::Named(alacritty_terminal::vte::ansi::NamedColor::Foreground),
                bg: Color::Named(alacritty_terminal::vte::ansi::NamedColor::Background),
                flags: Flags::empty(),
                zerowidth: Vec::new(),
                underline_color: None,
                hyperlink: None,
            })
            .collect::<Vec<_>>();
        cells.resize_with(columns, || history::DecodedCell {
            c: ' ',
            fg: Color::Named(alacritty_terminal::vte::ansi::NamedColor::Foreground),
            bg: Color::Named(alacritty_terminal::vte::ansi::NamedColor::Background),
            flags: Flags::empty(),
            zerowidth: Vec::new(),
            underline_color: None,
            hyperlink: None,
        });
        if wrap {
            cells[columns - 1].flags.insert(Flags::WRAPLINE);
        }
        history::DecodedRow {
            columns: columns as u16,
            occupied: text.chars().count() as u16,
            hyperlinks: Vec::new(),
            cells,
        }
    }

    #[test]
    fn decoded_rows_chunk_unicode_at_scalar_boundaries_and_validate_offsets() -> Result<()> {
        let mut row = decoded("界e ", 4, false);
        row.cells[1].zerowidth.push('\u{301}');
        let first = decoded_chunk(&row, 0, 3)?;
        assert_eq!(first.text, "界");
        assert_eq!(first.total_bytes, 7);
        assert_eq!(first.next_offset, Some(3));
        let second = decoded_chunk(&row, 3, 4)?;
        assert_eq!(second.text, "e\u{301} ");
        assert_eq!(second.next_offset, None);
        assert!(decoded_chunk(&row, 1, 3).is_err());
        assert!(decoded_chunk(&row, 8, 3).is_err());
        Ok(())
    }

    #[test]
    fn decoded_projection_keeps_wrapped_extent_and_skips_wide_spacers() -> Result<()> {
        let mut row = decoded("界 x", 5, true);
        row.cells[1].flags.insert(Flags::WIDE_CHAR_SPACER);
        row.cells[4].flags.insert(Flags::LEADING_WIDE_CHAR_SPACER);
        let chunk = decoded_chunk(&row, 0, 64)?;
        assert_eq!(chunk.text, "界x ");
        assert!(chunk.wrap);
        Ok(())
    }

    #[test]
    fn live_normal_projection_works_during_alternate_screen_and_is_read_only() -> Result<()> {
        let mut terminal = Terminal::new(Geometry::new(3, 12)?, false);
        terminal.output(b"normal\r\nsecond\r\n");
        terminal.output(b"\x1b[?1049halt\x1b[2Jalternate");
        let before = terminal.checkpoint()?;
        let count = terminal.normal_text_row_count();
        assert!(count >= 3);
        assert!(
            terminal
                .normal_text_chunk(0, 0, 64)?
                .text
                .starts_with("normal")
        );
        assert_eq!(terminal.checkpoint()?, before);
        terminal.output(b"\x1b[?1049l");
        assert!(
            terminal
                .normal_text_chunk(count - 2, 0, 64)?
                .text
                .contains("second")
        );
        Ok(())
    }
}
