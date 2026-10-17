//! PigADE experiment: explicit logical grid schema, including both cursors.
//!
//! This is separate from upstream Grid serde, which intentionally omits cursors.
use crate::term::checkpoint::{
    bounded,
    pools::{CellCheckpointV2, ExportPools, ImportPools},
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use std::fmt;

use super::{Charsets, Cursor, Grid, Row, Storage};
use crate::index::Point;
use crate::term::cell::Cell;
use crate::term::checkpoint::CheckpointError;
use crate::vte::ansi::{CharsetIndex, StandardCharset};

/// Frozen wire charset names rather than enum discriminants/private memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CharsetV1 {
    Ascii,
    LineDrawing,
}
impl From<StandardCharset> for CharsetV1 {
    fn from(value: StandardCharset) -> Self {
        match value {
            StandardCharset::Ascii => Self::Ascii,
            StandardCharset::SpecialCharacterAndLineDrawing => Self::LineDrawing,
        }
    }
}
impl From<CharsetV1> for StandardCharset {
    fn from(value: CharsetV1) -> Self {
        match value {
            CharsetV1::Ascii => Self::Ascii,
            CharsetV1::LineDrawing => Self::SpecialCharacterAndLineDrawing,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorCheckpointV1 {
    pub point: Point,
    pub template: CellCheckpointV2,
    pub charsets: [CharsetV1; 4],
    pub input_needs_wrap: bool,
}
impl CursorCheckpointV1 {
    fn export(cursor: &Cursor<Cell>, pools: &mut ExportPools) -> Self {
        let indices = [
            CharsetIndex::G0,
            CharsetIndex::G1,
            CharsetIndex::G2,
            CharsetIndex::G3,
        ];
        Self {
            point: cursor.point,
            template: pools.cell(&cursor.template),
            charsets: indices.map(|i| cursor.charsets[i].into()),
            input_needs_wrap: cursor.input_needs_wrap,
        }
    }
    fn restore(self, pools: &ImportPools) -> Result<Cursor<Cell>, CheckpointError> {
        let mut charsets = Charsets::default();
        for (index, value) in [
            CharsetIndex::G0,
            CharsetIndex::G1,
            CharsetIndex::G2,
            CharsetIndex::G3,
        ]
        .into_iter()
        .zip(self.charsets)
        {
            charsets[index] = value.into();
        }
        Ok(Cursor {
            point: self.point,
            template: pools.cell(self.template)?,
            charsets,
            input_needs_wrap: self.input_needs_wrap,
        })
    }
    fn validate(&self, lines: usize, columns: usize) -> Result<(), CheckpointError> {
        if self.point.line.0 < 0
            || self.point.line.0 as usize >= lines
            || self.point.column.0 >= columns
        {
            return Err(CheckpointError("cursor outside viewport"));
        }
        Ok(())
    }
}

/// Rows are canonicalized to ring offset zero and contain only active history
/// plus visible rows. A resize allocation cache is not semantic terminal state.
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GridCheckpointV1 {
    pub rows: Vec<RowCheckpointV2>,
    pub columns: usize,
    pub lines: usize,
    pub display_offset: usize,
    pub max_scroll_limit: usize,
    pub cursor: CursorCheckpointV1,
    pub saved_cursor: CursorCheckpointV1,
}

/// Decode one grid against both its historical per-grid cell cap and a caller
/// supplied aggregate budget. The aggregate budget lets a Term share one cap
/// across its normal and alternate grids without changing this grid's wire form.
pub(crate) struct GridCheckpointV1Seed<'a> {
    remaining: &'a mut usize,
    per_grid_limit: usize,
}

impl<'a> GridCheckpointV1Seed<'a> {
    pub(crate) fn new(remaining: &'a mut usize, per_grid_limit: usize) -> Self {
        Self {
            remaining,
            per_grid_limit,
        }
    }
}

impl<'de> DeserializeSeed<'de> for GridCheckpointV1Seed<'_> {
    type Value = GridCheckpointV1;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct GridVisitor<'a> {
            budget: &'a mut usize,
        }

        impl<'de> Visitor<'de> for GridVisitor<'_> {
            type Value = GridCheckpointV1;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded terminal grid checkpoint")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let rows = sequence
                    .next_element_seed(RowsSeed::new(self.budget))?
                    .ok_or_else(|| de::Error::missing_field("rows"))?;
                let columns = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("columns"))?;
                let lines = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("lines"))?;
                let display_offset = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("display_offset"))?;
                let max_scroll_limit = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("max_scroll_limit"))?;
                let cursor = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("cursor"))?;
                let saved_cursor = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("saved_cursor"))?;
                let grid = GridCheckpointV1 {
                    rows,
                    columns,
                    lines,
                    display_offset,
                    max_scroll_limit,
                    cursor,
                    saved_cursor,
                };
                grid.validate().map_err(de::Error::custom)?;
                Ok(grid)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                #[derive(Deserialize)]
                #[serde(field_identifier, rename_all = "snake_case")]
                enum Field {
                    Rows,
                    Columns,
                    Lines,
                    DisplayOffset,
                    MaxScrollLimit,
                    Cursor,
                    SavedCursor,
                }

                let mut rows = None;
                let mut columns = None;
                let mut lines = None;
                let mut display_offset = None;
                let mut max_scroll_limit = None;
                let mut cursor = None;
                let mut saved_cursor = None;
                while let Some(field) = map.next_key()? {
                    match field {
                        Field::Rows => {
                            if rows.is_some() {
                                return Err(de::Error::duplicate_field("rows"));
                            }
                            rows = Some(map.next_value_seed(RowsSeed::new(self.budget))?);
                        }
                        Field::Columns => set_once(&mut columns, map.next_value()?, "columns")?,
                        Field::Lines => set_once(&mut lines, map.next_value()?, "lines")?,
                        Field::DisplayOffset => {
                            set_once(&mut display_offset, map.next_value()?, "display_offset")?
                        }
                        Field::MaxScrollLimit => {
                            set_once(&mut max_scroll_limit, map.next_value()?, "max_scroll_limit")?
                        }
                        Field::Cursor => set_once(&mut cursor, map.next_value()?, "cursor")?,
                        Field::SavedCursor => {
                            set_once(&mut saved_cursor, map.next_value()?, "saved_cursor")?
                        }
                    }
                }
                let grid = GridCheckpointV1 {
                    rows: rows.ok_or_else(|| de::Error::missing_field("rows"))?,
                    columns: columns.ok_or_else(|| de::Error::missing_field("columns"))?,
                    lines: lines.ok_or_else(|| de::Error::missing_field("lines"))?,
                    display_offset: display_offset
                        .ok_or_else(|| de::Error::missing_field("display_offset"))?,
                    max_scroll_limit: max_scroll_limit
                        .ok_or_else(|| de::Error::missing_field("max_scroll_limit"))?,
                    cursor: cursor.ok_or_else(|| de::Error::missing_field("cursor"))?,
                    saved_cursor: saved_cursor
                        .ok_or_else(|| de::Error::missing_field("saved_cursor"))?,
                };
                grid.validate().map_err(de::Error::custom)?;
                Ok(grid)
            }
        }

        fn set_once<T, E: de::Error>(
            slot: &mut Option<T>,
            value: T,
            name: &'static str,
        ) -> Result<(), E> {
            if slot.is_some() {
                return Err(E::duplicate_field(name));
            }
            *slot = Some(value);
            Ok(())
        }

        let initial = (*self.remaining).min(self.per_grid_limit);
        let mut budget = initial;
        let grid = deserializer.deserialize_struct(
            "GridCheckpointV1",
            &[
                "rows",
                "columns",
                "lines",
                "display_offset",
                "max_scroll_limit",
                "cursor",
                "saved_cursor",
            ],
            GridVisitor {
                budget: &mut budget,
            },
        )?;
        *self.remaining -= initial - budget;
        Ok(grid)
    }
}

impl<'de> Deserialize<'de> for GridCheckpointV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut remaining = 16_777_216;
        GridCheckpointV1Seed::new(&mut remaining, 16_777_216).deserialize(deserializer)
    }
}
impl GridCheckpointV1 {
    pub fn validate(&self) -> Result<(), CheckpointError> {
        if self.columns < 2 || self.columns > 4096 || self.lines == 0 || self.lines > 512 {
            return Err(CheckpointError(
                "grid dimensions outside checkpoint safety limits",
            ));
        }
        if self.max_scroll_limit > 10_000
            || self.rows.len() < self.lines
            || self.rows.len() - self.lines > self.max_scroll_limit
            || self.display_offset > self.rows.len() - self.lines
        {
            return Err(CheckpointError("invalid grid history bounds"));
        }
        if self
            .rows
            .len()
            .checked_mul(self.columns)
            .is_none_or(|n| n > 16_777_216)
        {
            return Err(CheckpointError("grid checkpoint exceeds cell safety limit"));
        }
        if self
            .rows
            .iter()
            .any(|row| row.cells.len() != self.columns || row.occupied > self.columns)
        {
            return Err(CheckpointError("invalid grid row shape"));
        }
        self.cursor.validate(self.lines, self.columns)?;
        self.saved_cursor.validate(self.lines, self.columns)?;
        Ok(())
    }
    pub(crate) fn restore(self, pools: &ImportPools) -> Result<Grid<Cell>, CheckpointError> {
        self.validate()?;
        let rows = self
            .rows
            .into_iter()
            .map(|row| {
                let cells = row
                    .cells
                    .into_iter()
                    .map(|cell| pools.cell(cell))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Row::from_vec(cells, row.occupied))
            })
            .collect::<Result<Vec<_>, CheckpointError>>()?;
        Ok(Grid {
            raw: Storage::from_checkpoint_rows(rows, self.lines),
            history_observer: None,
            columns: self.columns,
            lines: self.lines,
            display_offset: self.display_offset,
            max_scroll_limit: self.max_scroll_limit,
            cursor: self.cursor.restore(pools)?,
            saved_cursor: self.saved_cursor.restore(pools)?,
        })
    }
}
impl Grid<Cell> {
    pub(crate) fn export_checkpoint(&self, pools: &mut ExportPools) -> GridCheckpointV1 {
        GridCheckpointV1 {
            rows: self
                .raw
                .checkpoint_rows()
                .map(|row| {
                    let cells = (0..self.columns)
                        .map(|column| pools.cell(&row[crate::index::Column(column)]))
                        .collect();
                    RowCheckpointV2 {
                        cells,
                        occupied: row.occ,
                    }
                })
                .collect(),
            columns: self.columns,
            lines: self.lines,
            display_offset: self.display_offset,
            max_scroll_limit: self.max_scroll_limit,
            cursor: CursorCheckpointV1::export(&self.cursor, pools),
            saved_cursor: CursorCheckpointV1::export(&self.saved_cursor, pools),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RowCheckpointV2 {
    pub cells: Vec<CellCheckpointV2>,
    pub occupied: usize,
}

struct RowSeed<'a>(&'a mut usize);
impl<'de> DeserializeSeed<'de> for RowSeed<'_> {
    type Value = RowCheckpointV2;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct RowVisitor<'a>(&'a mut usize);
        impl<'de> Visitor<'de> for RowVisitor<'_> {
            type Value = RowCheckpointV2;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded checkpoint row")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let cells = sequence
                    .next_element_seed(CellsSeed(self.0))?
                    .ok_or_else(|| de::Error::missing_field("cells"))?;
                let occupied = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("occupied"))?;
                finish_row(cells, occupied)
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                #[derive(Deserialize)]
                #[serde(field_identifier, rename_all = "snake_case")]
                enum Field {
                    Cells,
                    Occupied,
                }
                let mut cells = None;
                let mut occupied = None;
                while let Some(field) = map.next_key()? {
                    match field {
                        Field::Cells => {
                            if cells.is_some() {
                                return Err(de::Error::duplicate_field("cells"));
                            }
                            cells = Some(map.next_value_seed(CellsSeed(self.0))?);
                        }
                        Field::Occupied => {
                            if occupied.is_some() {
                                return Err(de::Error::duplicate_field("occupied"));
                            }
                            occupied = Some(map.next_value()?);
                        }
                    }
                }
                finish_row(
                    cells.ok_or_else(|| de::Error::missing_field("cells"))?,
                    occupied.ok_or_else(|| de::Error::missing_field("occupied"))?,
                )
            }
        }
        deserializer.deserialize_struct(
            "RowCheckpointV2",
            &["cells", "occupied"],
            RowVisitor(self.0),
        )
    }
}
fn finish_row<E: de::Error>(
    cells: Vec<CellCheckpointV2>,
    occupied: usize,
) -> Result<RowCheckpointV2, E> {
    if cells.len() < 2 || occupied > cells.len() {
        return Err(E::custom("invalid checkpoint row shape"));
    }
    Ok(RowCheckpointV2 { cells, occupied })
}
struct CellsSeed<'a>(&'a mut usize);
impl<'de> DeserializeSeed<'de> for CellsSeed<'_> {
    type Value = Vec<CellCheckpointV2>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct CellsVisitor<'a>(&'a mut usize);
        impl<'de> Visitor<'de> for CellsVisitor<'_> {
            type Value = Vec<CellCheckpointV2>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("checkpoint cells within row and grid budgets")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let limit = 4096.min(*self.0);
                if sequence.size_hint().is_some_and(|hint| hint > limit) {
                    return Err(de::Error::custom("checkpoint row/grid cell limit exceeded"));
                }
                let mut cells = Vec::new();
                while cells.len() < limit {
                    match sequence.next_element()? {
                        Some(cell) => {
                            cells.push(cell);
                            *self.0 -= 1;
                        }
                        None => return Ok(cells),
                    }
                }
                if sequence
                    .next_element_seed(bounded::Reject("checkpoint row/grid cell limit exceeded"))?
                    .is_some()
                {
                    unreachable!();
                }
                Ok(cells)
            }
        }
        deserializer.deserialize_seq(CellsVisitor(self.0))
    }
}
struct RowsSeed<'a> {
    budget: &'a mut usize,
}

impl<'a> RowsSeed<'a> {
    fn new(budget: &'a mut usize) -> Self {
        Self { budget }
    }
}

impl<'de> DeserializeSeed<'de> for RowsSeed<'_> {
    type Value = Vec<RowCheckpointV2>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct RowsVisitor<'a> {
            budget: &'a mut usize,
        }
        impl<'de> Visitor<'de> for RowsVisitor<'_> {
            type Value = Vec<RowCheckpointV2>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("at most 10512 terminal rows within the cell budget")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                const MAX_ROWS: usize = 10_512;
                if sequence.size_hint().is_some_and(|hint| hint > MAX_ROWS) {
                    return Err(de::Error::custom("checkpoint row count limit exceeded"));
                }
                let mut rows = Vec::new();
                while rows.len() < MAX_ROWS {
                    match sequence.next_element_seed(RowSeed(self.budget))? {
                        Some(row) => rows.push(row),
                        None => return Ok(rows),
                    }
                }
                if sequence
                    .next_element_seed(bounded::Reject("checkpoint row count limit exceeded"))?
                    .is_some()
                {
                    unreachable!();
                }
                Ok(rows)
            }
        }
        deserializer.deserialize_seq(RowsVisitor {
            budget: self.budget,
        })
    }
}

#[cfg(test)]
mod decode_tests {
    use super::*;
    use crate::{
        index::{Column, Line},
        term::checkpoint::pools::CellCheckpointV2,
        vte::ansi::{Color, NamedColor},
    };
    use serde::de::value::{Error, SeqDeserializer};

    fn small_grid() -> GridCheckpointV1 {
        let cell = CellCheckpointV2 {
            c: ' ',
            fg: Color::Named(NamedColor::White),
            bg: Color::Named(NamedColor::Black),
            flags: crate::term::cell::Flags::empty(),
            extra: None,
        };
        let cursor = CursorCheckpointV1 {
            point: Point::new(Line(0), Column(0)),
            template: cell.clone(),
            charsets: [CharsetV1::Ascii; 4],
            input_needs_wrap: false,
        };
        GridCheckpointV1 {
            rows: vec![RowCheckpointV2 {
                cells: vec![cell.clone(), cell],
                occupied: 0,
            }],
            columns: 2,
            lines: 1,
            display_offset: 0,
            max_scroll_limit: 0,
            cursor: cursor.clone(),
            saved_cursor: cursor,
        }
    }
    struct AdvertisedRows;
    impl Iterator for AdvertisedRows {
        type Item = u8;
        fn next(&mut self) -> Option<u8> {
            panic!("oversized count must be rejected before reading or allocating rows")
        }
        fn size_hint(&self) -> (usize, Option<usize>) {
            (5_000_000, Some(5_000_000))
        }
    }
    #[test]
    fn advertised_five_million_rows_rejected_before_first_element() {
        let decoder = SeqDeserializer::<_, Error>::new(AdvertisedRows);
        let mut remaining = 16_777_216;
        let error = RowsSeed::new(&mut remaining)
            .deserialize(decoder)
            .unwrap_err();
        assert!(error.to_string().contains("row count"));
    }
    #[test]
    fn postcard_malicious_row_count_header_is_rejected() {
        struct BadCount;
        impl Serialize for BadCount {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                use serde::ser::SerializeSeq;
                serializer.serialize_seq(Some(5_000_000))?.end()
            }
        }
        // Rows are the first field in the frozen Grid wire schema. No five
        // million-element container or encoded body is materialized in this test.
        let header = postcard::to_allocvec(&BadCount).unwrap();
        assert!(header.len() < 8);
        assert!(postcard::from_bytes::<GridCheckpointV1>(&header).is_err());
    }
    #[test]
    fn aggregate_cell_budget_rejects_row_before_reading_cells() {
        let mut remaining = 1;
        let decoder = SeqDeserializer::<_, Error>::new([0u8, 0].into_iter());
        let error = CellsSeed(&mut remaining).deserialize(decoder).unwrap_err();
        assert!(error.to_string().contains("cell limit"));
        assert_eq!(remaining, 1);
    }

    #[test]
    fn shared_grid_budget_rejects_second_grid_cells_before_decoding_them() {
        let encoded = serde_json::to_string(&small_grid()).unwrap();
        let mut remaining = 2;
        let mut first = serde_json::Deserializer::from_str(&encoded);
        let decoded = GridCheckpointV1Seed::new(&mut remaining, 16_777_216)
            .deserialize(&mut first)
            .unwrap();
        assert_eq!(decoded.rows[0].cells.len(), 2);
        assert_eq!(remaining, 0);

        let mut second = serde_json::Deserializer::from_str(&encoded);
        let error = GridCheckpointV1Seed::new(&mut remaining, 16_777_216)
            .deserialize(&mut second)
            .unwrap_err();
        assert!(error.to_string().contains("cell limit"), "{error}");
        assert_eq!(remaining, 0);
    }
}
