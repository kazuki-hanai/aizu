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
const MAX_PERCENT_DECODE_ROUNDS: usize = 8;
const MAX_CLASSIFICATION_BYTES: usize = 4_096;
/// Placeholder substituted for a redacted credential-like token.
const REDACTED_PLACEHOLDER: &str = "[redacted]";
/// Placeholder substituted for a redacted private filesystem path.
const PATH_PLACEHOLDER: &str = "[path]";
const PRIVATE_KEY_LABELS: &[&str] = &[
    "PRIVATE KEY",
    "RSA PRIVATE KEY",
    "EC PRIVATE KEY",
    "DSA PRIVATE KEY",
    "OPENSSH PRIVATE KEY",
    "ENCRYPTED PRIVATE KEY",
    "SECRET KEY",
    "PGP PRIVATE KEY BLOCK",
];
/// Provider-issued credential prefixes. These are matched only at the start of a
/// token and require a plausible credential length; substring matching would corrupt
/// normal words such as `risk-based`, `task-specific`, `Asia`, or `Aizawa`.
const PROVIDER_TOKEN_PREFIXES: &[&str] = &[
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
    "xapp-",
    "ya29.",
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

    let normalized = normalize_excerpt(value);
    if has_obfuscated_private_key_begin(&normalized) {
        return Some("[redacted private key]".to_owned());
    }
    let normalized = redact_sensitive(&normalized);
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
        if state.block == BlockState::PrivateKey {
            if private_key_end(&uppercase) {
                state.block = BlockState::None;
            }
            // Keep the original line count without exposing any bytes from the key body.
            redacted.push(String::new());
            continue;
        }
        // Enter fail-closed mode only for private/secret-key blocks. Public
        // certificates and ordinary BEGIN/END report markers remain visible.
        if let Some(begin) = private_key_begin(&uppercase) {
            let prefix = line.get(..begin).unwrap_or_default();
            let prefix = redact_line(prefix, &mut state);
            redacted.push(if prefix.is_empty() {
                "[redacted private key]".to_owned()
            } else {
                format!("{prefix} [redacted private key]")
            });
            if !private_key_end(&uppercase) {
                state.block = BlockState::PrivateKey;
            }
            continue;
        }
        if state.block == BlockState::PublicCertificate {
            if uppercase.contains("-----END CERTIFICATE-----") {
                state.block = BlockState::None;
                redacted.push(if line.trim() == "-----END CERTIFICATE-----" {
                    line.to_owned()
                } else {
                    redact_line(line, &mut state)
                });
            } else if is_certificate_payload(line) && !looks_like_known_credential(line.trim()) {
                redacted.push(line.to_owned());
            } else {
                redacted.push(redact_line(line, &mut state));
            }
            continue;
        }
        // Private markers take precedence if malformed input places both public and
        // private BEGIN markers on one line.
        if uppercase.contains("-----BEGIN CERTIFICATE-----") {
            let complete = uppercase.contains("-----END CERTIFICATE-----");
            if !complete {
                state.block = BlockState::PublicCertificate;
            }
            redacted.push(if line.trim() == "-----BEGIN CERTIFICATE-----" {
                line.to_owned()
            } else {
                redact_line(line, &mut state)
            });
            continue;
        }
        redacted.push(redact_line(line, &mut state));
    }
    redacted.join("\n")
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum BlockState {
    #[default]
    None,
    PrivateKey,
    PublicCertificate,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SecretState {
    #[default]
    None,
    AwaitDelimiter,
    AwaitValue,
    AwaitBearerValue,
    AwaitTokenOrDelimiter,
}

#[derive(Default)]
struct RedactionState {
    block: BlockState,
    secret: SecretState,
}

fn private_key_begin(uppercase: &str) -> Option<usize> {
    pem_labels(uppercase, "-----BEGIN")
        .find_map(|(offset, label)| is_private_key_label(label).then_some(offset))
}

fn private_key_end(uppercase: &str) -> bool {
    pem_labels(uppercase, "-----END").any(|(_, label)| is_private_key_label(label))
}

fn pem_labels<'a>(line: &'a str, marker: &'static str) -> impl Iterator<Item = (usize, &'a str)> {
    line.match_indices(marker).filter_map(move |(offset, _)| {
        let remainder = line.get(offset + marker.len()..)?.trim_start();
        let end = remainder.find("-----")?;
        let label = remainder.get(..end)?.trim();
        (!label.is_empty()).then_some((offset, label))
    })
}

fn is_private_key_label(label: &str) -> bool {
    PRIVATE_KEY_LABELS.contains(&label)
}

fn has_obfuscated_private_key_begin(value: &str) -> bool {
    let uppercase = value.to_ascii_uppercase();
    if uppercase
        .lines()
        .any(|line| private_key_begin(line).is_some())
    {
        return false;
    }
    let collapsed: String = uppercase
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    PRIVATE_KEY_LABELS.iter().any(|label| {
        let label: String = label
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        collapsed.contains(&format!("-----BEGIN{label}-----"))
            || collapsed.contains(&format!("-----BEGIN{label}"))
    })
}

fn is_certificate_payload(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 20
        && line.len().is_multiple_of(4)
        && line
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
}

fn redact_line(line: &str, state: &mut RedactionState) -> String {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        let lower = token.to_ascii_lowercase();

        if let Some(masked) = redact_direct_token(token, state, index + 1 < tokens.len()) {
            out.push(masked);
            index += 1;
            continue;
        }

        // Authorization with whitespace around the delimiter, e.g.
        // `Authorization : Bearer value` or `Authorization =Bearer value`.
        if lower == "authorization"
            && tokens
                .get(index + 1)
                .is_some_and(|next| split_leading_separator(next).is_some())
        {
            out.push(token.to_owned());
            state.secret = SecretState::AwaitValue;
            index += 1;
            continue;
        }

        if looks_like_non_file_uri(token) {
            out.push(token.to_owned());
            index += 1;
            continue;
        }
        if looks_like_private_path(token) {
            out.push(PATH_PLACEHOLDER.to_owned());
            index += 1;
            continue;
        }
        if looks_like_credential_value(token) {
            out.push(REDACTED_PLACEHOLDER.to_owned());
            index += 1;
            continue;
        }

        // Handle whitespace-separated assignments such as
        // `AWS_SECRET_ACCESS_KEY = value` without treating normal prose that happens
        // to contain the word "password" as an assignment.
        if is_sensitive_key(token)
            && tokens
                .get(index + 1)
                .is_some_and(|separator| split_leading_separator(separator).is_some())
        {
            out.push(token.to_owned());
            state.secret = SecretState::AwaitValue;
            index += 1;
            continue;
        }

        if let Some((masked, next_index)) = redact_contextual_phrase(&tokens, index) {
            out.extend(masked);
            index = next_index;
            continue;
        }

        // Keep the key/header visible and carry only the delimiter expectation across
        // a line boundary. If the next line is ordinary prose rather than `:`/`=`,
        // the pending state is cancelled without redacting it.
        if index + 1 == tokens.len() && matches!(lower.as_str(), "bearer" | "basic") {
            out.push(token.to_owned());
            state.secret = SecretState::AwaitBearerValue;
            index += 1;
            continue;
        }
        if index + 1 == tokens.len()
            && matches!(lower.as_str(), "token" | "blob" | "base64" | "encoded")
        {
            out.push(token.to_owned());
            state.secret = SecretState::AwaitTokenOrDelimiter;
            index += 1;
            continue;
        }
        if index + 1 == tokens.len() && (is_sensitive_key(token) || lower == "authorization") {
            out.push(token.to_owned());
            state.secret = SecretState::AwaitDelimiter;
            index += 1;
            continue;
        }

        out.push(token.to_owned());
        index += 1;
    }
    out.join(" ")
}

fn redact_direct_token(
    token: &str,
    state: &mut RedactionState,
    has_following_token: bool,
) -> Option<String> {
    if let Some(masked) = redact_expected_context(token, state, has_following_token)
        .or_else(|| redact_expected_delimiter(token, state))
        .or_else(|| redact_expected_secret(token, state))
        .or_else(|| redact_file_url(token))
        .or_else(|| redact_url_userinfo(token))
        .or_else(|| redact_uri_secrets(token))
    {
        return Some(masked);
    }
    if let Some(redaction) = redact_key_value(token) {
        return Some(match redaction {
            KeyValueRedaction::Inline(masked) => masked,
            KeyValueRedaction::NeedsValue(label) => {
                state.secret = SecretState::AwaitValue;
                label
            }
        });
    }
    let redaction = redact_authorization_header(token)?;
    state.secret = if redaction.needs_value {
        SecretState::AwaitValue
    } else {
        SecretState::None
    };
    Some(redaction.label)
}

fn redact_expected_context(
    token: &str,
    state: &mut RedactionState,
    has_following_token: bool,
) -> Option<String> {
    match state.secret {
        SecretState::AwaitBearerValue => {
            state.secret = SecretState::None;
            contextual_value_should_redact("bearer", token, has_following_token)
                .then(|| REDACTED_PLACEHOLDER.to_owned())
        }
        SecretState::AwaitTokenOrDelimiter => {
            state.secret = SecretState::None;
            if split_leading_separator(token).is_some() {
                state.secret = SecretState::AwaitValue;
                return redact_expected_secret(token, state);
            }
            contextual_value_should_redact("token", token, has_following_token)
                .then(|| REDACTED_PLACEHOLDER.to_owned())
        }
        _ => None,
    }
}

fn redact_contextual_phrase(tokens: &[&str], index: usize) -> Option<(Vec<String>, usize)> {
    let token = *tokens.get(index)?;
    let lower = token.to_ascii_lowercase();
    // A bare auth scheme is only treated as a credential when the next token is
    // credential-shaped. This avoids corrupting ordinary prose such as
    // "Bearer plants grow well here".
    if matches!(lower.as_str(), "bearer" | "basic")
        && tokens.get(index + 1).is_some_and(|value| {
            contextual_value_should_redact(lower.as_str(), value, index + 2 < tokens.len())
        })
    {
        return Some((
            vec![token.to_owned(), REDACTED_PLACEHOLDER.to_owned()],
            index + 2,
        ));
    }

    // Human-readable agent text may say `Token <value>` or
    // `Secret value <value>` instead of using key/value punctuation.
    if !is_sensitive_key(token) && !matches!(lower.as_str(), "blob" | "base64" | "encoded") {
        return None;
    }
    let filler = tokens
        .get(index + 1)
        .is_some_and(|next| matches!(next.to_ascii_lowercase().as_str(), "value" | "is"));
    let candidate_index = index + usize::from(filler) + 1;
    let candidate = tokens.get(candidate_index)?;
    if !contextual_value_should_redact(token, candidate, candidate_index + 1 < tokens.len()) {
        return None;
    }
    let mut masked = vec![token.to_owned()];
    if filler {
        masked.push(tokens[index + 1].to_owned());
    }
    masked.push(REDACTED_PLACEHOLDER.to_owned());
    Some((masked, candidate_index + 1))
}

fn redact_expected_delimiter(token: &str, state: &mut RedactionState) -> Option<String> {
    if state.secret != SecretState::AwaitDelimiter {
        return None;
    }
    state.secret = SecretState::None;
    split_leading_separator(token)?;
    state.secret = SecretState::AwaitValue;
    redact_expected_secret(token, state)
}

fn redact_expected_secret(token: &str, state: &mut RedactionState) -> Option<String> {
    if state.secret != SecretState::AwaitValue {
        return None;
    }
    let lower = token.to_ascii_lowercase();
    if matches!(lower.as_str(), "=" | ":" | "bearer" | "basic" | "token") {
        return Some(token.to_owned());
    }
    if let Some((separator, remainder)) = split_leading_separator(token) {
        let lower_remainder = remainder.to_ascii_lowercase();
        if remainder.is_empty() || matches!(lower_remainder.as_str(), "bearer" | "basic" | "token")
        {
            return Some(token.to_owned());
        }
        state.secret = SecretState::None;
        return Some(format!("{separator}{REDACTED_PLACEHOLDER}"));
    }
    state.secret = SecretState::None;
    Some(REDACTED_PLACEHOLDER.to_owned())
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

fn split_leading_separator(token: &str) -> Option<(char, &str)> {
    let separator = token
        .chars()
        .next()
        .filter(|value| matches!(value, ':' | '='))?;
    Some((separator, token.get(separator.len_utf8()..)?))
}

fn redact_file_url(token: &str) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    let file = lower.find("file:")?;
    if file > 0
        && !matches!(
            token.as_bytes().get(file.wrapping_sub(1)),
            Some(b'=' | b':' | b'"' | b'\'' | b'`' | b'(' | b'[' | b'{')
        )
    {
        return None;
    }
    let path_start = file + 5;
    let path = token.get(path_start..)?;
    let Some(classified_path) = bounded_percent_decode(path) else {
        return Some(format!(
            "{}{PATH_PLACEHOLDER}",
            token.get(..path_start).unwrap_or("file:")
        ));
    };
    let classified_path = classified_path.to_ascii_lowercase();
    let absolute = path.starts_with(['/', '\\'])
        || classified_path.starts_with("%2f")
        || classified_path.starts_with("%5c")
        || classified_path.starts_with(['/', '\\'])
        || (path.as_bytes().get(1) == Some(&b':')
            && matches!(path.as_bytes().get(2), Some(b'\\' | b'/')));
    absolute.then(|| {
        format!(
            "{}{PATH_PLACEHOLDER}",
            token.get(..path_start).unwrap_or("file:")
        )
    })
}

fn redact_url_userinfo(token: &str) -> Option<String> {
    let authority_start = uri_authority_start(token)?;
    let remainder = token.get(authority_start..)?;
    let authority_end = remainder
        .find(['/', '?', '#'])
        .map_or(token.len(), |offset| authority_start + offset);
    let at = token.get(authority_start..authority_end)?.rfind('@')? + authority_start;
    let userinfo = token.get(authority_start..at)?;
    (at > authority_start && (userinfo.contains(':') || looks_like_credential_value(userinfo)))
        .then(|| {
            format!(
                "{}{REDACTED_PLACEHOLDER}@{}",
                token.get(..authority_start).unwrap_or_default(),
                token.get(at + 1..).unwrap_or_default()
            )
        })
}

fn redact_uri_secrets(token: &str) -> Option<String> {
    if !looks_like_non_file_uri(token) {
        return None;
    }
    let Some(classified) = bounded_percent_decode(token) else {
        return Some("[redacted URI]".to_owned());
    };
    if redact_url_userinfo(&classified).is_some() {
        return Some("[redacted URI]".to_owned());
    }
    let sensitive_assignment =
        classified.chars().any(char::is_control) || uri_has_sensitive_assignment(&classified);
    let credential_component = classified
        .split(|character: char| {
            matches!(
                character,
                '/' | '?' | '&' | '#' | ',' | ';' | '=' | ':' | '@'
            )
        })
        .any(looks_like_credential_value);
    (sensitive_assignment || credential_component).then(|| "[redacted URI]".to_owned())
}

fn uri_has_sensitive_assignment(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    SENSITIVE_KEYS.iter().any(|key| {
        lower.match_indices(key).any(|(offset, _)| {
            let before = lower.as_bytes().get(offset.wrapping_sub(1)).copied();
            if offset > 0
                && !matches!(
                    before,
                    Some(b'?' | b'&' | b'#' | b',' | b';' | b'=' | b':' | b'/' | b'[')
                )
            {
                return false;
            }
            let mut value_start = offset + key.len();
            if lower.as_bytes().get(value_start) == Some(&b']') {
                value_start += 1;
            }
            if !matches!(lower.as_bytes().get(value_start), Some(b'=' | b':')) {
                return false;
            }
            value_start += 1;
            let value_end = lower
                .get(value_start..)
                .and_then(|value| {
                    value.find(|character: char| {
                        matches!(character, '&' | '#' | ',' | ';') || character.is_whitespace()
                    })
                })
                .map_or(lower.len(), |length| value_start + length);
            let value = uri.get(value_start..value_end).unwrap_or_default();
            if value.is_empty() {
                return false;
            }
            *key != "token" || looks_like_credential_value(value) || looks_like_token_value(value)
        })
    })
}

fn bounded_percent_decode(value: &str) -> Option<String> {
    if value.len() > MAX_CLASSIFICATION_BYTES {
        return None;
    }
    let mut current = value.as_bytes().to_vec();
    for _ in 0..MAX_PERCENT_DECODE_ROUNDS {
        let mut decoded = Vec::with_capacity(current.len());
        let mut index = 0;
        let mut changed = false;
        while index < current.len() {
            if current[index] == b'%' {
                let (Some(high), Some(low)) = (
                    current.get(index + 1).and_then(|byte| hex_value(*byte)),
                    current.get(index + 2).and_then(|byte| hex_value(*byte)),
                ) else {
                    return None;
                };
                decoded.push((high << 4) | low);
                index += 3;
                changed = true;
            } else {
                decoded.push(current[index]);
                index += 1;
            }
        }
        current = decoded;
        if !changed {
            return String::from_utf8(current).ok();
        }
    }
    let decoded = String::from_utf8(current).ok()?;
    (!contains_percent_encoding(&decoded)).then_some(decoded)
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn contains_percent_encoding(value: &str) -> bool {
    value.as_bytes().windows(3).any(|window| {
        window[0] == b'%' && hex_value(window[1]).is_some() && hex_value(window[2]).is_some()
    })
}

fn uri_authority_start(token: &str) -> Option<usize> {
    if let Some(scheme) = token.find("://") {
        return Some(scheme + 3);
    }
    if let Some(relative) = token.find("//")
        && (relative == 0
            || matches!(
                token.as_bytes().get(relative.wrapping_sub(1)),
                Some(b'=' | b':' | b'"' | b'\'' | b'`' | b'(' | b'[' | b'{')
            ))
    {
        return Some(relative + 2);
    }
    let colon = token.find(':')?;
    let scheme_start = token
        .get(..colon)?
        .rfind(['=', '"', '\'', '`', '(', '[', '{'])
        .map_or(0, |position| position + 1);
    let scheme = token.get(scheme_start..colon)?;
    (scheme.len() >= 2
        && scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphabetic()
                || (index > 0 && matches!(byte, b'+' | b'-' | b'.' | b'0'..=b'9'))
        }))
    .then_some(colon + 1)
}

fn looks_like_non_file_uri(token: &str) -> bool {
    let Some(authority_start) = uri_authority_start(token) else {
        return false;
    };
    let prefix = token
        .get(..authority_start)
        .unwrap_or_default()
        .to_ascii_lowercase();
    !prefix.ends_with("file:")
        && !prefix.ends_with("file://")
        && token
            .get(authority_start..)
            .is_some_and(|value| !value.is_empty())
}

fn looks_like_credential_value(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ':' | ';' | '"' | '\'' | '`'
        )
    });
    looks_like_known_credential(token) || looks_like_high_entropy_blob(token)
}

fn looks_like_known_credential(token: &str) -> bool {
    looks_like_provider_token(token)
        || looks_like_aws_access_key(token)
        || looks_like_google_api_key(token)
        || looks_like_jwt(token)
}

fn looks_like_provider_token(token: &str) -> bool {
    PROVIDER_TOKEN_PREFIXES.iter().any(|prefix| {
        token.starts_with(prefix)
            && token.len() >= prefix.len() + 8
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    })
}

fn looks_like_aws_access_key(token: &str) -> bool {
    token.len() == 20
        && (token.starts_with("AKIA") || token.starts_with("ASIA"))
        && token
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn looks_like_google_api_key(token: &str) -> bool {
    token.starts_with("AIza")
        && token.len() >= 30
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn looks_like_secret_candidate(token: &str) -> bool {
    if looks_like_credential_value(token) {
        return true;
    }
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ':' | ';' | '"' | '\'' | '`'
        )
    });
    (token.len() >= 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit()))
        || (token.len() >= 6
            && token.bytes().any(|byte| byte.is_ascii_digit())
            && token.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'_' | b'-' | b'=')
            }))
}

fn looks_like_bearer_value(token: &str) -> bool {
    if looks_like_credential_value(token) || looks_like_long_random_candidate(token) {
        return true;
    }
    let token = trimmed_token(token);
    if token.to_ascii_lowercase().starts_with("version") {
        return false;
    }
    (6..=10).contains(&token.len())
        && token.bytes().any(|byte| byte.is_ascii_digit())
        && token.bytes().all(|byte| byte.is_ascii_alphanumeric())
        || looks_like_token_value(token)
}

fn contextual_value_should_redact(key: &str, candidate: &str, has_following_token: bool) -> bool {
    if looks_like_credential_value(candidate) {
        return true;
    }
    let key = key
        .to_ascii_lowercase()
        .trim_matches(|character: char| {
            !character.is_ascii_alphanumeric() && character != '_' && character != '-'
        })
        .to_owned();
    if matches!(
        key.as_str(),
        "token" | "blob" | "base64" | "encoded" | "bearer" | "basic"
    ) {
        let shaped = if matches!(key.as_str(), "bearer" | "basic") {
            looks_like_bearer_value(candidate)
        } else {
            looks_like_token_value(candidate)
        };
        shaped && !(has_following_token && looks_like_ordinary_context_candidate(candidate))
    } else {
        looks_like_secret_candidate(candidate)
    }
}

fn looks_like_ordinary_context_candidate(token: &str) -> bool {
    let token = trimmed_token(token);
    let lower = token.to_ascii_lowercase();
    if token.bytes().all(|byte| byte.is_ascii_lowercase()) {
        let vowels = token
            .bytes()
            .filter(|byte| matches!(byte, b'a' | b'e' | b'i' | b'o' | b'u' | b'y'))
            .count();
        return vowels * 5 >= token.len()
            && vowels * 2 <= token.len()
            && !has_long_alphabet_sequence(token);
    }
    token
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && lower.contains(|character: char| character.is_ascii_digit())
        && (["version", "release", "build", "storage", "deployment"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
            || lower.ends_with("identifier"))
}

fn has_long_alphabet_sequence(token: &str) -> bool {
    let bytes = token.as_bytes();
    let mut run = 1;
    for pair in bytes.windows(2) {
        if pair[1] == pair[0].saturating_add(1) {
            run += 1;
            if run >= 8 {
                return true;
            }
        } else {
            run = 1;
        }
    }
    false
}

fn looks_like_token_value(token: &str) -> bool {
    let token = trimmed_token(token);
    if token.len() < 24
        || !token.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'_' | b'-' | b'=')
        })
    {
        return false;
    }
    true
}

fn looks_like_long_random_candidate(token: &str) -> bool {
    let token = trimmed_token(token);
    token.len() >= 24
        && token.bytes().any(|byte| byte.is_ascii_digit())
        && token.bytes().any(|byte| byte.is_ascii_lowercase())
        && token.bytes().any(|byte| byte.is_ascii_uppercase())
        && token.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'_' | b'-' | b'=')
        })
}

fn trimmed_token(token: &str) -> &str {
    token.trim_matches(|character: char| {
        matches!(
            character,
            '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ':' | ';' | '"' | '\'' | '`'
        )
    })
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
    if token.len() < 40 || !token.len().is_multiple_of(4) {
        return false;
    }
    if token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        // Long hexadecimal strings are often checksums or commit IDs. They are
        // redacted only when a preceding secret-key cue marks them as a value.
        return false;
    }
    let mut base64_symbol = false;
    for byte in token.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => {}
            b'+' | b'/' | b'=' => base64_symbol = true,
            _ => return false,
        }
    }
    base64_symbol
}

fn looks_like_private_path(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ':' | ';' | '"' | '\'' | '`'
        )
    });
    token.starts_with("~/")
        || contains_unc_path(token)
        || token.as_bytes().iter().enumerate().any(|(index, byte)| {
            *byte == b'/'
                && (index == 0
                    || matches!(
                        token.as_bytes()[index - 1],
                        b'=' | b':' | b'"' | b'\'' | b'`' | b'(' | b'[' | b'{'
                    ))
        })
        || token
            .as_bytes()
            .windows(3)
            .enumerate()
            .any(|(index, window)| {
                window[0].is_ascii_alphabetic()
                    && window[1] == b':'
                    && matches!(window[2], b'\\' | b'/')
                    && (index == 0
                        || matches!(
                            token.as_bytes()[index - 1],
                            b'=' | b'"' | b'\'' | b'`' | b'(' | b'[' | b'{'
                        ))
            })
}

fn contains_unc_path(token: &str) -> bool {
    token.match_indices(r"\\").any(|(offset, _)| {
        let Some(remainder) = token.get(offset + 2..) else {
            return false;
        };
        if let Some(extended) = remainder.strip_prefix("?\\") {
            return extended.as_bytes().get(1) == Some(&b':')
                && matches!(extended.as_bytes().get(2), Some(b'\\' | b'/'));
        }
        let mut parts = remainder.split('\\');
        let server = parts.next().unwrap_or_default();
        let share = parts.next().unwrap_or_default();
        !server.is_empty()
            && !share.is_empty()
            && server
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            && share
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    })
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
    #[allow(clippy::too_many_lines)]
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
                "Authorization : Bearer abc123",
                "Authorization : Bearer [redacted]",
            ),
            (
                "Authorization = Bearer abc123",
                "Authorization = Bearer [redacted]",
            ),
            ("password\n:hunter2", "password\n:[redacted]"),
            (
                "Authorization\n:\nBearer\nhunter2",
                "Authorization\n:\nBearer\n[redacted]",
            ),
            ("Bearer\nhunter2", "Bearer\n[redacted]"),
            (
                "Token\naB1cD2eF3gH4iJ5kL6mN7pQ8rS9tU0vW1xY2zA3b",
                "Token\n[redacted]",
            ),
            ("password :hunter2", "password :[redacted]"),
            ("Bearer hunter2", "Bearer [redacted]"),
            (
                "Bearer abcdefghijklmnopqrstuvwxyz123456",
                "Bearer [redacted]",
            ),
            (
                "Fetch https://alice:hunter2@example.com/resource",
                "Fetch https://[redacted]@example.com/resource",
            ),
            (
                "See https://example.com/@scope/docs",
                "See https://example.com/@scope/docs",
            ),
            (
                "Open https://user:pa@ss@host.example/path",
                "Open https://[redacted]@host.example/path",
            ),
            (
                "Open //user:pass@host.example/path",
                "Open //[redacted]@host.example/path",
            ),
            (
                "Open https:user:pass@host.example/path",
                "Open https:[redacted]@host.example/path",
            ),
            (
                "Open https://user%3Ahunter2@host.example/path",
                "Open [redacted URI]",
            ),
            (
                "Open //user%3Ahunter2@host.example/path",
                "Open [redacted URI]",
            ),
            (
                "Open https:user%3Ahunter2@host.example/path",
                "Open [redacted URI]",
            ),
            (
                "Open https://user%253Ahunter2@host.example/path",
                "Open [redacted URI]",
            ),
            (
                "Open file:///Users/alice/.ssh/id_ed25519",
                "Open file:[path]",
            ),
            (
                "Open file:README.md for details",
                "Open file:README.md for details",
            ),
            ("Open file:%2Froot%2F.ssh%2Fid_rsa", "Open file:[path]"),
            (
                "Open file:%252Froot%252F.ssh%252Fid_rsa",
                "Open file:[path]",
            ),
            (
                "Open file:%2525252Froot%2525252F.ssh%2525252Fid_rsa",
                "Open file:[path]",
            ),
            (
                "Open file:%25252525252Froot%25252525252F.ssh%25252525252Fid_rsa",
                "Open file:[path]",
            ),
            ("Open file:%2Groot%2F.ssh%2Fid_rsa", "Open file:[path]"),
            (r"Open \\server\share\Alice\secret.txt", "Open [path]"),
            (r"Open \\?\C:\Users\Alice\secret.txt", "Open [path]"),
            (r"path=\\server\share\Alice\secret.txt", "[path]"),
            ("Open /root/.ssh/id_rsa", "Open [path]"),
            ("Open /etc/aizu/private.conf", "Open [path]"),
            ("path=/etc/aizu/private.conf", "[path]"),
            (
                "Open https://example.com/root/docs",
                "Open https://example.com/root/docs",
            ),
            (
                "Open https://example.com/home/start",
                "Open https://example.com/home/start",
            ),
            (
                "Open https://host.example/callback?token=ghp_1234567890abcdefghijklmnopqrstuvwxyz",
                "Open [redacted URI]",
            ),
            (
                "Open https://host.example/login?password=hunter2",
                "Open [redacted URI]",
            ),
            (
                "Open https://host.example/path#xoxb-1234567890-abcdefgh",
                "Open [redacted URI]",
            ),
            (
                "url=https://host.example/callback?api_key=sk-1234567890abcdef",
                "[redacted URI]",
            ),
            (
                "Open data:text/plain,password=hunter2",
                "Open [redacted URI]",
            ),
            (
                "Open https://host.example/path?token%3Dghp_1234567890abcdefghijklmnopqrstuvwxyz",
                "Open [redacted URI]",
            ),
            (
                "Open https://host.example/path?pass%77ord=hunter2",
                "Open [redacted URI]",
            ),
            (
                "Open https://host.example/path?token%253Dghp_1234567890abcdefghijklmnopqrstuvwxyz",
                "Open [redacted URI]",
            ),
            (
                "Open https://host.example/path?token%252525253Dghp_1234567890abcdefghijklmnopqrstuvwxyz",
                "Open [redacted URI]",
            ),
            (
                "Open data:text/plain,password%3Dhunter2",
                "Open [redacted URI]",
            ),
            (
                "Open https://host.example/docs?token=estimate",
                "Open https://host.example/docs?token=estimate",
            ),
            (
                "Open https://host.example/docs?mytoken=estimate",
                "Open https://host.example/docs?mytoken=estimate",
            ),
            (
                "Open https://host/path?foo=password=hunter2",
                "Open [redacted URI]",
            ),
            (
                "Open https://host/path?foo=password%3Dhunter2",
                "Open [redacted URI]",
            ),
            (
                "Open https://host/path?user[password]=hunter2",
                "Open [redacted URI]",
            ),
            (
                "Open data:text/plain,foo=password=hunter2",
                "Open [redacted URI]",
            ),
            (
                "Open https://host/path?foo=%0Apassword=hunter2",
                "Open [redacted URI]",
            ),
            (
                "Open https://host/path?password%GGhunter2",
                "Open [redacted URI]",
            ),
            (
                "Open https://host/path?token%ghp_1234567890abcdefghijklmnopqrstuvwxyz",
                "Open [redacted URI]",
            ),
            (
                "Open https://host/path?password%2=hunter2",
                "Open [redacted URI]",
            ),
            (
                "Contact mailto:user@example.com",
                "Contact mailto:user@example.com",
            ),
            (
                "SSH target ssh:user@example.com",
                "SSH target ssh:user@example.com",
            ),
            (r"Regex \\d+ matches digits", r"Regex \\d+ matches digits"),
            (
                r"Windows escape \\n is a newline",
                r"Windows escape \\n is a newline",
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
                "Slack app token xapp-1-A1234567890-1234567890-abcdef",
                "Slack app token [redacted]",
            ),
            (
                "OpenAI sk-1234567890abcdefghijklmnopqrstuvwxyz",
                "OpenAI [redacted]",
            ),
            (
                "Token aB1cD2eF3gH4iJ5kL6mN7pQ8rS9tU0vW1xY2zA3b",
                "Token [redacted]",
            ),
            (
                "Secret value 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "Secret value [redacted]",
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
                "Blob YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYQ==",
                "Blob [redacted]",
            ),
            (
                "Blob QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==",
                "Blob [redacted]",
            ),
            (
                "Blob QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFB",
                "Blob [redacted]",
            ),
            (
                "Blob AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_ABCD",
                "Blob [redacted]",
            ),
            (
                "Base64 QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFB",
                "Base64 [redacted]",
            ),
            (
                "Encoded AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_ABCD",
                "Encoded [redacted]",
            ),
            (
                "Authorization is required before deployment",
                "Authorization is required before deployment",
            ),
            (
                "Bearer plants grow well here",
                "Bearer plants grow well here",
            ),
            (
                "Deploy to Asia after the checks",
                "Deploy to Asia after the checks",
            ),
            ("Use a risk-based rollout", "Use a risk-based rollout"),
            ("Run task-specific checks", "Run task-specific checks"),
            ("Use disk-backed storage", "Use disk-backed storage"),
            ("Ask Aizawa for review", "Ask Aizawa for review"),
            (
                "Artifact BuildIDAbC1234567890DefGhIjKlMnOpQrStUvWxYz completed",
                "Artifact BuildIDAbC1234567890DefGhIjKlMnOpQrStUvWxYz completed",
            ),
            (
                "Artifact Build_ID_AbCdEfGhIjKlMnOpQrStUvWxYz123456789 completed",
                "Artifact Build_ID_AbCdEfGhIjKlMnOpQrStUvWxYz123456789 completed",
            ),
            (
                "Token version2026 identifies the format",
                "Token version2026 identifies the format",
            ),
            (
                "Bearer version2026 compatibility is documented",
                "Bearer version2026 compatibility is documented",
            ),
            (
                "Bearer aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "Bearer [redacted]",
            ),
            ("Token aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "Token [redacted]"),
            (
                "Bearer abcdefghijklmnopqrstuvwxyzabcdef",
                "Bearer [redacted]",
            ),
            ("Token abcdefghijklmnopqrstuvwxyzabcdef", "Token [redacted]"),
            ("Blob abcdefghijklmnopqrstuvwxyzabcdef", "Blob [redacted]"),
            (
                "Base64 ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMN",
                "Base64 [redacted]",
            ),
            (
                "Encoded abcdefghijklmnopqrstuvwxyzabcdef",
                "Encoded [redacted]",
            ),
            (
                "Bearer internationalization remains supported",
                "Bearer internationalization remains supported",
            ),
            (
                "Token version2026identifier remains documented",
                "Token version2026identifier remains documented",
            ),
            (
                "Blob storageidentifier2026 remains documented",
                "Blob storageidentifier2026 remains documented",
            ),
            (
                "Token releasecandidate2026identifier remains documented",
                "Token releasecandidate2026identifier remains documented",
            ),
            (
                "Bearer releaseabcdefghijklmnopqrstuvwxyz",
                "Bearer [redacted]",
            ),
            ("Token buildabcdefghijklmnopqrstuvwxyz", "Token [redacted]"),
            (
                "Base64 versionABCDEFGHIJKLMNOPQRSTUVWXYZ",
                "Base64 [redacted]",
            ),
            (
                "Encoded abcdefghijklmnop123identifier",
                "Encoded [redacted]",
            ),
            (
                "Token antidisestablishmentarianism remains a word",
                "Token antidisestablishmentarianism remains a word",
            ),
            (
                "Blob customerreferenceidentifier remains documented",
                "Blob customerreferenceidentifier remains documented",
            ),
            (
                "Bearer abcdefghijklmnopqrstuvwxyzabcdef expires tomorrow",
                "Bearer [redacted] expires tomorrow",
            ),
            (
                "Token abcdefghijklmnopqrstuvwxyzabcdef is active",
                "Token [redacted] is active",
            ),
            (
                "Blob ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMN decoded successfully",
                "Blob [redacted] decoded successfully",
            ),
            (
                "Base64 ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMN decodes correctly",
                "Base64 [redacted] decodes correctly",
            ),
            (
                "Encoded abcdefghijklmnopqrstuvwxyzabcdef was received",
                "Encoded [redacted] was received",
            ),
            (
                "Bearer abcdefghijklmnopqrstuvwxyzabcdef, expires tomorrow",
                "Bearer [redacted] expires tomorrow",
            ),
            (
                "Bearer\nabcdefghijklmnopqrstuvwxyzabcdef expires tomorrow",
                "Bearer\n[redacted] expires tomorrow",
            ),
            (
                "Token value abcdefghijklmnopqrstuvwxyzabcdef is active",
                "Token value [redacted] is active",
            ),
        ] {
            assert_eq!(
                safe_agent_excerpt(message).as_deref(),
                Some(expected),
                "unexpected redaction for: {message}"
            );
        }

        let oversized_uri = format!(
            "https://host.example/path?token%3Dghp_1234567890abcdefghijklmnopqrstuvwxyz{}",
            "x".repeat(MAX_CLASSIFICATION_BYTES)
        );
        assert_eq!(
            safe_agent_excerpt(&oversized_uri).as_deref(),
            Some("[redacted URI]")
        );
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

        let split_marker = concat!(
            "-----BEGIN PRIVATE\n",
            "KEY-----\n",
            "shortSecretBody\n",
            "-----END PRIVATE KEY-----\n",
            "safe ending"
        );
        let excerpt = safe_agent_excerpt(split_marker).expect("malformed key block is redacted");
        assert_eq!(excerpt, "[redacted private key]");

        let misleading_end = concat!(
            "-----BEGIN PRIVATE KEY-----\n",
            "firstSecret\n",
            "-----END CERTIFICATE-----\n",
            "secondSecret\n",
            "-----END PRIVATE KEY-----\n",
            "safe ending"
        );
        let excerpt = safe_agent_excerpt(misleading_end).expect("key block stays fail closed");
        assert!(excerpt.contains("[redacted private key]"));
        assert!(excerpt.contains("safe ending"));
        assert!(!excerpt.contains("firstSecret"));
        assert!(!excerpt.contains("secondSecret"));
        assert!(!excerpt.contains("END PRIVATE KEY"));

        for obfuscated in [
            "-----BEGIN\nPRIVATE KEY-----\nsecretBody\n-----END PRIVATE KEY-----",
            "-----BE\nGIN PRIVATE KEY-----\nsecretBody\n-----END PRIVATE KEY-----",
            "-----BEGIN EC\nPRIVATE KEY-----\nsecretBody\n-----END EC PRIVATE KEY-----",
            "-----BEGIN DSA\nPRIVATE KEY-----\nsecretBody\n-----END DSA PRIVATE KEY-----",
            "-----BEGIN PGP\nPRIVATE KEY BLOCK-----\nsecretBody\n-----END PGP PRIVATE KEY BLOCK-----",
        ] {
            assert_eq!(
                safe_agent_excerpt(obfuscated).as_deref(),
                Some("[redacted private key]")
            );
        }
    }

    #[test]
    fn excerpts_preserve_non_secret_begin_end_blocks() {
        for message in [
            "-----BEGIN REPORT-----\nAll checks passed\n-----END REPORT-----\nDone",
            "-----BEGIN REPORT----- private results follow\nnormal report body\n-----END REPORT-----",
            "-----BEGIN CERTIFICATE-----\nYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYQ==\n-----END CERTIFICATE-----",
        ] {
            assert_eq!(
                safe_agent_excerpt(message).as_deref(),
                Some(message),
                "non-secret block should remain visible"
            );
        }

        assert_eq!(
            safe_agent_excerpt(
                "-----BEGIN CERTIFICATE-----\npassword=hunter2\n-----END CERTIFICATE-----"
            )
            .as_deref(),
            Some("-----BEGIN CERTIFICATE-----\npassword=[redacted]\n-----END CERTIFICATE-----")
        );
        assert_eq!(
            safe_agent_excerpt(
                "-----BEGIN CERTIFICATE----- password=hunter2 -----END CERTIFICATE-----"
            )
            .as_deref(),
            Some("-----BEGIN CERTIFICATE----- password=[redacted] -----END CERTIFICATE-----")
        );
        assert_eq!(
            safe_agent_excerpt(
                "-----BEGIN CERTIFICATE-----\nAKIAIOSFODNN7EXAMPLE\n-----END CERTIFICATE-----"
            )
            .as_deref(),
            Some("-----BEGIN CERTIFICATE-----\n[redacted]\n-----END CERTIFICATE-----")
        );
        let mixed = "-----BEGIN CERTIFICATE----- -----BEGIN PRIVATE KEY-----\nsecretBody\n-----END PRIVATE KEY-----";
        let excerpt = safe_agent_excerpt(mixed).expect("private marker wins");
        assert!(excerpt.contains("[redacted private key]"));
        assert!(!excerpt.contains("secretBody"));

        let nested = concat!(
            "-----BEGIN CERTIFICATE-----\n",
            "-----BEGIN PRIVATE KEY-----\n",
            "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFB\n",
            "-----END PRIVATE KEY-----\n",
            "-----END CERTIFICATE-----"
        );
        let excerpt = safe_agent_excerpt(nested).expect("nested private marker wins");
        assert!(excerpt.contains("[redacted private key]"));
        assert!(!excerpt.contains("QUFBQUFB"));
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
