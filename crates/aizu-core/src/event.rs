use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::{Uuid, Variant, Version};

use crate::MAX_EVENT_BYTES;

pub const SCHEMA_VERSION: u32 = 1;
const MAX_JSON_NESTING: usize = 32;
const EVENT_RESERVED_FIELDS: [&str; 10] = [
    "schema_version",
    "id",
    "kind",
    "occurred_at",
    "source",
    "title",
    "body",
    "outcome",
    "urgency",
    "metadata",
];
const SOURCE_RESERVED_FIELDS: [&str; 4] = ["source_id", "display_name", "agent", "session_id"];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    High,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EventKind {
    #[serde(rename = "task.completed")]
    TaskCompleted,
    #[serde(rename = "agent.question")]
    AgentQuestion,
}

impl EventKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskCompleted => "task.completed",
            Self::AgentQuestion => "agent.question",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Source {
    pub source_id: Uuid,
    pub display_name: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NormalizedEvent {
    pub schema_version: u32,
    pub id: Uuid,
    pub kind: EventKind,
    #[serde(with = "utc_z")]
    pub occurred_at: DateTime<Utc>,
    pub source: Source,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(default)]
    pub urgency: Urgency,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl NormalizedEvent {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedSchema(self.schema_version));
        }
        if self.id.get_version() != Some(Version::SortRand)
            || self.id.get_variant() != Variant::RFC4122
        {
            return Err(ValidationError::EventIdMustBeV7);
        }
        if self.source.source_id.get_variant() != Variant::RFC4122 {
            return Err(ValidationError::SourceIdMustBeRfc4122);
        }
        validate_identifier("source.display_name", &self.source.display_name, 1, 200)?;
        validate_identifier("source.agent", &self.source.agent, 1, 100)?;
        if let Some(session_id) = &self.source.session_id {
            validate_identifier("source.session_id", session_id, 0, 200)?;
        }
        validate_identifier("title", &self.title, 1, 120)?;
        if let Some(body) = &self.body {
            validate_body(body)?;
        }
        validate_extra_fields("event", &self.extra, &EVENT_RESERVED_FIELDS)?;
        validate_extra_fields("source", &self.source.extra, &SOURCE_RESERVED_FIELDS)?;
        if let Some(metadata) = &self.metadata {
            validate_json_depth(&Value::Object(metadata.clone()), 0)?;
        }
        for value in self.extra.values().chain(self.source.extra.values()) {
            validate_json_depth(value, 0)?;
        }

        match (self.kind, self.outcome) {
            (EventKind::TaskCompleted, None) => {
                return Err(ValidationError::OutcomeRequired);
            }
            (EventKind::AgentQuestion, Some(_)) => {
                return Err(ValidationError::OutcomeForbidden);
            }
            _ => {}
        }

        let serialized = serde_json::to_vec(self)?;
        if serialized.len() > MAX_EVENT_BYTES {
            return Err(ValidationError::EventTooLarge {
                actual: serialized.len(),
                maximum: MAX_EVENT_BYTES,
            });
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, ValidationError> {
        self.validate()?;
        Ok(serde_json::to_string(self)?)
    }
}

/// Untrusted request accepted from `aizu emit` and generic hook adapters.
///
/// Trusted identity, schema, event id, sequence, and insertion timestamps are
/// deliberately absent. Unknown request fields are accepted but ignored.
#[derive(Clone, Debug, Deserialize, Default)]
pub struct EmitRequest {
    pub kind: Option<EventKind>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub outcome: Option<Outcome>,
    pub urgency: Option<Urgency>,
    pub agent: Option<String>,
    pub session_id: Option<String>,
    pub occurred_at: Option<String>,
    pub metadata: Option<Value>,
    #[serde(flatten)]
    pub ignored: BTreeMap<String, Value>,
}

impl EmitRequest {
    pub fn normalize(
        self,
        source_id: Uuid,
        display_name: String,
        default_kind: Option<EventKind>,
    ) -> Result<NormalizedEvent, ValidationError> {
        let kind = self
            .kind
            .or(default_kind)
            .ok_or(ValidationError::KindRequired)?;
        let title = self.title.ok_or(ValidationError::TitleRequired)?;
        let agent = self.agent.unwrap_or_else(|| "generic".to_owned());
        let occurred_at = match self.occurred_at {
            Some(raw) => parse_utc_timestamp(&raw)?,
            None => Utc::now(),
        };
        let occurred_at = DateTime::from_timestamp_millis(occurred_at.timestamp_millis())
            .ok_or(ValidationError::InvalidTimestamp)?;
        let metadata = match self.metadata {
            Some(Value::Object(object)) => Some(object),
            Some(_) => return Err(ValidationError::MetadataMustBeObject),
            None => None,
        };
        let outcome = match kind {
            EventKind::TaskCompleted => Some(self.outcome.unwrap_or(Outcome::Unknown)),
            EventKind::AgentQuestion if self.outcome.is_some() => {
                return Err(ValidationError::OutcomeForbidden);
            }
            EventKind::AgentQuestion => None,
        };

        let event = NormalizedEvent {
            schema_version: SCHEMA_VERSION,
            id: Uuid::now_v7(),
            kind,
            occurred_at,
            source: Source {
                source_id,
                display_name,
                agent,
                session_id: self.session_id,
                extra: BTreeMap::new(),
            },
            title,
            body: self.body,
            outcome,
            urgency: self.urgency.unwrap_or_default(),
            metadata,
            extra: BTreeMap::new(),
        };
        event.validate()?;
        Ok(event)
    }
}

pub fn parse_utc_timestamp(raw: &str) -> Result<DateTime<Utc>, ValidationError> {
    if !raw.ends_with('Z') {
        return Err(ValidationError::TimestampMustBeUtc);
    }
    let parsed =
        DateTime::parse_from_rfc3339(raw).map_err(|_| ValidationError::InvalidTimestamp)?;
    Ok(parsed.with_timezone(&Utc))
}

#[must_use]
pub fn format_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) mod utc_z {
    use chrono::{DateTime, Utc};
    use serde::{Deserialize, Deserializer, Serializer};

    use super::{format_timestamp, parse_utc_timestamp};

    pub fn serialize<S>(timestamp: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format_timestamp(*timestamp))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        parse_utc_timestamp(&raw).map_err(serde::de::Error::custom)
    }
}

fn validate_identifier(
    field: &'static str,
    value: &str,
    minimum: usize,
    maximum: usize,
) -> Result<(), ValidationError> {
    let length = value.chars().count();
    if !(minimum..=maximum).contains(&length) {
        return Err(ValidationError::InvalidLength {
            field,
            minimum,
            maximum,
            actual: length,
        });
    }
    if value.chars().any(is_c0_or_del) {
        return Err(ValidationError::ControlCharacter { field });
    }
    Ok(())
}

fn validate_body(body: &str) -> Result<(), ValidationError> {
    let length = body.chars().count();
    if length > 1_000 {
        return Err(ValidationError::InvalidLength {
            field: "body",
            minimum: 0,
            maximum: 1_000,
            actual: length,
        });
    }
    if body
        .chars()
        .any(|character| is_c0_or_del(character) && !matches!(character, '\t' | '\n' | '\r'))
    {
        return Err(ValidationError::ControlCharacter { field: "body" });
    }
    Ok(())
}

const fn is_c0_or_del(character: char) -> bool {
    character <= '\u{001F}' || character == '\u{007F}'
}

fn validate_extra_fields(
    object: &'static str,
    fields: &BTreeMap<String, Value>,
    reserved: &[&str],
) -> Result<(), ValidationError> {
    if let Some(field) = fields
        .keys()
        .find(|field| reserved.contains(&field.as_str()))
    {
        return Err(ValidationError::ReservedExtraField {
            object,
            field: field.clone(),
        });
    }
    Ok(())
}

fn validate_json_depth(value: &Value, depth: usize) -> Result<(), ValidationError> {
    if depth > MAX_JSON_NESTING {
        return Err(ValidationError::JsonNestingTooDeep {
            maximum: MAX_JSON_NESTING,
        });
    }
    match value {
        Value::Array(values) => {
            for value in values {
                validate_json_depth(value, depth + 1)?;
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                validate_json_depth(value, depth + 1)?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("event kind is required")]
    KindRequired,
    #[error("event title is required")]
    TitleRequired,
    #[error("task.completed requires an outcome")]
    OutcomeRequired,
    #[error("agent.question must not include an outcome")]
    OutcomeForbidden,
    #[error("occurred_at must be a valid RFC 3339 timestamp")]
    InvalidTimestamp,
    #[error("occurred_at must use the UTC Z suffix")]
    TimestampMustBeUtc,
    #[error("metadata must be a JSON object")]
    MetadataMustBeObject,
    #[error("{field} contains a control character")]
    ControlCharacter { field: &'static str },
    #[error("{field} length must be {minimum}..={maximum} Unicode scalar values, got {actual}")]
    InvalidLength {
        field: &'static str,
        minimum: usize,
        maximum: usize,
        actual: usize,
    },
    #[error("event id must be UUIDv7")]
    EventIdMustBeV7,
    #[error("source id must use the RFC 4122 UUID variant")]
    SourceIdMustBeRfc4122,
    #[error("unsupported event schema version {0}")]
    UnsupportedSchema(u32),
    #[error("serialized event is {actual} bytes; maximum is {maximum}")]
    EventTooLarge { actual: usize, maximum: usize },
    #[error("{object} extra field {field:?} collides with a reserved field")]
    ReservedExtraField { object: &'static str, field: String },
    #[error("JSON nesting exceeds the maximum depth of {maximum}")]
    JsonNestingTooDeep { maximum: usize },
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_id() -> Uuid {
        Uuid::parse_str("7a4881c7-c667-47dc-b544-f98a46ab17ca").unwrap()
    }

    #[test]
    fn task_outcome_defaults_to_unknown() {
        let event = EmitRequest {
            kind: Some(EventKind::TaskCompleted),
            title: Some("Done".into()),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap();

        assert_eq!(event.outcome, Some(Outcome::Unknown));
        assert_eq!(event.id.get_version(), Some(Version::SortRand));
    }

    #[test]
    fn question_rejects_outcome() {
        let error = EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("Question".into()),
            outcome: Some(Outcome::Succeeded),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap_err();

        assert!(matches!(error, ValidationError::OutcomeForbidden));
    }

    #[test]
    fn body_allows_newlines_but_not_nul() {
        EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("Question".into()),
            body: Some("line one\n\tline two".into()),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap();

        let error = EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("Question".into()),
            body: Some("bad\0body".into()),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap_err();
        assert!(matches!(
            error,
            ValidationError::ControlCharacter { field: "body" }
        ));
    }

    #[test]
    fn timestamp_requires_z_suffix() {
        let error = parse_utc_timestamp("2026-08-12T21:00:00+09:00").unwrap_err();
        assert!(matches!(error, ValidationError::TimestampMustBeUtc));
        assert_eq!(
            format_timestamp(parse_utc_timestamp("2026-08-12T12:00:00Z").unwrap()),
            "2026-08-12T12:00:00.000Z"
        );
    }

    #[test]
    fn normalized_event_deserialization_rejects_non_z_timestamp() {
        let value = serde_json::json!({
            "schema_version": 1,
            "id": Uuid::now_v7(),
            "kind": "agent.question",
            "occurred_at": "2026-08-12T21:00:00+09:00",
            "source": {
                "source_id": source_id(),
                "display_name": "local",
                "agent": "generic"
            },
            "title": "Question"
        });

        assert!(serde_json::from_value::<NormalizedEvent>(value).is_err());
    }

    #[test]
    fn ignores_untrusted_identity_fields() {
        let request: EmitRequest = serde_json::from_value(serde_json::json!({
            "kind": "agent.question",
            "title": "Question",
            "id": "attacker-controlled",
            "schema_version": 999,
            "source": {"source_id": "attacker-controlled"}
        }))
        .unwrap();

        let event = request
            .normalize(source_id(), "trusted".into(), None)
            .unwrap();
        assert_eq!(event.schema_version, 1);
        assert_eq!(event.source.source_id, source_id());
        assert_eq!(event.source.display_name, "trusted");
    }

    #[test]
    fn rejects_excessively_nested_metadata() {
        let mut value = Value::Null;
        for _ in 0..=MAX_JSON_NESTING {
            value = serde_json::json!([value]);
        }
        let error = EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("Question".into()),
            metadata: Some(serde_json::json!({"nested": value})),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap_err();
        assert!(matches!(error, ValidationError::JsonNestingTooDeep { .. }));
    }

    #[test]
    fn rejects_oversized_serialized_event() {
        let error = EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("Question".into()),
            metadata: Some(serde_json::json!({"padding": "x".repeat(MAX_EVENT_BYTES)})),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap_err();
        assert!(matches!(error, ValidationError::EventTooLarge { .. }));
    }

    #[test]
    fn follows_schema_control_character_boundaries() {
        EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("Unicode next-line \u{0085} is not C0".into()),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap();

        let error = EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("DEL \u{007f}".into()),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap_err();
        assert!(matches!(
            error,
            ValidationError::ControlCharacter { field: "title" }
        ));
    }

    #[test]
    fn rejects_non_rfc_source_uuid_variant() {
        let mut event = EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("Question".into()),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap();
        event.source.source_id = Uuid::nil();

        assert!(matches!(
            event.validate().unwrap_err(),
            ValidationError::SourceIdMustBeRfc4122
        ));
    }

    #[test]
    fn enforces_unicode_scalar_length_boundaries() {
        EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("界".repeat(120)),
            body: Some("🙂".repeat(1_000)),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap();

        let title_error = EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("界".repeat(121)),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap_err();
        assert!(matches!(
            title_error,
            ValidationError::InvalidLength { field: "title", .. }
        ));

        let body_error = EmitRequest {
            kind: Some(EventKind::AgentQuestion),
            title: Some("Question".into()),
            body: Some("🙂".repeat(1_001)),
            ..EmitRequest::default()
        }
        .normalize(source_id(), "local".into(), None)
        .unwrap_err();
        assert!(matches!(
            body_error,
            ValidationError::InvalidLength { field: "body", .. }
        ));
    }
}
