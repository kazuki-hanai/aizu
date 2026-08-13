use std::time::Duration;

use chrono::{DateTime, Utc};
use thiserror::Error;
use uuid::Uuid;

use crate::protocol::ProtocolError;
use crate::{
    BridgeFrame, BridgeStreamValidator, DesktopError, DesktopState, FrameDecoder, GapResult,
    IngestResult, PROTOCOL_VERSION, ParsedBridgeFrame, SshFailureCategory, classify_ssh_failure,
};

pub const BRIDGE_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
pub const BRIDGE_STALE_TIMEOUT: Duration = Duration::from_secs(45);
pub const MAX_CAPTURED_STDERR_BYTES: usize = 8 * 1024;

/// Whether the desktop should reconnect automatically or wait for user action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectDisposition {
    Retry,
    UserActionRequired,
}

/// Privacy-safe reason for terminating a remote bridge stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteDiagnostic {
    RemoteError { code: String, message: String },
    UnexpectedEof,
    StartupTimeout,
    StaleTimeout,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteTermination {
    pub disposition: ReconnectDisposition,
    pub diagnostic: RemoteDiagnostic,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteStreamReport {
    pub frames: usize,
    pub ingested: usize,
    pub duplicates: usize,
    pub gaps: usize,
    pub heartbeats: usize,
    pub unknown_frames: usize,
    pub termination: Option<RemoteTermination>,
}

impl RemoteStreamReport {
    fn merge(&mut self, other: Self) {
        self.frames += other.frames;
        self.ingested += other.ingested;
        self.duplicates += other.duplicates;
        self.gaps += other.gaps;
        self.heartbeats += other.heartbeats;
        self.unknown_frames += other.unknown_frames;
        if other.termination.is_some() {
            self.termination = other.termination;
        }
    }
}

/// Bounded bridge stderr capture used only for failure categorization.
///
/// Raw stderr is deliberately not exposed because it may contain a username,
/// path, or host-specific SSH configuration detail.
#[derive(Clone, Debug, Default)]
pub struct BoundedBridgeStderr {
    buffer: Vec<u8>,
    bytes_seen: usize,
}

impl BoundedBridgeStderr {
    pub fn push(&mut self, chunk: &[u8]) {
        self.bytes_seen = self.bytes_seen.saturating_add(chunk.len());
        let remaining = MAX_CAPTURED_STDERR_BYTES.saturating_sub(self.buffer.len());
        self.buffer
            .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }

    #[must_use]
    pub fn bytes_seen(&self) -> usize {
        self.bytes_seen
    }

    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.bytes_seen > self.buffer.len()
    }

    #[must_use]
    pub fn classify(&self) -> SshFailureCategory {
        classify_ssh_failure(&self.buffer)
    }
}

/// Stateful, bounded receiver for one `aizu bridge --follow` SSH child.
///
/// The consumer owns no process or shell behavior. It validates stdout as
/// protocol-only NDJSON and persists each cursor transition before advancing
/// its in-memory validator.
#[derive(Debug)]
pub struct RemoteBridgeConsumer {
    desktop: DesktopState,
    source_key: String,
    decoder: FrameDecoder,
    validator: BridgeStreamValidator,
    started_at: DateTime<Utc>,
    last_frame_at: DateTime<Utc>,
    handshake_complete: bool,
    termination: Option<RemoteTermination>,
}

impl RemoteBridgeConsumer {
    pub fn open(
        desktop: DesktopState,
        source_key: &str,
        local_label: &str,
        started_at: DateTime<Utc>,
    ) -> Result<Self, RemoteConsumerError> {
        desktop.register_source(source_key, local_label)?;
        let source = desktop
            .source(source_key)?
            .ok_or_else(|| DesktopError::SourceNotRegistered(source_key.to_owned()))?;
        let validator =
            BridgeStreamValidator::new(PROTOCOL_VERSION, source.cursor, source.pinned_source_id)?;
        Ok(Self {
            desktop,
            source_key: source_key.to_owned(),
            decoder: FrameDecoder::new(),
            validator,
            started_at,
            last_frame_at: started_at,
            handshake_complete: false,
            termination: None,
        })
    }

    #[must_use]
    pub const fn cursor(&self) -> i64 {
        self.validator.cursor()
    }

    #[must_use]
    pub const fn source_id(&self) -> Option<Uuid> {
        self.validator.source_id()
    }

    #[must_use]
    pub const fn handshake_complete(&self) -> bool {
        self.handshake_complete
    }

    #[must_use]
    pub fn termination(&self) -> Option<&RemoteTermination> {
        self.termination.as_ref()
    }

    /// Consumes one bounded-reader chunk without accumulating a chunk-sized
    /// frame list. Processing one byte at a time keeps decoded frame memory
    /// bounded by the protocol line limit and this method's fixed-size report.
    pub fn push_stdout(
        &mut self,
        chunk: &[u8],
        received_at: DateTime<Utc>,
    ) -> Result<RemoteStreamReport, RemoteConsumerError> {
        let mut report = RemoteStreamReport::default();
        for byte in chunk {
            let frames = self.decoder.push(std::slice::from_ref(byte))?;
            for frame in frames {
                report.merge(self.accept_frame(&frame, received_at)?);
            }
        }
        Ok(report)
    }

    /// Completes stdout handling. EOF is retryable in the always-follow MVP,
    /// unless the remote already emitted a terminal error frame.
    pub fn finish_stdout(&mut self) -> Result<RemoteTermination, RemoteConsumerError> {
        self.decoder.finish()?;
        if let Some(termination) = &self.termination {
            return Ok(termination.clone());
        }
        Ok(RemoteTermination {
            disposition: ReconnectDisposition::Retry,
            diagnostic: RemoteDiagnostic::UnexpectedEof,
        })
    }

    /// Returns a timeout action using an injected wall clock. The process
    /// owner remains responsible for graceful child cancellation.
    #[must_use]
    pub fn timeout_at(&self, now: DateTime<Utc>) -> Option<RemoteTermination> {
        if let Some(termination) = &self.termination {
            return Some(termination.clone());
        }
        let (reference, timeout, diagnostic) = if self.handshake_complete {
            (
                self.last_frame_at,
                BRIDGE_STALE_TIMEOUT,
                RemoteDiagnostic::StaleTimeout,
            )
        } else {
            (
                self.started_at,
                BRIDGE_STARTUP_TIMEOUT,
                RemoteDiagnostic::StartupTimeout,
            )
        };
        let elapsed = now.signed_duration_since(reference).to_std().ok()?;
        (elapsed >= timeout).then_some(RemoteTermination {
            disposition: ReconnectDisposition::Retry,
            diagnostic,
        })
    }

    fn accept_frame(
        &mut self,
        frame: &ParsedBridgeFrame,
        received_at: DateTime<Utc>,
    ) -> Result<RemoteStreamReport, RemoteConsumerError> {
        let mut report = RemoteStreamReport {
            frames: 1,
            ..RemoteStreamReport::default()
        };

        // At-least-once delivery permits an exact event replay. Validate it
        // against durable data without moving the stream cursor backwards.
        if let ParsedBridgeFrame::Known(BridgeFrame::Event { sequence, event }) = frame
            && self.handshake_complete
            && *sequence <= self.validator.cursor()
        {
            let stream_source = self
                .validator
                .source_id()
                .ok_or(ProtocolError::MissingStreamSource)?;
            if event.source.source_id != stream_source {
                return Err(ProtocolError::SourceIdentityMismatch {
                    expected: stream_source,
                    actual: event.source.source_id,
                }
                .into());
            }
            match self
                .desktop
                .ingest_event(&self.source_key, *sequence, event, received_at)?
            {
                IngestResult::Duplicate => report.duplicates = 1,
                IngestResult::Inserted { .. } => {
                    return Err(RemoteConsumerError::UnexpectedReplayInsertion(*sequence));
                }
            }
            self.last_frame_at = received_at;
            return Ok(report);
        }

        let mut candidate = self.validator.clone();
        candidate.accept(frame)?;
        match frame {
            ParsedBridgeFrame::Known(BridgeFrame::Hello {
                source_id,
                latest_sequence,
                ..
            }) => {
                if self.validator.cursor() > *latest_sequence {
                    return Err(RemoteConsumerError::CursorAhead {
                        cursor: self.validator.cursor(),
                        high_watermark: *latest_sequence,
                    });
                }
                self.desktop.pin_source(&self.source_key, *source_id)?;
                self.handshake_complete = true;
            }
            ParsedBridgeFrame::Known(BridgeFrame::Event { sequence, event }) => {
                match self
                    .desktop
                    .ingest_event(&self.source_key, *sequence, event, received_at)?
                {
                    IngestResult::Inserted { .. } => report.ingested = 1,
                    IngestResult::Duplicate => report.duplicates = 1,
                }
            }
            ParsedBridgeFrame::Known(BridgeFrame::Gap {
                requested_after,
                lost_through_sequence,
                ..
            }) => match self.desktop.record_gap(
                &self.source_key,
                *requested_after,
                *lost_through_sequence,
                received_at,
            )? {
                GapResult::Recorded { .. } => report.gaps = 1,
                GapResult::Duplicate => {}
            },
            ParsedBridgeFrame::Known(BridgeFrame::Heartbeat { .. }) => report.heartbeats = 1,
            ParsedBridgeFrame::Known(BridgeFrame::Error { code, message }) => {
                let (code, message) = safe_remote_error(code, message);
                let termination = RemoteTermination {
                    disposition: classify_remote_error(&code),
                    diagnostic: RemoteDiagnostic::RemoteError { code, message },
                };
                report.termination = Some(termination.clone());
                self.termination = Some(termination);
            }
            ParsedBridgeFrame::Unknown { .. } => report.unknown_frames = 1,
        }
        self.validator = candidate;
        self.last_frame_at = received_at;
        Ok(report)
    }
}

fn classify_remote_error(code: &str) -> ReconnectDisposition {
    match code {
        "spool_unavailable" | "internal" => ReconnectDisposition::Retry,
        _ => ReconnectDisposition::UserActionRequired,
    }
}

fn safe_remote_error(code: &str, _untrusted_message: &str) -> (String, String) {
    let message = match code {
        "incompatible_protocol" => "The remote Aizu protocol is incompatible.",
        "spool_unavailable" => "The remote event spool is temporarily unavailable.",
        "incompatible_database" => "The remote Aizu database is incompatible.",
        "unsupported_storage" => "The remote event spool uses unsupported storage.",
        "spool_corrupt" => "The remote event spool failed an integrity check.",
        "cursor_ahead" => "The saved cursor is ahead of the remote event spool.",
        "source_identity_changed" => "The remote source identity changed.",
        "invalid_request" => "The remote bridge rejected the request.",
        "internal" => "The remote bridge stopped because of an internal error.",
        _ => {
            return (
                "remote_error".to_owned(),
                "The remote bridge reported an error.".to_owned(),
            );
        }
    };
    (code.to_owned(), message.to_owned())
}

#[derive(Debug, Error)]
pub enum RemoteConsumerError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Desktop(#[from] DesktopError),
    #[error("desktop cursor {cursor} is ahead of remote high watermark {high_watermark}")]
    CursorAhead { cursor: i64, high_watermark: i64 },
    #[error("replayed sequence {0} unexpectedly inserted new durable data")]
    UnexpectedReplayInsertion(i64),
}

impl RemoteConsumerError {
    #[must_use]
    pub const fn reconnect_disposition(&self) -> ReconnectDisposition {
        match self {
            Self::Desktop(DesktopError::ConcurrentSourceChange) => ReconnectDisposition::Retry,
            Self::Protocol(_)
            | Self::Desktop(_)
            | Self::CursorAhead { .. }
            | Self::UnexpectedReplayInsertion(_) => ReconnectDisposition::UserActionRequired,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration as ChronoDuration, TimeZone};
    use tempfile::TempDir;

    use super::*;
    use crate::{EmitRequest, EventKind, HistoryItem, Outcome};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 12, 12, 0, 0)
            .single()
            .unwrap()
    }

    fn source_id(value: &str) -> Uuid {
        Uuid::parse_str(value).unwrap()
    }

    fn event(source_id: Uuid, kind: EventKind) -> crate::NormalizedEvent {
        EmitRequest {
            kind: Some(kind),
            title: Some("Agent state changed".to_owned()),
            outcome: (kind == EventKind::TaskCompleted).then_some(Outcome::Succeeded),
            agent: Some("codex".to_owned()),
            occurred_at: Some("2026-08-12T12:00:00Z".to_owned()),
            ..EmitRequest::default()
        }
        .normalize(source_id, "untrusted remote label".to_owned(), None)
        .unwrap()
    }

    fn state(temp: &TempDir) -> DesktopState {
        DesktopState::open(temp.path().join("desktop.sqlite3")).unwrap()
    }

    fn lines(frames: &[BridgeFrame]) -> Vec<u8> {
        frames
            .iter()
            .flat_map(|frame| frame.to_line().unwrap())
            .collect()
    }

    #[test]
    fn hello_event_and_heartbeat_are_durably_consumed() {
        let temp = TempDir::new().unwrap();
        let desktop = state(&temp);
        let id = source_id("7a4881c7-c667-47dc-b544-f98a46ab17ca");
        let mut consumer =
            RemoteBridgeConsumer::open(desktop.clone(), "ssh:build", "Build server", now())
                .unwrap();
        let report = consumer
            .push_stdout(
                &lines(&[
                    BridgeFrame::hello(id, Some(1), 1),
                    BridgeFrame::Event {
                        sequence: 1,
                        event: Box::new(event(id, EventKind::AgentQuestion)),
                    },
                    BridgeFrame::Heartbeat { sent_at: now() },
                ]),
                now(),
            )
            .unwrap();

        assert_eq!(report.frames, 3);
        assert_eq!(report.ingested, 1);
        assert_eq!(report.heartbeats, 1);
        assert_eq!(consumer.cursor(), 1);
        assert_eq!(desktop.source("ssh:build").unwrap().unwrap().cursor, 1);
        assert_eq!(desktop.pending_outbox(None).unwrap().len(), 1);
    }

    #[test]
    fn malformed_protocol_is_non_retryable_and_does_not_pin() {
        let temp = TempDir::new().unwrap();
        let desktop = state(&temp);
        let mut consumer =
            RemoteBridgeConsumer::open(desktop.clone(), "ssh:bad", "Bad", now()).unwrap();
        let error = consumer
            .push_stdout(b"{\"type\":\"hello\",\"type\":\"event\"}\n", now())
            .unwrap_err();

        assert_eq!(
            error.reconnect_disposition(),
            ReconnectDisposition::UserActionRequired
        );
        assert_eq!(
            desktop.source("ssh:bad").unwrap().unwrap().pinned_source_id,
            None
        );
    }

    #[test]
    fn protocol_version_and_pinned_identity_mismatches_stop_before_ingest() {
        let temp = TempDir::new().unwrap();
        let desktop = state(&temp);
        let id = source_id("7a4881c7-c667-47dc-b544-f98a46ab17ca");
        let other = source_id("6a4881c7-c667-47dc-b544-f98a46ab17ca");
        desktop.register_source("ssh:build", "Build").unwrap();
        desktop.pin_source("ssh:build", id).unwrap();

        let mut wrong_version =
            RemoteBridgeConsumer::open(desktop.clone(), "ssh:build", "Build", now()).unwrap();
        let version_line = BridgeFrame::Hello {
            protocol_version: 2,
            source_id: id,
            oldest_sequence: None,
            latest_sequence: 0,
        }
        .to_line()
        .unwrap();
        assert!(matches!(
            wrong_version.push_stdout(&version_line, now()).unwrap_err(),
            RemoteConsumerError::Protocol(ProtocolError::ProtocolVersionMismatch { .. })
        ));

        let mut wrong_pin =
            RemoteBridgeConsumer::open(desktop, "ssh:build", "Build", now()).unwrap();
        assert!(matches!(
            wrong_pin
                .push_stdout(
                    &BridgeFrame::hello(other, None, 0).to_line().unwrap(),
                    now()
                )
                .unwrap_err(),
            RemoteConsumerError::Protocol(ProtocolError::SourceIdentityMismatch { .. })
        ));
    }

    #[test]
    fn gap_is_atomic_and_followed_by_first_retained_event() {
        let temp = TempDir::new().unwrap();
        let desktop = state(&temp);
        let id = source_id("7a4881c7-c667-47dc-b544-f98a46ab17ca");
        let mut consumer =
            RemoteBridgeConsumer::open(desktop.clone(), "ssh:gap", "Gap source", now()).unwrap();
        let report = consumer
            .push_stdout(
                &lines(&[
                    BridgeFrame::hello(id, Some(3), 3),
                    BridgeFrame::Gap {
                        requested_after: 0,
                        oldest_sequence: Some(3),
                        lost_through_sequence: 2,
                    },
                    BridgeFrame::Event {
                        sequence: 3,
                        event: Box::new(event(id, EventKind::TaskCompleted)),
                    },
                ]),
                now(),
            )
            .unwrap();

        assert_eq!(report.gaps, 1);
        assert_eq!(report.ingested, 1);
        assert_eq!(consumer.cursor(), 3);
        let history = desktop.recent_history(None).unwrap();
        assert_eq!(history.len(), 2);
        assert!(
            history
                .iter()
                .any(|item| matches!(item, HistoryItem::Gap(_)))
        );
    }

    #[test]
    fn exact_replay_is_deduplicated_after_reconnect() {
        let temp = TempDir::new().unwrap();
        let desktop = state(&temp);
        let id = source_id("7a4881c7-c667-47dc-b544-f98a46ab17ca");
        let event = event(id, EventKind::AgentQuestion);
        let initial = lines(&[
            BridgeFrame::hello(id, Some(1), 1),
            BridgeFrame::Event {
                sequence: 1,
                event: Box::new(event.clone()),
            },
        ]);
        RemoteBridgeConsumer::open(desktop.clone(), "ssh:dedup", "Dedup", now())
            .unwrap()
            .push_stdout(&initial, now())
            .unwrap();

        let mut reconnect =
            RemoteBridgeConsumer::open(desktop.clone(), "ssh:dedup", "Dedup", now()).unwrap();
        let replay = lines(&[
            BridgeFrame::hello(id, Some(1), 1),
            BridgeFrame::Event {
                sequence: 1,
                event: Box::new(event),
            },
        ]);
        let report = reconnect.push_stdout(&replay, now()).unwrap();

        assert_eq!(report.duplicates, 1);
        assert_eq!(desktop.recent_history(None).unwrap().len(), 1);
        assert_eq!(desktop.pending_outbox(None).unwrap().len(), 1);
    }

    #[test]
    fn cursor_ahead_remote_error_timeouts_and_stderr_are_classified() {
        let temp = TempDir::new().unwrap();
        let desktop = state(&temp);
        let id = source_id("7a4881c7-c667-47dc-b544-f98a46ab17ca");
        desktop.register_source("ssh:rewind", "Rewind").unwrap();
        desktop.pin_source("ssh:rewind", id).unwrap();
        desktop.record_gap("ssh:rewind", 0, 2, now()).unwrap();
        let mut rewind =
            RemoteBridgeConsumer::open(desktop, "ssh:rewind", "Rewind", now()).unwrap();
        assert!(matches!(
            rewind
                .push_stdout(&BridgeFrame::hello(id, None, 1).to_line().unwrap(), now())
                .unwrap_err(),
            RemoteConsumerError::CursorAhead { .. }
        ));

        let temp = TempDir::new().unwrap();
        let mut remote_error =
            RemoteBridgeConsumer::open(state(&temp), "ssh:error", "Error", now()).unwrap();
        let report = remote_error
            .push_stdout(
                &BridgeFrame::terminal_error("incompatible_protocol", "update required")
                    .to_line()
                    .unwrap(),
                now(),
            )
            .unwrap();
        assert_eq!(
            report.termination.unwrap().disposition,
            ReconnectDisposition::UserActionRequired
        );

        let temp = TempDir::new().unwrap();
        let mut timeout =
            RemoteBridgeConsumer::open(state(&temp), "ssh:slow", "Slow", now()).unwrap();
        assert_eq!(
            timeout.timeout_at(now() + ChronoDuration::seconds(20)),
            Some(RemoteTermination {
                disposition: ReconnectDisposition::Retry,
                diagnostic: RemoteDiagnostic::StartupTimeout,
            })
        );
        let id = source_id("5a4881c7-c667-47dc-b544-f98a46ab17ca");
        timeout
            .push_stdout(
                &BridgeFrame::hello(id, None, 0).to_line().unwrap(),
                now() + ChronoDuration::seconds(1),
            )
            .unwrap();
        assert_eq!(
            timeout.timeout_at(now() + ChronoDuration::seconds(46)),
            Some(RemoteTermination {
                disposition: ReconnectDisposition::Retry,
                diagnostic: RemoteDiagnostic::StaleTimeout,
            })
        );
        assert_eq!(
            timeout.finish_stdout().unwrap(),
            RemoteTermination {
                disposition: ReconnectDisposition::Retry,
                diagnostic: RemoteDiagnostic::UnexpectedEof,
            }
        );

        let mut stderr = BoundedBridgeStderr::default();
        stderr.push(&vec![b'x'; MAX_CAPTURED_STDERR_BYTES + 100]);
        assert!(stderr.is_truncated());
        assert_eq!(stderr.bytes_seen(), MAX_CAPTURED_STDERR_BYTES + 100);
    }
}
