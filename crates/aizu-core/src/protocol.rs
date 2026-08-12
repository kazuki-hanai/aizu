use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};

use chrono::{DateTime, Utc};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{MAX_FRAME_BYTES, NormalizedEvent, ValidationError};

pub const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME_NESTING: usize = 64;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BridgeFrame {
    Hello {
        protocol_version: u32,
        source_id: Uuid,
        oldest_sequence: Option<i64>,
        latest_sequence: i64,
    },
    Event {
        sequence: i64,
        event: Box<NormalizedEvent>,
    },
    Heartbeat {
        #[serde(with = "crate::event::utc_z")]
        sent_at: DateTime<Utc>,
    },
    Gap {
        requested_after: i64,
        oldest_sequence: Option<i64>,
        lost_through_sequence: i64,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParsedBridgeFrame {
    Known(BridgeFrame),
    Unknown { frame_type: String },
}

impl BridgeFrame {
    #[must_use]
    pub fn hello(source_id: Uuid, oldest_sequence: Option<i64>, latest_sequence: i64) -> Self {
        Self::Hello {
            protocol_version: PROTOCOL_VERSION,
            source_id,
            oldest_sequence,
            latest_sequence,
        }
    }

    #[must_use]
    pub fn terminal_error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn to_line(&self) -> Result<Vec<u8>, ProtocolError> {
        validate_frame(self)?;

        let mut bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge {
                actual: bytes.len(),
                maximum: MAX_FRAME_BYTES,
            });
        }
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn write_to(&self, writer: &mut impl Write) -> Result<(), ProtocolError> {
        writer.write_all(&self.to_line()?)?;
        writer.flush()?;
        Ok(())
    }
}

/// Parses one bounded NDJSON frame and rejects duplicate keys at any nesting
/// level. One trailing LF (and an optional preceding CR) is accepted.
pub fn parse_frame_line(input: &[u8]) -> Result<ParsedBridgeFrame, ProtocolError> {
    let line = input
        .strip_suffix(b"\r\n")
        .or_else(|| input.strip_suffix(b"\n"))
        .unwrap_or(input);
    if line.is_empty() {
        return Err(ProtocolError::EmptyFrame);
    }
    if line.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge {
            actual: line.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    if line.contains(&b'\n') || line.contains(&b'\r') {
        return Err(ProtocolError::EmbeddedLineBreak);
    }

    let mut deserializer = serde_json::Deserializer::from_slice(line);
    let value = StrictValueSeed { depth: 0 }.deserialize(&mut deserializer)?;
    deserializer.end()?;
    let frame_type = value
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str)
        .ok_or(ProtocolError::MissingFrameType)?
        .to_owned();

    if !matches!(
        frame_type.as_str(),
        "hello" | "event" | "heartbeat" | "gap" | "error"
    ) {
        return Ok(ParsedBridgeFrame::Unknown { frame_type });
    }

    let frame: BridgeFrame = serde_json::from_value(value)?;
    validate_frame(&frame)?;
    Ok(ParsedBridgeFrame::Known(frame))
}

fn validate_frame(frame: &BridgeFrame) -> Result<(), ProtocolError> {
    match frame {
        BridgeFrame::Hello {
            oldest_sequence,
            latest_sequence,
            ..
        } => {
            if *latest_sequence < 0
                || oldest_sequence.is_some_and(|oldest| {
                    oldest <= 0 || oldest > *latest_sequence || *latest_sequence == 0
                })
            {
                return Err(ProtocolError::InvalidFrameInvariant("hello sequence range"));
            }
        }
        BridgeFrame::Event { sequence, event } => {
            if *sequence <= 0 {
                return Err(ProtocolError::InvalidFrameInvariant("event sequence"));
            }
            event.validate()?;
        }
        BridgeFrame::Heartbeat { .. } => {}
        BridgeFrame::Gap {
            requested_after,
            oldest_sequence,
            lost_through_sequence,
        } => {
            if *requested_after < 0
                || *lost_through_sequence <= *requested_after
                || oldest_sequence.is_some_and(|oldest| oldest != lost_through_sequence + 1)
            {
                return Err(ProtocolError::InvalidFrameInvariant("gap sequence range"));
            }
        }
        BridgeFrame::Error { code, message } => {
            if code.is_empty()
                || code.chars().count() > 64
                || code.chars().any(char::is_control)
                || message.chars().count() > 512
                || message.chars().any(char::is_control)
            {
                return Err(ProtocolError::InvalidFrameInvariant("error fields"));
            }
        }
    }
    Ok(())
}

struct StrictValueSeed {
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for StrictValueSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if self.depth > MAX_FRAME_NESTING {
            return Err(serde::de::Error::custom("JSON nesting is too deep"));
        }
        deserializer.deserialize_any(StrictValueVisitor { depth: self.depth })
    }
}

struct StrictValueVisitor {
    depth: usize,
}

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value > i64::MAX as u64 {
            return Err(serde::de::Error::custom(
                "JSON integer exceeds the signed 64-bit range",
            ));
        }
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StrictValueSeed { depth: self.depth }.deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(StrictValueSeed {
            depth: self.depth + 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = BTreeSet::new();
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON object key {key:?}"
                )));
            }
            let value = map.next_value_seed(StrictValueSeed {
                depth: self.depth + 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("bridge frame is {actual} bytes; maximum is {maximum}")]
    FrameTooLarge { actual: usize, maximum: usize },
    #[error("bridge frame is empty")]
    EmptyFrame,
    #[error("bridge frame contains an embedded line break")]
    EmbeddedLineBreak,
    #[error("bridge frame is missing a string type field")]
    MissingFrameType,
    #[error("bridge frame violates the {0} invariant")]
    InvalidFrameInvariant(&'static str),
    #[error("event validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("invalid bridge JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bridge I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_hello_as_single_ndjson_line() {
        let frame = BridgeFrame::hello(
            Uuid::parse_str("7a4881c7-c667-47dc-b544-f98a46ab17ca").unwrap(),
            None,
            140,
        );
        let line = frame.to_line().unwrap();

        assert_eq!(line.last(), Some(&b'\n'));
        assert!(!line[..line.len() - 1].contains(&b'\n'));
        let decoded: BridgeFrame = serde_json::from_slice(&line[..line.len() - 1]).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn rejects_overlong_error_message() {
        let frame = BridgeFrame::terminal_error("internal", "x".repeat(513));
        assert!(matches!(
            frame.to_line().unwrap_err(),
            ProtocolError::InvalidFrameInvariant("error fields")
        ));
    }

    #[test]
    fn parser_rejects_duplicate_keys() {
        let line = br#"{"type":"hello","type":"event","protocol_version":1,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":null,"latest_sequence":0}"#;
        assert!(matches!(
            parse_frame_line(line).unwrap_err(),
            ProtocolError::Json(_)
        ));
    }

    #[test]
    fn parser_preserves_unknown_frame_type() {
        let parsed = parse_frame_line(br#"{"type":"future.frame","value":1}"#).unwrap();
        assert_eq!(
            parsed,
            ParsedBridgeFrame::Unknown {
                frame_type: "future.frame".into()
            }
        );
    }

    #[test]
    fn parser_validates_event_and_sequence_invariants() {
        assert!(matches!(
            parse_frame_line(
                br#"{"type":"hello","protocol_version":1,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":1,"latest_sequence":0}"#
            )
            .unwrap_err(),
            ProtocolError::InvalidFrameInvariant("hello sequence range")
        ));
        assert!(matches!(
            parse_frame_line(
                br#"{"type":"gap","requested_after":2,"oldest_sequence":4,"lost_through_sequence":4}"#
            )
            .unwrap_err(),
            ProtocolError::InvalidFrameInvariant("gap sequence range")
        ));
    }

    #[test]
    fn parser_accepts_lf_and_crlf_but_not_lone_cr() {
        let frame = br#"{"type":"hello","protocol_version":1,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":null,"latest_sequence":0}"#;
        assert!(parse_frame_line(&[frame.as_slice(), b"\n"].concat()).is_ok());
        assert!(parse_frame_line(&[frame.as_slice(), b"\r\n"].concat()).is_ok());
        assert!(matches!(
            parse_frame_line(&[frame.as_slice(), b"\r"].concat()).unwrap_err(),
            ProtocolError::EmbeddedLineBreak
        ));
    }

    #[test]
    fn parser_rejects_oversized_and_out_of_range_values() {
        let oversized = vec![b'x'; crate::MAX_FRAME_BYTES + 1];
        assert!(matches!(
            parse_frame_line(&oversized).unwrap_err(),
            ProtocolError::FrameTooLarge { .. }
        ));
        assert!(matches!(
            parse_frame_line(br#"{"type":"future","value":18446744073709551615}"#).unwrap_err(),
            ProtocolError::Json(_)
        ));
    }
}
