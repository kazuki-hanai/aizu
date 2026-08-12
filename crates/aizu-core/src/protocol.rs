use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};
use std::mem;

use chrono::{DateTime, Utc};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use thiserror::Error;
use uuid::{Uuid, Variant};

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

/// Incrementally splits a byte stream into bounded NDJSON frames.
///
/// The decoder enforces the line limit before a delimiter arrives, so callers
/// never need to allocate an unbounded buffer for a malicious SSH peer.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    #[must_use]
    pub const fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<ParsedBridgeFrame>, ProtocolError> {
        let mut frames = Vec::new();
        for byte in chunk {
            if *byte == b'\n' {
                let mut line = mem::take(&mut self.buffer);
                line.push(b'\n');
                frames.push(parse_frame_line(&line)?);
                continue;
            }
            self.buffer.push(*byte);
            let delimiter_cr =
                self.buffer.len() == MAX_FRAME_BYTES + 1 && self.buffer.last() == Some(&b'\r');
            if self.buffer.len() > MAX_FRAME_BYTES && !delimiter_cr {
                return Err(ProtocolError::FrameTooLarge {
                    actual: self.buffer.len(),
                    maximum: MAX_FRAME_BYTES,
                });
            }
        }
        Ok(frames)
    }

    /// Verifies that EOF occurs at a frame boundary.
    pub fn finish(&mut self) -> Result<Option<ParsedBridgeFrame>, ProtocolError> {
        if self.buffer.is_empty() {
            return Ok(None);
        }
        Err(ProtocolError::UnterminatedFrame {
            buffered: self.buffer.len(),
        })
    }

    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }
}

/// Stateful validation for a single bridge process stream.
#[derive(Clone, Debug)]
pub struct BridgeStreamValidator {
    expected_protocol: u32,
    pinned_source_id: Option<Uuid>,
    source_id: Option<Uuid>,
    cursor: i64,
    required_gap_through: Option<i64>,
    phase: StreamPhase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamPhase {
    AwaitingHello,
    Active,
    Terminal,
}

impl BridgeStreamValidator {
    pub fn new(
        expected_protocol: u32,
        after: i64,
        pinned_source_id: Option<Uuid>,
    ) -> Result<Self, ProtocolError> {
        if after < 0 {
            return Err(ProtocolError::InvalidInitialCursor(after));
        }
        Ok(Self {
            expected_protocol,
            pinned_source_id,
            source_id: None,
            cursor: after,
            required_gap_through: None,
            phase: StreamPhase::AwaitingHello,
        })
    }

    pub fn accept(&mut self, frame: &ParsedBridgeFrame) -> Result<(), ProtocolError> {
        if let ParsedBridgeFrame::Known(frame) = frame {
            validate_frame(frame)?;
        }
        match self.phase {
            StreamPhase::AwaitingHello => self.accept_before_hello(frame),
            StreamPhase::Active => self.accept_active(frame),
            StreamPhase::Terminal => Err(ProtocolError::FrameAfterTerminal),
        }
    }

    #[must_use]
    pub const fn cursor(&self) -> i64 {
        self.cursor
    }

    #[must_use]
    pub const fn source_id(&self) -> Option<Uuid> {
        self.source_id
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self.phase, StreamPhase::Terminal)
    }

    fn accept_before_hello(&mut self, frame: &ParsedBridgeFrame) -> Result<(), ProtocolError> {
        match frame {
            ParsedBridgeFrame::Known(BridgeFrame::Hello {
                protocol_version,
                source_id,
                oldest_sequence,
                latest_sequence,
            }) => {
                if *protocol_version != self.expected_protocol {
                    return Err(ProtocolError::ProtocolVersionMismatch {
                        expected: self.expected_protocol,
                        actual: *protocol_version,
                    });
                }
                if let Some(pinned) = self.pinned_source_id
                    && pinned != *source_id
                {
                    return Err(ProtocolError::SourceIdentityMismatch {
                        expected: pinned,
                        actual: *source_id,
                    });
                }
                self.source_id = Some(*source_id);
                self.required_gap_through = match oldest_sequence {
                    Some(oldest) if self.cursor < oldest.saturating_sub(1) => Some(oldest - 1),
                    None if self.cursor < *latest_sequence => Some(*latest_sequence),
                    _ => None,
                };
                self.phase = StreamPhase::Active;
                Ok(())
            }
            ParsedBridgeFrame::Known(BridgeFrame::Error { .. }) => {
                self.phase = StreamPhase::Terminal;
                Ok(())
            }
            ParsedBridgeFrame::Known(frame) => Err(ProtocolError::FrameBeforeHello(
                known_frame_type(frame).to_owned(),
            )),
            ParsedBridgeFrame::Unknown { frame_type } => {
                Err(ProtocolError::FrameBeforeHello(frame_type.clone()))
            }
        }
    }

    fn accept_active(&mut self, frame: &ParsedBridgeFrame) -> Result<(), ProtocolError> {
        match frame {
            ParsedBridgeFrame::Unknown { .. }
            | ParsedBridgeFrame::Known(BridgeFrame::Heartbeat { .. }) => Ok(()),
            ParsedBridgeFrame::Known(BridgeFrame::Event { sequence, event }) => {
                if let Some(lost_through) = self.required_gap_through {
                    return Err(ProtocolError::RequiredGapMissing { lost_through });
                }
                let source_id = self.source_id.ok_or(ProtocolError::MissingStreamSource)?;
                if event.source.source_id != source_id {
                    return Err(ProtocolError::SourceIdentityMismatch {
                        expected: source_id,
                        actual: event.source.source_id,
                    });
                }
                let expected = self
                    .cursor
                    .checked_add(1)
                    .ok_or(ProtocolError::SequenceOverflow)?;
                if *sequence != expected {
                    return Err(ProtocolError::SequenceDiscontinuity {
                        expected,
                        actual: *sequence,
                    });
                }
                self.cursor = *sequence;
                Ok(())
            }
            ParsedBridgeFrame::Known(BridgeFrame::Gap {
                requested_after,
                lost_through_sequence,
                ..
            }) => {
                if *requested_after != self.cursor {
                    return Err(ProtocolError::GapCursorMismatch {
                        expected: self.cursor,
                        actual: *requested_after,
                    });
                }
                if let Some(required) = self.required_gap_through
                    && *lost_through_sequence != required
                {
                    return Err(ProtocolError::RequiredGapMismatch {
                        expected: required,
                        actual: *lost_through_sequence,
                    });
                }
                self.cursor = *lost_through_sequence;
                self.required_gap_through = None;
                Ok(())
            }
            ParsedBridgeFrame::Known(BridgeFrame::Error { .. }) => {
                self.phase = StreamPhase::Terminal;
                Ok(())
            }
            ParsedBridgeFrame::Known(BridgeFrame::Hello { .. }) => {
                Err(ProtocolError::DuplicateHello)
            }
        }
    }
}

const fn known_frame_type(frame: &BridgeFrame) -> &'static str {
    match frame {
        BridgeFrame::Hello { .. } => "hello",
        BridgeFrame::Event { .. } => "event",
        BridgeFrame::Heartbeat { .. } => "heartbeat",
        BridgeFrame::Gap { .. } => "gap",
        BridgeFrame::Error { .. } => "error",
    }
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

    let value = parse_strict_json_value(line, MAX_FRAME_BYTES)?;
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

/// Parses bounded JSON while rejecting duplicate object keys, excessive
/// nesting, and integers outside the signed 64-bit range.
pub fn parse_strict_json_value(input: &[u8], maximum_bytes: usize) -> Result<Value, ProtocolError> {
    if input.len() > maximum_bytes {
        return Err(ProtocolError::FrameTooLarge {
            actual: input.len(),
            maximum: maximum_bytes,
        });
    }
    validate_json_integer_tokens(input)?;
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = StrictValueSeed { depth: 0 }.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

fn validate_json_integer_tokens(input: &[u8]) -> Result<(), ProtocolError> {
    let mut index = 0;
    while index < input.len() {
        match input[index] {
            b'"' => {
                index += 1;
                while index < input.len() {
                    match input[index] {
                        b'\\' => index = index.saturating_add(2),
                        b'"' => {
                            index += 1;
                            break;
                        }
                        _ => index += 1,
                    }
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = index;
                index += 1;
                while index < input.len()
                    && matches!(input[index], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    index += 1;
                }
                let token = &input[start..index];
                if !token.iter().any(|byte| matches!(byte, b'.' | b'e' | b'E'))
                    && std::str::from_utf8(token)
                        .ok()
                        .and_then(|raw| raw.parse::<i64>().ok())
                        .is_none()
                {
                    return Err(ProtocolError::IntegerOutOfRange);
                }
            }
            _ => index += 1,
        }
    }
    Ok(())
}

fn validate_frame(frame: &BridgeFrame) -> Result<(), ProtocolError> {
    match frame {
        BridgeFrame::Hello {
            source_id,
            oldest_sequence,
            latest_sequence,
            ..
        } => {
            if source_id.get_variant() != Variant::RFC4122
                || *latest_sequence < 0
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
    #[error("bridge stream ended with an unterminated {buffered}-byte frame")]
    UnterminatedFrame { buffered: usize },
    #[error("bridge frame is missing a string type field")]
    MissingFrameType,
    #[error("JSON integer exceeds the signed 64-bit range")]
    IntegerOutOfRange,
    #[error("bridge frame violates the {0} invariant")]
    InvalidFrameInvariant(&'static str),
    #[error("initial bridge cursor must be non-negative, got {0}")]
    InvalidInitialCursor(i64),
    #[error("bridge protocol version mismatch: expected {expected}, got {actual}")]
    ProtocolVersionMismatch { expected: u32, actual: u32 },
    #[error("bridge source identity mismatch: expected {expected}, got {actual}")]
    SourceIdentityMismatch { expected: Uuid, actual: Uuid },
    #[error("bridge frame {0:?} arrived before hello")]
    FrameBeforeHello(String),
    #[error("bridge emitted hello more than once")]
    DuplicateHello,
    #[error("bridge frame arrived after a terminal error")]
    FrameAfterTerminal,
    #[error("bridge stream source identity is missing")]
    MissingStreamSource,
    #[error("bridge sequence overflow")]
    SequenceOverflow,
    #[error("bridge event sequence discontinuity: expected {expected}, got {actual}")]
    SequenceDiscontinuity { expected: i64, actual: i64 },
    #[error("bridge gap cursor mismatch: expected {expected}, got {actual}")]
    GapCursorMismatch { expected: i64, actual: i64 },
    #[error("bridge event arrived before required gap through sequence {lost_through}")]
    RequiredGapMissing { lost_through: i64 },
    #[error("bridge gap did not cover hello-declared loss: expected {expected}, got {actual}")]
    RequiredGapMismatch { expected: i64, actual: i64 },
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
    fn strict_json_parser_rejects_nested_duplicate_keys() {
        assert!(matches!(
            parse_strict_json_value(br#"{"outer":{"value":1,"value":2}}"#, 1024).unwrap_err(),
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
    fn parser_preserves_unknown_optional_event_fields() {
        let line = br#"{"type":"event","sequence":1,"future_frame_field":true,"event":{"schema_version":1,"id":"0198a012-3456-7abc-8def-0123456789ab","kind":"agent.question","occurred_at":"2026-08-12T12:34:56.789Z","source":{"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","display_name":"build-server","agent":"generic","future_source_field":"kept"},"title":"Question","future_event_field":{"value":1}}}"#;
        let ParsedBridgeFrame::Known(BridgeFrame::Event { event, .. }) =
            parse_frame_line(line).unwrap()
        else {
            panic!("expected an event frame");
        };
        assert_eq!(event.extra["future_event_field"]["value"], 1);
        assert_eq!(event.source.extra["future_source_field"], "kept");
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
        assert!(matches!(
            parse_frame_line(
                br#"{"type":"hello","protocol_version":1,"source_id":"00000000-0000-0000-0000-000000000000","oldest_sequence":null,"latest_sequence":0}"#
            )
            .unwrap_err(),
            ProtocolError::InvalidFrameInvariant("hello sequence range")
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
            ProtocolError::IntegerOutOfRange
        ));
        assert!(matches!(
            parse_frame_line(br#"{"type":"future","value":-9223372036854775809}"#).unwrap_err(),
            ProtocolError::IntegerOutOfRange
        ));
        assert!(
            parse_frame_line(br#"{"type":"future","value":-9223372036854775808,"float":1.25e20}"#)
                .is_ok()
        );
    }

    #[test]
    fn incremental_decoder_handles_split_and_multiple_frames() {
        let hello = br#"{"type":"hello","protocol_version":1,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":null,"latest_sequence":0}"#;
        let unknown = br#"{"type":"future.frame","value":1}"#;
        let split = hello.len() / 2;
        let mut decoder = FrameDecoder::new();

        assert!(decoder.push(&hello[..split]).unwrap().is_empty());
        assert_eq!(decoder.buffered_len(), split);
        let frames = decoder
            .push(&[&hello[split..], b"\n", unknown, b"\r\n"].concat())
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert!(matches!(
            frames[0],
            ParsedBridgeFrame::Known(BridgeFrame::Hello { .. })
        ));
        assert!(matches!(
            &frames[1],
            ParsedBridgeFrame::Unknown { frame_type } if frame_type == "future.frame"
        ));
        assert_eq!(decoder.buffered_len(), 0);
        assert!(decoder.finish().unwrap().is_none());
    }

    #[test]
    fn incremental_decoder_rejects_unbounded_line_before_delimiter() {
        let mut decoder = FrameDecoder::new();
        let oversized = vec![b'x'; crate::MAX_FRAME_BYTES + 1];
        assert!(matches!(
            decoder.push(&oversized).unwrap_err(),
            ProtocolError::FrameTooLarge { .. }
        ));
    }

    #[test]
    fn incremental_decoder_rejects_unterminated_frame_at_eof() {
        let mut decoder = FrameDecoder::new();
        decoder
            .push(
                br#"{"type":"hello","protocol_version":1,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":null,"latest_sequence":0}"#,
            )
            .unwrap();
        assert!(matches!(
            decoder.finish().unwrap_err(),
            ProtocolError::UnterminatedFrame { buffered } if buffered > 0
        ));
    }

    #[test]
    fn stream_validator_accepts_gap_then_event_and_terminal_error() {
        let source_id = Uuid::parse_str("7a4881c7-c667-47dc-b544-f98a46ab17ca").unwrap();
        let mut validator =
            BridgeStreamValidator::new(PROTOCOL_VERSION, 0, Some(source_id)).unwrap();
        let frames = [
            br#"{"type":"hello","protocol_version":1,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":3,"latest_sequence":3}"#
                .as_slice(),
            br#"{"type":"gap","requested_after":0,"oldest_sequence":3,"lost_through_sequence":2}"#
                .as_slice(),
            br#"{"type":"future.frame","value":1}"#.as_slice(),
            br#"{"type":"event","sequence":3,"event":{"schema_version":1,"id":"0198a012-3456-7abc-8def-0123456789ab","kind":"agent.question","occurred_at":"2026-08-12T12:34:56.789Z","source":{"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","display_name":"build-server","agent":"generic"},"title":"Question"}}"#
                .as_slice(),
            br#"{"type":"heartbeat","sent_at":"2026-08-12T12:35:30Z"}"#.as_slice(),
            br#"{"type":"error","code":"internal","message":"stream stopped"}"#.as_slice(),
        ];
        for frame in frames {
            validator.accept(&parse_frame_line(frame).unwrap()).unwrap();
        }
        assert_eq!(validator.cursor(), 3);
        assert_eq!(validator.source_id(), Some(source_id));
        assert!(validator.is_terminal());
        assert!(matches!(
            validator
                .accept(&parse_frame_line(frames[4]).unwrap())
                .unwrap_err(),
            ProtocolError::FrameAfterTerminal
        ));
    }

    #[test]
    fn stream_validator_rejects_order_identity_and_sequence_errors() {
        let source_id = Uuid::parse_str("7a4881c7-c667-47dc-b544-f98a46ab17ca").unwrap();
        let event = parse_frame_line(
            br#"{"type":"event","sequence":2,"event":{"schema_version":1,"id":"0198a012-3456-7abc-8def-0123456789ab","kind":"agent.question","occurred_at":"2026-08-12T12:34:56.789Z","source":{"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","display_name":"build-server","agent":"generic"},"title":"Question"}}"#,
        )
        .unwrap();
        assert!(matches!(
            BridgeStreamValidator::new(1, 0, None)
                .unwrap()
                .accept(&event)
                .unwrap_err(),
            ProtocolError::FrameBeforeHello(_)
        ));

        let wrong_version = parse_frame_line(
            br#"{"type":"hello","protocol_version":2,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":null,"latest_sequence":0}"#,
        )
        .unwrap();
        assert!(matches!(
            BridgeStreamValidator::new(1, 0, None)
                .unwrap()
                .accept(&wrong_version)
                .unwrap_err(),
            ProtocolError::ProtocolVersionMismatch {
                expected: 1,
                actual: 2
            }
        ));

        let hello = parse_frame_line(
            br#"{"type":"hello","protocol_version":1,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":1,"latest_sequence":2}"#,
        )
        .unwrap();
        let other_source = Uuid::parse_str("6a4881c7-c667-47dc-b544-f98a46ab17ca").unwrap();
        assert!(matches!(
            BridgeStreamValidator::new(1, 0, Some(other_source))
                .unwrap()
                .accept(&hello)
                .unwrap_err(),
            ProtocolError::SourceIdentityMismatch { .. }
        ));

        let mut validator = BridgeStreamValidator::new(1, 0, Some(source_id)).unwrap();
        validator.accept(&hello).unwrap();
        assert!(matches!(
            validator.accept(&event).unwrap_err(),
            ProtocolError::SequenceDiscontinuity {
                expected: 1,
                actual: 2
            }
        ));

        let invalid_direct = ParsedBridgeFrame::Known(BridgeFrame::hello(Uuid::nil(), None, 0));
        assert!(matches!(
            BridgeStreamValidator::new(1, 0, None)
                .unwrap()
                .accept(&invalid_direct)
                .unwrap_err(),
            ProtocolError::InvalidFrameInvariant("hello sequence range")
        ));

        let pruned_hello = parse_frame_line(
            br#"{"type":"hello","protocol_version":1,"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","oldest_sequence":3,"latest_sequence":3}"#,
        )
        .unwrap();
        let sequence_one = parse_frame_line(
            br#"{"type":"event","sequence":1,"event":{"schema_version":1,"id":"0198a012-3456-7abc-8def-0123456789ab","kind":"agent.question","occurred_at":"2026-08-12T12:34:56.789Z","source":{"source_id":"7a4881c7-c667-47dc-b544-f98a46ab17ca","display_name":"build-server","agent":"generic"},"title":"Question"}}"#,
        )
        .unwrap();
        let mut missing_gap = BridgeStreamValidator::new(1, 0, Some(source_id)).unwrap();
        missing_gap.accept(&pruned_hello).unwrap();
        assert!(matches!(
            missing_gap.accept(&sequence_one).unwrap_err(),
            ProtocolError::RequiredGapMissing { lost_through: 2 }
        ));

        let short_gap = parse_frame_line(
            br#"{"type":"gap","requested_after":0,"oldest_sequence":2,"lost_through_sequence":1}"#,
        )
        .unwrap();
        assert!(matches!(
            missing_gap.accept(&short_gap).unwrap_err(),
            ProtocolError::RequiredGapMismatch {
                expected: 2,
                actual: 1
            }
        ));
    }
}
