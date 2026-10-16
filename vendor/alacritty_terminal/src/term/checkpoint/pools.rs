//! Semantic cell DTOs with shared extras and hyperlink pools. This preserves
//! sharing instead of repeating a large OSC8 URI in every serialized cell.
use super::{CheckpointError, bounded};
use crate::term::cell::{Cell, CellExtra, Flags, Hyperlink};
use crate::vte::ansi::Color;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellCheckpointV2 {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    pub extra: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtraCheckpointV2 {
    #[serde(deserialize_with = "bounded::combining")]
    pub zerowidth: Vec<char>,
    pub underline_color: Option<Color>,
    pub hyperlink: Option<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HyperlinkCheckpointV2 {
    #[serde(deserialize_with = "bounded::text")]
    pub id: String,
    #[serde(deserialize_with = "bounded::text")]
    pub uri: String,
}
#[derive(Default)]
pub(crate) struct ExportPools {
    extra_ids: HashMap<*const CellExtra, u32>,
    link_ids: HashMap<*const (), u32>,
    link_values: HashMap<Hyperlink, u32>,
    pub extras: Vec<ExtraCheckpointV2>,
    pub hyperlinks: Vec<HyperlinkCheckpointV2>,
}
impl ExportPools {
    pub fn cell(&mut self, cell: &Cell) -> CellCheckpointV2 {
        let extra = cell.extra.as_ref().map(|extra| {
            let pointer = Arc::as_ptr(extra);
            if let Some(index) = self.extra_ids.get(&pointer) {
                return *index;
            }
            let (zerowidth, underline_color, hyperlink) = extra.checkpoint_parts();
            let hyperlink = hyperlink.map(|link| {
                let pointer = link.checkpoint_identity();
                if let Some(index) = self.link_ids.get(&pointer) {
                    return *index;
                }
                if let Some(index) = self.link_values.get(&link) {
                    self.link_ids.insert(pointer, *index);
                    return *index;
                }
                let index = self.hyperlinks.len() as u32;
                self.hyperlinks.push(HyperlinkCheckpointV2 {
                    id: link.id().to_owned(),
                    uri: link.uri().to_owned(),
                });
                self.link_ids.insert(pointer, index);
                self.link_values.insert(link, index);
                index
            });
            let index = self.extras.len() as u32;
            self.extras.push(ExtraCheckpointV2 {
                zerowidth,
                underline_color,
                hyperlink,
            });
            self.extra_ids.insert(pointer, index);
            index
        });
        CellCheckpointV2 {
            c: cell.c,
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags,
            extra,
        }
    }
}
pub(crate) struct ImportPools {
    extras: Vec<Arc<CellExtra>>,
}
impl ImportPools {
    pub fn new(
        extras: Vec<ExtraCheckpointV2>,
        links: Vec<HyperlinkCheckpointV2>,
    ) -> Result<Self, CheckpointError> {
        let links: Vec<_> = links
            .into_iter()
            .map(|link| Hyperlink::new(Some(link.id), link.uri))
            .collect();
        let mut restored = Vec::with_capacity(extras.len());
        for extra in extras {
            let hyperlink = extra
                .hyperlink
                .map(|index| {
                    links
                        .get(index as usize)
                        .cloned()
                        .ok_or(CheckpointError("invalid hyperlink pool index"))
                })
                .transpose()?;
            restored.push(Arc::new(CellExtra::from_checkpoint_parts(
                extra.zerowidth,
                extra.underline_color,
                hyperlink,
            )));
        }
        Ok(Self { extras: restored })
    }
    pub fn cell(&self, cell: CellCheckpointV2) -> Result<Cell, CheckpointError> {
        let extra = cell
            .extra
            .map(|index| {
                self.extras
                    .get(index as usize)
                    .cloned()
                    .ok_or(CheckpointError("invalid extra pool index"))
            })
            .transpose()?;
        Ok(Cell {
            c: cell.c,
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags,
            extra,
        })
    }
}

use serde::{
    Deserializer,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use std::fmt;

pub(crate) struct ExtraPoolSeed {
    count: usize,
}
impl ExtraPoolSeed {
    pub fn new(count: usize) -> Self {
        Self { count }
    }
}
impl<'de> DeserializeSeed<'de> for ExtraPoolSeed {
    type Value = Vec<ExtraCheckpointV2>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct PoolVisitor(usize);
        impl<'de> Visitor<'de> for PoolVisitor {
            type Value = Vec<ExtraCheckpointV2>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "exactly {} referenced extras", self.0)
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                if sequence.size_hint().is_some_and(|hint| hint != self.0) {
                    return Err(de::Error::custom("checkpoint extra pool length mismatch"));
                }
                let mut values = Vec::new();
                let mut combining = super::MAX_COMBINING_CHARS;
                for _ in 0..self.0 {
                    values.push(
                        sequence
                            .next_element_seed(ExtraSeed(&mut combining))?
                            .ok_or_else(|| de::Error::custom("checkpoint extra pool truncated"))?,
                    );
                }
                if sequence
                    .next_element_seed(bounded::Reject("checkpoint extra pool too long"))?
                    .is_some()
                {
                    unreachable!();
                }
                Ok(values)
            }
        }
        deserializer.deserialize_seq(PoolVisitor(self.count))
    }
}
struct ExtraSeed<'a>(&'a mut usize);
impl<'de> DeserializeSeed<'de> for ExtraSeed<'_> {
    type Value = ExtraCheckpointV2;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct ExtraVisitor<'a>(&'a mut usize);
        impl<'de> Visitor<'de> for ExtraVisitor<'_> {
            type Value = ExtraCheckpointV2;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a pooled extra within the aggregate combining budget")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let zerowidth = sequence
                    .next_element_seed(CombiningSeed(self.0))?
                    .ok_or_else(|| de::Error::missing_field("zerowidth"))?;
                let underline_color = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("underline_color"))?;
                let hyperlink = sequence
                    .next_element()?
                    .ok_or_else(|| de::Error::missing_field("hyperlink"))?;
                Ok(ExtraCheckpointV2 {
                    zerowidth,
                    underline_color,
                    hyperlink,
                })
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                #[derive(Deserialize)]
                #[serde(field_identifier, rename_all = "snake_case")]
                enum Field {
                    Zerowidth,
                    UnderlineColor,
                    Hyperlink,
                }
                let mut zerowidth = None;
                let mut underline_color = None;
                let mut hyperlink = None;
                while let Some(field) = map.next_key()? {
                    match field {
                        Field::Zerowidth => {
                            if zerowidth.is_some() {
                                return Err(de::Error::duplicate_field("zerowidth"));
                            }
                            zerowidth = Some(map.next_value_seed(CombiningSeed(self.0))?);
                        }
                        Field::UnderlineColor => {
                            if underline_color.is_some() {
                                return Err(de::Error::duplicate_field("underline_color"));
                            }
                            underline_color = Some(map.next_value()?);
                        }
                        Field::Hyperlink => {
                            if hyperlink.is_some() {
                                return Err(de::Error::duplicate_field("hyperlink"));
                            }
                            hyperlink = Some(map.next_value()?);
                        }
                    }
                }
                Ok(ExtraCheckpointV2 {
                    zerowidth: zerowidth.ok_or_else(|| de::Error::missing_field("zerowidth"))?,
                    underline_color: underline_color
                        .ok_or_else(|| de::Error::missing_field("underline_color"))?,
                    hyperlink: hyperlink.ok_or_else(|| de::Error::missing_field("hyperlink"))?,
                })
            }
        }
        deserializer.deserialize_struct(
            "ExtraCheckpointV2",
            &["zerowidth", "underline_color", "hyperlink"],
            ExtraVisitor(self.0),
        )
    }
}
struct CombiningSeed<'a>(&'a mut usize);
impl<'de> DeserializeSeed<'de> for CombiningSeed<'_> {
    type Value = Vec<char>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct CombiningVisitor<'a>(&'a mut usize);
        impl<'de> Visitor<'de> for CombiningVisitor<'_> {
            type Value = Vec<char>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("characters within the terminal combining budget")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                if sequence.size_hint().is_some_and(|hint| hint > *self.0) {
                    return Err(de::Error::custom(
                        "checkpoint aggregate combining budget exceeded",
                    ));
                }
                let mut values = Vec::new();
                while *self.0 > 0 {
                    match sequence.next_element()? {
                        Some(value) => {
                            values.push(value);
                            *self.0 -= 1;
                        }
                        None => return Ok(values),
                    }
                }
                if sequence
                    .next_element_seed(bounded::Reject(
                        "checkpoint aggregate combining budget exceeded",
                    ))?
                    .is_some()
                {
                    unreachable!();
                }
                Ok(values)
            }
        }
        deserializer.deserialize_seq(CombiningVisitor(self.0))
    }
}
