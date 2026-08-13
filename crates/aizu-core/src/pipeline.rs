use chrono::{DateTime, Duration, Utc};
use thiserror::Error;

use crate::{
    BacklogPlan, DesktopError, DesktopState, NotificationContext, NotificationDecision,
    NotificationPolicy, OutboxOutcome, PreparedNotification, Spool, SpoolError, SuppressionReason,
    aggregate_backlog_count,
};

const INGEST_PAGE_SIZE: usize = 256;
const OUTBOX_PAGE_SIZE: usize = 100;
const MAX_NOTIFICATION_ATTEMPTS: i64 = 5;

/// Platform notification boundary. Implementations schedule a native
/// notification and return only a privacy-safe error category.
pub trait Notifier {
    fn notify(&self, notification: &PreparedNotification) -> Result<(), NotifyError>;
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum NotifyError {
    #[error("notification permission is denied")]
    PermissionDenied,
    #[error("notification scheduling failed temporarily")]
    Retryable,
    #[error("notification scheduling failed permanently")]
    Terminal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PipelineReport {
    pub ingested: usize,
    pub duplicates: usize,
    pub gaps: usize,
    pub delivered: usize,
    pub suppressed: usize,
    pub retryable_failures: usize,
    pub terminal_failures: usize,
    pub deferred: usize,
}

/// Ingests the currently retained source spool into durable desktop state.
///
/// This function never deletes from the source spool. Cursor advancement and
/// outbox creation remain atomic in `DesktopState`, so it is safe to call after
/// a crash or reconnect.
pub fn ingest_spool(
    spool: &Spool,
    desktop: &DesktopState,
    source_key: &str,
    local_label: &str,
    received_at: DateTime<Utc>,
) -> Result<PipelineReport, PipelineError> {
    let snapshot = spool.snapshot()?;
    desktop.register_source(source_key, local_label)?;
    desktop.pin_source(source_key, snapshot.source_id)?;
    let mut source = desktop
        .source(source_key)?
        .ok_or_else(|| DesktopError::SourceNotRegistered(source_key.to_owned()))?;
    if source.cursor > snapshot.latest_sequence {
        return Err(PipelineError::SourceRewind {
            cursor: source.cursor,
            high_watermark: snapshot.latest_sequence,
        });
    }

    let mut report = PipelineReport::default();
    let lost_through = match snapshot.oldest_sequence {
        Some(oldest) if source.cursor < oldest.saturating_sub(1) => Some(oldest - 1),
        None if source.cursor < snapshot.latest_sequence => Some(snapshot.latest_sequence),
        _ => None,
    };
    if let Some(lost_through) = lost_through {
        desktop.record_gap(source_key, source.cursor, lost_through, received_at)?;
        source.cursor = lost_through;
        report.gaps += 1;
    }

    loop {
        let events = spool.events_after(source.cursor, Some(INGEST_PAGE_SIZE))?;
        if events.is_empty() {
            break;
        }
        let count = events.len();
        for item in events {
            match desktop.ingest_event(source_key, item.sequence, &item.event, received_at)? {
                crate::IngestResult::Inserted { .. } => report.ingested += 1,
                crate::IngestResult::Duplicate => report.duplicates += 1,
            }
            source.cursor = item.sequence;
        }
        if count < INGEST_PAGE_SIZE {
            break;
        }
    }
    Ok(report)
}

/// Applies notification policy and drains a bounded durable outbox page.
pub fn dispatch_outbox(
    desktop: &DesktopState,
    notifier: &impl Notifier,
    policy: &NotificationPolicy,
    now: DateTime<Utc>,
    local_minute: Option<u16>,
) -> Result<PipelineReport, PipelineError> {
    let Some(high_watermark) = desktop.pending_outbox_high_watermark_at(now)? else {
        return Ok(PipelineReport::default());
    };
    let mut report = PipelineReport::default();
    let mut offset = 0;
    let mut eligible_count = 0usize;
    let mut individual = Vec::new();
    loop {
        let items =
            desktop.pending_outbox_page_at(Some(OUTBOX_PAGE_SIZE), offset, high_watermark, now)?;
        if items.is_empty() {
            break;
        }
        offset = offset.saturating_add(items.len());
        for item in &items {
            if item.attempt_count >= MAX_NOTIFICATION_ATTEMPTS {
                continue;
            }
            if let NotificationDecision::Notify(notification) = policy.decide(
                &item.event,
                &item.source_label,
                notification_context(item, now, local_minute),
            ) {
                eligible_count = eligible_count.saturating_add(1);
                if individual.len() < 3 {
                    individual.push((item.id, notification));
                }
            }
        }
        if items.len() < OUTBOX_PAGE_SIZE {
            break;
        }
    }

    let notifications = individual
        .iter()
        .map(|(_, notification)| notification.clone())
        .collect();
    let delivery = match aggregate_backlog_count(eligible_count, notifications) {
        BacklogPlan::Empty => None,
        BacklogPlan::Individual(notifications) => {
            let outcomes = individual
                .iter()
                .zip(notifications)
                .map(|((id, _), notification)| (*id, notifier.notify(&notification)))
                .collect();
            Some(BatchDelivery::Individual(outcomes))
        }
        BacklogPlan::Summary(summary) => Some(BatchDelivery::Summary(notifier.notify(&summary))),
    };

    drain_outbox_window(
        desktop,
        policy,
        delivery.as_ref(),
        high_watermark,
        now,
        local_minute,
        &mut report,
    )?;
    Ok(report)
}

enum BatchDelivery {
    Individual(Vec<(i64, Result<(), NotifyError>)>),
    Summary(Result<(), NotifyError>),
}

fn drain_outbox_window(
    desktop: &DesktopState,
    policy: &NotificationPolicy,
    delivery: Option<&BatchDelivery>,
    high_watermark: i64,
    now: DateTime<Utc>,
    local_minute: Option<u16>,
    report: &mut PipelineReport,
) -> Result<(), PipelineError> {
    loop {
        let items =
            desktop.pending_outbox_page_at(Some(OUTBOX_PAGE_SIZE), 0, high_watermark, now)?;
        if items.is_empty() {
            return Ok(());
        }
        for item in items {
            if item.attempt_count >= MAX_NOTIFICATION_ATTEMPTS {
                desktop.finish_outbox(
                    item.id,
                    OutboxOutcome::FailedTerminal("retry_limit_reached".to_owned()),
                    now,
                )?;
                report.terminal_failures += 1;
                continue;
            }
            match policy.decide(
                &item.event,
                &item.source_label,
                notification_context(&item, now, local_minute),
            ) {
                NotificationDecision::Suppress(reason) => {
                    if reason == SuppressionReason::QuietHours {
                        desktop.defer_outbox(item.id, now + Duration::minutes(1))?;
                        report.deferred += 1;
                    } else {
                        desktop.finish_outbox(
                            item.id,
                            OutboxOutcome::Suppressed(reason.as_str().to_owned()),
                            now,
                        )?;
                        report.suppressed += 1;
                    }
                }
                NotificationDecision::Notify(_) => match delivery {
                    Some(BatchDelivery::Summary(outcome)) => {
                        finish_batch_outcome(desktop, item.id, outcome, now, report)?;
                    }
                    Some(BatchDelivery::Individual(outcomes)) => {
                        let outcome = outcomes
                            .iter()
                            .find(|(id, _)| *id == item.id)
                            .map(|(_, outcome)| outcome)
                            .ok_or(PipelineError::IncompleteBatch)?;
                        finish_batch_outcome(desktop, item.id, outcome, now, report)?;
                    }
                    None => return Err(PipelineError::IncompleteBatch),
                },
            }
        }
    }
}

fn notification_context(
    item: &crate::OutboxItem,
    now: DateTime<Utc>,
    local_minute: Option<u16>,
) -> NotificationContext {
    NotificationContext {
        received_at: item.received_at,
        now,
        local_minute,
    }
}

fn finish_batch_outcome(
    desktop: &DesktopState,
    outbox_id: i64,
    outcome: &Result<(), NotifyError>,
    now: DateTime<Utc>,
    report: &mut PipelineReport,
) -> Result<(), PipelineError> {
    match outcome {
        Ok(()) => {
            desktop.finish_outbox(outbox_id, OutboxOutcome::Delivered, now)?;
            report.delivered += 1;
        }
        Err(error) => finish_notify_error(desktop, outbox_id, error, now, report)?,
    }
    Ok(())
}

fn finish_notify_error(
    desktop: &DesktopState,
    outbox_id: i64,
    error: &NotifyError,
    now: DateTime<Utc>,
    report: &mut PipelineReport,
) -> Result<(), PipelineError> {
    match error {
        NotifyError::PermissionDenied => {
            desktop.finish_outbox(
                outbox_id,
                OutboxOutcome::Suppressed("permission_denied".to_owned()),
                now,
            )?;
            report.suppressed += 1;
        }
        NotifyError::Retryable => {
            desktop.finish_outbox(
                outbox_id,
                OutboxOutcome::FailedRetryable("native_api_retryable".to_owned()),
                now,
            )?;
            report.retryable_failures += 1;
        }
        NotifyError::Terminal => {
            desktop.finish_outbox(
                outbox_id,
                OutboxOutcome::FailedTerminal("native_api_terminal".to_owned()),
                now,
            )?;
            report.terminal_failures += 1;
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error(transparent)]
    Spool(#[from] SpoolError),
    #[error(transparent)]
    Desktop(#[from] DesktopError),
    #[error("desktop cursor {cursor} is ahead of source high watermark {high_watermark}")]
    SourceRewind { cursor: i64, high_watermark: i64 },
    #[error("notification batch changed while it was being dispatched")]
    IncompleteBatch,
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use chrono::TimeZone;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        AgentAdapter, ClaudeCodeAdapter, CodexAdapter, EmitRequest, EventKind, Outcome, QuietHours,
        StatePaths,
    };

    struct FakeNotifier {
        notifications: RefCell<Vec<PreparedNotification>>,
        error: Option<NotifyError>,
    }

    impl FakeNotifier {
        fn successful() -> Self {
            Self {
                notifications: RefCell::new(Vec::new()),
                error: None,
            }
        }
    }

    impl Notifier for FakeNotifier {
        fn notify(&self, notification: &PreparedNotification) -> Result<(), NotifyError> {
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            self.notifications.borrow_mut().push(notification.clone());
            Ok(())
        }
    }

    fn timestamp() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 12, 12, 0, 0)
            .single()
            .expect("timestamp")
    }

    fn fixture() -> (TempDir, Spool, DesktopState) {
        let directory = tempfile::tempdir().expect("tempdir");
        let spool = Spool::open_with_display_name(
            StatePaths::new(directory.path().join("source")),
            "untrusted-host".to_owned(),
        )
        .expect("spool");
        let desktop =
            DesktopState::open(directory.path().join("desktop/desktop.sqlite3")).expect("desktop");
        (directory, spool, desktop)
    }

    #[test]
    fn local_pipeline_is_durable_idempotent_and_delivers_safe_content() {
        let (_directory, spool, desktop) = fixture();
        spool
            .emit(
                EmitRequest {
                    title: Some("secret title".to_owned()),
                    body: Some("secret body".to_owned()),
                    outcome: Some(Outcome::Succeeded),
                    ..EmitRequest::default()
                },
                Some(EventKind::TaskCompleted),
            )
            .expect("emit");
        assert_eq!(
            ingest_spool(&spool, &desktop, "local", "My Mac", timestamp())
                .expect("ingest")
                .ingested,
            1
        );
        assert_eq!(
            ingest_spool(&spool, &desktop, "local", "My Mac", timestamp())
                .expect("re-ingest")
                .ingested,
            0
        );

        let notifier = FakeNotifier::successful();
        let report = dispatch_outbox(
            &desktop,
            &notifier,
            &NotificationPolicy::default(),
            timestamp(),
            None,
        )
        .expect("dispatch");
        assert_eq!(report.delivered, 1);
        let notifications = notifier.notifications.borrow();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].body, "Task finished on My Mac");
        assert!(!notifications[0].body.contains("secret"));
        assert!(desktop.pending_outbox(None).expect("outbox").is_empty());
    }

    #[test]
    fn trusted_codex_details_reach_the_notifier_with_the_remote_source_label() {
        let (_directory, spool, desktop) = fixture();
        let request = CodexAdapter
            .parse_hook(
                "Stop",
                br#"{"session_id":"thread-1","cwd":"/home/dev/aizu","hook_event_name":"Stop","last_assistant_message":"Implemented reconnect handling and all checks pass."}"#,
            )
            .expect("valid Codex hook")
            .pop()
            .expect("one event");
        spool.emit(request, None).expect("emit");
        ingest_spool(&spool, &desktop, "ssh:mini-pc", "Mini PC", timestamp()).expect("ingest");

        let notifier = FakeNotifier::successful();
        let policy = NotificationPolicy {
            agent_details_enabled: true,
            ..NotificationPolicy::default()
        };
        dispatch_outbox(&desktop, &notifier, &policy, timestamp(), None).expect("dispatch");

        let notifications = notifier.notifications.borrow();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].title, "Codex task completed");
        assert_eq!(
            notifications[0].body,
            "Implemented reconnect handling and all checks pass.\naizu on Mini PC"
        );
    }

    #[test]
    fn trusted_claude_question_details_reach_the_notifier_with_the_remote_source_label() {
        let (_directory, spool, desktop) = fixture();
        let request = ClaudeCodeAdapter
            .parse_hook(
                "PermissionRequest",
                br#"{"session_id":"session-1","cwd":"/home/dev/aizu","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"mise run check","description":"Run the full Aizu checks?"}}"#,
            )
            .expect("valid Claude Code hook")
            .pop()
            .expect("one event");
        spool.emit(request, None).expect("emit");
        ingest_spool(
            &spool,
            &desktop,
            "ssh:build-host",
            "Build host",
            timestamp(),
        )
        .expect("ingest");

        let notifier = FakeNotifier::successful();
        let policy = NotificationPolicy {
            agent_details_enabled: true,
            ..NotificationPolicy::default()
        };
        dispatch_outbox(&desktop, &notifier, &policy, timestamp(), None).expect("dispatch");

        let notifications = notifier.notifications.borrow();
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].title,
            "Claude Code is waiting for permission"
        );
        assert_eq!(
            notifications[0].body,
            "Run the full Aizu checks?\naizu on Build host"
        );
        assert!(!notifications[0].body.contains("mise run check"));
    }

    #[test]
    fn disabled_or_untrusted_details_never_reach_the_notifier() {
        let (_directory, spool, desktop) = fixture();
        let trusted = CodexAdapter
            .parse_hook(
                "PermissionRequest",
                br#"{"cwd":"/home/dev/aizu","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"description":"Publish the private release?"}}"#,
            )
            .expect("valid Codex hook")
            .pop()
            .expect("one event");
        spool.emit(trusted, None).expect("emit trusted event");
        ingest_spool(
            &spool,
            &desktop,
            "ssh:release-host",
            "Release host",
            timestamp(),
        )
        .expect("ingest trusted event");

        let hidden_notifier = FakeNotifier::successful();
        dispatch_outbox(
            &desktop,
            &hidden_notifier,
            &NotificationPolicy::default(),
            timestamp(),
            None,
        )
        .expect("dispatch hidden detail");
        let hidden = hidden_notifier.notifications.borrow();
        assert_eq!(hidden.len(), 1);
        assert_eq!(
            hidden[0].body,
            "Bash approval is needed in aizu on Release host"
        );
        assert!(!hidden[0].body.contains("private release"));
        drop(hidden);

        let (_directory, spool, desktop) = fixture();
        spool
            .emit(
                EmitRequest {
                    title: Some("Spoofed Codex title".to_owned()),
                    body: Some("Untrusted private question".to_owned()),
                    metadata: Some(serde_json::json!({ "aizu_adapter": "third-party" })),
                    ..EmitRequest::default()
                },
                Some(EventKind::AgentQuestion),
            )
            .expect("emit untrusted event");
        ingest_spool(
            &spool,
            &desktop,
            "ssh:untrusted-host",
            "Untrusted host",
            timestamp(),
        )
        .expect("ingest untrusted event");

        let untrusted_notifier = FakeNotifier::successful();
        let visible_policy = NotificationPolicy {
            agent_details_enabled: true,
            ..NotificationPolicy::default()
        };
        dispatch_outbox(
            &desktop,
            &untrusted_notifier,
            &visible_policy,
            timestamp(),
            None,
        )
        .expect("dispatch untrusted event");
        let untrusted = untrusted_notifier.notifications.borrow();
        assert_eq!(untrusted.len(), 1);
        assert_eq!(untrusted[0].title, "Agent is waiting for input");
        assert_eq!(
            untrusted[0].body,
            "Agent is waiting for input on Untrusted host"
        );
        assert!(!untrusted[0].body.contains("private question"));
    }

    #[test]
    fn permission_denied_suppresses_without_losing_history() {
        let (_directory, spool, desktop) = fixture();
        spool
            .emit(
                EmitRequest {
                    title: Some("Question".to_owned()),
                    ..EmitRequest::default()
                },
                Some(EventKind::AgentQuestion),
            )
            .expect("emit");
        ingest_spool(&spool, &desktop, "local", "Local", timestamp()).expect("ingest");
        let notifier = FakeNotifier {
            notifications: RefCell::new(Vec::new()),
            error: Some(NotifyError::PermissionDenied),
        };
        let report = dispatch_outbox(
            &desktop,
            &notifier,
            &NotificationPolicy::default(),
            timestamp(),
            None,
        )
        .expect("dispatch");
        assert_eq!(report.suppressed, 1);
        assert_eq!(desktop.recent_history(None).expect("history").len(), 1);
    }

    #[test]
    fn a_large_backlog_schedules_one_summary_and_completes_each_outbox_item() {
        let (_directory, spool, desktop) = fixture();
        for _ in 0..1_001 {
            spool
                .emit(
                    EmitRequest {
                        title: Some("Finished".to_owned()),
                        outcome: Some(Outcome::Succeeded),
                        ..EmitRequest::default()
                    },
                    Some(EventKind::TaskCompleted),
                )
                .expect("emit");
        }
        ingest_spool(&spool, &desktop, "local", "Local", timestamp()).expect("ingest");
        let notifier = FakeNotifier::successful();
        let report = dispatch_outbox(
            &desktop,
            &notifier,
            &NotificationPolicy::default(),
            timestamp(),
            None,
        )
        .expect("dispatch");
        assert_eq!(report.delivered, 1_001);
        assert_eq!(notifier.notifications.borrow().len(), 1);
        assert!(
            notifier.notifications.borrow()[0]
                .body
                .starts_with("1001 agent events")
        );
    }

    #[test]
    fn quiet_hour_events_are_deferred_then_summarized() {
        let (_directory, spool, desktop) = fixture();
        for _ in 0..4 {
            spool
                .emit(
                    EmitRequest {
                        title: Some("Finished".to_owned()),
                        outcome: Some(Outcome::Succeeded),
                        ..EmitRequest::default()
                    },
                    Some(EventKind::TaskCompleted),
                )
                .expect("emit");
        }
        ingest_spool(&spool, &desktop, "local", "Local", timestamp()).expect("ingest");
        let notifier = FakeNotifier::successful();
        let policy = NotificationPolicy {
            quiet_hours: Some(QuietHours {
                start_minute: 22 * 60,
                end_minute: 7 * 60,
                questions_bypass: false,
            }),
            ..NotificationPolicy::default()
        };

        let deferred = dispatch_outbox(&desktop, &notifier, &policy, timestamp(), Some(23 * 60))
            .expect("defer in quiet hours");
        assert_eq!(deferred.deferred, 4);
        assert!(notifier.notifications.borrow().is_empty());

        let delivered = dispatch_outbox(
            &desktop,
            &notifier,
            &policy,
            timestamp() + Duration::minutes(1),
            Some(7 * 60),
        )
        .expect("deliver after quiet hours");
        assert_eq!(delivered.delivered, 4);
        assert_eq!(notifier.notifications.borrow().len(), 1);
        assert!(
            notifier.notifications.borrow()[0]
                .body
                .starts_with("4 agent events")
        );
    }
}
