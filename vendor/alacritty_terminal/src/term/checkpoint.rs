//! PigADE experimental, versioned semantic Term checkpoint. This does not include
//! the VTE processor/parser: the owner must checkpoint it at the same sequence.
//! The decoder must also impose a byte limit BEFORE serde deserialization.
use std::fmt;

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
pub(crate) mod bounded;
pub(crate) mod pools;
pub use pools::{CellCheckpointV2, ExtraCheckpointV2, HyperlinkCheckpointV2};
use pools::{ExportPools, ImportPools};

use super::{Config, Osc52, TabStops, Term, TermDamageState, TermMode, color};
use crate::grid::checkpoint::GridCheckpointV1;
use crate::index::{Line, Point};
use crate::selection::Selection;
use crate::vi_mode::ViModeCursor;
use crate::vte::ansi::{CharsetIndex, CursorShape, CursorStyle, KeyboardModes, Rgb};

pub const CHECKPOINT_CORE: &str = "alacritty_terminal/0.26.0+pigade-state.2";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckpointError(pub &'static str);
impl fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for CheckpointError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShapeV1 {
    Block,
    Underline,
    Beam,
    HollowBlock,
    Hidden,
}
impl From<CursorShape> for CursorShapeV1 {
    fn from(value: CursorShape) -> Self {
        match value {
            CursorShape::Block => Self::Block,
            CursorShape::Underline => Self::Underline,
            CursorShape::Beam => Self::Beam,
            CursorShape::HollowBlock => Self::HollowBlock,
            CursorShape::Hidden => Self::Hidden,
        }
    }
}
impl From<CursorShapeV1> for CursorShape {
    fn from(value: CursorShapeV1) -> Self {
        match value {
            CursorShapeV1::Block => Self::Block,
            CursorShapeV1::Underline => Self::Underline,
            CursorShapeV1::Beam => Self::Beam,
            CursorShapeV1::HollowBlock => Self::HollowBlock,
            CursorShapeV1::Hidden => Self::Hidden,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CursorStyleV1 {
    pub shape: CursorShapeV1,
    pub blinking: bool,
}
impl From<CursorStyle> for CursorStyleV1 {
    fn from(value: CursorStyle) -> Self {
        Self {
            shape: value.shape.into(),
            blinking: value.blinking,
        }
    }
}
impl From<CursorStyleV1> for CursorStyle {
    fn from(value: CursorStyleV1) -> Self {
        Self {
            shape: value.shape.into(),
            blinking: value.blinking,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigCheckpointV1 {
    pub scrolling_history: usize,
    pub default_cursor_style: CursorStyleV1,
    pub vi_mode_cursor_style: Option<CursorStyleV1>,
    #[serde(deserialize_with = "bounded::text")]
    pub semantic_escape_chars: String,
    pub kitty_keyboard: bool,
    pub osc52: Osc52,
}
impl From<&Config> for ConfigCheckpointV1 {
    fn from(value: &Config) -> Self {
        Self {
            scrolling_history: value.scrolling_history,
            default_cursor_style: value.default_cursor_style.into(),
            vi_mode_cursor_style: value.vi_mode_cursor_style.map(Into::into),
            semantic_escape_chars: value.semantic_escape_chars.clone(),
            kitty_keyboard: value.kitty_keyboard,
            osc52: value.osc52,
        }
    }
}
impl From<ConfigCheckpointV1> for Config {
    fn from(value: ConfigCheckpointV1) -> Self {
        Self {
            scrolling_history: value.scrolling_history,
            default_cursor_style: value.default_cursor_style.into(),
            vi_mode_cursor_style: value.vi_mode_cursor_style.map(Into::into),
            semantic_escape_chars: value.semantic_escape_chars,
            kitty_keyboard: value.kitty_keyboard,
            osc52: value.osc52,
        }
    }
}

/// Every semantic field of Term; event sink and damage caches are recreated.
/// Term timers and partial UTF-8/CSI/OSC/synchronized buffers live in the
/// separate Processor checkpoint and must be published in the same envelope.
#[derive(Clone, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TermCheckpointV1 {
    pub schema: u32,
    pub core: String,
    pub is_focused: bool,
    pub vi_mode_cursor: Point,
    pub selection: Option<Selection>,
    pub grid: GridCheckpointV1,
    pub inactive_grid: GridCheckpointV1,
    pub extra_pool: Vec<ExtraCheckpointV2>,
    pub hyperlink_pool: Vec<HyperlinkCheckpointV2>,
    pub active_charset: u8,
    pub tabs: Vec<bool>,
    pub mode: u32,
    pub scroll_region: (i32, i32),
    pub colors: Vec<Option<[u8; 3]>>,
    pub cursor_style: Option<CursorStyleV1>,
    pub title: Option<String>,
    pub title_stack: Vec<Option<String>>,
    pub keyboard_mode_stack: Vec<u8>,
    pub inactive_keyboard_mode_stack: Vec<u8>,
    pub config: ConfigCheckpointV1,
    pub next_hyperlink_id: u64,
}

const MAX_COMBINING_CHARS: usize = 33_554_432;
fn index_count<I: Iterator<Item = u32> + Clone>(indices: I) -> Result<usize, CheckpointError> {
    let mut references = 0usize;
    let mut maximum = None;
    for index in indices.clone() {
        references += 1;
        maximum = Some(maximum.map_or(index, |old: u32| old.max(index)));
    }
    let Some(maximum) = maximum else {
        return Ok(0);
    };
    if maximum as usize >= references {
        return Err(CheckpointError("noncanonical checkpoint pool index"));
    }
    let mut seen = vec![false; maximum as usize + 1];
    for index in indices {
        seen[index as usize] = true;
    }
    if seen.iter().any(|value| !*value) {
        return Err(CheckpointError("checkpoint pool index gap"));
    }
    Ok(seen.len())
}
fn extra_count(
    grid: &GridCheckpointV1,
    inactive: &GridCheckpointV1,
) -> Result<usize, CheckpointError> {
    let indices = grid
        .rows
        .iter()
        .chain(&inactive.rows)
        .flat_map(|row| row.cells.iter())
        .filter_map(|cell| cell.extra)
        .chain(
            [
                grid.cursor.template.extra,
                grid.saved_cursor.template.extra,
                inactive.cursor.template.extra,
                inactive.saved_cursor.template.extra,
            ]
            .into_iter()
            .flatten(),
        );
    index_count(indices)
}
fn hyperlink_count(extras: &[ExtraCheckpointV2]) -> Result<usize, CheckpointError> {
    index_count(extras.iter().filter_map(|extra| extra.hyperlink))
}

impl<'de> Deserialize<'de> for TermCheckpointV1 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(field_identifier, rename_all = "snake_case")]
        enum Field {
            Schema,
            Core,
            IsFocused,
            ViModeCursor,
            Selection,
            Grid,
            InactiveGrid,
            ExtraPool,
            HyperlinkPool,
            ActiveCharset,
            Tabs,
            Mode,
            ScrollRegion,
            Colors,
            CursorStyle,
            Title,
            TitleStack,
            KeyboardModeStack,
            InactiveKeyboardModeStack,
            Config,
            NextHyperlinkId,
        }
        struct TermVisitor;
        impl<'de> Visitor<'de> for TermVisitor {
            type Value = TermCheckpointV1;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a bounded pooled terminal checkpoint")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let schema: u32 = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("schema"))?;
                if schema != 2 {
                    return Err(de::Error::custom("unsupported terminal checkpoint schema"));
                }
                let core: String = sequence
                    .next_element_seed(bounded::TextSeed(128))?
                    .ok_or_else(|| de::Error::missing_field("core"))?;
                if core != CHECKPOINT_CORE {
                    return Err(de::Error::custom("unsupported terminal checkpoint core"));
                }
                let is_focused: bool = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("is_focused"))?;
                let vi_mode_cursor: Point = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("vi_mode_cursor"))?;
                let selection: Option<Selection> = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("selection"))?;
                let grid: GridCheckpointV1 = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("grid"))?;
                let inactive_grid: GridCheckpointV1 = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("inactive_grid"))?;
                let extra_pool: Vec<ExtraCheckpointV2> = sequence
                    .next_element_seed(pools::ExtraPoolSeed::new(
                        extra_count(&grid, &inactive_grid).map_err(de::Error::custom)?,
                    ))?
                    .ok_or_else(|| de::Error::missing_field("extra_pool"))?;
                let hyperlink_pool: Vec<HyperlinkCheckpointV2> = sequence
                    .next_element_seed(bounded::VecSeed::exactly(
                        hyperlink_count(&extra_pool).map_err(de::Error::custom)?,
                    ))?
                    .ok_or_else(|| de::Error::missing_field("hyperlink_pool"))?;
                let active_charset: u8 = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("active_charset"))?;
                let tabs: Vec<bool> = sequence
                    .next_element_seed(bounded::VecSeed::up_to(4096))?
                    .ok_or_else(|| de::Error::missing_field("tabs"))?;
                let mode: u32 = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("mode"))?;
                let scroll_region: (i32, i32) = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("scroll_region"))?;
                let colors: Vec<Option<[u8; 3]>> = sequence
                    .next_element_seed(bounded::VecSeed::exactly(color::COUNT))?
                    .ok_or_else(|| de::Error::missing_field("colors"))?;
                let cursor_style: Option<CursorStyleV1> = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("cursor_style"))?;
                let title: Option<String> = sequence
                    .next_element_seed(bounded::OptionalTextSeed(65536))?
                    .ok_or_else(|| de::Error::missing_field("title"))?;
                let title_stack: Vec<Option<String>> = sequence
                    .next_element_seed(bounded::VecSeed::<bounded::OptionalText>::up_to(
                        super::TITLE_STACK_MAX_DEPTH,
                    ))?
                    .ok_or_else(|| de::Error::missing_field("title_stack"))?
                    .into_iter()
                    .map(|text| text.0)
                    .collect();
                let keyboard_mode_stack: Vec<u8> = sequence
                    .next_element_seed(bounded::VecSeed::up_to(
                        super::KEYBOARD_MODE_STACK_MAX_DEPTH,
                    ))?
                    .ok_or_else(|| de::Error::missing_field("keyboard_mode_stack"))?;
                let inactive_keyboard_mode_stack: Vec<u8> = sequence
                    .next_element_seed(bounded::VecSeed::up_to(
                        super::KEYBOARD_MODE_STACK_MAX_DEPTH,
                    ))?
                    .ok_or_else(|| de::Error::missing_field("inactive_keyboard_mode_stack"))?;
                let config: ConfigCheckpointV1 = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("config"))?;
                let next_hyperlink_id: u64 = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("next_hyperlink_id"))?;
                Ok(TermCheckpointV1 {
                    schema,
                    core,
                    is_focused,
                    vi_mode_cursor,
                    selection,
                    grid,
                    inactive_grid,
                    extra_pool,
                    hyperlink_pool,
                    active_charset,
                    tabs,
                    mode,
                    scroll_region,
                    colors,
                    cursor_style,
                    title,
                    title_stack,
                    keyboard_mode_stack,
                    inactive_keyboard_mode_stack,
                    config,
                    next_hyperlink_id,
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut schema: Option<u32> = None;
                let mut core: Option<String> = None;
                let mut is_focused: Option<bool> = None;
                let mut vi_mode_cursor: Option<Point> = None;
                let mut selection: Option<Option<Selection>> = None;
                let mut grid: Option<GridCheckpointV1> = None;
                let mut inactive_grid: Option<GridCheckpointV1> = None;
                let mut extra_pool: Option<Vec<ExtraCheckpointV2>> = None;
                let mut hyperlink_pool: Option<Vec<HyperlinkCheckpointV2>> = None;
                let mut active_charset: Option<u8> = None;
                let mut tabs: Option<Vec<bool>> = None;
                let mut mode: Option<u32> = None;
                let mut scroll_region: Option<(i32, i32)> = None;
                let mut colors: Option<Vec<Option<[u8; 3]>>> = None;
                let mut cursor_style: Option<Option<CursorStyleV1>> = None;
                let mut title: Option<Option<String>> = None;
                let mut title_stack: Option<Vec<Option<String>>> = None;
                let mut keyboard_mode_stack: Option<Vec<u8>> = None;
                let mut inactive_keyboard_mode_stack: Option<Vec<u8>> = None;
                let mut config: Option<ConfigCheckpointV1> = None;
                let mut next_hyperlink_id: Option<u64> = None;
                while let Some(field) = map.next_key()? {
                    match field {
                        Field::Schema => {
                            if schema.is_some() {
                                return Err(de::Error::duplicate_field("schema"));
                            }
                            schema = Some(map.next_value()?);
                        }
                        Field::Core => {
                            if core.is_some() {
                                return Err(de::Error::duplicate_field("core"));
                            }
                            core = Some(map.next_value_seed(bounded::TextSeed(128))?);
                        }
                        Field::IsFocused => {
                            if is_focused.is_some() {
                                return Err(de::Error::duplicate_field("is_focused"));
                            }
                            is_focused = Some(map.next_value()?);
                        }
                        Field::ViModeCursor => {
                            if vi_mode_cursor.is_some() {
                                return Err(de::Error::duplicate_field("vi_mode_cursor"));
                            }
                            vi_mode_cursor = Some(map.next_value()?);
                        }
                        Field::Selection => {
                            if selection.is_some() {
                                return Err(de::Error::duplicate_field("selection"));
                            }
                            selection = Some(map.next_value()?);
                        }
                        Field::Grid => {
                            if grid.is_some() {
                                return Err(de::Error::duplicate_field("grid"));
                            }
                            grid = Some(map.next_value()?);
                        }
                        Field::InactiveGrid => {
                            if inactive_grid.is_some() {
                                return Err(de::Error::duplicate_field("inactive_grid"));
                            }
                            inactive_grid = Some(map.next_value()?);
                        }
                        Field::ExtraPool => {
                            if extra_pool.is_some() {
                                return Err(de::Error::duplicate_field("extra_pool"));
                            }
                            extra_pool = Some(
                                map.next_value_seed(pools::ExtraPoolSeed::new(
                                    extra_count(
                                        grid.as_ref().ok_or_else(|| {
                                            de::Error::custom("grid must precede extra pool")
                                        })?,
                                        inactive_grid.as_ref().ok_or_else(|| {
                                            de::Error::custom(
                                                "inactive grid must precede extra pool",
                                            )
                                        })?,
                                    )
                                    .map_err(de::Error::custom)?,
                                ))?,
                            );
                        }
                        Field::HyperlinkPool => {
                            if hyperlink_pool.is_some() {
                                return Err(de::Error::duplicate_field("hyperlink_pool"));
                            }
                            hyperlink_pool = Some(
                                map.next_value_seed(bounded::VecSeed::exactly(
                                    hyperlink_count(extra_pool.as_ref().ok_or_else(|| {
                                        de::Error::custom("extra pool must precede hyperlinks")
                                    })?)
                                    .map_err(de::Error::custom)?,
                                ))?,
                            );
                        }
                        Field::ActiveCharset => {
                            if active_charset.is_some() {
                                return Err(de::Error::duplicate_field("active_charset"));
                            }
                            active_charset = Some(map.next_value()?);
                        }
                        Field::Tabs => {
                            if tabs.is_some() {
                                return Err(de::Error::duplicate_field("tabs"));
                            }
                            tabs = Some(map.next_value_seed(bounded::VecSeed::up_to(4096))?);
                        }
                        Field::Mode => {
                            if mode.is_some() {
                                return Err(de::Error::duplicate_field("mode"));
                            }
                            mode = Some(map.next_value()?);
                        }
                        Field::ScrollRegion => {
                            if scroll_region.is_some() {
                                return Err(de::Error::duplicate_field("scroll_region"));
                            }
                            scroll_region = Some(map.next_value()?);
                        }
                        Field::Colors => {
                            if colors.is_some() {
                                return Err(de::Error::duplicate_field("colors"));
                            }
                            colors =
                                Some(map.next_value_seed(bounded::VecSeed::exactly(color::COUNT))?);
                        }
                        Field::CursorStyle => {
                            if cursor_style.is_some() {
                                return Err(de::Error::duplicate_field("cursor_style"));
                            }
                            cursor_style = Some(map.next_value()?);
                        }
                        Field::Title => {
                            if title.is_some() {
                                return Err(de::Error::duplicate_field("title"));
                            }
                            title = Some(map.next_value_seed(bounded::OptionalTextSeed(65536))?);
                        }
                        Field::TitleStack => {
                            if title_stack.is_some() {
                                return Err(de::Error::duplicate_field("title_stack"));
                            }
                            title_stack = Some(
                                map.next_value_seed(
                                    bounded::VecSeed::<bounded::OptionalText>::up_to(
                                        super::TITLE_STACK_MAX_DEPTH,
                                    ),
                                )?
                                .into_iter()
                                .map(|text| text.0)
                                .collect(),
                            );
                        }
                        Field::KeyboardModeStack => {
                            if keyboard_mode_stack.is_some() {
                                return Err(de::Error::duplicate_field("keyboard_mode_stack"));
                            }
                            keyboard_mode_stack = Some(map.next_value_seed(
                                bounded::VecSeed::up_to(super::KEYBOARD_MODE_STACK_MAX_DEPTH),
                            )?);
                        }
                        Field::InactiveKeyboardModeStack => {
                            if inactive_keyboard_mode_stack.is_some() {
                                return Err(de::Error::duplicate_field(
                                    "inactive_keyboard_mode_stack",
                                ));
                            }
                            inactive_keyboard_mode_stack = Some(map.next_value_seed(
                                bounded::VecSeed::up_to(super::KEYBOARD_MODE_STACK_MAX_DEPTH),
                            )?);
                        }
                        Field::Config => {
                            if config.is_some() {
                                return Err(de::Error::duplicate_field("config"));
                            }
                            config = Some(map.next_value()?);
                        }
                        Field::NextHyperlinkId => {
                            if next_hyperlink_id.is_some() {
                                return Err(de::Error::duplicate_field("next_hyperlink_id"));
                            }
                            next_hyperlink_id = Some(map.next_value()?);
                        }
                    }
                }
                Ok(TermCheckpointV1 {
                    schema: schema.ok_or_else(|| de::Error::missing_field("schema"))?,
                    core: core.ok_or_else(|| de::Error::missing_field("core"))?,
                    is_focused: is_focused.ok_or_else(|| de::Error::missing_field("is_focused"))?,
                    vi_mode_cursor: vi_mode_cursor
                        .ok_or_else(|| de::Error::missing_field("vi_mode_cursor"))?,
                    selection: selection.ok_or_else(|| de::Error::missing_field("selection"))?,
                    grid: grid.ok_or_else(|| de::Error::missing_field("grid"))?,
                    inactive_grid: inactive_grid
                        .ok_or_else(|| de::Error::missing_field("inactive_grid"))?,
                    extra_pool: extra_pool.ok_or_else(|| de::Error::missing_field("extra_pool"))?,
                    hyperlink_pool: hyperlink_pool
                        .ok_or_else(|| de::Error::missing_field("hyperlink_pool"))?,
                    active_charset: active_charset
                        .ok_or_else(|| de::Error::missing_field("active_charset"))?,
                    tabs: tabs.ok_or_else(|| de::Error::missing_field("tabs"))?,
                    mode: mode.ok_or_else(|| de::Error::missing_field("mode"))?,
                    scroll_region: scroll_region
                        .ok_or_else(|| de::Error::missing_field("scroll_region"))?,
                    colors: colors.ok_or_else(|| de::Error::missing_field("colors"))?,
                    cursor_style: cursor_style
                        .ok_or_else(|| de::Error::missing_field("cursor_style"))?,
                    title: title.ok_or_else(|| de::Error::missing_field("title"))?,
                    title_stack: title_stack
                        .ok_or_else(|| de::Error::missing_field("title_stack"))?,
                    keyboard_mode_stack: keyboard_mode_stack
                        .ok_or_else(|| de::Error::missing_field("keyboard_mode_stack"))?,
                    inactive_keyboard_mode_stack: inactive_keyboard_mode_stack
                        .ok_or_else(|| de::Error::missing_field("inactive_keyboard_mode_stack"))?,
                    config: config.ok_or_else(|| de::Error::missing_field("config"))?,
                    next_hyperlink_id: next_hyperlink_id
                        .ok_or_else(|| de::Error::missing_field("next_hyperlink_id"))?,
                })
            }
        }
        deserializer.deserialize_struct(
            "TermCheckpointV1",
            &[
                "schema",
                "core",
                "is_focused",
                "vi_mode_cursor",
                "selection",
                "grid",
                "inactive_grid",
                "extra_pool",
                "hyperlink_pool",
                "active_charset",
                "tabs",
                "mode",
                "scroll_region",
                "colors",
                "cursor_style",
                "title",
                "title_stack",
                "keyboard_mode_stack",
                "inactive_keyboard_mode_stack",
                "config",
                "next_hyperlink_id",
            ],
            TermVisitor,
        )
    }
}
impl TermCheckpointV1 {
    pub fn validate(&self) -> Result<(), CheckpointError> {
        if self.schema != 2 || self.core != CHECKPOINT_CORE {
            return Err(CheckpointError(
                "unsupported terminal checkpoint schema/core",
            ));
        }
        self.grid.validate()?;
        self.inactive_grid.validate()?;
        let extra_count = extra_count(&self.grid, &self.inactive_grid)?;
        if self.extra_pool.len() != extra_count
            || self.hyperlink_pool.len() != hyperlink_count(&self.extra_pool)?
        {
            return Err(CheckpointError("checkpoint shared pool shape mismatch"));
        }
        let combining_count = self
            .extra_pool
            .iter()
            .try_fold(0usize, |sum, extra| sum.checked_add(extra.zerowidth.len()))
            .ok_or(CheckpointError("combining budget overflow"))?;
        if combining_count > MAX_COMBINING_CHARS
            || self
                .hyperlink_pool
                .iter()
                .any(|link| link.id.len() > 65536 || link.uri.len() > 65536)
            || self.title.as_ref().is_some_and(|title| title.len() > 65536)
            || self
                .title_stack
                .iter()
                .flatten()
                .any(|title| title.len() > 65536)
        {
            return Err(CheckpointError(
                "terminal text/combining decode budget exceeded",
            ));
        }
        let (normal, alternate) = if self.mode & TermMode::ALT_SCREEN.bits() != 0 {
            (&self.inactive_grid, &self.grid)
        } else {
            (&self.grid, &self.inactive_grid)
        };
        if normal.max_scroll_limit != self.config.scrolling_history
            || alternate.max_scroll_limit != 0
            || alternate.rows.len() != alternate.lines
        {
            return Err(CheckpointError(
                "terminal normal/alternate history profile mismatch",
            ));
        }

        if self.grid.columns != self.inactive_grid.columns
            || self.grid.lines != self.inactive_grid.lines
        {
            return Err(CheckpointError("terminal grids disagree on dimensions"));
        }
        if self.active_charset > 3 || self.tabs.len() != self.grid.columns {
            return Err(CheckpointError("invalid charset or tabstop shape"));
        }
        if self.scroll_region.0 < 0
            || self.scroll_region.1 <= self.scroll_region.0
            || self.scroll_region.1 as usize > self.grid.lines
        {
            return Err(CheckpointError("invalid terminal scroll region"));
        }
        if self.mode & TermMode::VI.bits() != 0 {
            let oldest_line = -((self.grid.rows.len() - self.grid.lines) as i32);
            if self.vi_mode_cursor.line.0 < oldest_line
                || self.vi_mode_cursor.line.0 >= self.grid.lines as i32
                || self.vi_mode_cursor.column.0 >= self.grid.columns
            {
                return Err(CheckpointError("vi cursor outside available grid"));
            }
        }
        if self.colors.len() != color::COUNT {
            return Err(CheckpointError("invalid terminal palette size"));
        }
        if self.title_stack.len() > super::TITLE_STACK_MAX_DEPTH
            || self.keyboard_mode_stack.len() > super::KEYBOARD_MODE_STACK_MAX_DEPTH
            || self.inactive_keyboard_mode_stack.len() > super::KEYBOARD_MODE_STACK_MAX_DEPTH
        {
            return Err(CheckpointError("terminal stack exceeds safety limit"));
        }
        if self
            .keyboard_mode_stack
            .iter()
            .chain(&self.inactive_keyboard_mode_stack)
            .any(|bits| KeyboardModes::from_bits(*bits).is_none())
        {
            return Err(CheckpointError("invalid keyboard mode bits"));
        }
        if self.config.scrolling_history > 10_000
            || self.config.semantic_escape_chars.len() > 65536
            || self.next_hyperlink_id == u64::MAX
        {
            return Err(CheckpointError(
                "terminal configuration outside checkpoint safety limits",
            ));
        }
        Ok(())
    }
}
impl<T> Term<T> {
    pub fn export_checkpoint(&self) -> TermCheckpointV1 {
        let mut pools = ExportPools::default();
        let grid = self.grid.export_checkpoint(&mut pools);
        let inactive_grid = self.inactive_grid.export_checkpoint(&mut pools);
        TermCheckpointV1 {
            schema: 2,
            core: CHECKPOINT_CORE.into(),
            is_focused: self.is_focused,
            vi_mode_cursor: self.vi_mode_cursor.point,
            selection: self.selection.clone(),
            grid,
            inactive_grid,
            extra_pool: pools.extras,
            hyperlink_pool: pools.hyperlinks,
            active_charset: self.active_charset as u8,
            tabs: self.tabs.tabs.clone(),
            mode: self.mode.bits(),
            scroll_region: (self.scroll_region.start.0, self.scroll_region.end.0),
            colors: (0..color::COUNT)
                .map(|index| self.colors[index].map(|rgb| [rgb.r, rgb.g, rgb.b]))
                .collect(),
            cursor_style: self.cursor_style.map(Into::into),
            title: self.title.clone(),
            title_stack: self.title_stack.clone(),
            keyboard_mode_stack: self
                .keyboard_mode_stack
                .iter()
                .map(|mode| mode.bits())
                .collect(),
            inactive_keyboard_mode_stack: self
                .inactive_keyboard_mode_stack
                .iter()
                .map(|mode| mode.bits())
                .collect(),
            config: (&self.config).into(),
            next_hyperlink_id: self.next_hyperlink_id,
        }
    }
    pub fn from_checkpoint(
        checkpoint: TermCheckpointV1,
        event_proxy: T,
    ) -> Result<Self, CheckpointError> {
        checkpoint.validate()?;
        let mut colors = color::Colors::default();
        for (index, color) in checkpoint.colors.into_iter().enumerate() {
            colors[index] = color.map(|[r, g, b]| Rgb { r, g, b });
        }
        let damage = TermDamageState::new(checkpoint.grid.columns, checkpoint.grid.lines);
        let pools = ImportPools::new(checkpoint.extra_pool, checkpoint.hyperlink_pool)?;
        Ok(Self {
            is_focused: checkpoint.is_focused,
            vi_mode_cursor: ViModeCursor::new(checkpoint.vi_mode_cursor),
            selection: checkpoint.selection,
            grid: checkpoint.grid.restore(&pools)?,
            inactive_grid: checkpoint.inactive_grid.restore(&pools)?,
            active_charset: [
                CharsetIndex::G0,
                CharsetIndex::G1,
                CharsetIndex::G2,
                CharsetIndex::G3,
            ][checkpoint.active_charset as usize],
            tabs: TabStops {
                tabs: checkpoint.tabs,
            },
            mode: TermMode::from_bits_retain(checkpoint.mode),
            scroll_region: Line(checkpoint.scroll_region.0)..Line(checkpoint.scroll_region.1),
            colors,
            cursor_style: checkpoint.cursor_style.map(Into::into),
            event_proxy,
            title: checkpoint.title,
            title_stack: checkpoint.title_stack,
            keyboard_mode_stack: checkpoint
                .keyboard_mode_stack
                .into_iter()
                .map(KeyboardModes::from_bits_retain)
                .collect(),
            inactive_keyboard_mode_stack: checkpoint
                .inactive_keyboard_mode_stack
                .into_iter()
                .map(KeyboardModes::from_bits_retain)
                .collect(),
            damage,
            config: checkpoint.config.into(),
            next_hyperlink_id: checkpoint.next_hyperlink_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventListener};
    use crate::grid::Scroll;
    use crate::index::{Column, Side};
    use crate::selection::SelectionType;
    use crate::term::test::TermSize;
    use crate::vte::ansi::Processor;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Listener(Arc<Mutex<usize>>);
    impl EventListener for Listener {
        fn send_event(&self, _: Event) {
            *self.0.lock().unwrap() += 1;
        }
    }
    fn terminal() -> Term<Listener> {
        let config = Config {
            kitty_keyboard: true,
            osc52: Osc52::Disabled,
            ..Config::default()
        };
        Term::new(config, &TermSize::new(40, 12), Listener::default())
    }
    fn state(term: &Term<Listener>) -> serde_json::Value {
        serde_json::to_value(term.export_checkpoint()).unwrap()
    }
    fn restore(term: &Term<Listener>) -> Term<Listener> {
        let wire = serde_json::to_vec(&term.export_checkpoint()).unwrap();
        let checkpoint = serde_json::from_slice(&wire).unwrap();
        let listener = Listener::default();
        let restored = Term::from_checkpoint(checkpoint, listener.clone()).unwrap();
        assert_eq!(
            *listener.0.lock().unwrap(),
            0,
            "import must not replay external effects"
        );
        assert_eq!(state(term), state(&restored));
        restored
    }
    fn feed(term: &mut Term<Listener>, bytes: &[u8]) {
        let mut parser = Processor::<crate::vte::ansi::StdSyncHandler>::new();
        parser.advance(term, bytes);
    }
    #[test]
    fn checkpoint_preserves_both_screens_cursors_history_and_continuation() {
        let mut original = terminal();
        for i in 0..80 {
            feed(
                &mut original,
                format!("line {i} Русский e\u{301} 界\r\n").as_bytes(),
            );
        }
        feed(
            &mut original,
            b"\x1b[38;2;12;34;56m\x1b[4;8H\x1b7normal saved\x1b[?1049h\x1b[2Jalternate",
        );
        feed(&mut original,b"\x1b[5;11H\x1b7\x1b[3;8H\x1b[44mattrs\x1b[3g\x1bH\x1b[2;10r\x1b[?6h\x1b[?2004h\x1b[?1006h");
        let mut restored = restore(&original);
        for bytes in [
            b"\x1b8restore\r\tTAB".as_slice(),
            b"\n\n\n\n\n\n\n\n\n\nscroll",
            b"\x1b[?1049lnormal again\x1b8saved",
            b"\x1b[?1049halt again",
        ] {
            feed(&mut original, bytes);
            feed(&mut restored, bytes);
            assert_eq!(state(&original), state(&restored));
        }
        for (columns, lines) in [(23, 8), (61, 18), (40, 12)] {
            original.resize(TermSize::new(columns, lines));
            restored.resize(TermSize::new(columns, lines));
            assert_eq!(state(&original), state(&restored));
            feed(&mut original, b"\x1b[?1049l\x1b8after resize");
            feed(&mut restored, b"\x1b[?1049l\x1b8after resize");
            assert_eq!(state(&original), state(&restored));
        }
    }
    #[test]
    fn checkpoint_preserves_title_keyboard_palette_charset_focus_and_selection() {
        let mut original = terminal();
        feed(&mut original,b"\x1b]0;base title\x07\x1b[22;0t\x1b]0;current title\x07\x1b[>3u\x1b[>7u\x1b]4;42;rgb:12/34/56\x07\x1b[5 q\x1b)0\x0eqx\x0f");
        original.is_focused = true;
        original.vi_mode_cursor = ViModeCursor::new(Point::new(Line(2), Column(4)));
        original.selection = Some(Selection::new(
            SelectionType::Block,
            Point::new(Line(0), Column(0)),
            Side::Left,
        ));
        let checkpoint = original.export_checkpoint();
        assert_eq!(checkpoint.title_stack.len(), 1);
        assert_eq!(checkpoint.keyboard_mode_stack.len(), 2);
        assert_eq!(checkpoint.colors[42], Some([0x12, 0x34, 0x56]));
        assert!(checkpoint.selection.is_some());
        assert!(checkpoint.is_focused);
        let mut restored = restore(&original);
        feed(&mut original, b"\x1b[23;0t\x1b[<1u\x0elqk\x0f\tZ");
        feed(&mut restored, b"\x1b[23;0t\x1b[<1u\x0elqk\x0f\tZ");
        assert_eq!(state(&original), state(&restored));
    }
    #[test]
    fn canonical_grid_preserves_ring_offset_wrap_and_display_history() {
        let mut original = terminal();
        for i in 0..123 {
            feed(
                &mut original,
                format!("{i:03} {}\r\n", "wrapped".repeat(13)).as_bytes(),
            );
        }
        original.scroll_display(Scroll::Delta(7));
        let mut restored = restore(&original);
        for text in ["tail\r\n", "末尾 e\u{301}\r\n"] {
            feed(&mut original, text.as_bytes());
            feed(&mut restored, text.as_bytes());
            assert_eq!(state(&original), state(&restored));
        }
    }
    #[test]
    fn anonymous_hyperlink_ids_are_per_terminal_and_survive_checkpoint() {
        let mut a = terminal();
        let mut b = terminal();
        let first = b"\x1b]8;;https://example.test/a\x07A\x1b]8;;\x07";
        feed(&mut a, first);
        feed(&mut b, first);
        assert_eq!(state(&a), state(&b));
        let mut restored = restore(&a);
        let next = b"\x1b]8;;https://example.test/b\x07B\x1b]8;;\x07";
        feed(&mut a, next);
        feed(&mut restored, next);
        assert_eq!(state(&a), state(&restored));
        assert_eq!(a.export_checkpoint().next_hyperlink_id, 2);
    }
    #[test]
    fn invalid_checkpoint_is_rejected_before_core_use() {
        let original = terminal().export_checkpoint();
        let mut invalid = original.clone();
        invalid.schema = 999;
        assert!(Term::from_checkpoint(invalid, Listener::default()).is_err());
        let mut invalid = original.clone();
        invalid.grid.cursor.point.column = Column(usize::MAX);
        assert!(Term::from_checkpoint(invalid, Listener::default()).is_err());
        let mut invalid = original.clone();
        invalid.grid.rows.pop();
        assert!(Term::from_checkpoint(invalid, Listener::default()).is_err());
        let mut invalid = original.clone();
        invalid.scroll_region = (9, 3);
        assert!(Term::from_checkpoint(invalid, Listener::default()).is_err());
        let mut invalid = original.clone();
        invalid.colors.clear();
        assert!(Term::from_checkpoint(invalid, Listener::default()).is_err());
        let mut invalid = original;
        invalid.inactive_keyboard_mode_stack.push(255);
        assert!(Term::from_checkpoint(invalid, Listener::default()).is_err());
    }
    #[derive(Clone, Default, Serialize, Deserialize)]
    struct LogicalTimeout(bool);
    impl crate::vte::ansi::Timeout for LogicalTimeout {
        fn set_timeout(&mut self, _: std::time::Duration) {
            self.0 = true;
        }
        fn clear_timeout(&mut self) {
            self.0 = false;
        }
        fn pending_timeout(&self) -> bool {
            self.0
        }
    }
    #[test]
    fn upstream_reference_corpus_composed_checkpoint_and_tail() {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct RefConfig {
            history_size: u32,
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ref");
        let mut paths: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect();
        paths.sort();
        let mut tested = 0;
        for path in paths {
            let recording_path = path.join("alacritty.recording");
            assert!(std::fs::metadata(&recording_path).unwrap().len() <= 2 * 1024 * 1024);
            let recording = std::fs::read(recording_path).unwrap();
            let size: TermSize =
                serde_json::from_slice(&std::fs::read(path.join("size.json")).unwrap()).unwrap();
            let config: RefConfig =
                serde_json::from_slice(&std::fs::read(path.join("config.json")).unwrap()).unwrap();
            let config = Config {
                scrolling_history: config.history_size as usize,
                ..Config::default()
            };
            let mut continuous = Term::new(config.clone(), &size, Listener::default());
            let mut continuous_parser = Processor::<LogicalTimeout>::new();
            continuous_parser.advance(&mut continuous, &recording);
            let expected = state(&continuous);
            for cut in [
                recording.len() / 3,
                recording.len() * 2 / 3,
                recording.len(),
            ] {
                let mut original = Term::new(config.clone(), &size, Listener::default());
                let mut parser = Processor::<LogicalTimeout>::new();
                parser.advance(&mut original, &recording[..cut]);
                let mut restored = restore(&original);
                let parser_wire = serde_json::to_vec(&parser.export_checkpoint()).unwrap();
                let mut restored_parser: Processor<LogicalTimeout> =
                    Processor::from_checkpoint(serde_json::from_slice(&parser_wire).unwrap())
                        .unwrap();
                parser.advance(&mut original, &recording[cut..]);
                restored_parser.advance(&mut restored, &recording[cut..]);
                assert_eq!(
                    state(&original),
                    expected,
                    "chunked fixture {} cut={cut}",
                    path.display()
                );
                assert_eq!(
                    state(&restored),
                    expected,
                    "checkpoint fixture {} cut={cut}",
                    path.display()
                );
                assert_eq!(
                    serde_json::to_value(parser.export_checkpoint()).unwrap(),
                    serde_json::to_value(restored_parser.export_checkpoint()).unwrap()
                );
                // Continue beyond the captured endpoint to expose latent cursor,
                // charset, margin, saved state and REP/parser differences.
                let tail = b"\x1b[?1049l\x1b8\r\tX\n\x1b[?1049hY\x1b[?1049l";
                parser.advance(&mut original, tail);
                restored_parser.advance(&mut restored, tail);
                assert_eq!(
                    state(&restored),
                    state(&original),
                    "fixture continuation {} cut={cut}",
                    path.display()
                );
            }
            tested += 1;
        }
        assert_eq!(
            tested, 45,
            "upstream corpus changed: update pinned evidence"
        );
    }
    #[test]
    fn pooled_long_hyperlink_is_small_and_preserves_sharing() {
        let mut original = terminal();
        let uri = format!("https://example.test/{}", "x".repeat(32768));
        let bytes = format!("\x1b]8;;{uri}\x07{}\x1b]8;;\x07", "A".repeat(4200));
        feed(&mut original, bytes.as_bytes());
        let checkpoint = original.export_checkpoint();
        checkpoint.validate().unwrap();
        assert_eq!(checkpoint.extra_pool.len(), 1);
        assert_eq!(checkpoint.hyperlink_pool.len(), 1);
        let encoded = postcard::to_allocvec(&checkpoint).unwrap();
        eprintln!(
            "pooled OSC8 regression raw_bytes={} checkpoint_bytes={}",
            bytes.len(),
            encoded.len()
        );
        assert!(
            encoded.len() < 256 * 1024,
            "raw={} snapshot={}",
            bytes.len(),
            encoded.len()
        );
        let imported: TermCheckpointV1 = postcard::from_bytes(&encoded).unwrap();
        let mut restored = Term::from_checkpoint(imported, Listener::default()).unwrap();
        assert_eq!(
            encoded,
            postcard::to_allocvec(&restored.export_checkpoint()).unwrap()
        );
        feed(&mut original, b"tail\x1b[?1049halt\x1b[?1049l");
        feed(&mut restored, b"tail\x1b[?1049halt\x1b[?1049l");
        assert_eq!(state(&original), state(&restored));
    }
    #[test]
    fn four_thousand_combining_marks_remain_restorable() {
        let mut original = terminal();
        feed(
            &mut original,
            format!("A{}", "\u{301}".repeat(4097)).as_bytes(),
        );
        let checkpoint = original.export_checkpoint();
        checkpoint.validate().unwrap();
        let encoded = postcard::to_allocvec(&checkpoint).unwrap();
        let mut restored =
            Term::from_checkpoint(postcard::from_bytes(&encoded).unwrap(), Listener::default())
                .unwrap();
        assert_eq!(state(&original), state(&restored));
        feed(&mut original, "\u{301}B".as_bytes());
        feed(&mut restored, "\u{301}B".as_bytes());
        assert_eq!(state(&original), state(&restored));
    }
    #[test]
    fn repeated_explicit_hyperlink_ids_reexport_canonically() {
        let mut original = terminal();
        feed(&mut original,b"\x1b]8;id=one;https://example.test/\x07one\x1b]8;;\x07\x1b]8;id=one;https://example.test/\x07two\x1b]8;;\x07");
        let encoded = postcard::to_allocvec(&original.export_checkpoint()).unwrap();
        let restored =
            Term::from_checkpoint(postcard::from_bytes(&encoded).unwrap(), Listener::default())
                .unwrap();
        assert_eq!(
            encoded,
            postcard::to_allocvec(&restored.export_checkpoint()).unwrap()
        );
    }
}
