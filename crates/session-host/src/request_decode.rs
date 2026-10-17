//! Streaming preflight for the bounded IPC request's PTY input field.
//!
//! `Command` is internally tagged. Its derived decoder first buffers the whole
//! command map as Serde `Content`, including arrays, before applying the field
//! deserializer. This visitor counts Input bytes without constructing that
//! buffer. Other command payloads remain subject to the IPC frame bound and
//! their normal Serde decoding; this is not a general JSON allocation budget.

use serde::Deserialize;

use crate::input::MAX_INPUT_BYTES;

pub(crate) fn validate(bytes: &[u8]) -> anyhow::Result<()> {
    let scan: RequestScan = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        !(scan.is_input && scan.oversized_input),
        "PTY input chunk exceeds {MAX_INPUT_BYTES} bytes"
    );
    Ok(())
}

#[derive(Default)]
struct RequestScan {
    is_input: bool,
    oversized_input: bool,
}

impl<'de> Deserialize<'de> for RequestScan {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = RequestScan;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an IPC request object")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut scan = RequestScan::default();
                let mut command_seen = false;
                while let Some(key) = map.next_key::<String>()? {
                    if key == "command" {
                        if command_seen {
                            return Err(serde::de::Error::duplicate_field("command"));
                        }
                        command_seen = true;
                        let command = map.next_value::<CommandScan>()?;
                        scan.is_input = command.is_input;
                        scan.oversized_input = command.oversized_input;
                    } else {
                        let _: serde::de::IgnoredAny = map.next_value()?;
                    }
                }
                Ok(scan)
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

#[derive(Default)]
struct CommandScan {
    is_input: bool,
    oversized_input: bool,
}

impl<'de> Deserialize<'de> for CommandScan {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = CommandScan;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an internally tagged command object")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut scan = CommandScan::default();
                let mut tag_seen = false;
                let mut bytes_seen = false;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "command" => {
                            if tag_seen {
                                return Err(serde::de::Error::duplicate_field("command"));
                            }
                            tag_seen = true;
                            scan.is_input = map.next_value::<String>()? == "input";
                        }
                        "bytes" => {
                            if bytes_seen {
                                return Err(serde::de::Error::duplicate_field("bytes"));
                            }
                            bytes_seen = true;
                            let CountArray(count, is_array) =
                                map.next_value_seed(CountArraySeed {
                                    reject_oversized: scan.is_input,
                                })?;
                            scan.oversized_input = is_array && count > MAX_INPUT_BYTES;
                        }
                        _ => {
                            let _: serde::de::IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(scan)
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

struct CountArray(usize, bool);

struct RejectExtraInput;

impl<'de> serde::de::DeserializeSeed<'de> for RejectExtraInput {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, _: D) -> Result<(), D::Error> {
        Err(serde::de::Error::custom(format!(
            "PTY input chunk exceeds {MAX_INPUT_BYTES} bytes"
        )))
    }
}

struct CountArraySeed {
    reject_oversized: bool,
}

impl<'de> serde::de::DeserializeSeed<'de> for CountArraySeed {
    type Value = CountArray;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        struct Visitor(bool);
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = CountArray;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("any JSON value")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut count = 0usize;
                loop {
                    if self.0 && count == MAX_INPUT_BYTES {
                        if sequence.next_element_seed(RejectExtraInput)?.is_some() {
                            unreachable!("oversized input rejected by extra-element seed");
                        }
                        return Ok(CountArray(count, true));
                    }
                    if sequence.next_element::<serde::de::IgnoredAny>()?.is_none() {
                        break;
                    }
                    count = count.saturating_add(1);
                }
                Ok(CountArray(count, true))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(CountArray(0, false))
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(CountArray(0, false))
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(CountArray(0, false))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(CountArray(0, false))
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(CountArray(0, false))
            }
            fn visit_string<E: serde::de::Error>(self, _: String) -> Result<Self::Value, E> {
                Ok(CountArray(0, false))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(CountArray(0, false))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(CountArray(0, false))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(CountArray(0, false))
            }
        }
        deserializer.deserialize_any(Visitor(self.reject_oversized))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, PROTOCOL, Request};
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    use uuid::Uuid;

    fn input_prefix() -> String {
        format!(
            r#"{{"protocol":{PROTOCOL},"command":{{"command":"input","session":"{}","generation":"{}","bytes":["#,
            Uuid::nil(),
            Uuid::nil()
        )
    }

    fn framed(bytes: &[u8]) -> (UnixStream, UnixStream) {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        sender
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .unwrap();
        sender.write_all(bytes).unwrap();
        (sender, receiver)
    }

    fn append_zeros(raw: &mut String, count: usize) {
        for index in 0..count {
            if index != 0 {
                raw.push(',');
            }
            raw.push('0');
        }
    }

    #[test]
    fn oversized_tag_first_input_is_rejected_before_truncated_tail() {
        let mut raw = input_prefix();
        append_zeros(&mut raw, MAX_INPUT_BYTES);
        raw.push_str(",{"); // malformed next value; the cap rejects before parsing it.
        let error = validate(raw.as_bytes()).unwrap_err();
        assert!(error.to_string().contains("exceeds 16384 bytes"), "{error}");
    }

    #[test]
    fn oversized_input_is_rejected_when_bytes_precede_the_tag() {
        let mut raw = format!(
            r#"{{"protocol":{PROTOCOL},"command":{{"session":"{}","generation":"{}","bytes":["#,
            Uuid::nil(),
            Uuid::nil()
        );
        append_zeros(&mut raw, MAX_INPUT_BYTES + 1);
        raw.push_str(r#"],"command":"input"}}"#);
        assert!(validate(raw.as_bytes()).is_err());
    }

    #[test]
    fn maximum_input_after_tag_and_before_tag_passes_and_decodes() {
        let mut tag_first = input_prefix();
        append_zeros(&mut tag_first, MAX_INPUT_BYTES);
        tag_first.push_str("]}}");
        validate(tag_first.as_bytes()).unwrap();
        let (_sender, mut receiver) = framed(tag_first.as_bytes());
        let decoded: Request = crate::receive_request(&mut receiver).unwrap();
        assert!(
            matches!(decoded.command, Command::Input { bytes, .. } if bytes.len() == MAX_INPUT_BYTES)
        );

        let mut bytes_first = format!(
            r#"{{"protocol":{PROTOCOL},"command":{{"session":"{}","generation":"{}","bytes":["#,
            Uuid::nil(),
            Uuid::nil()
        );
        append_zeros(&mut bytes_first, MAX_INPUT_BYTES);
        bytes_first.push_str(r#"],"command":"input"}}"#);
        validate(bytes_first.as_bytes()).unwrap();
        let (_sender, mut receiver) = framed(bytes_first.as_bytes());
        let decoded: Request = crate::receive_request(&mut receiver).unwrap();
        assert!(
            matches!(decoded.command, Command::Input { bytes, .. } if bytes.len() == MAX_INPUT_BYTES)
        );
    }

    #[test]
    fn duplicate_top_level_command_is_rejected() {
        let raw = format!(
            r#"{{"protocol":{PROTOCOL},"command":{{"command":"list"}},"command":{{"command":"input"}}}}"#
        );
        assert!(validate(raw.as_bytes()).is_err());
    }

    #[test]
    fn duplicate_command_tag_and_duplicate_bytes_are_rejected() {
        let tag_duplicate = format!(
            r#"{{"protocol":{PROTOCOL},"command":{{"command":"input","command":"list","bytes":[]}}}}"#
        );
        assert!(validate(tag_duplicate.as_bytes()).is_err());

        let bytes_duplicate = format!(
            r#"{{"protocol":{PROTOCOL},"command":{{"command":"input","bytes":[],"bytes":[]}}}}"#
        );
        assert!(validate(bytes_duplicate.as_bytes()).is_err());
    }

    #[test]
    fn malformed_non_byte_element_within_limit_passes_preflight_but_fails_decode() {
        let mut raw = input_prefix();
        raw.push_str(r#"0,"not-a-byte"]}}"#);
        validate(raw.as_bytes()).unwrap();
        let (_sender, mut receiver) = framed(raw.as_bytes());
        assert!(crate::receive_request(&mut receiver).is_err());
    }
}
