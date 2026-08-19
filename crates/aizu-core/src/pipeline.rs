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
        AgentAdapter, ClaudeCodeAdapter, CodexAdapter, EmitRequest, EventKind, HistoryItem,
        Outcome, QuietHours, StatePaths,
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
        ingest_spool(
            &spool,
            &desktop,
            "ssh:remote-host",
            "Remote host",
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
        assert_eq!(notifications[0].title, "Codex task completed");
        assert_eq!(
            notifications[0].body,
            "Implemented reconnect handling and all checks pass.\naizu on Remote host"
        );
    }

    #[test]
    fn trusted_agent_secrets_are_redacted_before_spool_history_and_notification() {
        let (_directory, spool, desktop) = fixture();
        let payload = concat!(
            r#"{"session_id":"thread-1","cwd":"/home/dev/aizu","hook_event_name":"Stop","last_assistant_message":"Deployment finished.\nAuthorization: Bearer ghp_"#,
            "exampletoken000000000000000000",
            r#"\n-----BEGIN PRIVATE"#,
            r#" KEY-----\nQWxhZGRpbjpvcGVuIHNlc2FtZTEyMzQ1Njc4OUFCQ0RFRg==\n-----END PRIVATE KEY-----\nSee /Users/alice/private.log"}"#
        );
        let request = CodexAdapter
            .parse_hook("Stop", payload.as_bytes())
            .expect("valid Codex hook")
            .pop()
            .expect("one event");
        let body = request.body.clone().expect("redacted agent message");
        assert!(body.contains("Deployment finished."));
        assert!(body.contains("Authorization: Bearer [redacted]"));
        assert!(body.contains("[redacted private key]"));
        assert!(body.contains("See [path]"));
        for leaked in ["ghp_", "QWxhZGR", "PRIVATE KEY-----", "/Users/alice"] {
            assert!(!body.contains(leaked), "adapter leaked {leaked}");
        }

        spool.emit(request, None).expect("emit redacted event");
        let stored = spool.events_after(0, Some(10)).expect("read source spool");
        let stored_body = stored[0]
            .event
            .body
            .as_deref()
            .expect("stored redacted message");
        assert_eq!(stored_body, body);

        ingest_spool(
            &spool,
            &desktop,
            "ssh:remote-host",
            "Remote host",
            timestamp(),
        )
        .expect("ingest");
        let history = desktop.recent_history(Some(10)).expect("history");
        let HistoryItem::Event(history_event) = &history[0] else {
            panic!("expected event history");
        };
        assert_eq!(history_event.event.body.as_deref(), Some(body.as_str()));

        let notifier = FakeNotifier::successful();
        dispatch_outbox(
            &desktop,
            &notifier,
            &NotificationPolicy::default(),
            timestamp(),
            None,
        )
        .expect("dispatch");
        let notifications = notifier.notifications.borrow();
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].body.contains("Deployment finished."));
        assert!(notifications[0].body.contains("[redacted]"));
        assert!(notifications[0].body.contains("[redacted private key]"));
        assert!(notifications[0].body.contains("[path]"));
        for leaked in ["ghp_", "QWxhZGR", "PRIVATE KEY-----", "/Users/alice"] {
            assert!(
                !notifications[0].body.contains(leaked),
                "notification leaked {leaked}"
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn adversarial_secret_corpus_is_redacted_at_every_pipeline_stage() {
        for (message, expected, leaked) in [
            (
                "Authorization : Bearer abc123",
                "Authorization : Bearer [redacted]",
                "abc123",
            ),
            (
                "Authorization = Bearer abc123",
                "Authorization = Bearer [redacted]",
                "abc123",
            ),
            ("password\n:hunter2", "password\n:[redacted]", "hunter2"),
            (
                "password=\"correct horse battery staple\" after deploy",
                "password=[redacted] after deploy",
                "correct horse battery staple",
            ),
            (
                "password: 'correct horse battery staple'; deploy finished",
                "password: [redacted]; deploy finished",
                "correct horse battery staple",
            ),
            (
                "password=\"correct horse\nbattery staple\"\nfinished",
                "password=[redacted]\nfinished",
                "battery staple",
            ),
            (
                "password=\"correct horse\nbattery staple",
                "password=[redacted]",
                "battery staple",
            ),
            (
                "password=hunter2 deployment finished",
                "password=[redacted] deployment finished",
                "hunter2",
            ),
            (
                "pass\u{200B}word=hunter2 after deploy",
                "password=[redacted] after deploy",
                "hunter2",
            ),
            (
                "database.password=hunter2 after deploy",
                "database.password=[redacted] after deploy",
                "hunter2",
            ),
            (
                "credentials[password]=hunter2 after deploy",
                "credentials[password]=[redacted] after deploy",
                "hunter2",
            ),
            (
                "config={\"user\":{\"password\":\"hunter2\"}} after deploy",
                "config={\"user\":{\"password\":[redacted]}} after deploy",
                "hunter2",
            ),
            (
                "Authorization\n:\nBearer\nhunter2",
                "Authorization\n:\nBearer\n[redacted]",
                "hunter2",
            ),
            ("password :hunter2", "password :[redacted]", "hunter2"),
            ("password := hunter2", "password := [redacted]", "hunter2"),
            ("password :: hunter2", "password :: [redacted]", "hunter2"),
            ("password:: secret", "password:: [redacted]", "secret"),
            (
                "Authorization := hunter2",
                "Authorization := [redacted]",
                "hunter2",
            ),
            (
                "Authorization == hunter2",
                "Authorization == [redacted]",
                "hunter2",
            ),
            (
                "Authorization: Bea\u{200B}rer abc123 after deploy",
                "Authorization: Bearer [redacted] after deploy",
                "abc123",
            ),
            ("Bearer hunter2", "Bearer [redacted]", "hunter2"),
            ("Bearer abcdef", "Bearer [redacted]", "abcdef"),
            (
                "Bearer: secret surrounding message",
                "Bearer: [redacted] surrounding message",
                "secret",
            ),
            (
                "Basic: secret surrounding message",
                "Basic: [redacted] surrounding message",
                "secret",
            ),
            (
                "**Bearer** hunter2 surrounding message",
                "**Bearer** [redacted] surrounding message",
                "hunter2",
            ),
            (
                "(Bearer) hunter2 surrounding message",
                "(Bearer) [redacted] surrounding message",
                "hunter2",
            ),
            (
                "Bearer : , hunter2 surrounding message",
                "Bearer : , [redacted] surrounding message",
                "hunter2",
            ),
            (
                "Blob: secret surrounding message",
                "Blob: [redacted] surrounding message",
                "secret",
            ),
            (
                "Encoded: payload surrounding message",
                "Encoded: [redacted] surrounding message",
                "payload",
            ),
            (
                "Bearer = secret surrounding message",
                "Bearer = [redacted] surrounding message",
                "secret",
            ),
            (
                "Bearer\n= secret surrounding message",
                "Bearer\n= [redacted] surrounding message",
                "secret",
            ),
            (
                "Base64 = QUFB surrounding message",
                "Base64 = [redacted] surrounding message",
                "QUFB",
            ),
            ("Bearer\nhunter2", "Bearer\n[redacted]", "hunter2"),
            (
                "Bearer abcdefghijklmnopqrstuvwxyz123456",
                "Bearer [redacted]",
                "abcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Bearer aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "Bearer [redacted]",
                "aaaaaaaaaaaaaaaa",
            ),
            (
                "Slack xapp-1-A1234567890-1234567890-abcdef",
                "Slack [redacted]",
                "xapp-1-",
            ),
            (
                "Deploy uses ghp_\u{200B}1234567890abcdefghijklmnopqrstuvwxyz after deploy",
                "Deploy uses [redacted] after deploy",
                "ghp_",
            ),
            (
                "Generated key AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_ABCD",
                "Generated key [redacted]",
                "AbCdEfGh",
            ),
            (
                concat!("Wrapped **ghp_", "1234567890abcdefghijklmnopqrstuvwxyz**"),
                "Wrapped [redacted]",
                "ghp_",
            ),
            (
                "Token aB1cD2eF3gH4iJ5kL6mN7pQ8rS9tU0vW1xY2zA3b",
                "Token [redacted]",
                "aB1cD2",
            ),
            (
                "Token\naB1cD2eF3gH4iJ5kL6mN7pQ8rS9tU0vW1xY2zA3b",
                "Token\n[redacted]",
                "aB1cD2",
            ),
            (
                "Token aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "Token [redacted]",
                "aaaaaaaaaaaaaaaa",
            ),
            ("Token secret", "Token [redacted]", "secret"),
            ("Base64 QUFB", "Base64 [redacted]", "QUFB"),
            (
                "The password is \"correct horse battery staple\"; deploy finished",
                "The password is [redacted]; deploy finished",
                "correct horse battery staple",
            ),
            (
                "Password value `correct horse battery staple`; deploy finished",
                "Password value [redacted]; deploy finished",
                "correct horse battery staple",
            ),
            (
                "password is letmein after deploy",
                "password is [redacted] after deploy",
                "letmein",
            ),
            (
                "API key: abc123 after deploy",
                "API key: [redacted] after deploy",
                "abc123",
            ),
            (
                "The password is (hunter2) after deploy",
                "The password is [redacted] after deploy",
                "hunter2",
            ),
            (
                "API key: [abc123] after deploy",
                "API key: [redacted] after deploy",
                "abc123",
            ),
            (
                "password value is equals hunter2 after deploy",
                "password value is [redacted]",
                "hunter2",
            ),
            (
                "API key value is equals abc123 after deploy",
                "API key value is [redacted]",
                "abc123",
            ),
            (
                "Secret key is hunter2 after deploy",
                "Secret key is [redacted] after deploy",
                "hunter2",
            ),
            (
                "Blob value surrounding message",
                "Blob value [redacted] message",
                "surrounding",
            ),
            ("Blob value", "Blob [redacted]", "Blob value"),
            ("Token value", "Token [redacted]", "Token value"),
            ("Encoded value", "Encoded [redacted]", "Encoded value"),
            (
                "Bearer Bearer secret after",
                "Bearer [redacted] secret after",
                "Bearer Bearer",
            ),
            (
                "Bearer Basic secret after",
                "Bearer [redacted] secret after",
                "Bearer Basic",
            ),
            (
                "Bearer := secret after",
                "Bearer := [redacted] after",
                "secret",
            ),
            (
                "Bearer == secret after",
                "Bearer == [redacted] after",
                "secret",
            ),
            (
                "Bearer\n:=\nsecret after",
                "Bearer\n:=\n[redacted] after",
                "secret",
            ),
            ("Bearer::secret tail", "Bearer::[redacted] tail", "secret"),
            ("Token:=secret tail", "Token:=[redacted] tail", "secret"),
            (
                "Base64:::===QUFB tail",
                "Base64:::===[redacted] tail",
                "QUFB",
            ),
            (
                "Secret value 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                "Secret value [redacted]",
                "0123456789abcdef",
            ),
            (
                "Blob YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYQ==",
                "Blob [redacted]",
                "YWFhYWFh",
            ),
            (
                "Blob QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==",
                "Blob [redacted]",
                "QUFBQUFB",
            ),
            (
                "Blob QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFB",
                "Blob [redacted]",
                "QUFBQUFB",
            ),
            (
                "Blob AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_ABCD",
                "Blob [redacted]",
                "AbCdEfGh",
            ),
            (
                "Base64 QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFB",
                "Base64 [redacted]",
                "QUFBQUFB",
            ),
            (
                "Encoded AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_ABCD",
                "Encoded [redacted]",
                "AbCdEfGh",
            ),
            (
                "Bearer abcdefghijklmnopqrstuvwxyzabcdef",
                "Bearer [redacted]",
                "abcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Token abcdefghijklmnopqrstuvwxyzabcdef",
                "Token [redacted]",
                "abcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Blob abcdefghijklmnopqrstuvwxyzabcdef",
                "Blob [redacted]",
                "abcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Base64 ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMN",
                "Base64 [redacted]",
                "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            ),
            (
                "Encoded abcdefghijklmnopqrstuvwxyzabcdef",
                "Encoded [redacted]",
                "abcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Bearer releaseabcdefghijklmnopqrstuvwxyz",
                "Bearer [redacted]",
                "releaseabcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Token buildabcdefghijklmnopqrstuvwxyz",
                "Token [redacted]",
                "buildabcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Base64 versionABCDEFGHIJKLMNOPQRSTUVWXYZ",
                "Base64 [redacted]",
                "versionABCDEFGHIJKLMNOPQRSTUVWXYZ",
            ),
            (
                "Encoded abcdefghijklmnop123identifier",
                "Encoded [redacted]",
                "abcdefghijklmnop123identifier",
            ),
            (
                "Bearer abcdefghijklmnopqrstuvwxyzabcdef expires tomorrow",
                "Bearer [redacted] expires tomorrow",
                "abcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Token abcdefghijklmnopqrstuvwxyzabcdef is active",
                "Token [redacted] is active",
                "abcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Blob ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMN decoded successfully",
                "Blob [redacted] decoded successfully",
                "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            ),
            (
                "Base64 ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEFGHIJKLMN decodes correctly",
                "Base64 [redacted] decodes correctly",
                "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            ),
            (
                "Encoded abcdefghijklmnopqrstuvwxyzabcdef was received",
                "Encoded [redacted] was received",
                "abcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Bearer\nabcdefghijklmnopqrstuvwxyzabcdef expires tomorrow",
                "Bearer\n[redacted] expires tomorrow",
                "abcdefghijklmnopqrstuvwxyz",
            ),
            (
                "Token value abcdefghijklmnopqrstuvwxyzabcdef is active",
                "Token value [redacted] is active",
                "abcdefghijklmnopqrstuvwxyzabcdef",
            ),
            (
                "Token is abcdefghijklmnopqrstuvwxyzabcdef",
                "Token is [redacted]",
                "abcdefghijklmnopqrstuvwxyzabcdef",
            ),
            (
                "Token value is abcdefghijklmnopqrstuvwxyzabcdef",
                "Token value is [redacted]",
                "abcdefghijklmnopqrstuvwxyzabcdef",
            ),
            (
                "Token value equals abcdefghijklmnopqrstuvwxyzabcdef",
                "Token value equals [redacted]",
                "abcdefghijklmnopqrstuvwxyzabcdef",
            ),
            (
                "Token value is equals abcdefghijklmnopqrstuvwxyzabcdef after deploy",
                "Token value is [redacted]",
                "abcdefghijklmnopqrstuvwxyzabcdef",
            ),
            (
                "Authorization: Bearer value abcdefghijklmnopqrstuvwxyzabcdef",
                "Authorization: Bearer value [redacted]",
                "abcdefghijklmnopqrstuvwxyzabcdef",
            ),
            (
                "token: Bearer abc123 deployment finished",
                "token: Bearer [redacted] deployment finished",
                "abc123",
            ),
            (
                "Bearer correcthorsebatterystaple expires tomorrow",
                "Bearer [redacted] expires tomorrow",
                "correcthorsebatterystaple",
            ),
            (
                "Token mountainriverfalconsecret is active",
                "Token [redacted] is active",
                "mountainriverfalconsecret",
            ),
            (
                "Blob summerwinterautumnspring decoded successfully",
                "Blob [redacted] decoded successfully",
                "summerwinterautumnspring",
            ),
            (
                "Token releasefoobarbazquxquux2026 is active",
                "Token [redacted] is active",
                "releasefoobarbazquxquux2026",
            ),
            (
                "Encoded secretabc123identifier was received",
                "Encoded [redacted] was received",
                "secretabc123identifier",
            ),
            (
                "Bearer\ncorrecthorsebatterystaple expires tomorrow",
                "Bearer\n[redacted] expires tomorrow",
                "correcthorsebatterystaple",
            ),
            (
                "-----BEGIN PRIVATE\nKEY-----\nshortSecretBody\n-----END PRIVATE KEY-----\nsafe ending",
                "[redacted private key]",
                "shortSecretBody",
            ),
            (
                concat!(
                    "-----BEGIN PRIVATE",
                    " KEY-----\nfirstSecret\n-----END CERTIFICATE-----\nsecondSecret\n-----END PRIVATE KEY-----\nsafe ending"
                ),
                "[redacted private key]",
                "secondSecret",
            ),
            (
                "-----BEGIN\nPRIVATE KEY-----\nsecretBody\n-----END PRIVATE KEY-----",
                "[redacted private key]",
                "secretBody",
            ),
            (
                "-----BE\nGIN PRIVATE KEY-----\nsecretBody\n-----END PRIVATE KEY-----",
                "[redacted private key]",
                "secretBody",
            ),
            (
                "-----BEGIN EC\nPRIVATE KEY-----\nsecretBody\n-----END EC PRIVATE KEY-----",
                "[redacted private key]",
                "secretBody",
            ),
            (
                "-----BEGIN DSA\nPRIVATE KEY-----\nsecretBody\n-----END DSA PRIVATE KEY-----",
                "[redacted private key]",
                "secretBody",
            ),
            (
                "-----BEGIN PGP\nPRIVATE KEY BLOCK-----\nsecretBody\n-----END PGP PRIVATE KEY BLOCK-----",
                "[redacted private key]",
                "secretBody",
            ),
            (
                "-----BEGIN PRI\u{200B}VATE KEY-----\nsecretBody\n-----END PRIVATE KEY-----",
                "[redacted private key]",
                "secretBody",
            ),
            (
                "-----BEGIN PRI\u{FEFF}VATE KEY-----\nsecretBody\n-----END PRIVATE KEY-----",
                "[redacted private key]",
                "secretBody",
            ),
            (
                "-----BEGIN CERTIFICATE-----\npassword=hunter2\n-----END CERTIFICATE-----",
                "password=[redacted]",
                "hunter2",
            ),
            (
                "-----BEGIN CERTIFICATE----- password=hunter2 -----END CERTIFICATE-----",
                "password=[redacted]",
                "hunter2",
            ),
            (
                "-----BEGIN CERTIFICATE-----\nAKIAIOSFODNN7EXAMPLE\n-----END CERTIFICATE-----",
                "[redacted]",
                "AKIAIOS",
            ),
            (
                concat!(
                    "-----BEGIN CERTIFICATE----- -----BEGIN PRIVATE",
                    " KEY-----\nsecretBody\n-----END PRIVATE KEY-----"
                ),
                "[redacted private key]",
                "secretBody",
            ),
            (
                concat!(
                    "-----BEGIN CERTIFICATE-----\n-----BEGIN PRIVATE",
                    " KEY-----\nQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFB\n-----END PRIVATE KEY-----\n-----END CERTIFICATE-----"
                ),
                "[redacted private key]",
                "QUFBQUFB",
            ),
            (
                "Open file:///Users/alice/.ssh/id_ed25519",
                "Open file:[path]",
                "/Users/alice",
            ),
            (
                "Open \"/Users/alice/Secret Project/.env\" after deploy",
                "Open [path] after deploy",
                "Secret Project",
            ),
            (
                r#"Open "C:\Users\Alice\Secret Project\.env" after deploy"#,
                "Open [path] after deploy",
                "Secret Project",
            ),
            (
                "Open \"/Users/alice/Secret\nProject/.env\" after deploy",
                "Open [path] after deploy",
                "Project/.env",
            ),
            (
                r"Open /Users/alice/Secret\ Project/.env after deploy",
                "Open [path] after deploy",
                "Project/.env",
            ),
            (
                "Open /Users/alice/Secret Project; after deploy",
                "Open [path]; after deploy",
                "Secret Project",
            ),
            (
                "Open /Users/alice/Secret Project after deploy",
                "Open [path]",
                "Secret Project",
            ),
            (
                "Open `/Users/alice/Secret Project` after deploy",
                "Open [path] after deploy",
                "Secret Project",
            ),
            (
                r"Open \\server\share\Alice\secret.txt",
                "Open [path]",
                "Alice",
            ),
            (
                r"Open \\?\C:\Users\Alice\secret.txt",
                "Open [path]",
                "Alice",
            ),
            (r"path=\\server\share\Alice\secret.txt", "[path]", "Alice"),
            ("Open /root/.ssh/id_rsa", "Open [path]", "/root/"),
            ("Open /etc/aizu/private.conf", "Open [path]", "/etc/"),
            (
                "Open **/Users/alice/.ssh/id_ed25519**",
                "Open [path]",
                "/Users/alice",
            ),
            (
                "Open “/Users/alice/.ssh/id_ed25519”",
                "Open [path]",
                "/Users/alice",
            ),
            ("path=/etc/aizu/private.conf", "[path]", "/etc/"),
            (
                "Open %2FUsers%2Falice%2F.ssh%2Fid_ed25519",
                "Open [path]",
                "%2FUsers",
            ),
            (
                "Open file:%2Froot%2F.ssh%2Fid_rsa",
                "Open file:[path]",
                "%2Froot",
            ),
            (
                "Open file:%252Froot%252F.ssh%252Fid_rsa",
                "Open file:[path]",
                "%252Froot",
            ),
            (
                "Open file:%2525252Froot%2525252F.ssh%2525252Fid_rsa",
                "Open file:[path]",
                "%2525252Froot",
            ),
            (
                "Open file:%25252525252Froot%25252525252F.ssh%25252525252Fid_rsa",
                "Open file:[path]",
                "%25252525252Froot",
            ),
            (
                "Open file:%2Groot%2F.ssh%2Fid_rsa",
                "Open file:[path]",
                "%2Groot",
            ),
            (
                "Open https://user:pa@ss@host.example/path",
                "Open https://[redacted]@host.example/path",
                "pa@ss",
            ),
            (
                "Open //user:pass@host.example/path",
                "Open //[redacted]@host.example/path",
                "user:pass",
            ),
            (
                "Open https:user:pass@host.example/path",
                "Open https:[redacted]@host.example/path",
                "user:pass",
            ),
            (
                "Open https://user%3Ahunter2@host.example/path",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open //user%3Ahunter2@host.example/path",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https:user%3Ahunter2@host.example/path",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://user%253Ahunter2@host.example/path",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                concat!(
                    "Open https://host.example/callback?token=ghp_",
                    "1234567890abcdefghijklmnopqrstuvwxyz"
                ),
                "Open [redacted URI]",
                "ghp_",
            ),
            (
                "Open https://host.example/login?password=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host.example/path#xoxb-1234567890-abcdefgh",
                "Open [redacted URI]",
                "xoxb-",
            ),
            (
                "url=https://host.example/callback?api_key=sk-1234567890abcdef",
                "[redacted URI]",
                "sk-",
            ),
            (
                "Open data:text/plain,password=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host.example/view?path=/Users/alice/.ssh/id_ed25519",
                "Open [redacted URI]",
                "/Users/alice",
            ),
            (
                "Open https://host/path?token=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?token%3Dhunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?authorization=Bearer%20hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host.example/path?token%3Dghp_1234567890abcdefghijklmnopqrstuvwxyz",
                "Open [redacted URI]",
                "ghp_",
            ),
            (
                "Open https://host.example/path?pass%77ord=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host.example/path?token%253Dghp_1234567890abcdefghijklmnopqrstuvwxyz",
                "Open [redacted URI]",
                "ghp_",
            ),
            (
                "Open https://host.example/path?token%252525253Dghp_1234567890abcdefghijklmnopqrstuvwxyz",
                "Open [redacted URI]",
                "ghp_",
            ),
            (
                "Open data:text/plain,password%3Dhunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?password%GGhunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                concat!(
                    "Open https://host/path?token%ghp_",
                    "1234567890abcdefghijklmnopqrstuvwxyz"
                ),
                "Open [redacted URI]",
                "ghp_",
            ),
            (
                "Open https://host/path?password%2=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?foo=password=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?foo=password%3Dhunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?user[password]=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open data:text/plain,foo=password=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?foo=%0Apassword=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?password[]=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?password%5B%5D=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?password[confirmation]=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?user.password=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?user%2Epassword=hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?password==hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?password::hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?password[]==hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?foo=password==hunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
            (
                "Open https://host/path?password%3D%3Dhunter2",
                "Open [redacted URI]",
                "hunter2",
            ),
        ] {
            assert_pipeline_redacts(message, expected, leaked);
        }
    }

    #[test]
    fn oversized_encoded_values_fail_closed_before_persistence() {
        let message = format!(
            "https://host.example/path?token%3Dghp_1234567890abcdefghijklmnopqrstuvwxyz{}",
            "x".repeat(crate::MAX_EVENT_BYTES)
        );
        assert_pipeline_redacts(&message, "[redacted URI]", "ghp_");

        let message = format!(
            "%2FUsers%2Falice%2FSecret{}",
            "x".repeat(crate::MAX_EVENT_BYTES)
        );
        assert_pipeline_redacts(&message, "[path]", "%2FUsers");
    }

    fn assert_pipeline_redacts(message: &str, expected: &str, leaked: &str) {
        let (_directory, spool, desktop) = fixture();
        let payload = serde_json::json!({
            "session_id": "thread-review-corpus",
            "cwd": "/home/dev/aizu",
            "hook_event_name": "Stop",
            "last_assistant_message": message
        });
        let request = CodexAdapter
            .parse_hook("Stop", payload.to_string().as_bytes())
            .expect("valid Codex hook")
            .pop()
            .expect("one event");
        let body = request.body.clone().expect("redacted agent message");
        assert!(body.contains(expected), "adapter output: {body}");
        assert!(!body.contains(leaked), "adapter leaked {leaked}: {body}");

        spool.emit(request, None).expect("emit redacted event");
        let stored = spool.events_after(0, Some(10)).expect("source spool");
        let stored_body = stored[0]
            .event
            .body
            .as_deref()
            .expect("stored redacted message");
        assert!(
            stored_body.contains(expected),
            "spool output: {stored_body}"
        );
        assert!(
            !stored_body.contains(leaked),
            "source spool leaked {leaked}: {stored_body}"
        );

        ingest_spool(
            &spool,
            &desktop,
            "ssh:review-host",
            "Review host",
            timestamp(),
        )
        .expect("ingest");
        let history = desktop.recent_history(Some(10)).expect("history");
        let HistoryItem::Event(history_event) = &history[0] else {
            panic!("expected event history");
        };
        let history_body = history_event
            .event
            .body
            .as_deref()
            .expect("history redacted message");
        assert!(history_body.contains(expected), "history: {history_body}");
        assert!(
            !history_body.contains(leaked),
            "history leaked {leaked}: {history_body}"
        );

        let notifier = FakeNotifier::successful();
        dispatch_outbox(
            &desktop,
            &notifier,
            &NotificationPolicy::default(),
            timestamp(),
            None,
        )
        .expect("dispatch");
        let notifications = notifier.notifications.borrow();
        let notification_body = &notifications[0].body;
        assert!(
            notification_body.contains(expected),
            "notification: {notification_body}"
        );
        assert!(
            !notification_body.contains(leaked),
            "notification leaked {leaked}: {notification_body}"
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
            &NotificationPolicy {
                agent_details_enabled: false,
                ..NotificationPolicy::default()
            },
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
