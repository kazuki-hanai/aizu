use std::fmt::Write;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::Value;

use crate::{
    EventKind, NormalizedEvent, Outcome, TERMINAL_ACTIVATION_METADATA_KEY, TerminalActivation,
};

/// Maximum trusted source-label length shown in a notification.
const MAX_SOURCE_LABEL_CHARS: usize = 200;
/// Source clocks farther away than this are not trusted for age suppression.
pub const CLOCK_SKEW_THRESHOLD: Duration = Duration::minutes(5);

/// Desktop-local notification settings. Remote event fields cannot override these values.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct NotificationPolicy {
    pub paused: bool,
    pub completion_enabled: bool,
    pub question_enabled: bool,
    /// Shows a bounded agent-provided message for first-party adapters. Enabled by
    /// default so notifications carry the agent's message; sensitive tokens inside the
    /// message are masked rather than hiding the whole message.
    pub agent_details_enabled: bool,
    pub max_completion_age: Duration,
    pub quiet_hours: Option<QuietHours>,
}

impl Default for NotificationPolicy {
    fn default() -> Self {
        Self {
            paused: false,
            completion_enabled: true,
            question_enabled: true,
            agent_details_enabled: true,
            max_completion_age: Duration::hours(24),
            quiet_hours: None,
        }
    }
}

/// A local-time quiet-hours interval represented as minutes since midnight.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuietHours {
    pub start_minute: u16,
    pub end_minute: u16,
    pub questions_bypass: bool,
}

impl QuietHours {
    #[must_use]
    pub fn contains(self, local_minute: u16) -> bool {
        if self.start_minute >= 1_440
            || self.end_minute >= 1_440
            || local_minute >= 1_440
            || self.start_minute == self.end_minute
        {
            return false;
        }
        if self.start_minute < self.end_minute {
            (self.start_minute..self.end_minute).contains(&local_minute)
        } else {
            local_minute >= self.start_minute || local_minute < self.end_minute
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SuppressionReason {
    Paused,
    CompletionDisabled,
    QuestionDisabled,
    QuietHours,
    CompletionExpired,
}

impl SuppressionReason {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::CompletionDisabled => "completion_disabled",
            Self::QuestionDisabled => "question_disabled",
            Self::QuietHours => "quiet_hours",
            Self::CompletionExpired => "completion_expired",
        }
    }
}

/// Privacy-safe content ready for a platform notification adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedNotification {
    pub identifier: String,
    pub title: String,
    pub body: String,
    pub activation: Option<TerminalActivation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationDecision {
    Notify(PreparedNotification),
    Suppress(SuppressionReason),
}

/// Context measured by the receiving desktop, not supplied by a remote source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotificationContext {
    pub received_at: DateTime<Utc>,
    pub now: DateTime<Utc>,
    /// Current local minute (`0..1440`) as resolved by the platform adapter.
    pub local_minute: Option<u16>,
    /// Only local source events may activate a terminal on this desktop.
    pub allow_terminal_activation: bool,
}

impl NotificationPolicy {
    #[must_use]
    pub fn decide(
        &self,
        event: &NormalizedEvent,
        local_source_label: &str,
        context: NotificationContext,
    ) -> NotificationDecision {
        if self.paused {
            return NotificationDecision::Suppress(SuppressionReason::Paused);
        }
        match event.kind {
            EventKind::TaskCompleted if !self.completion_enabled => {
                return NotificationDecision::Suppress(SuppressionReason::CompletionDisabled);
            }
            EventKind::AgentQuestion if !self.question_enabled => {
                return NotificationDecision::Suppress(SuppressionReason::QuestionDisabled);
            }
            _ => {}
        }
        if let (Some(quiet), Some(local_minute)) = (self.quiet_hours, context.local_minute)
            && quiet.contains(local_minute)
            && !(event.kind == EventKind::AgentQuestion && quiet.questions_bypass)
        {
            return NotificationDecision::Suppress(SuppressionReason::QuietHours);
        }

        let clock_skewed = is_clock_skewed(event.occurred_at, context.received_at);
        if event.kind == EventKind::TaskCompleted {
            let age_basis = if clock_skewed {
                context.received_at
            } else {
                event.occurred_at
            };
            if context.now.signed_duration_since(age_basis) > self.max_completion_age {
                return NotificationDecision::Suppress(SuppressionReason::CompletionExpired);
            }
        }

        let source = safe_source_label(local_source_label);
        let (generic_title, mut body) = match (event.kind, event.outcome) {
            (EventKind::AgentQuestion, _) => (
                "Agent is waiting for input".to_owned(),
                format!("Agent is waiting for input on {source}"),
            ),
            (EventKind::TaskCompleted, Some(Outcome::Failed)) => {
                ("Task failed".to_owned(), format!("Task failed on {source}"))
            }
            (EventKind::TaskCompleted, Some(Outcome::Cancelled)) => (
                "Task cancelled".to_owned(),
                format!("Task cancelled on {source}"),
            ),
            (EventKind::TaskCompleted, _) => (
                "Task finished".to_owned(),
                format!("Task finished on {source}"),
            ),
        };
        let title = trusted_adapter_title(event).unwrap_or(generic_title);
        if let Some(trusted_body) = trusted_adapter_body(event, &source, self.agent_details_enabled)
        {
            body = trusted_body;
        }
        if event.kind == EventKind::AgentQuestion
            && context.now.signed_duration_since(event.occurred_at) > self.max_completion_age
        {
            write!(
                body,
                " (earlier event at {})",
                event.occurred_at.to_rfc3339_opts(SecondsFormat::Secs, true)
            )
            .expect("writing to a String cannot fail");
        }
        NotificationDecision::Notify(PreparedNotification {
            identifier: format!("aizu-event-{}-{}", event.source.source_id, event.id),
            title,
            body,
            activation: context
                .allow_terminal_activation
                .then(|| trusted_terminal_activation(event))
                .flatten(),
        })
    }
}

fn trusted_terminal_activation(event: &NormalizedEvent) -> Option<TerminalActivation> {
    if !trusted_adapter(event) {
        return None;
    }
    let value = event
        .metadata
        .as_ref()?
        .get(TERMINAL_ACTIVATION_METADATA_KEY)?;
    TerminalActivation::from_metadata(value)
}

fn trusted_adapter_title(event: &NormalizedEvent) -> Option<String> {
    if !trusted_adapter(event) {
        return None;
    }
    let title = match (event.source.agent.as_str(), event.kind, event.outcome) {
        ("codex", EventKind::AgentQuestion, _) => "Codex is waiting for permission",
        ("codex", EventKind::TaskCompleted, _) => "Codex task completed",
        ("claude-code", EventKind::AgentQuestion, _) => "Claude Code is waiting for permission",
        ("claude-code", EventKind::TaskCompleted, Some(Outcome::Failed)) => {
            "Claude Code task failed"
        }
        ("claude-code", EventKind::TaskCompleted, _) => "Claude Code task completed",
        _ => return None,
    };
    Some(title.to_owned())
}

fn trusted_adapter_body(
    event: &NormalizedEvent,
    source: &str,
    agent_details_enabled: bool,
) -> Option<String> {
    let metadata = event.metadata.as_ref()?;
    if !trusted_adapter(event) {
        return None;
    }
    let workspace = metadata
        .get("working_directory_name")
        .and_then(Value::as_str)
        .and_then(safe_metadata_label);
    let location = workspace.map_or_else(
        || source.to_owned(),
        |workspace| format!("{workspace} on {source}"),
    );
    if agent_details_enabled
        && let Some(detail) = event
            .body
            .as_deref()
            .and_then(crate::adapter::safe_agent_excerpt)
    {
        return Some(format!("{detail}\n{location}"));
    }
    if event.kind == EventKind::AgentQuestion {
        let tool = metadata
            .get("tool_name")
            .and_then(Value::as_str)
            .and_then(safe_metadata_label);
        return Some(tool.map_or_else(
            || format!("Input is needed in {location}"),
            |tool| format!("{tool} approval is needed in {location}"),
        ));
    }
    Some(match event.outcome {
        Some(Outcome::Failed) => format!("Failed in {location}"),
        Some(Outcome::Cancelled) => format!("Cancelled in {location}"),
        _ => format!("Finished in {location}"),
    })
}

fn trusted_adapter(event: &NormalizedEvent) -> bool {
    let adapter = event
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("aizu_adapter"))
        .and_then(Value::as_str);
    matches!(
        (event.source.agent.as_str(), adapter),
        ("codex", Some("codex-v1")) | ("claude-code", Some("claude-code-v1"))
    )
}

fn safe_metadata_label(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.chars().count() <= 64
        && value
            .chars()
            .all(|character| !character.is_control() && character != '/' && character != '\\'))
    .then_some(value)
}

#[must_use]
pub fn is_clock_skewed(occurred_at: DateTime<Utc>, received_at: DateTime<Utc>) -> bool {
    received_at
        .signed_duration_since(occurred_at)
        .abs()
        .gt(&CLOCK_SKEW_THRESHOLD)
}

/// A notification batch after applying the desktop backlog flood policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BacklogPlan {
    Individual(Vec<PreparedNotification>),
    Summary(PreparedNotification),
    Empty,
}

/// Collapses more than three eligible events into one privacy-safe summary.
#[must_use]
pub fn aggregate_backlog(notifications: Vec<PreparedNotification>) -> BacklogPlan {
    aggregate_backlog_count(notifications.len(), notifications)
}

/// Builds a backlog plan from a complete count while retaining at most three individual payloads.
#[must_use]
pub fn aggregate_backlog_count(
    count: usize,
    notifications: Vec<PreparedNotification>,
) -> BacklogPlan {
    match count {
        0 => BacklogPlan::Empty,
        1..=3 => BacklogPlan::Individual(notifications),
        count => BacklogPlan::Summary(PreparedNotification {
            identifier: "aizu-backlog-summary".to_owned(),
            title: "Aizu backlog".to_owned(),
            body: format!("{count} agent events arrived while disconnected"),
            activation: None,
        }),
    }
}

fn safe_source_label(label: &str) -> String {
    let sanitized: String = label
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_SOURCE_LABEL_CHARS)
        .collect();
    if sanitized.trim().is_empty() {
        "this source".to_owned()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone;
    use uuid::Uuid;

    use super::*;
    use crate::{Source, Urgency, event::SCHEMA_VERSION};

    fn event(
        kind: EventKind,
        outcome: Option<Outcome>,
        occurred_at: DateTime<Utc>,
    ) -> NormalizedEvent {
        NormalizedEvent {
            schema_version: SCHEMA_VERSION,
            id: Uuid::now_v7(),
            kind,
            occurred_at,
            source: Source {
                source_id: Uuid::new_v4(),
                display_name: "spoofed remote".to_owned(),
                agent: "generic".to_owned(),
                session_id: None,
                extra: BTreeMap::new(),
            },
            title: "sensitive remote title".to_owned(),
            body: Some("sensitive remote body".to_owned()),
            outcome,
            urgency: Urgency::High,
            metadata: None,
            extra: BTreeMap::new(),
        }
    }

    fn timestamp(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 12, hour, 0, 0)
            .single()
            .expect("valid test timestamp")
    }

    #[test]
    fn notification_content_uses_only_local_label_and_safe_templates() {
        let event = event(
            EventKind::TaskCompleted,
            Some(Outcome::Failed),
            timestamp(12),
        );
        let decision = NotificationPolicy::default().decide(
            &event,
            "My Mac",
            NotificationContext {
                received_at: timestamp(12),
                now: timestamp(12),
                local_minute: None,
                allow_terminal_activation: false,
            },
        );
        let NotificationDecision::Notify(notification) = decision else {
            panic!("expected notification")
        };
        assert_eq!(notification.title, "Task failed");
        assert_eq!(notification.body, "Task failed on My Mac");
        assert!(!notification.body.contains("spoofed"));
        assert!(!notification.body.contains("sensitive"));
    }

    #[test]
    fn first_party_adapter_notifications_identify_the_agent_and_state() {
        let mut event = event(
            EventKind::TaskCompleted,
            Some(Outcome::Failed),
            timestamp(12),
        );
        event.title = "Claude Code task failed".to_owned();
        event.source.agent = "claude-code".to_owned();
        // No agent message on this event, so the body falls back to the safe location
        // template even though agent details are enabled by default.
        event.body = None;
        event.metadata = Some(serde_json::Map::from_iter([(
            "aizu_adapter".to_owned(),
            serde_json::Value::String("claude-code-v1".to_owned()),
        )]));

        let NotificationDecision::Notify(notification) = NotificationPolicy::default().decide(
            &event,
            "My Mac",
            NotificationContext {
                received_at: timestamp(12),
                now: timestamp(12),
                local_minute: None,
                allow_terminal_activation: false,
            },
        ) else {
            panic!("expected notification")
        };

        assert_eq!(notification.title, "Claude Code task failed");
        assert_eq!(notification.body, "Failed in My Mac");
    }

    #[test]
    fn only_trusted_local_events_expose_a_valid_terminal_activation() {
        let mut event = event(
            EventKind::TaskCompleted,
            Some(Outcome::Succeeded),
            timestamp(12),
        );
        event.source.agent = "codex".to_owned();
        event.metadata = serde_json::json!({
            "aizu_adapter": "codex-v1",
            TERMINAL_ACTIVATION_METADATA_KEY: {
                "application": "iterm2",
                "application_session": "w0t0p0:ABCD",
                "tmux": {"socket_name": "work", "pane_id": "%7"}
            }
        })
        .as_object()
        .cloned();
        let local_context = NotificationContext {
            received_at: timestamp(12),
            now: timestamp(12),
            local_minute: None,
            allow_terminal_activation: true,
        };

        let NotificationDecision::Notify(local) =
            NotificationPolicy::default().decide(&event, "My Mac", local_context)
        else {
            panic!("expected local notification")
        };
        let activation = local.activation.expect("trusted local activation");
        assert_eq!(activation.application, crate::TerminalApplication::Iterm2);
        assert_eq!(activation.tmux.expect("tmux target").pane_id, "%7");

        let remote_context = NotificationContext {
            allow_terminal_activation: false,
            ..local_context
        };
        let NotificationDecision::Notify(remote) =
            NotificationPolicy::default().decide(&event, "Remote host", remote_context)
        else {
            panic!("expected remote notification")
        };
        assert!(remote.activation.is_none());

        event
            .metadata
            .as_mut()
            .expect("metadata")
            .remove("aizu_adapter");
        let NotificationDecision::Notify(untrusted) =
            NotificationPolicy::default().decide(&event, "My Mac", local_context)
        else {
            panic!("expected untrusted notification")
        };
        assert!(untrusted.activation.is_none());
    }

    #[test]
    fn agent_details_show_by_default_and_redact_secrets_for_first_party_adapters() {
        let mut event = event(EventKind::AgentQuestion, None, timestamp(12));
        event.title = "Codex is waiting for permission".to_owned();
        event.source.agent = "codex".to_owned();
        event.body = Some("Run the release checks?".to_owned());
        event.metadata = serde_json::json!({
            "aizu_adapter": "codex-v1",
            "working_directory_name": "aizu",
            "tool_name": "Bash"
        })
        .as_object()
        .cloned();
        let context = NotificationContext {
            received_at: timestamp(12),
            now: timestamp(12),
            local_minute: None,
            allow_terminal_activation: false,
        };

        // Agent details are on by default, so the agent message reaches the notification.
        let NotificationDecision::Notify(visible) =
            NotificationPolicy::default().decide(&event, "My Mac", context)
        else {
            panic!("expected notification")
        };
        assert_eq!(visible.body, "Run the release checks?\naizu on My Mac");

        // Turning the setting off falls back to the safe generic template.
        let NotificationDecision::Notify(hidden) = NotificationPolicy {
            agent_details_enabled: false,
            ..NotificationPolicy::default()
        }
        .decide(&event, "My Mac", context) else {
            panic!("expected notification")
        };
        assert_eq!(hidden.body, "Bash approval is needed in aizu on My Mac");

        // A secret inside the message is masked in place, but the message still shows.
        event.title = "Authorization: Bearer attacker-controlled".to_owned();
        event.body = Some("Read `/Users/alice/.ssh/id_ed25519`?".to_owned());
        let NotificationDecision::Notify(redacted) =
            NotificationPolicy::default().decide(&event, "My Mac", context)
        else {
            panic!("expected notification")
        };
        assert_eq!(redacted.title, "Codex is waiting for permission");
        assert_eq!(redacted.body, "Read [path]?\naizu on My Mac");
        assert!(!redacted.body.contains("id_ed25519"));

        // Untrusted / non-first-party events never expose the raw body.
        event.metadata = None;
        let NotificationDecision::Notify(untrusted) =
            NotificationPolicy::default().decide(&event, "My Mac", context)
        else {
            panic!("expected notification")
        };
        assert_eq!(untrusted.body, "Agent is waiting for input on My Mac");
    }

    #[test]
    fn clock_skew_uses_received_time_for_completion_expiry() {
        let event = event(
            EventKind::TaskCompleted,
            Some(Outcome::Succeeded),
            timestamp(1),
        );
        let policy = NotificationPolicy {
            max_completion_age: Duration::hours(1),
            ..NotificationPolicy::default()
        };
        let decision = policy.decide(
            &event,
            "Local",
            NotificationContext {
                received_at: timestamp(12),
                now: timestamp(12),
                local_minute: None,
                allow_terminal_activation: false,
            },
        );
        assert!(matches!(decision, NotificationDecision::Notify(_)));
    }

    #[test]
    fn quiet_hours_handle_midnight_and_optional_question_bypass() {
        let quiet = QuietHours {
            start_minute: 22 * 60,
            end_minute: 7 * 60,
            questions_bypass: true,
        };
        assert!(quiet.contains(23 * 60));
        assert!(quiet.contains(6 * 60));
        assert!(!quiet.contains(12 * 60));

        let policy = NotificationPolicy {
            quiet_hours: Some(quiet),
            ..NotificationPolicy::default()
        };
        let decision = policy.decide(
            &event(EventKind::AgentQuestion, None, timestamp(12)),
            "Local",
            NotificationContext {
                received_at: timestamp(12),
                now: timestamp(12),
                local_minute: Some(23 * 60),
                allow_terminal_activation: false,
            },
        );
        assert!(matches!(decision, NotificationDecision::Notify(_)));
    }

    #[test]
    fn old_questions_are_delivered_with_their_occurrence_time() {
        let question = event(EventKind::AgentQuestion, None, timestamp(1));
        let decision = NotificationPolicy {
            max_completion_age: Duration::hours(1),
            ..NotificationPolicy::default()
        }
        .decide(
            &question,
            "Remote Mac",
            NotificationContext {
                received_at: timestamp(12),
                now: timestamp(12),
                local_minute: None,
                allow_terminal_activation: false,
            },
        );
        let NotificationDecision::Notify(notification) = decision else {
            panic!("old questions must still be delivered")
        };
        assert!(notification.body.contains("2026-08-12T01:00:00Z"));
    }

    #[test]
    fn backlog_over_three_is_one_summary() {
        let items = (0..10)
            .map(|index| PreparedNotification {
                identifier: format!("id-{index}"),
                title: "Task finished".to_owned(),
                body: "Task finished on Local".to_owned(),
                activation: None,
            })
            .collect();
        let BacklogPlan::Summary(summary) = aggregate_backlog(items) else {
            panic!("expected summary")
        };
        assert_eq!(summary.body, "10 agent events arrived while disconnected");
    }
}
