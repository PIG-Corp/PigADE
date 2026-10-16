//! Allocation-aware serde seeds. Do not trust format-provided size hints as
//! allocation capacities. Reject an excessive hint before consuming any element.
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, SeqAccess, Visitor},
};
use std::{fmt, marker::PhantomData};

pub(crate) struct Reject(pub &'static str);
impl<'de> DeserializeSeed<'de> for Reject {
    type Value = ();
    fn deserialize<D: Deserializer<'de>>(self, _: D) -> Result<(), D::Error> {
        Err(de::Error::custom(self.0))
    }
}

pub(crate) struct VecSeed<T> {
    pub limit: usize,
    pub exact: bool,
    pub marker: PhantomData<T>,
}
impl<T> VecSeed<T> {
    pub fn up_to(limit: usize) -> Self {
        Self {
            limit,
            exact: false,
            marker: PhantomData,
        }
    }
    pub fn exactly(limit: usize) -> Self {
        Self {
            limit,
            exact: true,
            marker: PhantomData,
        }
    }
}
impl<'de, T: Deserialize<'de>> DeserializeSeed<'de> for VecSeed<T> {
    type Value = Vec<T>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct SeqVisitor<T>(VecSeed<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for SeqVisitor<T> {
            type Value = Vec<T>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a sequence bounded to {} elements", self.0.limit)
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                if sequence.size_hint().is_some_and(|hint| {
                    hint > self.0.limit || (self.0.exact && hint != self.0.limit)
                }) {
                    return Err(de::Error::custom(
                        "checkpoint sequence length exceeds declared bound",
                    ));
                }
                let mut values = Vec::new();
                while values.len() < self.0.limit {
                    match sequence.next_element()? {
                        Some(value) => values.push(value),
                        None => {
                            if self.0.exact {
                                return Err(de::Error::custom(
                                    "checkpoint sequence length mismatch",
                                ));
                            }
                            return Ok(values);
                        }
                    }
                }
                // The rejection seed errors BEFORE deserializing an extra element.
                if sequence
                    .next_element_seed(Reject("checkpoint sequence element limit exceeded"))?
                    .is_some()
                {
                    unreachable!();
                }
                Ok(values)
            }
        }
        deserializer.deserialize_seq(SeqVisitor(self))
    }
}

pub(crate) struct TextSeed(pub usize);
impl<'de> DeserializeSeed<'de> for TextSeed {
    type Value = String;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<String, D::Error> {
        struct TextVisitor(usize);
        impl Visitor<'_> for TextVisitor {
            type Value = String;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "UTF-8 text bounded to {} bytes", self.0)
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
                if value.len() > self.0 {
                    return Err(E::custom("checkpoint text limit exceeded"));
                }
                Ok(value.to_owned())
            }
            fn visit_string<E: de::Error>(self, value: String) -> Result<String, E> {
                if value.len() > self.0 {
                    return Err(E::custom("checkpoint text limit exceeded"));
                }
                Ok(value)
            }
        }
        deserializer.deserialize_str(TextVisitor(self.0))
    }
}

pub(crate) struct OptionalTextSeed(pub usize);
impl<'de> DeserializeSeed<'de> for OptionalTextSeed {
    type Value = Option<String>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct OptionalVisitor(usize);
        impl<'de> Visitor<'de> for OptionalVisitor {
            type Value = Option<String>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("optional bounded UTF-8 text")
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_some<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                TextSeed(self.0).deserialize(deserializer).map(Some)
            }
        }
        deserializer.deserialize_option(OptionalVisitor(self.0))
    }
}

pub(crate) struct OptionalText(pub Option<String>);
impl<'de> Deserialize<'de> for OptionalText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        OptionalTextSeed(65536).deserialize(deserializer).map(Self)
    }
}
pub(crate) fn text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    TextSeed(65536).deserialize(deserializer)
}
pub(crate) fn combining<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<char>, D::Error> {
    VecSeed::up_to(super::MAX_COMBINING_CHARS).deserialize(deserializer)
}
