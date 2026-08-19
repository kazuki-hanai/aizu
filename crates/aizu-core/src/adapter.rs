use std::collections::BTreeMap;

use serde_json::{Map, Value};
use thiserror::Error;

use crate::{EmitRequest, EventKind, Outcome, parse_strict_json_value};

/// Converts an agent-owned hook payload into privacy-safe emit requests.
pub trait AgentAdapter {
    fn parse_hook(&self, event_name: &str, input: &[u8]) -> Result<Vec<EmitRequest>, AdapterError>;
}

/// First-party adapter for Claude Code command hooks.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeCodeAdapter;

const NOTIFICATION_EXCERPT_MAX_CHARS: usize = 240;
/// Placeholder substituted for a redacted credential-like token.
const REDACTED_PLACEHOLDER: &str = "[redacted]";
/// Placeholder substituted for a redacted private filesystem path.
const PATH_PLACEHOLDER: &str = "[path]";
/// Substrings that mark a token as an actual credential value. Any token that
/// contains one of these is masked in full.
const CREDENTIAL_VALUE_MARKERS: &[&str] = &[
    "ghp_",
    "github_pat_",
    "sk-ant-",
    "sk-proj-",
    "xoxb-",
    "xoxp-",
    "akia",
    "-----begin",
];
/// Keys whose `key=value` / `key: value` pair carries a secret value. The value
/// is masked while the key is preserved so the sentence stays readable.
const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "api_key",
    "api-key",
    "apikey",
    "access_token",
    "access-token",
    "refresh_token",
    "refresh-token",
    "token",
];

impl AgentAdapter for ClaudeCodeAdapter {
    fn parse_hook(&self, event_name: &str, input: &[u8]) -> Result<Vec<EmitRequest>, AdapterError> {
        let payload = parse_strict_json_value(input, crate::MAX_FRAME_BYTES)?;
        let object = payload
            .as_object()
            .ok_or(AdapterError::PayloadMustBeObject)?;
        let payload_event = required_string(object, "hook_event_name")?;
        if payload_event != event_name {
            return Err(AdapterError::EventNameMismatch {
                argument: event_name.to_owned(),
                payload: payload_event.to_owned(),
            });
        }

        let session_id = optional_string(object, "session_id")?
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let metadata = adapter_metadata(object, "claude-code-v1")?;
        let (kind, title, body, outcome) = match event_name {
            "Stop" => (
                EventKind::TaskCompleted,
                "Claude Code task completed",
                assistant_message(object)?,
                Some(Outcome::Unknown),
            ),
            "StopFailure" => (
                EventKind::TaskCompleted,
                "Claude Code task failed",
                assistant_message(object)?,
                Some(Outcome::Failed),
            ),
            "PermissionRequest" => (
                EventKind::AgentQuestion,
                "Claude Code is waiting for permission",
                permission_description(object, false)?,
                None,
            ),
            _ => return Err(AdapterError::UnsupportedEvent(event_name.to_owned())),
        };

        Ok(vec![EmitRequest {
            kind: Some(kind),
            title: Some(title.to_owned()),
            body,
            outcome,
            urgency: None,
            agent: Some("claude-code".to_owned()),
            session_id,
            occurred_at: None,
            metadata,
            ignored: BTreeMap::new(),
        }])
    }
}

/// First-party adapter for Codex command hooks.
#[derive(Clone, Copy, Debug, Default)]
pub struct CodexAdapter;

impl AgentAdapter for CodexAdapter {
    fn parse_hook(&self, event_name: &str, input: &[u8]) -> Result<Vec<EmitRequest>, AdapterError> {
        let payload = parse_strict_json_value(input, crate::MAX_FRAME_BYTES)?;
        let object = payload
            .as_object()
            .ok_or(AdapterError::PayloadMustBeObject)?;
        let payload_event = required_string(object, "hook_event_name")?;
        if payload_event != event_name {
            return Err(AdapterError::EventNameMismatch {
                argument: event_name.to_owned(),
                payload: payload_event.to_owned(),
            });
        }

        let session_id = optional_string(object, "session_id")?
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let metadata = adapter_metadata(object, "codex-v1")?;
        let (kind, title, body, outcome) = match event_name {
            "Stop" => (
                EventKind::TaskCompleted,
                "Codex task completed",
                assistant_message(object)?,
                Some(Outcome::Unknown),
            ),
            "PermissionRequest" => (
                EventKind::AgentQuestion,
                "Codex is waiting for permission",
                permission_description(object, true)?,
                None,
            ),
            _ => return Err(AdapterError::UnsupportedEvent(event_name.to_owned())),
        };

        Ok(vec![EmitRequest {
            kind: Some(kind),
            title: Some(title.to_owned()),
            body,
            outcome,
            urgency: None,
            agent: Some("codex".to_owned()),
            session_id,
            occurred_at: None,
            metadata,
            ignored: BTreeMap::new(),
        }])
    }
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, AdapterError> {
    optional_string(object, key)?.ok_or_else(|| AdapterError::MissingField(key.to_owned()))
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, AdapterError> {
    object
        .get(key)
        .map(Value::as_str)
        .transpose()
        .map_err(|()| AdapterError::InvalidFieldType(key.to_owned()))
}

fn optional_nullable_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, AdapterError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(AdapterError::InvalidFieldType(key.to_owned())),
    }
}

fn assistant_message(object: &Map<String, Value>) -> Result<Option<String>, AdapterError> {
    optional_nullable_string(object, "last_assistant_message")
        .map(|message| message.and_then(safe_agent_excerpt))
}

fn permission_description(
    object: &Map<String, Value>,
    use_tool_fallback: bool,
) -> Result<Option<String>, AdapterError> {
    let Some(tool_input) = object.get("tool_input") else {
        return Ok(None);
    };
    let Some(tool_input) = tool_input.as_object() else {
        return Ok(None);
    };
    if let Some(description) =
        optional_nullable_string(tool_input, "description")?.and_then(safe_agent_excerpt)
    {
        return Ok(Some(description));
    }

    let question = tool_input
        .get("questions")
        .and_then(Value::as_array)
        .and_then(|questions| questions.first())
        .and_then(Value::as_object)
        .and_then(|question| question.get("question"))
        .and_then(Value::as_str)
        .and_then(safe_agent_excerpt);
    if question.is_some() || !use_tool_fallback {
        return Ok(question);
    }

    let tool_name =
        optional_string(object, "tool_name")?.filter(|name| is_safe_metadata_label(name));
    Ok(tool_name.map(|name| format!("Allow this {name} request?")))
}

/// Produces the bounded excerpt shown in agent notifications and activity.
///
/// The agent message is preserved so it appears in every notification. Credential
/// values and private absolute paths are masked in place (`[redacted]` / `[path]`)
/// rather than causing the whole message to be dropped, so a single sensitive token
/// no longer silently hides the entire message. Values with unusable, non-whitespace
/// control characters are rejected because they cannot be rendered safely. Horizontal
/// whitespace is normalized while line and paragraph breaks are retained for the Aizu
/// banner.
#[must_use]
pub fn safe_agent_excerpt(value: &str) -> Option<String> {
    if value
        .chars()
        .any(|character| character.is_control() && !character.is_whitespace())
    {
        return None;
    }

    let normalized = redact_sensitive(&normalize_excerpt(value));
    let normalized = normalized.trim();
    if normalized.is_empty() {
        return None;
    }

    let length = normalized.chars().count();
    if length <= NOTIFICATION_EXCERPT_MAX_CHARS {
        return Some(normalized.to_owned());
    }

    let prefix: String = normalized
        .chars()
        .take(NOTIFICATION_EXCERPT_MAX_CHARS - 3)
        .collect();
    Some(format!("{prefix}..."))
}

fn normalize_excerpt(value: &str) -> String {
    let normalized_newlines = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut normalized = normalized_newlines
        .split('\n')
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    while normalized.contains("\n\n\n") {
        normalized = normalized.replace("\n\n\n", "\n\n");
    }
    normalized
}

/// Masks credential values and private paths token by token while keeping the rest
/// of the message intact. Line breaks are preserved so multi-line agent messages
/// still render in the banner.
fn redact_sensitive(value: &str) -> String {
    value
        .split('\n')
        .map(redact_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn redact_line(line: &str) -> String {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        let lower = token.to_ascii_lowercase();

        if looks_like_private_path(token) {
            out.push(PATH_PLACEHOLDER.to_owned());
            index += 1;
            continue;
        }
        if CREDENTIAL_VALUE_MARKERS
            .iter()
            .any(|marker| lower.contains(marker))
        {
            out.push(REDACTED_PLACEHOLDER.to_owned());
            index += 1;
            continue;
        }
        if let Some(masked) = redact_key_value(token) {
            out.push(masked);
            index += 1;
            continue;
        }

        // Authorization headers: keep the label (and any auth scheme word) but mask
        // the credential value that follows.
        if lower.trim_end_matches([':', '=']) == "authorization" {
            out.push(token.to_owned());
            let mut next = index + 1;
            if let Some(scheme) = tokens.get(next)
                && matches!(
                    scheme.to_ascii_lowercase().as_str(),
                    "bearer" | "basic" | "token"
                )
            {
                out.push((*scheme).to_owned());
                next += 1;
            }
            if next < tokens.len() {
                out.push(REDACTED_PLACEHOLDER.to_owned());
                next += 1;
            }
            index = next;
            continue;
        }
        // Bare `Bearer <token>` scheme without an Authorization label.
        if lower == "bearer" && index + 1 < tokens.len() {
            out.push(token.to_owned());
            out.push(REDACTED_PLACEHOLDER.to_owned());
            index += 2;
            continue;
        }

        out.push(token.to_owned());
        index += 1;
    }
    out.join(" ")
}

/// Masks the value half of a `key=value` or `key: value` token when the key names a
/// known secret, keeping the key so the sentence remains readable.
fn redact_key_value(token: &str) -> Option<String> {
    for separator in ['=', ':'] {
        if let Some(position) = token.find(separator) {
            let (key, remainder) = token.split_at(position);
            let value = &remainder[separator.len_utf8()..];
            if value.is_empty() {
                continue;
            }
            let normalized_key = key
                .to_ascii_lowercase()
                .trim_matches(|character: char| {
                    !character.is_ascii_alphanumeric() && character != '_' && character != '-'
                })
                .to_owned();
            if SENSITIVE_KEYS.contains(&normalized_key.as_str()) {
                return Some(format!("{key}{separator}{REDACTED_PLACEHOLDER}"));
            }
        }
    }
    None
}

fn looks_like_private_path(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ':' | ';' | '"' | '\'' | '`'
        )
    });
    token.starts_with("/Users/")
        || token.starts_with("/home/")
        || token.starts_with("/private/")
        || token.starts_with("/tmp/")
        || token.starts_with("/var/folders/")
        || token.starts_with("~/")
        || (token.as_bytes().get(1) == Some(&b':')
            && matches!(token.as_bytes().get(2), Some(b'\\' | b'/')))
}

fn adapter_metadata(
    object: &Map<String, Value>,
    adapter: &str,
) -> Result<Option<Value>, AdapterError> {
    let basename = optional_string(object, "cwd")?.and_then(|cwd| {
        cwd.trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !name.is_empty())
    });
    let tool_name = optional_string(object, "tool_name")?
        .filter(|name| is_safe_metadata_label(name))
        .map(str::to_owned);
    let mut metadata =
        Map::from_iter([("aizu_adapter".to_owned(), Value::String(adapter.to_owned()))]);
    if let Some(name) = basename.filter(|name| is_safe_metadata_label(name)) {
        metadata.insert(
            "working_directory_name".to_owned(),
            Value::String(name.to_owned()),
        );
    }
    if let Some(tool_name) = tool_name {
        metadata.insert("tool_name".to_owned(), Value::String(tool_name));
    }
    Ok(Some(Value::Object(metadata)))
}

fn is_safe_metadata_label(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 64
        && value
            .chars()
            .all(|character| !character.is_control() && character != '/' && character != '\\')
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("hook payload must be a JSON object")]
    PayloadMustBeObject,
    #[error("hook payload is missing required field {0}")]
    MissingField(String),
    #[error("hook payload field {0} has an invalid type")]
    InvalidFieldType(String),
    #[error("hook event argument {argument:?} does not match payload event {payload:?}")]
    EventNameMismatch { argument: String, payload: String },
    #[error("unsupported Claude Code hook event {0:?}")]
    UnsupportedEvent(String),
    #[error(transparent)]
    Protocol(#[from] crate::protocol::ProtocolError),
}

trait TransposeOption<T> {
    fn transpose(self) -> Result<Option<T>, ()>;
}

impl<T> TransposeOption<T> for Option<Option<T>> {
    fn transpose(self) -> Result<Option<T>, ()> {
        match self {
            None => Ok(None),
            Some(Some(value)) => Ok(Some(value)),
            Some(None) => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn claude_stop_uses_a_bounded_safe_final_message_excerpt() {
        let payload = br#"{
          "session_id":"session-123",
          "transcript_path":"/Users/private/.claude/transcript.jsonl",
          "cwd":"/Users/private/work/aizu",
          "hook_event_name":"Stop",
          "last_assistant_message":"Implemented SSH reconnect handling.\nAll focused tests pass."
        }"#;
        let request = ClaudeCodeAdapter
            .parse_hook("Stop", payload)
            .expect("valid hook")
            .pop()
            .expect("one event");
        assert_eq!(request.kind, Some(EventKind::TaskCompleted));
        assert_eq!(request.outcome, Some(Outcome::Unknown));
        assert_eq!(request.agent.as_deref(), Some("claude-code"));
        assert_eq!(request.session_id.as_deref(), Some("session-123"));
        assert_eq!(
            request.body.as_deref(),
            Some("Implemented SSH reconnect handling.\nAll focused tests pass.")
        );
        assert_eq!(
            request.metadata,
            Some(json!({ "aizu_adapter": "claude-code-v1", "working_directory_name": "aizu" }))
        );
        let serialized = serde_json::to_string(&request.metadata).expect("metadata JSON");
        assert!(!serialized.contains("private"));
        assert!(!serialized.contains("private"));
    }

    #[test]
    fn failure_and_permission_map_to_distinct_event_kinds() {
        let failed = ClaudeCodeAdapter
            .parse_hook(
                "StopFailure",
                br#"{"hook_event_name":"StopFailure","error":"rate_limit","error_details":"secret"}"#,
            )
            .expect("failure event");
        assert_eq!(failed[0].outcome, Some(Outcome::Failed));
        assert!(failed[0].body.is_none());

        let question = ClaudeCodeAdapter
            .parse_hook(
                "PermissionRequest",
                br#"{"hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf build","description":"Remove the stale build directory?"}}"#,
            )
            .expect("permission event");
        assert_eq!(question[0].kind, Some(EventKind::AgentQuestion));
        assert_eq!(question[0].outcome, None);
        assert_eq!(
            question[0].body.as_deref(),
            Some("Remove the stale build directory?")
        );
        assert!(
            !question[0]
                .body
                .as_deref()
                .unwrap_or_default()
                .contains("rm -rf")
        );
    }

    #[test]
    fn codex_stop_and_permission_drop_sensitive_fields() {
        let stop = CodexAdapter
            .parse_hook(
                "Stop",
                br#"{"session_id":"thread-123","turn_id":"turn-1","cwd":"/private/work/aizu","hook_event_name":"Stop","last_assistant_message":"Updated the connection test and verified reconnect."}"#,
            )
            .expect("stop");
        assert_eq!(stop[0].kind, Some(EventKind::TaskCompleted));
        assert_eq!(stop[0].outcome, Some(Outcome::Unknown));
        assert_eq!(stop[0].agent.as_deref(), Some("codex"));
        assert_eq!(
            stop[0].body.as_deref(),
            Some("Updated the connection test and verified reconnect.")
        );
        assert_eq!(
            stop[0].metadata,
            Some(json!({ "aizu_adapter": "codex-v1", "working_directory_name": "aizu" }))
        );

        let permission = CodexAdapter
            .parse_hook(
                "PermissionRequest",
                br#"{"session_id":"thread-123","turn_id":"turn-1","cwd":"/private/work/aizu","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"ssh remote-host deploy","description":"Deploy Aizu to the configured SSH host?"}}"#,
            )
            .expect("permission");
        assert_eq!(permission[0].kind, Some(EventKind::AgentQuestion));
        assert_eq!(
            permission[0].body.as_deref(),
            Some("Deploy Aizu to the configured SSH host?")
        );
        assert_eq!(
            permission[0]
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("tool_name"))
                .and_then(Value::as_str),
            Some("Bash")
        );
        let serialized = serde_json::to_string(&permission[0].metadata).expect("metadata");
        assert!(!serialized.contains("private"));
        assert!(!serialized.contains("ssh remote-host deploy"));
    }

    #[test]
    fn codex_permission_without_description_uses_only_the_safe_tool_name() {
        let permission = CodexAdapter
            .parse_hook(
                "PermissionRequest",
                br#"{"session_id":"thread-123","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"cat /Users/alice/.ssh/id_ed25519"}}"#,
            )
            .expect("permission");

        assert_eq!(
            permission[0].body.as_deref(),
            Some("Allow this Bash request?")
        );
        let rendered = format!("{:?}", permission[0]);
        assert!(!rendered.contains("id_ed25519"));
        assert!(!rendered.contains("/Users/alice"));
    }

    #[test]
    fn excerpts_redact_secrets_and_paths_but_keep_the_message() {
        // Secrets and private paths are masked in place so the agent message still
        // reaches the notification instead of vanishing entirely.
        for (message, expected) in [
            ("Use password=hunter2", "Use password=[redacted]"),
            ("Read /Users/alice/private.txt", "Read [path]"),
            ("token: Bearer abc123", "token: Bearer [redacted]"),
            ("Read `/home/user/private.txt` next", "Read [path] next"),
            ("Open `/Users/alice/private.txt` next", "Open [path] next"),
            ("Inspect `/tmp/aizu-debug.log` next", "Inspect [path] next"),
            (
                "Deploy uses ghp_exampletoken0000000000000000000000",
                "Deploy uses [redacted]",
            ),
        ] {
            let payload = json!({
                "hook_event_name": "Stop",
                "last_assistant_message": message
            });
            let request = CodexAdapter
                .parse_hook("Stop", payload.to_string().as_bytes())
                .expect("valid hook");
            assert_eq!(
                request[0].body.as_deref(),
                Some(expected),
                "message should be redacted, not dropped: {message}"
            );
        }
    }

    #[test]
    fn excerpts_reject_unusable_control_characters() {
        // Binary/garbage that cannot be rendered safely is still rejected entirely.
        let payload = json!({
            "hook_event_name": "Stop",
            "last_assistant_message": "bad\u{0000}message"
        });
        let request = CodexAdapter
            .parse_hook("Stop", payload.to_string().as_bytes())
            .expect("valid hook");
        assert_eq!(request[0].body, None);
    }

    #[test]
    fn excerpts_preserve_line_breaks_and_are_capped() {
        let long = format!("first   line\r\n\r\n\r\n\t{}", "x".repeat(300));
        let excerpt = safe_agent_excerpt(&long).expect("safe excerpt");
        assert_eq!(excerpt.chars().count(), NOTIFICATION_EXCERPT_MAX_CHARS);
        assert!(excerpt.starts_with("first line\n\n"));
        assert!(excerpt.ends_with("..."));
        assert!(!excerpt.contains("\n\n\n"));
    }

    #[test]
    fn absent_or_null_text_keeps_the_generic_body_fallback() {
        for payload in [
            br#"{"hook_event_name":"Stop"}"#.as_slice(),
            br#"{"hook_event_name":"Stop","last_assistant_message":null}"#.as_slice(),
        ] {
            let request = ClaudeCodeAdapter
                .parse_hook("Stop", payload)
                .expect("valid hook");
            assert_eq!(request[0].body, None);
        }

        let request = ClaudeCodeAdapter
            .parse_hook(
                "PermissionRequest",
                br#"{"hook_event_name":"PermissionRequest","tool_input":{"description":null}}"#,
            )
            .expect("valid hook");
        assert_eq!(request[0].body, None);
    }

    #[test]
    fn claude_ask_user_question_uses_only_the_first_explicit_question() {
        let request = ClaudeCodeAdapter
            .parse_hook(
                "PermissionRequest",
                br#"{
                    "hook_event_name":"PermissionRequest",
                    "tool_name":"AskUserQuestion",
                    "tool_input":{
                        "questions":[
                            {"question":"Which release channel should I use?","header":"Channel","options":[{"label":"Stable","description":"Production"}]},
                            {"question":"This second question is not shown","header":"Other","options":[]}
                        ],
                        "command":"raw content must not be copied"
                    }
                }"#,
            )
            .expect("valid hook");
        assert_eq!(
            request[0].body.as_deref(),
            Some("Which release channel should I use?")
        );
        assert!(
            !request[0]
                .body
                .as_deref()
                .unwrap_or_default()
                .contains("raw content")
        );
    }

    #[test]
    fn mismatches_invalid_types_and_unknown_events_are_rejected() {
        assert!(matches!(
            ClaudeCodeAdapter.parse_hook("Stop", br#"{"hook_event_name":"PermissionRequest"}"#),
            Err(AdapterError::EventNameMismatch { .. })
        ));
        assert!(matches!(
            ClaudeCodeAdapter.parse_hook("Stop", br#"{"hook_event_name":"Stop","cwd":42}"#),
            Err(AdapterError::InvalidFieldType(field)) if field == "cwd"
        ));
        assert!(matches!(
            ClaudeCodeAdapter.parse_hook("SessionStart", br#"{"hook_event_name":"SessionStart"}"#),
            Err(AdapterError::UnsupportedEvent(event)) if event == "SessionStart"
        ));
    }
}
