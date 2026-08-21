use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{AgentKind, MAX_FRAME_BYTES, parse_strict_json_value};

/// Version of the local, ephemeral approval protocol.
pub const LOCAL_APPROVAL_PROTOCOL_VERSION: u16 = 2;
/// Maximum encoded local approval frame size, excluding a trailing newline.
pub const MAX_LOCAL_APPROVAL_FRAME_BYTES: usize = 32_768;
/// Maximum exact command size shown for a local approval.
pub const MAX_LOCAL_APPROVAL_COMMAND_BYTES: usize = 16_384;
/// Maximum exact `WebFetch` URL size shown for a local approval.
pub const MAX_LOCAL_APPROVAL_URL_BYTES: usize = 8_192;
/// Maximum tool label size accepted from an agent payload.
pub const MAX_LOCAL_APPROVAL_TOOL_BYTES: usize = 64;
/// Safe event marker used to suppress the duplicate passive notification after
/// an approval banner was actually presented.
pub const LOCAL_APPROVAL_PRESENTED_METADATA_KEY: &str = "aizu_local_approval_presented";

/// One-shot decision returned to an agent hook.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    Deny,
}

/// Exact, ephemeral target shown by the local approval UI.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalApprovalTarget {
    ShellCommand { command: String },
    WebFetch { url: String },
}

/// Ephemeral request sent from a local first-party hook to the desktop app.
///
/// This type intentionally does not implement `Debug`: the target may contain
/// sensitive input and must not be copied into logs, history, or the spool.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalApprovalRequest {
    pub version: u16,
    pub request_id: Uuid,
    pub agent: AgentKind,
    pub tool_name: String,
    pub target: LocalApprovalTarget,
}

impl LocalApprovalRequest {
    /// Creates and validates a request without retaining unrelated hook fields.
    pub fn new(
        request_id: Uuid,
        agent: AgentKind,
        tool_name: String,
        target: LocalApprovalTarget,
    ) -> Result<Self, ApprovalError> {
        validate_tool_name(&tool_name)?;
        validate_target(&target)?;
        Ok(Self {
            version: LOCAL_APPROVAL_PROTOCOL_VERSION,
            request_id,
            agent,
            tool_name,
            target,
        })
    }

    /// Validates a decoded request at the desktop trust boundary.
    pub fn validate(&self) -> Result<(), ApprovalError> {
        if self.version != LOCAL_APPROVAL_PROTOCOL_VERSION {
            return Err(ApprovalError::UnsupportedVersion(self.version));
        }
        validate_tool_name(&self.tool_name)?;
        validate_target(&self.target)
    }
}

/// Bounded response returned by the desktop approval broker.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalApprovalResponse {
    Decision {
        request_id: Uuid,
        decision: ApprovalDecision,
    },
    Unavailable {
        request_id: Uuid,
        presented: bool,
    },
}

impl LocalApprovalResponse {
    #[must_use]
    pub const fn request_id(self) -> Uuid {
        match self {
            Self::Decision { request_id, .. } | Self::Unavailable { request_id, .. } => request_id,
        }
    }
}

/// Extracts a typed, exact target from a supported first-party
/// `PermissionRequest`. Unsupported tools return `None` so the agent's normal
/// approval UI remains authoritative.
pub fn local_approval_request_from_hook(
    agent: AgentKind,
    event_name: &str,
    input: &[u8],
) -> Result<Option<LocalApprovalRequest>, ApprovalError> {
    if event_name != "PermissionRequest" {
        return Ok(None);
    }
    let payload = parse_strict_json_value(input, MAX_FRAME_BYTES)?;
    let object = payload
        .as_object()
        .ok_or(ApprovalError::PayloadMustBeObject)?;
    let payload_event = object
        .get("hook_event_name")
        .and_then(Value::as_str)
        .ok_or(ApprovalError::InvalidEventName)?;
    if payload_event != event_name {
        return Err(ApprovalError::InvalidEventName);
    }
    let Some(tool_name) = object.get("tool_name").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(tool_input) = object.get("tool_input").and_then(Value::as_object) else {
        return Ok(None);
    };
    let target = match (agent, tool_name) {
        (_, "Bash") => {
            let Some(command) = tool_input.get("command").and_then(Value::as_str) else {
                return Ok(None);
            };
            LocalApprovalTarget::ShellCommand {
                command: command.to_owned(),
            }
        }
        (AgentKind::ClaudeCode, "WebFetch") => {
            let Some(url) = tool_input.get("url").and_then(Value::as_str) else {
                return Ok(None);
            };
            LocalApprovalTarget::WebFetch {
                url: url.to_owned(),
            }
        }
        _ => return Ok(None),
    };
    LocalApprovalRequest::new(Uuid::new_v4(), agent, tool_name.to_owned(), target).map(Some)
}

fn validate_target(target: &LocalApprovalTarget) -> Result<(), ApprovalError> {
    match target {
        LocalApprovalTarget::ShellCommand { command } => validate_command(command),
        LocalApprovalTarget::WebFetch { url } => validate_web_url(url),
    }
}

fn validate_tool_name(value: &str) -> Result<(), ApprovalError> {
    if value.is_empty()
        || value.len() > MAX_LOCAL_APPROVAL_TOOL_BYTES
        || value.chars().any(is_unsafe_display_character)
    {
        return Err(ApprovalError::InvalidToolName);
    }
    Ok(())
}

fn validate_command(value: &str) -> Result<(), ApprovalError> {
    if value.is_empty()
        || value.len() > MAX_LOCAL_APPROVAL_COMMAND_BYTES
        || value.chars().any(|character| {
            character != '\n' && character != '\t' && is_unsafe_display_character(character)
        })
    {
        return Err(ApprovalError::InvalidCommand);
    }
    Ok(())
}

fn validate_web_url(value: &str) -> Result<(), ApprovalError> {
    if value.is_empty()
        || value.len() > MAX_LOCAL_APPROVAL_URL_BYTES
        || value.chars().any(is_unsafe_display_character)
    {
        return Err(ApprovalError::InvalidUrl);
    }
    let parsed = Url::parse(value).map_err(|_| ApprovalError::InvalidUrl)?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(ApprovalError::InvalidUrl);
    }
    Ok(())
}

fn is_unsafe_display_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
        )
}

#[derive(Debug, Error)]
pub enum ApprovalError {
    #[error("the approval payload must be a JSON object")]
    PayloadMustBeObject,
    #[error("the approval hook event name is invalid")]
    InvalidEventName,
    #[error("the approval tool name is invalid")]
    InvalidToolName,
    #[error("the approval command is invalid or too large")]
    InvalidCommand,
    #[error("the approval URL is invalid or too large")]
    InvalidUrl,
    #[error("local approval protocol version {0} is not supported")]
    UnsupportedVersion(u16),
    #[error(transparent)]
    Protocol(#[from] crate::protocol::ProtocolError),
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalError, LOCAL_APPROVAL_PROTOCOL_VERSION, LocalApprovalTarget,
        MAX_LOCAL_APPROVAL_COMMAND_BYTES, MAX_LOCAL_APPROVAL_URL_BYTES,
        local_approval_request_from_hook,
    };
    use crate::AgentKind;

    #[test]
    fn extracts_only_the_exact_command_and_bounded_labels() {
        for agent in [AgentKind::Codex, AgentKind::ClaudeCode] {
            let request = local_approval_request_from_hook(
                agent,
                "PermissionRequest",
                br#"{"hook_event_name":"PermissionRequest","cwd":"/private/work","tool_name":"Bash","tool_input":{"command":"printf 'hello\\nworld'","description":"Run it?"}}"#,
            )
            .expect("payload should parse")
            .expect("command request should be supported");

            assert_eq!(request.version, LOCAL_APPROVAL_PROTOCOL_VERSION);
            assert_eq!(request.agent, agent);
            assert_eq!(request.tool_name, "Bash");
            assert!(matches!(
                request.target,
                LocalApprovalTarget::ShellCommand { ref command }
                    if command == "printf 'hello\\nworld'"
            ));
            let encoded = serde_json::to_string(&request).expect("request should serialize");
            assert!(!encoded.contains("/private/work"));
            assert!(!encoded.contains("Run it?"));
        }
    }

    #[test]
    fn extracts_only_a_valid_claude_web_fetch_url() {
        let request = local_approval_request_from_hook(
            AgentKind::ClaudeCode,
            "PermissionRequest",
            br#"{"session_id":"private-session","transcript_path":"/private/transcript","cwd":"/private/work","hook_event_name":"PermissionRequest","tool_name":"WebFetch","tool_input":{"url":"https://docs.example.com/api?view=full#usage","prompt":"Summarize private instructions"}}"#,
        )
        .expect("payload should parse")
        .expect("WebFetch request should be supported");

        assert_eq!(request.tool_name, "WebFetch");
        assert!(matches!(
            request.target,
            LocalApprovalTarget::WebFetch { ref url }
                if url == "https://docs.example.com/api?view=full#usage"
        ));
        let encoded = serde_json::to_string(&request).expect("request should serialize");
        assert!(!encoded.contains("private-session"));
        assert!(!encoded.contains("/private/"));
        assert!(!encoded.contains("private instructions"));
    }

    #[test]
    fn unsupported_permission_inputs_fall_back_to_the_agent() {
        let request = local_approval_request_from_hook(
            AgentKind::ClaudeCode,
            "PermissionRequest",
            br#"{"hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_input":{"question":"Continue?"}}"#,
        )
        .expect("payload should parse");

        assert!(request.is_none());

        let request = local_approval_request_from_hook(
            AgentKind::Codex,
            "PermissionRequest",
            br#"{"hook_event_name":"PermissionRequest","tool_name":"WebFetch","tool_input":{"url":"https://example.com/"}}"#,
        )
        .expect("payload should parse");

        assert!(request.is_none());

        let request = local_approval_request_from_hook(
            AgentKind::Codex,
            "PermissionRequest",
            br#"{"hook_event_name":"PermissionRequest","tool_name":"mcp__deploy__run","tool_input":{"command":"deploy --production","environment":"production"}}"#,
        )
        .expect("payload should parse");

        assert!(request.is_none());
    }

    #[test]
    fn rejects_oversized_commands_without_truncating_them() {
        let command = "x".repeat(MAX_LOCAL_APPROVAL_COMMAND_BYTES + 1);
        let payload = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": command },
        }))
        .expect("fixture should serialize");

        assert!(matches!(
            local_approval_request_from_hook(AgentKind::Codex, "PermissionRequest", &payload),
            Err(ApprovalError::InvalidCommand)
        ));

        for command in ["printf ok\u{1b}[2J", "printf safe\u{202e}hidden"] {
            let payload = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "tool_name": "Bash",
                "tool_input": { "command": command },
            }))
            .expect("fixture should serialize");
            assert!(matches!(
                local_approval_request_from_hook(AgentKind::Codex, "PermissionRequest", &payload),
                Err(ApprovalError::InvalidCommand)
            ));
        }
    }

    #[test]
    fn rejects_invalid_web_fetch_urls_without_truncating_them() {
        let oversized_url = format!(
            "https://example.com/{}",
            "x".repeat(MAX_LOCAL_APPROVAL_URL_BYTES)
        );
        for url in [
            oversized_url.as_str(),
            "file:///private/data",
            "javascript:alert(1)",
            "https://example.com/safe\u{202e}hidden",
        ] {
            let payload = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "tool_name": "WebFetch",
                "tool_input": { "url": url, "prompt": "Read it" },
            }))
            .expect("fixture should serialize");
            assert!(matches!(
                local_approval_request_from_hook(
                    AgentKind::ClaudeCode,
                    "PermissionRequest",
                    &payload,
                ),
                Err(ApprovalError::InvalidUrl)
            ));
        }
    }
}
