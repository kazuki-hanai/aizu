use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{AgentKind, MAX_FRAME_BYTES, parse_strict_json_value};

/// Version of the local, ephemeral approval protocol.
pub const LOCAL_APPROVAL_PROTOCOL_VERSION: u16 = 3;
/// Maximum encoded local approval frame size, excluding a trailing newline.
pub const MAX_LOCAL_APPROVAL_FRAME_BYTES: usize = 32_768;
/// Maximum exact command size shown for a local approval.
pub const MAX_LOCAL_APPROVAL_COMMAND_BYTES: usize = 16_384;
/// Maximum exact `WebFetch` URL size shown for a local approval.
pub const MAX_LOCAL_APPROVAL_URL_BYTES: usize = 8_192;
/// Maximum tool label size accepted from an agent payload.
pub const MAX_LOCAL_APPROVAL_TOOL_BYTES: usize = 64;
/// Maximum bounded question size shown for a local `AskUserQuestion`.
pub const MAX_LOCAL_APPROVAL_QUESTION_BYTES: usize = 4_096;
/// Maximum bounded optional header size shown for a local `AskUserQuestion`.
pub const MAX_LOCAL_APPROVAL_HEADER_BYTES: usize = 256;
/// Maximum bounded option label size shown for a local `AskUserQuestion`.
pub const MAX_LOCAL_APPROVAL_OPTION_LABEL_BYTES: usize = 512;
/// Maximum bounded option description size shown for a local `AskUserQuestion`.
pub const MAX_LOCAL_APPROVAL_OPTION_DESCRIPTION_BYTES: usize = 2_048;
/// Maximum number of bounded options accepted for a local `AskUserQuestion`.
pub const MAX_LOCAL_APPROVAL_OPTIONS: usize = 8;
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

/// Bounded, ephemeral option shown for a local `AskUserQuestion`.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalApprovalOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Exact, ephemeral target shown by the local approval UI.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LocalApprovalTarget {
    ShellCommand {
        command: String,
    },
    WebFetch {
        url: String,
    },
    Question {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        question: String,
        options: Vec<LocalApprovalOption>,
    },
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
        validate_supported_target(agent, &tool_name, &target)?;
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
        validate_target(&self.target)?;
        validate_supported_target(self.agent, &self.tool_name, &self.target)
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
    Answer {
        request_id: Uuid,
        option_index: u16,
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
            Self::Decision { request_id, .. }
            | Self::Answer { request_id, .. }
            | Self::Unavailable { request_id, .. } => request_id,
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
    match event_name {
        "PermissionRequest" => {}
        "PreToolUse" => return question_request_from_hook(agent, input),
        _ => return Ok(None),
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

/// Extracts a single-select `AskUserQuestion` from a Claude Code `PreToolUse`
/// hook. Only the first, non-`multiSelect` question is supported; every other
/// shape returns `None` so the agent's terminal question remains authoritative.
fn question_request_from_hook(
    agent: AgentKind,
    input: &[u8],
) -> Result<Option<LocalApprovalRequest>, ApprovalError> {
    if agent != AgentKind::ClaudeCode {
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
    if payload_event != "PreToolUse" {
        return Err(ApprovalError::InvalidEventName);
    }
    let Some(tool_name) = object.get("tool_name").and_then(Value::as_str) else {
        return Ok(None);
    };
    if tool_name != "AskUserQuestion" {
        return Ok(None);
    }
    let Some(tool_input) = object.get("tool_input").and_then(Value::as_object) else {
        return Ok(None);
    };
    let Some(questions) = tool_input.get("questions").and_then(Value::as_array) else {
        return Ok(None);
    };
    if questions.len() != 1 {
        return Ok(None);
    }
    let Some(first) = questions.first().and_then(Value::as_object) else {
        return Ok(None);
    };
    if first.get("multiSelect").and_then(Value::as_bool) == Some(true) {
        return Ok(None);
    }
    let Some(question) = first.get("question").and_then(Value::as_str) else {
        return Ok(None);
    };
    let header = first
        .get("header")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Some(raw_options) = first.get("options").and_then(Value::as_array) else {
        return Ok(None);
    };
    let mut options = Vec::with_capacity(raw_options.len());
    for raw_option in raw_options {
        let Some(option) = raw_option.as_object() else {
            return Ok(None);
        };
        let Some(label) = option.get("label").and_then(Value::as_str) else {
            return Ok(None);
        };
        let description = option
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        options.push(LocalApprovalOption {
            label: label.to_owned(),
            description,
        });
    }
    if options.is_empty() {
        return Ok(None);
    }
    let target = LocalApprovalTarget::Question {
        header,
        question: question.to_owned(),
        options,
    };
    LocalApprovalRequest::new(Uuid::new_v4(), agent, tool_name.to_owned(), target).map(Some)
}

fn validate_target(target: &LocalApprovalTarget) -> Result<(), ApprovalError> {
    match target {
        LocalApprovalTarget::ShellCommand { command } => validate_command(command),
        LocalApprovalTarget::WebFetch { url } => validate_web_url(url),
        LocalApprovalTarget::Question {
            header,
            question,
            options,
        } => validate_question(header.as_deref(), question, options),
    }
}

fn validate_supported_target(
    agent: AgentKind,
    tool_name: &str,
    target: &LocalApprovalTarget,
) -> Result<(), ApprovalError> {
    if matches!(
        (agent, tool_name, target),
        (
            AgentKind::Codex | AgentKind::ClaudeCode,
            "Bash",
            LocalApprovalTarget::ShellCommand { .. },
        ) | (
            AgentKind::ClaudeCode,
            "WebFetch",
            LocalApprovalTarget::WebFetch { .. },
        ) | (
            AgentKind::ClaudeCode,
            "AskUserQuestion",
            LocalApprovalTarget::Question { .. },
        )
    ) {
        return Ok(());
    }
    Err(ApprovalError::UnsupportedTarget)
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

fn validate_question(
    header: Option<&str>,
    question: &str,
    options: &[LocalApprovalOption],
) -> Result<(), ApprovalError> {
    if question.is_empty()
        || question.len() > MAX_LOCAL_APPROVAL_QUESTION_BYTES
        || question.chars().any(is_unsafe_question_character)
    {
        return Err(ApprovalError::InvalidQuestion);
    }
    if let Some(header) = header
        && (header.is_empty()
            || header.len() > MAX_LOCAL_APPROVAL_HEADER_BYTES
            || header.chars().any(is_unsafe_display_character))
    {
        return Err(ApprovalError::InvalidQuestion);
    }
    if options.is_empty() || options.len() > MAX_LOCAL_APPROVAL_OPTIONS {
        return Err(ApprovalError::InvalidQuestion);
    }
    for option in options {
        if option.label.is_empty()
            || option.label.len() > MAX_LOCAL_APPROVAL_OPTION_LABEL_BYTES
            || option.label.chars().any(is_unsafe_display_character)
        {
            return Err(ApprovalError::InvalidQuestion);
        }
        if let Some(description) = option.description.as_deref()
            && (description.len() > MAX_LOCAL_APPROVAL_OPTION_DESCRIPTION_BYTES
                || description.chars().any(is_unsafe_question_character))
        {
            return Err(ApprovalError::InvalidQuestion);
        }
    }
    Ok(())
}

fn is_unsafe_question_character(character: char) -> bool {
    character != '\n' && character != '\t' && is_unsafe_display_character(character)
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
    #[error("the approval question is invalid or too large")]
    InvalidQuestion,
    #[error("the approval agent, tool, and target combination is not supported")]
    UnsupportedTarget,
    #[error("local approval protocol version {0} is not supported")]
    UnsupportedVersion(u16),
    #[error(transparent)]
    Protocol(#[from] crate::protocol::ProtocolError),
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalError, LOCAL_APPROVAL_PROTOCOL_VERSION, LocalApprovalResponse, LocalApprovalTarget,
        MAX_LOCAL_APPROVAL_COMMAND_BYTES, MAX_LOCAL_APPROVAL_OPTIONS,
        MAX_LOCAL_APPROVAL_QUESTION_BYTES, MAX_LOCAL_APPROVAL_URL_BYTES,
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

    #[test]
    fn rejects_noncanonical_agent_tool_and_target_combinations() {
        let request_id = uuid::Uuid::new_v4();
        for value in [
            serde_json::json!({
                "version": LOCAL_APPROVAL_PROTOCOL_VERSION,
                "requestId": request_id,
                "agent": "codex",
                "toolName": "WebFetch",
                "target": { "kind": "web_fetch", "url": "https://example.com/" },
            }),
            serde_json::json!({
                "version": LOCAL_APPROVAL_PROTOCOL_VERSION,
                "requestId": request_id,
                "agent": "claude-code",
                "toolName": "Bash",
                "target": { "kind": "web_fetch", "url": "https://example.com/" },
            }),
            serde_json::json!({
                "version": LOCAL_APPROVAL_PROTOCOL_VERSION,
                "requestId": request_id,
                "agent": "claude-code",
                "toolName": "WebFetch",
                "target": { "kind": "shell_command", "command": "printf unsafe" },
            }),
        ] {
            let request: super::LocalApprovalRequest =
                serde_json::from_value(value).expect("shape should decode");
            assert!(matches!(
                request.validate(),
                Err(ApprovalError::UnsupportedTarget)
            ));
        }
    }

    #[test]
    fn extracts_a_single_select_question_without_private_context() {
        let request = local_approval_request_from_hook(
            AgentKind::ClaudeCode,
            "PreToolUse",
            br#"{"session_id":"private-session","transcript_path":"/private/t.jsonl","cwd":"/private/work","hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"header":"Deploy","question":"Which environment?","multiSelect":false,"options":[{"label":"Staging","description":"safe"},{"label":"Production"}]}]}}"#,
        )
        .expect("payload should parse")
        .expect("question request should be supported");

        assert_eq!(request.version, LOCAL_APPROVAL_PROTOCOL_VERSION);
        assert_eq!(request.tool_name, "AskUserQuestion");
        let LocalApprovalTarget::Question {
            ref header,
            ref question,
            ref options,
        } = request.target
        else {
            panic!("expected a question target");
        };
        assert_eq!(header.as_deref(), Some("Deploy"));
        assert_eq!(question, "Which environment?");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].label, "Staging");
        assert_eq!(options[0].description.as_deref(), Some("safe"));
        assert_eq!(options[1].label, "Production");
        assert!(options[1].description.is_none());
        let encoded = serde_json::to_string(&request).expect("request should serialize");
        assert!(!encoded.contains("private-session"));
        assert!(!encoded.contains("/private/"));
    }

    #[test]
    fn multi_select_and_non_claude_questions_fall_back_to_the_terminal() {
        let multi = local_approval_request_from_hook(
            AgentKind::ClaudeCode,
            "PreToolUse",
            br#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Pick","multiSelect":true,"options":[{"label":"A"},{"label":"B"}]}]}}"#,
        )
        .expect("payload should parse");
        assert!(multi.is_none());

        let codex = local_approval_request_from_hook(
            AgentKind::Codex,
            "PreToolUse",
            br#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Pick","options":[{"label":"A"}]}]}}"#,
        )
        .expect("payload should parse");
        assert!(codex.is_none());

        let empty = local_approval_request_from_hook(
            AgentKind::ClaudeCode,
            "PreToolUse",
            br#"{"hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Pick","options":[]}]}}"#,
        )
        .expect("payload should parse");
        assert!(empty.is_none());

        let other_tool = local_approval_request_from_hook(
            AgentKind::ClaudeCode,
            "PreToolUse",
            br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"}}"#,
        )
        .expect("payload should parse");
        assert!(other_tool.is_none());
    }

    #[test]
    fn rejects_oversized_or_overcounted_questions() {
        let question = "x".repeat(MAX_LOCAL_APPROVAL_QUESTION_BYTES + 1);
        let payload = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "AskUserQuestion",
            "tool_input": { "questions": [{ "question": question, "options": [{ "label": "A" }] }] },
        }))
        .expect("fixture should serialize");
        assert!(matches!(
            local_approval_request_from_hook(AgentKind::ClaudeCode, "PreToolUse", &payload),
            Err(ApprovalError::InvalidQuestion)
        ));

        let options: Vec<_> = (0..=MAX_LOCAL_APPROVAL_OPTIONS)
            .map(|index| serde_json::json!({ "label": format!("option-{index}") }))
            .collect();
        let payload = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "AskUserQuestion",
            "tool_input": { "questions": [{ "question": "Pick", "options": options }] },
        }))
        .expect("fixture should serialize");
        assert!(matches!(
            local_approval_request_from_hook(AgentKind::ClaudeCode, "PreToolUse", &payload),
            Err(ApprovalError::InvalidQuestion)
        ));
    }

    #[test]
    fn answer_response_round_trips_and_reports_its_request_id() {
        let request_id = uuid::Uuid::new_v4();
        let response = LocalApprovalResponse::Answer {
            request_id,
            option_index: 2,
        };
        let encoded = serde_json::to_string(&response).expect("response should serialize");
        let decoded: LocalApprovalResponse =
            serde_json::from_str(&encoded).expect("response should decode");
        assert_eq!(decoded, response);
        assert_eq!(decoded.request_id(), request_id);
    }
}
