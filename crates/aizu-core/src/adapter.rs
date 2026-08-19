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
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "gldt-",
    "sk-",
    "sk-ant-",
    "sk-proj-",
    "sk_live_",
    "rk_live_",
    "sk_test_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
    "akia",
    "asia",
    "aiza",
    "ya29.",
    "-----begin",
    "-----end",
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
    "client_secret",
    "client-secret",
    "consumer_secret",
    "consumer-secret",
    "private_key",
    "private-key",
    "secret_access_key",
    "secret-access-key",
    "aws_secret_access_key",
    "aws-secret-access-key",
    "aws_session_token",
    "aws-session-token",
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
    let mut state = RedactionState::default();
    let mut redacted = Vec::new();
    for line in value.split('\n') {
        let uppercase = line.to_ascii_uppercase();
        if state.in_private_key {
            if uppercase.contains("-----END") && uppercase.contains("PRIVATE KEY-----") {
                state.in_private_key = false;
            }
            // Keep the original line count without exposing any bytes from the key body.
            redacted.push(String::new());
            continue;
        }
        if let Some(begin) = uppercase.find("-----BEGIN")
            && uppercase[begin..].contains("PRIVATE KEY-----")
        {
            let prefix = line.get(..begin).unwrap_or_default();
            let prefix = redact_line(prefix, &mut state);
            redacted.push(if prefix.is_empty() {
                "[redacted private key]".to_owned()
            } else {
                format!("{prefix} [redacted private key]")
            });
            state.in_private_key = !uppercase[begin..].contains("-----END");
            continue;
        }
        redacted.push(redact_line(line, &mut state));
    }
    redacted.join("\n")
}

#[derive(Default)]
struct RedactionState {
    in_private_key: bool,
    expect_secret_value: bool,
}

fn redact_line(line: &str, state: &mut RedactionState) -> String {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        let lower = token.to_ascii_lowercase();

        if state.expect_secret_value {
            if matches!(lower.as_str(), "=" | ":" | "bearer" | "basic" | "token") {
                out.push(token.to_owned());
            } else {
                out.push(REDACTED_PLACEHOLDER.to_owned());
                state.expect_secret_value = false;
            }
            index += 1;
            continue;
        }
        if looks_like_private_path(token) {
            out.push(PATH_PLACEHOLDER.to_owned());
            index += 1;
            continue;
        }
        if let Some(masked) = redact_url_userinfo(token) {
            out.push(masked);
            index += 1;
            continue;
        }
        if looks_like_credential_value(token) {
            out.push(REDACTED_PLACEHOLDER.to_owned());
            index += 1;
            continue;
        }
        if let Some(redaction) = redact_key_value(token) {
            match redaction {
                KeyValueRedaction::Inline(masked) => out.push(masked),
                KeyValueRedaction::NeedsValue(label) => {
                    out.push(label);
                    state.expect_secret_value = true;
                }
            }
            index += 1;
            continue;
        }

        if let Some(redaction) = redact_authorization_header(token) {
            out.push(redaction.label);
            state.expect_secret_value = redaction.needs_value;
            index += 1;
            continue;
        }

        // A bare auth scheme is only treated as a credential when the next token is
        // credential-shaped. This avoids corrupting ordinary prose such as
        // "Bearer plants grow well here".
        if matches!(lower.as_str(), "bearer" | "basic")
            && tokens
                .get(index + 1)
                .is_some_and(|value| looks_like_credential_value(value))
        {
            out.push(token.to_owned());
            out.push(REDACTED_PLACEHOLDER.to_owned());
            index += 2;
            continue;
        }

        // Handle whitespace-separated assignments such as
        // `AWS_SECRET_ACCESS_KEY = value` without treating normal prose that happens
        // to contain the word "password" as an assignment.
        if is_sensitive_key(token)
            && tokens
                .get(index + 1)
                .is_some_and(|separator| matches!(*separator, "=" | ":"))
        {
            out.push(token.to_owned());
            state.expect_secret_value = true;
            index += 1;
            continue;
        }

        out.push(token.to_owned());
        index += 1;
    }
    out.join(" ")
}

/// Masks the value half of a `key=value` or `key: value` token when the key names a
/// known secret, keeping the key so the sentence remains readable.
fn redact_key_value(token: &str) -> Option<KeyValueRedaction> {
    for separator in ['=', ':'] {
        if let Some(position) = token.find(separator) {
            let (key, remainder) = token.split_at(position);
            let value = &remainder[separator.len_utf8()..];
            if is_sensitive_key(key) {
                return Some(if value.is_empty() {
                    KeyValueRedaction::NeedsValue(token.to_owned())
                } else {
                    KeyValueRedaction::Inline(format!("{key}{separator}{REDACTED_PLACEHOLDER}"))
                });
            }
        }
    }
    None
}

enum KeyValueRedaction {
    Inline(String),
    NeedsValue(String),
}

struct AuthorizationRedaction {
    label: String,
    needs_value: bool,
}

fn redact_authorization_header(token: &str) -> Option<AuthorizationRedaction> {
    let lower = token.to_ascii_lowercase();
    let (position, separator) = lower
        .find(':')
        .map(|position| (position, ':'))
        .or_else(|| lower.find('=').map(|position| (position, '=')))?;
    if lower.get(..position)? != "authorization" {
        return None;
    }

    let remainder = token.get(position + separator.len_utf8()..)?;
    if remainder.is_empty() {
        return Some(AuthorizationRedaction {
            label: token.to_owned(),
            needs_value: true,
        });
    }
    let lower_remainder = remainder.to_ascii_lowercase();
    for scheme in ["bearer", "basic", "token"] {
        if lower_remainder == scheme {
            return Some(AuthorizationRedaction {
                label: token.to_owned(),
                needs_value: true,
            });
        }
        if lower_remainder.starts_with(scheme)
            && lower_remainder
                .get(scheme.len()..)
                .is_some_and(|value| !value.is_empty())
        {
            let prefix = token.get(..position + 1 + scheme.len())?;
            return Some(AuthorizationRedaction {
                label: format!("{prefix}{REDACTED_PLACEHOLDER}"),
                needs_value: false,
            });
        }
    }
    Some(AuthorizationRedaction {
        label: format!(
            "{}{separator}{REDACTED_PLACEHOLDER}",
            token.get(..position)?
        ),
        needs_value: false,
    })
}

fn is_sensitive_key(value: &str) -> bool {
    let normalized = value
        .to_ascii_lowercase()
        .trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && character != '_' && character != '-'
        })
        .to_owned();
    SENSITIVE_KEYS.contains(&normalized.as_str())
}

fn redact_url_userinfo(token: &str) -> Option<String> {
    let scheme_end = token.find("://")? + 3;
    let at = token.get(scheme_end..)?.find('@')? + scheme_end;
    (at > scheme_end).then(|| {
        format!(
            "{}{REDACTED_PLACEHOLDER}@{}",
            token.get(..scheme_end).unwrap_or_default(),
            token.get(at + 1..).unwrap_or_default()
        )
    })
}

fn looks_like_credential_value(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ':' | ';' | '"' | '\'' | '`'
        )
    });
    let lower = token.to_ascii_lowercase();
    CREDENTIAL_VALUE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
        || looks_like_jwt(token)
        || looks_like_high_entropy_blob(token)
}

fn looks_like_jwt(token: &str) -> bool {
    let segments: Vec<&str> = token.split('.').collect();
    segments.len() == 3
        && segments.iter().all(|segment| {
            segment.len() >= 8
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

fn looks_like_high_entropy_blob(token: &str) -> bool {
    if token.len() < 40
        || token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return false;
    }
    let mut lower = false;
    let mut upper = false;
    let mut digit = false;
    let mut base64_symbol = false;
    for byte in token.bytes() {
        match byte {
            b'a'..=b'z' => lower = true,
            b'A'..=b'Z' => upper = true,
            b'0'..=b'9' => digit = true,
            b'+' | b'/' | b'_' | b'-' | b'=' => base64_symbol = true,
            _ => return false,
        }
    }
    lower && upper && digit && (base64_symbol || token.len() >= 48)
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
    fn excerpts_redact_adversarial_credential_formats_without_corrupting_prose() {
        for (message, expected) in [
            ("password: hunter2", "password: [redacted]"),
            (
                "Authorization:Bearer abc123",
                "Authorization:Bearer [redacted]",
            ),
            (
                "Authorization:\nBearer\nabc123",
                "Authorization:\nBearer\n[redacted]",
            ),
            (
                "Fetch https://alice:hunter2@example.com/resource",
                "Fetch https://[redacted]@example.com/resource",
            ),
            (
                "AWS_SECRET_ACCESS_KEY = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
                "AWS_SECRET_ACCESS_KEY = [redacted]",
            ),
            (
                "AWS session uses ASIAIOSFODNN7EXAMPLE",
                "AWS session uses [redacted]",
            ),
            (
                "GitHub OAuth gho_1234567890abcdefghijklmnopqrstuvwxyz",
                "GitHub OAuth [redacted]",
            ),
            ("GitLab glpat-1234567890abcdefghijkl", "GitLab [redacted]"),
            (
                "OpenAI sk-1234567890abcdefghijklmnopqrstuvwxyz",
                "OpenAI [redacted]",
            ),
            (
                "JWT eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
                "JWT [redacted]",
            ),
            (
                "Blob QWxhZGRpbjpvcGVuIHNlc2FtZTEyMzQ1Njc4OUFCQ0RFRg==",
                "Blob [redacted]",
            ),
            (
                "Authorization is required before deployment",
                "Authorization is required before deployment",
            ),
            (
                "Bearer plants grow well here",
                "Bearer plants grow well here",
            ),
        ] {
            assert_eq!(
                safe_agent_excerpt(message).as_deref(),
                Some(expected),
                "unexpected redaction for: {message}"
            );
        }
    }

    #[test]
    fn excerpts_redact_complete_multiline_private_keys() {
        let message = concat!(
            "Use this key:\n",
            "-----BEGIN PRIVATE KEY-----\n",
            "QWxhZGRpbjpvcGVuIHNlc2FtZTEyMzQ1Njc4OUFCQ0RFRg==\n",
            "another-key-body-line\n",
            "-----END PRIVATE KEY-----\n",
            "for deployment"
        );
        let excerpt = safe_agent_excerpt(message).expect("redacted message remains visible");
        assert!(excerpt.contains("[redacted private key]"));
        assert!(excerpt.contains("Use this key:"));
        assert!(excerpt.contains("for deployment"));
        assert!(!excerpt.contains("QWxhZGR"));
        assert!(!excerpt.contains("another-key-body-line"));
        assert!(!excerpt.contains("BEGIN PRIVATE KEY"));
        assert!(!excerpt.contains("END PRIVATE KEY"));
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
