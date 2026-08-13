use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::NormalizedEvent;
use crate::event::{ValidationError, format_timestamp, parse_utc_timestamp};
use crate::notification::is_clock_skewed;

pub const DESKTOP_DATABASE_SCHEMA_VERSION: i64 = 3;
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_PAGE_SIZE: usize = 100;
const MAX_PAGE_SIZE: usize = 1_000;
const HISTORY_RETENTION_DAYS: i64 = 30;
const MAX_HISTORY_EVENTS: i64 = 10_000;
const MAX_DESKTOP_BYTES: i64 = 128 * 1024 * 1024;
const MAINTENANCE_BATCH: i64 = 512;

/// Durable desktop-side source, cursor, history, deduplication, and outbox state.
#[derive(Clone, Debug)]
pub struct DesktopState {
    path: PathBuf,
}

impl DesktopState {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DesktopError> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(DesktopError::EmptyDatabasePath);
        }
        let parent = path.parent().ok_or(DesktopError::EmptyDatabasePath)?;
        create_private_directory(parent)?;
        if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(DesktopError::UnsafeDatabasePath(path.to_path_buf()));
        }
        create_private_file(path)?;
        let state = Self {
            path: path.to_path_buf(),
        };
        state.initialize()?;
        state.apply_file_permissions()?;
        Ok(state)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Creates a source setting or updates its desktop-local label.
    pub fn register_source(
        &self,
        source_key: &str,
        local_label: &str,
    ) -> Result<SourceRegistration, DesktopError> {
        validate_local_text("source key", source_key, 200)?;
        validate_local_text("source label", local_label, 200)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = source_in_transaction(&transaction, source_key)?;
        let now = format_timestamp(Utc::now());
        let registration = if let Some(mut source) = existing {
            transaction.execute(
                "UPDATE desktop_sources SET local_label = ?1, updated_at = ?2 WHERE source_key = ?3",
                params![local_label, now, source_key],
            )?;
            local_label.clone_into(&mut source.local_label);
            SourceRegistration::Existing(source)
        } else {
            transaction.execute(
                "INSERT INTO desktop_sources (
                    source_key, local_label, pinned_source_id, cursor, created_at, updated_at
                 ) VALUES (?1, ?2, NULL, 0, ?3, ?3)",
                params![source_key, local_label, now],
            )?;
            SourceRegistration::Created(SourceRecord {
                source_key: source_key.to_owned(),
                local_label: local_label.to_owned(),
                pinned_source_id: None,
                cursor: 0,
            })
        };
        transaction.commit()?;
        Ok(registration)
    }

    /// Pins the first successful source identity without resetting an existing cursor.
    pub fn pin_source(&self, source_key: &str, source_id: Uuid) -> Result<PinResult, DesktopError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = source_in_transaction(&transaction, source_key)?
            .ok_or_else(|| DesktopError::SourceNotRegistered(source_key.to_owned()))?;
        if let Some(pinned) = source.pinned_source_id {
            if pinned == source_id {
                return Ok(PinResult::AlreadyPinned);
            }
            return Err(DesktopError::SourceIdentityChanged {
                source_key: source_key.to_owned(),
                expected: pinned,
                actual: source_id,
            });
        }
        reject_duplicate_source_id(&transaction, source_key, source_id)?;
        let resumed_cursor = known_source_high_watermark(&transaction, source_id)?;
        transaction.execute(
            "UPDATE desktop_sources
             SET pinned_source_id = ?1, cursor = ?2, updated_at = ?3
             WHERE source_key = ?4 AND pinned_source_id IS NULL",
            params![
                source_id.to_string(),
                resumed_cursor,
                format_timestamp(Utc::now()),
                source_key
            ],
        )?;
        transaction.commit()?;
        Ok(PinResult::NewlyPinned)
    }

    /// Explicitly accepts a replacement spool identity and resets only that setting's cursor.
    pub fn replace_source(&self, source_key: &str, source_id: Uuid) -> Result<(), DesktopError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        source_in_transaction(&transaction, source_key)?
            .ok_or_else(|| DesktopError::SourceNotRegistered(source_key.to_owned()))?;
        reject_duplicate_source_id(&transaction, source_key, source_id)?;
        let resumed_cursor = known_source_high_watermark(&transaction, source_id)?;
        transaction.execute(
            "UPDATE desktop_sources
             SET pinned_source_id = ?1, cursor = ?2, updated_at = ?3
             WHERE source_key = ?4",
            params![
                source_id.to_string(),
                resumed_cursor,
                format_timestamp(Utc::now()),
                source_key
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn source(&self, source_key: &str) -> Result<Option<SourceRecord>, DesktopError> {
        let connection = self.connection()?;
        source_from_connection(&connection, source_key)
    }

    /// Releases an inactive setting's identity ownership while preserving its history and dedup data.
    ///
    /// A later registration starts its cursor from zero. Existing events and tombstones prevent
    /// replayed frames from producing duplicate notifications.
    pub fn release_source_identity(&self, source_key: &str) -> Result<(), DesktopError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        source_in_transaction(&transaction, source_key)?
            .ok_or_else(|| DesktopError::SourceNotRegistered(source_key.to_owned()))?;
        transaction.execute(
            "UPDATE desktop_sources
             SET pinned_source_id = NULL, cursor = 0, updated_at = ?1
             WHERE source_key = ?2",
            params![format_timestamp(Utc::now()), source_key],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Atomically saves history, advances the cursor, and appends a pending outbox item.
    pub fn ingest_event(
        &self,
        source_key: &str,
        sequence: i64,
        event: &NormalizedEvent,
        received_at: DateTime<Utc>,
    ) -> Result<IngestResult, DesktopError> {
        if sequence <= 0 {
            return Err(DesktopError::InvalidSequence(sequence));
        }
        event.validate()?;
        let payload = event.to_json()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = source_in_transaction(&transaction, source_key)?
            .ok_or_else(|| DesktopError::SourceNotRegistered(source_key.to_owned()))?;
        let pinned = source
            .pinned_source_id
            .ok_or_else(|| DesktopError::SourceNotPinned(source_key.to_owned()))?;
        if pinned != event.source.source_id {
            return Err(DesktopError::SourceIdentityChanged {
                source_key: source_key.to_owned(),
                expected: pinned,
                actual: event.source.source_id,
            });
        }

        let matching = matching_events(&transaction, pinned, sequence, event.id)?;
        if !matching.is_empty() {
            if matching.len() == 1
                && matching[0].sequence == sequence
                && matching[0].event_id == event.id
                && matching[0].payload_digest == payload_digest(&payload)
            {
                return Ok(IngestResult::Duplicate);
            }
            return Err(DesktopError::ConflictingEvent {
                source_id: pinned,
                sequence,
                event_id: event.id,
            });
        }

        let expected = source
            .cursor
            .checked_add(1)
            .ok_or(DesktopError::CursorExhausted)?;
        if sequence != expected {
            return Err(DesktopError::UnexpectedSequence {
                expected,
                actual: sequence,
            });
        }
        let received_at = format_timestamp(received_at);
        transaction.execute(
            "INSERT INTO desktop_events (
                source_key, source_id, source_label, sequence, event_id, payload_json,
                received_at, clock_skewed
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                source_key,
                pinned.to_string(),
                source.local_label,
                sequence,
                event.id.to_string(),
                payload,
                received_at,
                is_clock_skewed(event.occurred_at, parse_utc_timestamp(&received_at)?)
            ],
        )?;
        let event_row_id = transaction.last_insert_rowid();
        let changed = transaction.execute(
            "UPDATE desktop_sources
             SET cursor = ?1, updated_at = ?2
             WHERE source_key = ?3 AND cursor = ?4 AND pinned_source_id = ?5",
            params![
                sequence,
                received_at,
                source_key,
                source.cursor,
                pinned.to_string()
            ],
        )?;
        if changed != 1 {
            return Err(DesktopError::ConcurrentSourceChange);
        }
        transaction.execute(
            "INSERT INTO notification_outbox (
                event_row_id, notification_identifier, state, reason, attempt_count,
                next_attempt_at, updated_at
             ) VALUES (?1, ?2, 'pending', NULL, 0, ?3, ?3)",
            params![
                event_row_id,
                format!("aizu-event-{pinned}-{}", event.id),
                received_at
            ],
        )?;
        let outbox_id = transaction.last_insert_rowid();
        transaction.commit()?;
        Ok(IngestResult::Inserted {
            event_row_id,
            outbox_id,
        })
    }

    /// Atomically records a loss warning and advances the cursor through the lost range.
    pub fn record_gap(
        &self,
        source_key: &str,
        requested_after: i64,
        lost_through_sequence: i64,
        received_at: DateTime<Utc>,
    ) -> Result<GapResult, DesktopError> {
        if requested_after < 0 || lost_through_sequence <= requested_after {
            return Err(DesktopError::InvalidGap {
                requested_after,
                lost_through_sequence,
            });
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let source = source_in_transaction(&transaction, source_key)?
            .ok_or_else(|| DesktopError::SourceNotRegistered(source_key.to_owned()))?;
        let source_id = source
            .pinned_source_id
            .ok_or_else(|| DesktopError::SourceNotPinned(source_key.to_owned()))?;
        let already_recorded: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM desktop_gaps
                WHERE source_id = ?1 AND lost_from_sequence = ?2 AND lost_through_sequence = ?3
             )",
            params![
                source_id.to_string(),
                requested_after + 1,
                lost_through_sequence
            ],
            |row| row.get(0),
        )?;
        if already_recorded && source.cursor >= lost_through_sequence {
            return Ok(GapResult::Duplicate);
        }
        if source.cursor != requested_after {
            return Err(DesktopError::UnexpectedGapCursor {
                expected: source.cursor,
                actual: requested_after,
            });
        }
        let received_at = format_timestamp(received_at);
        transaction.execute(
            "INSERT INTO desktop_gaps (
                source_key, source_id, source_label, lost_from_sequence,
                lost_through_sequence, received_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                source_key,
                source_id.to_string(),
                source.local_label,
                requested_after + 1,
                lost_through_sequence,
                received_at
            ],
        )?;
        let gap_row_id = transaction.last_insert_rowid();
        let changed = transaction.execute(
            "UPDATE desktop_sources
             SET cursor = ?1, updated_at = ?2
             WHERE source_key = ?3 AND cursor = ?4 AND pinned_source_id = ?5",
            params![
                lost_through_sequence,
                received_at,
                source_key,
                requested_after,
                source_id.to_string()
            ],
        )?;
        if changed != 1 {
            return Err(DesktopError::ConcurrentSourceChange);
        }
        transaction.commit()?;
        Ok(GapResult::Recorded { gap_row_id })
    }

    pub fn pending_outbox(&self, limit: Option<usize>) -> Result<Vec<OutboxItem>, DesktopError> {
        self.pending_outbox_at(limit, Utc::now())
    }

    pub fn pending_outbox_at(
        &self,
        limit: Option<usize>,
        now: DateTime<Utc>,
    ) -> Result<Vec<OutboxItem>, DesktopError> {
        let Some(high_watermark) = self.pending_outbox_high_watermark_at(now)? else {
            return Ok(Vec::new());
        };
        self.pending_outbox_page_at(limit, 0, high_watermark, now)
    }

    pub fn pending_outbox_high_watermark_at(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<i64>, DesktopError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT MAX(id) FROM notification_outbox
                 WHERE state IN ('pending', 'failed_retryable') AND next_attempt_at <= ?1",
                params![format_timestamp(now)],
                |row| row.get(0),
            )
            .map_err(DesktopError::from)
    }

    pub fn pending_outbox_page_at(
        &self,
        limit: Option<usize>,
        offset: usize,
        high_watermark: i64,
        now: DateTime<Utc>,
    ) -> Result<Vec<OutboxItem>, DesktopError> {
        let limit = validated_limit(limit)?;
        let offset = i64::try_from(offset).map_err(|_| DesktopError::InvalidPageSize)?;
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT o.id, o.notification_identifier, o.state, o.attempt_count,
                    e.source_label, e.sequence, e.payload_json, e.received_at, e.clock_skewed
             FROM notification_outbox o
             JOIN desktop_events e ON e.id = o.event_row_id
             WHERE o.state IN ('pending', 'failed_retryable')
               AND o.next_attempt_at <= ?1
               AND o.id <= ?2
             ORDER BY CASE
                 WHEN json_extract(e.payload_json, '$.kind') = 'agent.question' THEN 0
                 WHEN json_extract(e.payload_json, '$.outcome') IN ('failed', 'cancelled') THEN 1
                 ELSE 2
             END, e.received_at ASC, o.id ASC
             LIMIT ?3 OFFSET ?4",
        )?;
        let mut rows = statement.query(params![
            format_timestamp(now),
            high_watermark,
            limit,
            offset
        ])?;
        let mut items = Vec::new();
        while let Some(row) = rows.next()? {
            let payload: String = row.get(6)?;
            let event: NormalizedEvent = serde_json::from_str(&payload)?;
            event.validate()?;
            items.push(OutboxItem {
                id: row.get(0)?,
                notification_identifier: row.get(1)?,
                state: parse_outbox_state(&row.get::<_, String>(2)?)?,
                attempt_count: row.get(3)?,
                source_label: row.get(4)?,
                sequence: row.get(5)?,
                event,
                received_at: parse_stored_timestamp(row.get(7)?)?,
                clock_skewed: row.get(8)?,
            });
        }
        Ok(items)
    }

    pub fn finish_outbox(
        &self,
        outbox_id: i64,
        outcome: OutboxOutcome,
        updated_at: DateTime<Utc>,
    ) -> Result<(), DesktopError> {
        let (state, reason, increments_attempt) = match outcome {
            OutboxOutcome::Delivered => (OutboxState::Delivered, None, false),
            OutboxOutcome::Suppressed(reason) => {
                validate_reason(&reason)?;
                (OutboxState::Suppressed, Some(reason), false)
            }
            OutboxOutcome::FailedRetryable(reason) => {
                validate_reason(&reason)?;
                (OutboxState::FailedRetryable, Some(reason), true)
            }
            OutboxOutcome::FailedTerminal(reason) => {
                validate_reason(&reason)?;
                (OutboxState::FailedTerminal, Some(reason), true)
            }
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let attempts: i64 = transaction
            .query_row(
                "SELECT attempt_count FROM notification_outbox
                 WHERE id = ?1 AND state IN ('pending', 'failed_retryable')",
                params![outbox_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(DesktopError::OutboxNotPending(outbox_id))?;
        let next_attempt_at = if state == OutboxState::FailedRetryable {
            updated_at + notification_retry_delay(attempts)
        } else {
            updated_at
        };
        let changed = transaction.execute(
            "UPDATE notification_outbox
             SET state = ?1, reason = ?2,
                 attempt_count = attempt_count + ?3, next_attempt_at = ?4, updated_at = ?5
             WHERE id = ?6 AND state IN ('pending', 'failed_retryable')",
            params![
                state.as_str(),
                reason,
                i64::from(increments_attempt),
                format_timestamp(next_attempt_at),
                format_timestamp(updated_at),
                outbox_id
            ],
        )?;
        if changed == 1 {
            transaction.commit()?;
            Ok(())
        } else {
            Err(DesktopError::OutboxNotPending(outbox_id))
        }
    }

    /// Defers a still-pending outbox item without consuming a retry attempt.
    pub fn defer_outbox(
        &self,
        outbox_id: i64,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), DesktopError> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE notification_outbox
             SET next_attempt_at = ?1, updated_at = ?2
             WHERE id = ?3 AND state IN ('pending', 'failed_retryable')",
            params![
                format_timestamp(next_attempt_at),
                format_timestamp(Utc::now()),
                outbox_id
            ],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(DesktopError::OutboxNotPending(outbox_id))
        }
    }

    /// Requeues events suppressed only because notification permission was denied.
    /// The normal dispatcher then applies backlog aggregation and current policy.
    pub fn requeue_permission_suppressed(
        &self,
        updated_at: DateTime<Utc>,
    ) -> Result<usize, DesktopError> {
        let connection = self.connection()?;
        connection
            .execute(
                "UPDATE notification_outbox
                 SET state = 'pending', reason = NULL, attempt_count = 0,
                     next_attempt_at = ?1, updated_at = ?1
                 WHERE state = 'suppressed' AND reason = 'permission_denied'",
                params![format_timestamp(updated_at)],
            )
            .map_err(DesktopError::from)
    }

    pub fn recent_history(&self, limit: Option<usize>) -> Result<Vec<HistoryItem>, DesktopError> {
        let limit = validated_limit(limit)?;
        let connection = self.connection()?;
        let mut history = event_history(&connection, limit)?;
        history.extend(gap_history(&connection, limit)?);
        history.sort_by(|left, right| {
            right
                .received_at()
                .cmp(&left.received_at())
                .then_with(|| right.row_id().cmp(&left.row_id()))
        });
        history.truncate(usize::try_from(limit).map_err(|_| DesktopError::InvalidPageSize)?);
        Ok(history)
    }

    /// Prunes one bounded batch of terminal history while retaining deduplication tombstones.
    pub fn maintain_history(
        &self,
        now: DateTime<Utc>,
    ) -> Result<HistoryMaintenanceReport, DesktopError> {
        let mut connection = self.connection()?;
        let mut payload_bytes = history_payload_bytes(&connection)?;
        let over_bytes = payload_bytes > MAX_DESKTOP_BYTES;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cutoff = format_timestamp(now - ChronoDuration::days(HISTORY_RETENTION_DAYS));
        let mut statement = transaction.prepare(
            "SELECT e.id, e.source_id, e.sequence, e.event_id, e.payload_json,
                    LENGTH(CAST(e.payload_json AS BLOB)),
                    e.received_at < ?1,
                    e.id NOT IN (
                        SELECT id FROM desktop_events
                        ORDER BY received_at DESC, id DESC LIMIT ?2
                    )
             FROM desktop_events e JOIN notification_outbox o ON o.event_row_id = e.id
             WHERE o.state NOT IN ('pending', 'failed_retryable') AND (
                 e.received_at < ?1 OR
                 e.id NOT IN (SELECT id FROM desktop_events ORDER BY received_at DESC, id DESC LIMIT ?2) OR
                 ?3
             )
             ORDER BY e.received_at ASC, e.id ASC LIMIT ?4",
        )?;
        let candidates = statement
            .query_map(
                params![
                    cutoff,
                    MAX_HISTORY_EVENTS,
                    over_bytes,
                    MAINTENANCE_BATCH + 1
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, bool>(6)?,
                        row.get::<_, bool>(7)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let more_event_candidates =
            candidates.len() > usize::try_from(MAINTENANCE_BATCH).unwrap_or(512);
        let mut selected = Vec::new();
        for candidate in candidates
            .into_iter()
            .take(usize::try_from(MAINTENANCE_BATCH).unwrap_or(512))
        {
            let payload_size = candidate.5;
            if candidate.6 || candidate.7 || payload_bytes > MAX_DESKTOP_BYTES {
                payload_bytes = payload_bytes.saturating_sub(payload_size);
                selected.push(candidate);
            }
        }
        for (row_id, source_id, sequence, event_id, payload, _, _, _) in &selected {
            transaction.execute(
                "INSERT OR IGNORE INTO desktop_event_tombstones
                 (source_id, sequence, event_id, payload_sha256) VALUES (?1, ?2, ?3, ?4)",
                params![source_id, sequence, event_id, payload_digest(payload)],
            )?;
            transaction.execute("DELETE FROM desktop_events WHERE id = ?1", params![row_id])?;
        }
        let gaps = transaction.execute(
            "DELETE FROM desktop_gaps WHERE id IN (
                SELECT id FROM desktop_gaps WHERE received_at < ?1 OR id NOT IN (
                    SELECT id FROM desktop_gaps ORDER BY received_at DESC, id DESC LIMIT ?2
                ) ORDER BY received_at ASC, id ASC LIMIT ?3
             )",
            params![cutoff, MAX_HISTORY_EVENTS, MAINTENANCE_BATCH],
        )?;
        transaction.commit()?;
        connection.execute_batch("PRAGMA incremental_vacuum(256);")?;
        Ok(HistoryMaintenanceReport {
            events_pruned: selected.len(),
            gaps_pruned: gaps,
            remaining_work: more_event_candidates
                || gaps == usize::try_from(MAINTENANCE_BATCH).unwrap_or(512),
        })
    }

    /// Clears terminal history but preserves cursors, pending delivery, and deduplication keys.
    pub fn clear_history(&self) -> Result<HistoryMaintenanceReport, DesktopError> {
        let mut total = HistoryMaintenanceReport::default();
        loop {
            let report = self.maintain_history(Utc::now() + ChronoDuration::days(36_500))?;
            total.events_pruned += report.events_pruned;
            total.gaps_pruned += report.gaps_pruned;
            if !report.remaining_work {
                return Ok(total);
            }
        }
    }

    fn initialize(&self) -> Result<(), DesktopError> {
        let mut connection = Connection::open(&self.path)?;
        configure_connection(&connection)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS desktop_schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
             );",
        )?;
        let version: Option<i64> = transaction
            .query_row(
                "SELECT MAX(version) FROM desktop_schema_migrations",
                [],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if version.unwrap_or(0) > DESKTOP_DATABASE_SCHEMA_VERSION {
            return Err(DesktopError::DatabaseTooNew {
                found: version.unwrap_or_default(),
                supported: DESKTOP_DATABASE_SCHEMA_VERSION,
            });
        }
        if version.unwrap_or(0) < 1 {
            apply_migration_v1(&transaction)?;
        }
        if version.unwrap_or(0) < 2 {
            apply_migration_v2(&transaction)?;
        }
        if version.unwrap_or(0) < 3 {
            apply_migration_v3(&transaction)?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn connection(&self) -> Result<Connection, DesktopError> {
        let connection = Connection::open(&self.path)?;
        configure_connection(&connection)?;
        let version = database_version(&connection)?;
        if version > DESKTOP_DATABASE_SCHEMA_VERSION {
            return Err(DesktopError::DatabaseTooNew {
                found: version,
                supported: DESKTOP_DATABASE_SCHEMA_VERSION,
            });
        }
        Ok(connection)
    }

    fn apply_file_permissions(&self) -> Result<(), DesktopError> {
        set_private_file(&self.path)?;
        for suffix in ["-wal", "-shm"] {
            let mut path = self.path.clone().into_os_string();
            path.push(suffix);
            let path = PathBuf::from(path);
            if path.exists() {
                set_private_file(&path)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRecord {
    pub source_key: String,
    pub local_label: String,
    pub pinned_source_id: Option<Uuid>,
    pub cursor: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceRegistration {
    Created(SourceRecord),
    Existing(SourceRecord),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinResult {
    NewlyPinned,
    AlreadyPinned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngestResult {
    Inserted { event_row_id: i64, outbox_id: i64 },
    Duplicate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GapResult {
    Recorded { gap_row_id: i64 },
    Duplicate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboxState {
    Pending,
    Delivered,
    Suppressed,
    FailedRetryable,
    FailedTerminal,
}

impl OutboxState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Suppressed => "suppressed",
            Self::FailedRetryable => "failed_retryable",
            Self::FailedTerminal => "failed_terminal",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutboxOutcome {
    Delivered,
    Suppressed(String),
    FailedRetryable(String),
    FailedTerminal(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutboxItem {
    pub id: i64,
    pub notification_identifier: String,
    pub state: OutboxState,
    pub attempt_count: i64,
    pub source_label: String,
    pub sequence: i64,
    pub event: NormalizedEvent,
    pub received_at: DateTime<Utc>,
    pub clock_skewed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HistoryItem {
    Event(Box<HistoryEvent>),
    Gap(HistoryGap),
}

impl HistoryItem {
    fn received_at(&self) -> DateTime<Utc> {
        match self {
            Self::Event(item) => item.received_at,
            Self::Gap(item) => item.received_at,
        }
    }

    const fn row_id(&self) -> i64 {
        match self {
            Self::Event(item) => item.row_id,
            Self::Gap(item) => item.row_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEvent {
    pub row_id: i64,
    pub source_key: String,
    pub source_label: String,
    pub sequence: i64,
    pub event: NormalizedEvent,
    pub received_at: DateTime<Utc>,
    pub clock_skewed: bool,
    pub delivery_state: OutboxState,
    pub delivery_reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryGap {
    pub row_id: i64,
    pub source_key: String,
    pub source_label: String,
    pub lost_from_sequence: i64,
    pub lost_through_sequence: i64,
    pub received_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HistoryMaintenanceReport {
    pub events_pruned: usize,
    pub gaps_pruned: usize,
    pub remaining_work: bool,
}

#[derive(Debug, Error)]
pub enum DesktopError {
    #[error("desktop database path must not be empty")]
    EmptyDatabasePath,
    #[error("desktop database path must not be a symbolic link: {0}")]
    UnsafeDatabasePath(PathBuf),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("event validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("desktop database schema {found} is newer than supported schema {supported}")]
    DatabaseTooNew { found: i64, supported: i64 },
    #[error("SQLite WAL mode is unavailable (journal_mode={0})")]
    WalUnavailable(String),
    #[error("{field} must contain 1..={maximum} non-control characters")]
    InvalidLocalText { field: &'static str, maximum: usize },
    #[error("source setting is not registered: {0}")]
    SourceNotRegistered(String),
    #[error("source setting is not pinned: {0}")]
    SourceNotPinned(String),
    #[error("source identity changed for {source_key}: expected {expected}, got {actual}")]
    SourceIdentityChanged {
        source_key: String,
        expected: Uuid,
        actual: Uuid,
    },
    #[error("source identity {source_id} is already registered as {existing_source_key}")]
    DuplicateSourceRegistration {
        source_id: Uuid,
        existing_source_key: String,
    },
    #[error("event sequence must be positive, got {0}")]
    InvalidSequence(i64),
    #[error("expected event sequence {expected}, got {actual}")]
    UnexpectedSequence { expected: i64, actual: i64 },
    #[error("cursor is exhausted")]
    CursorExhausted,
    #[error(
        "event conflicts with durable data for source {source_id}, sequence {sequence}, id {event_id}"
    )]
    ConflictingEvent {
        source_id: Uuid,
        sequence: i64,
        event_id: Uuid,
    },
    #[error("source changed during a desktop transaction")]
    ConcurrentSourceChange,
    #[error("invalid gap {requested_after}..={lost_through_sequence}")]
    InvalidGap {
        requested_after: i64,
        lost_through_sequence: i64,
    },
    #[error("expected gap cursor {expected}, got {actual}")]
    UnexpectedGapCursor { expected: i64, actual: i64 },
    #[error("page size is invalid")]
    InvalidPageSize,
    #[error("stored timestamp is invalid: {0}")]
    InvalidStoredTimestamp(String),
    #[error("stored outbox state is invalid: {0}")]
    InvalidStoredOutboxState(String),
    #[error("outbox item {0} is missing or no longer pending")]
    OutboxNotPending(i64),
    #[error("outbox reason must contain 1..=100 non-control characters")]
    InvalidOutboxReason,
}

#[derive(Debug)]
struct MatchingEvent {
    sequence: i64,
    event_id: Uuid,
    payload_digest: String,
}

fn apply_migration_v1(transaction: &Transaction<'_>) -> Result<(), DesktopError> {
    transaction.execute_batch(
        "CREATE TABLE desktop_sources (
            source_key TEXT PRIMARY KEY,
            local_label TEXT NOT NULL,
            pinned_source_id TEXT UNIQUE,
            cursor INTEGER NOT NULL DEFAULT 0 CHECK (cursor >= 0),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
         );
         CREATE TABLE desktop_events (
            id INTEGER PRIMARY KEY,
            source_key TEXT NOT NULL,
            source_id TEXT NOT NULL,
            source_label TEXT NOT NULL,
            sequence INTEGER NOT NULL CHECK (sequence > 0),
            event_id TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            received_at TEXT NOT NULL,
            clock_skewed INTEGER NOT NULL CHECK (clock_skewed IN (0, 1)),
            UNIQUE (source_id, sequence),
            UNIQUE (source_id, event_id),
            FOREIGN KEY (source_key) REFERENCES desktop_sources(source_key)
         );
         CREATE TABLE desktop_gaps (
            id INTEGER PRIMARY KEY,
            source_key TEXT NOT NULL,
            source_id TEXT NOT NULL,
            source_label TEXT NOT NULL,
            lost_from_sequence INTEGER NOT NULL CHECK (lost_from_sequence > 0),
            lost_through_sequence INTEGER NOT NULL
                CHECK (lost_through_sequence >= lost_from_sequence),
            received_at TEXT NOT NULL,
            UNIQUE (source_id, lost_from_sequence, lost_through_sequence),
            FOREIGN KEY (source_key) REFERENCES desktop_sources(source_key)
         );
         CREATE TABLE notification_outbox (
            id INTEGER PRIMARY KEY,
            event_row_id INTEGER NOT NULL UNIQUE,
            notification_identifier TEXT NOT NULL UNIQUE,
            state TEXT NOT NULL CHECK (
                state IN ('pending', 'delivered', 'suppressed', 'failed_retryable', 'failed_terminal')
            ),
            reason TEXT,
            attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
            next_attempt_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            FOREIGN KEY (event_row_id) REFERENCES desktop_events(id) ON DELETE CASCADE
         );
         CREATE INDEX desktop_events_received_at ON desktop_events(received_at DESC);
         CREATE INDEX desktop_gaps_received_at ON desktop_gaps(received_at DESC);
         CREATE INDEX notification_outbox_state ON notification_outbox(state, id);
         INSERT INTO desktop_schema_migrations (version, applied_at)
         VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));",
    )?;
    Ok(())
}

fn apply_migration_v2(transaction: &Transaction<'_>) -> Result<(), DesktopError> {
    let has_column: bool = transaction.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pragma_table_info('notification_outbox')
            WHERE name = 'next_attempt_at'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_column {
        transaction.execute(
            "ALTER TABLE notification_outbox ADD COLUMN next_attempt_at TEXT",
            [],
        )?;
        transaction.execute(
            "UPDATE notification_outbox SET next_attempt_at = updated_at
             WHERE next_attempt_at IS NULL",
            [],
        )?;
    }
    transaction.execute(
        "INSERT INTO desktop_schema_migrations (version, applied_at)
         VALUES (2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        [],
    )?;
    Ok(())
}

fn apply_migration_v3(transaction: &Transaction<'_>) -> Result<(), DesktopError> {
    transaction.execute_batch(
        "CREATE TABLE desktop_event_tombstones (
            source_id TEXT NOT NULL,
            sequence INTEGER NOT NULL CHECK (sequence > 0),
            event_id TEXT NOT NULL,
            payload_sha256 TEXT NOT NULL,
            UNIQUE (source_id, sequence),
            UNIQUE (source_id, event_id)
         );
         INSERT INTO desktop_schema_migrations (version, applied_at)
         VALUES (3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));",
    )?;
    Ok(())
}

fn notification_retry_delay(attempt_count: i64) -> ChronoDuration {
    let seconds = match attempt_count {
        i64::MIN..=0 => 5,
        1 => 30,
        2 => 120,
        3 => 600,
        _ => 1_800,
    };
    ChronoDuration::seconds(seconds)
}

fn configure_connection(connection: &Connection) -> Result<(), DesktopError> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = FULL;
         PRAGMA auto_vacuum = INCREMENTAL;",
    )?;
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(DesktopError::WalUnavailable(journal_mode));
    }
    Ok(())
}

fn database_version(connection: &Connection) -> Result<i64, DesktopError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
            WHERE type = 'table' AND name = 'desktop_schema_migrations'
         )",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(0);
    }
    Ok(connection
        .query_row(
            "SELECT MAX(version) FROM desktop_schema_migrations",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?
        .unwrap_or(0))
}

fn source_in_transaction(
    transaction: &Transaction<'_>,
    source_key: &str,
) -> Result<Option<SourceRecord>, DesktopError> {
    source_query(transaction, source_key)
}

fn source_from_connection(
    connection: &Connection,
    source_key: &str,
) -> Result<Option<SourceRecord>, DesktopError> {
    source_query(connection, source_key)
}

fn source_query(
    connection: &Connection,
    source_key: &str,
) -> Result<Option<SourceRecord>, DesktopError> {
    connection
        .query_row(
            "SELECT source_key, local_label, pinned_source_id, cursor
             FROM desktop_sources WHERE source_key = ?1",
            params![source_key],
            |row| {
                let raw: Option<String> = row.get(2)?;
                let pinned_source_id = raw
                    .map(|value| {
                        Uuid::parse_str(&value).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                2,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })
                    })
                    .transpose()?;
                Ok(SourceRecord {
                    source_key: row.get(0)?,
                    local_label: row.get(1)?,
                    pinned_source_id,
                    cursor: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(DesktopError::from)
}

fn reject_duplicate_source_id(
    transaction: &Transaction<'_>,
    source_key: &str,
    source_id: Uuid,
) -> Result<(), DesktopError> {
    let existing: Option<String> = transaction
        .query_row(
            "SELECT source_key FROM desktop_sources
             WHERE pinned_source_id = ?1 AND source_key <> ?2",
            params![source_id.to_string(), source_key],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing_source_key) = existing {
        Err(DesktopError::DuplicateSourceRegistration {
            source_id,
            existing_source_key,
        })
    } else {
        Ok(())
    }
}

fn known_source_high_watermark(
    transaction: &Transaction<'_>,
    source_id: Uuid,
) -> Result<i64, DesktopError> {
    transaction
        .query_row(
            "SELECT MAX(sequence) FROM (
                SELECT sequence FROM desktop_events WHERE source_id = ?1
                UNION ALL
                SELECT sequence FROM desktop_event_tombstones WHERE source_id = ?1
                UNION ALL
                SELECT lost_through_sequence AS sequence FROM desktop_gaps WHERE source_id = ?1
             )",
            params![source_id.to_string()],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map(|value| value.unwrap_or(0))
        .map_err(DesktopError::from)
}

fn matching_events(
    transaction: &Transaction<'_>,
    source_id: Uuid,
    sequence: i64,
    event_id: Uuid,
) -> Result<Vec<MatchingEvent>, DesktopError> {
    let mut statement = transaction.prepare(
        "SELECT sequence, event_id, payload_json
         FROM desktop_events
         WHERE source_id = ?1 AND (sequence = ?2 OR event_id = ?3)",
    )?;
    let mut rows = statement.query(params![
        source_id.to_string(),
        sequence,
        event_id.to_string()
    ])?;
    let mut matching = Vec::new();
    while let Some(row) = rows.next()? {
        let raw: String = row.get(1)?;
        let stored_id = Uuid::parse_str(&raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        let payload: String = row.get(2)?;
        matching.push(MatchingEvent {
            sequence: row.get(0)?,
            event_id: stored_id,
            payload_digest: payload_digest(&payload),
        });
    }
    drop(rows);
    drop(statement);
    let mut tombstones = transaction.prepare(
        "SELECT sequence, event_id, payload_sha256 FROM desktop_event_tombstones
         WHERE source_id = ?1 AND (sequence = ?2 OR event_id = ?3)",
    )?;
    let mut rows = tombstones.query(params![
        source_id.to_string(),
        sequence,
        event_id.to_string()
    ])?;
    while let Some(row) = rows.next()? {
        let raw: String = row.get(1)?;
        let stored_id = Uuid::parse_str(&raw).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        matching.push(MatchingEvent {
            sequence: row.get(0)?,
            event_id: stored_id,
            payload_digest: row.get(2)?,
        });
    }
    Ok(matching)
}

fn payload_digest(payload: &str) -> String {
    format!("{:x}", Sha256::digest(payload.as_bytes()))
}

fn history_payload_bytes(connection: &Connection) -> Result<i64, DesktopError> {
    connection
        .query_row(
            "SELECT COALESCE(SUM(LENGTH(CAST(payload_json AS BLOB))), 0) FROM desktop_events",
            [],
            |row| row.get(0),
        )
        .map_err(DesktopError::from)
}

fn event_history(connection: &Connection, limit: i64) -> Result<Vec<HistoryItem>, DesktopError> {
    let mut statement = connection.prepare(
        "SELECT e.id, e.source_key, e.source_label, e.sequence, e.payload_json,
                e.received_at, e.clock_skewed, o.state, o.reason
         FROM desktop_events e
         JOIN notification_outbox o ON o.event_row_id = e.id
         ORDER BY e.received_at DESC, e.id DESC
         LIMIT ?1",
    )?;
    let mut rows = statement.query(params![limit])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let payload: String = row.get(4)?;
        let event: NormalizedEvent = serde_json::from_str(&payload)?;
        event.validate()?;
        items.push(HistoryItem::Event(Box::new(HistoryEvent {
            row_id: row.get(0)?,
            source_key: row.get(1)?,
            source_label: row.get(2)?,
            sequence: row.get(3)?,
            event,
            received_at: parse_stored_timestamp(row.get(5)?)?,
            clock_skewed: row.get(6)?,
            delivery_state: parse_outbox_state(&row.get::<_, String>(7)?)?,
            delivery_reason: row.get(8)?,
        })));
    }
    Ok(items)
}

fn gap_history(connection: &Connection, limit: i64) -> Result<Vec<HistoryItem>, DesktopError> {
    let mut statement = connection.prepare(
        "SELECT id, source_key, source_label, lost_from_sequence,
                lost_through_sequence, received_at
         FROM desktop_gaps
         ORDER BY received_at DESC, id DESC
         LIMIT ?1",
    )?;
    let mut rows = statement.query(params![limit])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        items.push(HistoryItem::Gap(HistoryGap {
            row_id: row.get(0)?,
            source_key: row.get(1)?,
            source_label: row.get(2)?,
            lost_from_sequence: row.get(3)?,
            lost_through_sequence: row.get(4)?,
            received_at: parse_stored_timestamp(row.get(5)?)?,
        }));
    }
    Ok(items)
}

fn parse_outbox_state(raw: &str) -> Result<OutboxState, DesktopError> {
    match raw {
        "pending" => Ok(OutboxState::Pending),
        "delivered" => Ok(OutboxState::Delivered),
        "suppressed" => Ok(OutboxState::Suppressed),
        "failed_retryable" => Ok(OutboxState::FailedRetryable),
        "failed_terminal" => Ok(OutboxState::FailedTerminal),
        _ => Err(DesktopError::InvalidStoredOutboxState(raw.to_owned())),
    }
}

fn parse_stored_timestamp(raw: String) -> Result<DateTime<Utc>, DesktopError> {
    parse_utc_timestamp(&raw).map_err(|_| DesktopError::InvalidStoredTimestamp(raw))
}

fn validated_limit(limit: Option<usize>) -> Result<i64, DesktopError> {
    let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(DesktopError::InvalidPageSize);
    }
    i64::try_from(limit).map_err(|_| DesktopError::InvalidPageSize)
}

fn validate_local_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), DesktopError> {
    let length = value.chars().count();
    if !(1..=maximum).contains(&length) || value.chars().any(char::is_control) {
        Err(DesktopError::InvalidLocalText { field, maximum })
    } else {
        Ok(())
    }
}

fn validate_reason(reason: &str) -> Result<(), DesktopError> {
    let length = reason.chars().count();
    if !(1..=100).contains(&length) || reason.chars().any(char::is_control) {
        Err(DesktopError::InvalidOutboxReason)
    } else {
        Ok(())
    }
}

fn create_private_directory(path: &Path) -> Result<(), DesktopError> {
    if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(DesktopError::UnsafeDatabasePath(path.to_path_buf()));
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn create_private_file(path: &Path) -> Result<(), DesktopError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?;
    set_private_file(path)
}

fn set_private_file(path: &Path) -> Result<(), DesktopError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{Duration as ChronoDuration, TimeZone};
    use tempfile::TempDir;

    use super::*;
    use crate::{EventKind, Outcome, Source, Urgency, event::SCHEMA_VERSION};

    struct TestState {
        directory: TempDir,
        state: DesktopState,
        source_id: Uuid,
    }

    impl TestState {
        fn new() -> Self {
            let directory = tempfile::tempdir().expect("temporary directory");
            let state = DesktopState::open(directory.path().join("desktop.sqlite3"))
                .expect("open desktop state");
            let source_id = Uuid::new_v4();
            state
                .register_source("local", "My Mac")
                .expect("register source");
            state.pin_source("local", source_id).expect("pin source");
            Self {
                directory,
                state,
                source_id,
            }
        }

        fn event(&self, kind: EventKind, outcome: Option<Outcome>) -> NormalizedEvent {
            NormalizedEvent {
                schema_version: SCHEMA_VERSION,
                id: Uuid::now_v7(),
                kind,
                occurred_at: timestamp(12),
                source: Source {
                    source_id: self.source_id,
                    display_name: "untrusted source label".to_owned(),
                    agent: "generic".to_owned(),
                    session_id: None,
                    extra: BTreeMap::new(),
                },
                title: "Task state changed".to_owned(),
                body: None,
                outcome,
                urgency: Urgency::Normal,
                metadata: None,
                extra: BTreeMap::new(),
            }
        }
    }

    fn timestamp(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 12, hour, 0, 0)
            .single()
            .expect("valid test timestamp")
    }

    #[test]
    fn previous_release_desktop_fixture_migrates_to_current_schema() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("desktop.sqlite3");
        let connection = Connection::open(&path).expect("create fixture database");
        connection
            .execute_batch(include_str!(
                "../../../tests/fixtures/desktop-schema-v1.sql"
            ))
            .expect("load version one fixture");
        drop(connection);

        let state = DesktopState::open(&path).expect("migrate previous release fixture");
        assert_eq!(
            state
                .source("local")
                .expect("read migrated source")
                .expect("fixture source remains")
                .local_label,
            "Fixture Mac"
        );
        drop(state);

        let connection = Connection::open(path).expect("inspect migrated fixture");
        let version: i64 = connection
            .query_row(
                "SELECT MAX(version) FROM desktop_schema_migrations",
                [],
                |row| row.get(0),
            )
            .expect("read schema version");
        let has_tombstones: bool = connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'table' AND name = 'desktop_event_tombstones'
                 )",
                [],
                |row| row.get(0),
            )
            .expect("read migrated table");
        assert_eq!(version, DESKTOP_DATABASE_SCHEMA_VERSION);
        assert!(has_tombstones);
    }

    #[test]
    fn source_identity_is_pinned_and_duplicate_registration_is_rejected() {
        let fixture = TestState::new();
        assert_eq!(
            fixture
                .state
                .pin_source("local", fixture.source_id)
                .expect("idempotent pin"),
            PinResult::AlreadyPinned
        );

        let changed_id = Uuid::new_v4();
        assert!(matches!(
            fixture.state.pin_source("local", changed_id),
            Err(DesktopError::SourceIdentityChanged { actual, .. }) if actual == changed_id
        ));

        fixture
            .state
            .register_source("remote-build", "Build server")
            .expect("register second setting");
        assert!(matches!(
            fixture.state.pin_source("remote-build", fixture.source_id),
            Err(DesktopError::DuplicateSourceRegistration { .. })
        ));
        assert_eq!(
            fixture
                .state
                .source("local")
                .expect("read source")
                .expect("source exists")
                .cursor,
            0
        );
    }

    #[test]
    fn releasing_a_source_allows_the_identity_under_a_new_setting() {
        let fixture = TestState::new();
        let first = fixture.event(EventKind::AgentQuestion, None);
        fixture
            .state
            .ingest_event("local", 1, &first, timestamp(12))
            .expect("ingest before release");
        fixture
            .state
            .release_source_identity("local")
            .expect("release source");
        fixture
            .state
            .register_source("renamed", "Renamed source")
            .expect("register replacement setting");
        assert_eq!(
            fixture
                .state
                .pin_source("renamed", fixture.source_id)
                .expect("pin released identity"),
            PinResult::NewlyPinned
        );
        assert_eq!(
            fixture
                .state
                .source("renamed")
                .expect("read rebound source")
                .expect("rebound source exists")
                .cursor,
            1
        );
        let second = fixture.event(EventKind::AgentQuestion, None);
        fixture
            .state
            .ingest_event("renamed", 2, &second, timestamp(13))
            .expect("next event after rebound");
        let released = fixture
            .state
            .source("local")
            .expect("read released source")
            .expect("source remains for history");
        assert_eq!(released.pinned_source_id, None);
        assert_eq!(released.cursor, 0);
    }

    #[test]
    fn event_ingest_cursor_and_outbox_are_atomic_and_durable() {
        let fixture = TestState::new();
        let event = fixture.event(EventKind::TaskCompleted, Some(Outcome::Succeeded));
        let IngestResult::Inserted {
            event_row_id,
            outbox_id,
        } = fixture
            .state
            .ingest_event("local", 1, &event, timestamp(12))
            .expect("ingest event")
        else {
            panic!("expected inserted event")
        };
        assert!(event_row_id > 0);
        assert!(outbox_id > 0);
        drop(fixture.state);

        let reopened = DesktopState::open(fixture.directory.path().join("desktop.sqlite3"))
            .expect("reopen state");
        assert_eq!(
            reopened
                .source("local")
                .expect("read source")
                .expect("source exists")
                .cursor,
            1
        );
        let pending = reopened.pending_outbox(None).expect("pending outbox");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event.id, event.id);
        assert_eq!(pending[0].source_label, "My Mac");
        assert_eq!(pending[0].state, OutboxState::Pending);
    }

    #[test]
    fn retry_backoff_is_durable_and_time_gated() {
        let fixture = TestState::new();
        let event = fixture.event(EventKind::AgentQuestion, None);
        let IngestResult::Inserted { outbox_id, .. } = fixture
            .state
            .ingest_event("local", 1, &event, timestamp(12))
            .expect("ingest")
        else {
            panic!("insert expected")
        };
        fixture
            .state
            .finish_outbox(
                outbox_id,
                OutboxOutcome::FailedRetryable("temporary".to_owned()),
                timestamp(12),
            )
            .expect("record retry");

        assert!(
            fixture
                .state
                .pending_outbox_at(None, timestamp(12) + ChronoDuration::seconds(4))
                .expect("query")
                .is_empty()
        );
        drop(fixture.state);
        let reopened =
            DesktopState::open(fixture.directory.path().join("desktop.sqlite3")).expect("reopen");
        assert_eq!(
            reopened
                .pending_outbox_at(None, timestamp(12) + ChronoDuration::seconds(5))
                .expect("query")
                .len(),
            1
        );
    }

    #[test]
    fn permission_suppressed_events_are_durably_requeued_for_aggregated_recovery() {
        let fixture = TestState::new();
        let event = fixture.event(EventKind::AgentQuestion, None);
        let IngestResult::Inserted { outbox_id, .. } = fixture
            .state
            .ingest_event("local", 1, &event, timestamp(12))
            .expect("ingest")
        else {
            panic!("insert expected")
        };
        fixture
            .state
            .finish_outbox(
                outbox_id,
                OutboxOutcome::Suppressed("permission_denied".to_owned()),
                timestamp(12),
            )
            .expect("suppress while denied");

        assert!(fixture.state.pending_outbox(None).unwrap().is_empty());
        assert_eq!(
            fixture
                .state
                .requeue_permission_suppressed(timestamp(13))
                .expect("requeue after permission recovery"),
            1
        );
        drop(fixture.state);

        let reopened =
            DesktopState::open(fixture.directory.path().join("desktop.sqlite3")).expect("reopen");
        let pending = reopened.pending_outbox(None).expect("pending recovery");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event.id, event.id);
        assert_eq!(
            reopened
                .requeue_permission_suppressed(timestamp(14))
                .expect("idempotent recovery"),
            0
        );
    }

    #[test]
    fn history_maintenance_preserves_pending_cursor_and_replay_dedup() {
        let fixture = TestState::new();
        let delivered = fixture.event(EventKind::TaskCompleted, Some(Outcome::Succeeded));
        let pending = fixture.event(EventKind::AgentQuestion, None);
        let IngestResult::Inserted { outbox_id, .. } = fixture
            .state
            .ingest_event("local", 1, &delivered, timestamp(12))
            .expect("delivered ingest")
        else {
            panic!("insert expected")
        };
        fixture
            .state
            .finish_outbox(outbox_id, OutboxOutcome::Delivered, timestamp(12))
            .expect("deliver");
        fixture
            .state
            .ingest_event("local", 2, &pending, timestamp(13))
            .expect("pending ingest");

        let report = fixture
            .state
            .maintain_history(timestamp(13) + ChronoDuration::days(31))
            .expect("maintain");

        assert_eq!(report.events_pruned, 1);
        assert_eq!(
            fixture.state.recent_history(None).expect("history").len(),
            1
        );
        assert_eq!(fixture.state.pending_outbox(None).expect("outbox").len(), 1);
        assert_eq!(fixture.state.source("local").unwrap().unwrap().cursor, 2);
        assert_eq!(
            fixture
                .state
                .ingest_event("local", 1, &delivered, timestamp(13))
                .expect("tombstone replay"),
            IngestResult::Duplicate
        );
    }

    #[test]
    fn duplicate_is_idempotent_and_conflicting_keys_preserve_existing_data() {
        let fixture = TestState::new();
        let event = fixture.event(EventKind::AgentQuestion, None);
        fixture
            .state
            .ingest_event("local", 1, &event, timestamp(12))
            .expect("initial ingest");
        assert_eq!(
            fixture
                .state
                .ingest_event("local", 1, &event, timestamp(13))
                .expect("duplicate ingest"),
            IngestResult::Duplicate
        );
        assert_eq!(fixture.state.pending_outbox(None).expect("outbox").len(), 1);

        let conflicting_sequence = fixture.event(EventKind::TaskCompleted, Some(Outcome::Failed));
        assert!(matches!(
            fixture
                .state
                .ingest_event("local", 1, &conflicting_sequence, timestamp(13)),
            Err(DesktopError::ConflictingEvent { .. })
        ));
        let mut conflicting_id = event.clone();
        conflicting_id.title = "Changed payload".to_owned();
        assert!(matches!(
            fixture
                .state
                .ingest_event("local", 2, &conflicting_id, timestamp(13)),
            Err(DesktopError::ConflictingEvent { .. })
        ));
        assert_eq!(
            fixture
                .state
                .source("local")
                .expect("source")
                .expect("registered")
                .cursor,
            1
        );
    }

    #[test]
    fn equal_event_ids_from_distinct_sources_do_not_conflict() {
        let fixture = TestState::new();
        let first = fixture.event(EventKind::AgentQuestion, None);
        fixture
            .state
            .ingest_event("local", 1, &first, timestamp(12))
            .expect("first source event");

        let second_source_id = Uuid::new_v4();
        fixture
            .state
            .register_source("remote", "Remote")
            .expect("register remote");
        fixture
            .state
            .pin_source("remote", second_source_id)
            .expect("pin remote");
        let mut second = first.clone();
        second.source.source_id = second_source_id;
        fixture
            .state
            .ingest_event("remote", 1, &second, timestamp(12))
            .expect("same event id is scoped to source");

        let pending = fixture.state.pending_outbox(None).expect("outbox");
        assert_eq!(pending.len(), 2);
        assert_ne!(
            pending[0].notification_identifier,
            pending[1].notification_identifier
        );
    }

    #[test]
    fn failed_ingest_does_not_advance_cursor_or_create_outbox() {
        let fixture = TestState::new();
        let event = fixture.event(EventKind::TaskCompleted, Some(Outcome::Succeeded));
        assert!(matches!(
            fixture
                .state
                .ingest_event("local", 2, &event, timestamp(12)),
            Err(DesktopError::UnexpectedSequence {
                expected: 1,
                actual: 2
            })
        ));
        assert_eq!(
            fixture
                .state
                .source("local")
                .expect("source")
                .expect("registered")
                .cursor,
            0
        );
        assert!(
            fixture
                .state
                .pending_outbox(None)
                .expect("outbox")
                .is_empty()
        );
        assert!(
            fixture
                .state
                .recent_history(None)
                .expect("history")
                .is_empty()
        );
    }

    #[test]
    fn gap_warning_and_cursor_advance_are_atomic_and_idempotent() {
        let fixture = TestState::new();
        let first = fixture.event(EventKind::AgentQuestion, None);
        fixture
            .state
            .ingest_event("local", 1, &first, timestamp(12))
            .expect("first event");
        assert!(matches!(
            fixture
                .state
                .record_gap("local", 1, 5, timestamp(13))
                .expect("record gap"),
            GapResult::Recorded { .. }
        ));
        assert_eq!(
            fixture
                .state
                .record_gap("local", 1, 5, timestamp(13))
                .expect("repeat gap"),
            GapResult::Duplicate
        );
        assert_eq!(
            fixture
                .state
                .source("local")
                .expect("source")
                .expect("registered")
                .cursor,
            5
        );
        let next = fixture.event(EventKind::TaskCompleted, Some(Outcome::Cancelled));
        fixture
            .state
            .ingest_event("local", 6, &next, timestamp(14))
            .expect("event after gap");
        let history = fixture.state.recent_history(None).expect("history");
        assert_eq!(
            history
                .iter()
                .filter(|item| matches!(item, HistoryItem::Gap(_)))
                .count(),
            1
        );
        assert!(history.iter().any(|item| matches!(
            item,
            HistoryItem::Gap(HistoryGap {
                lost_from_sequence: 2,
                lost_through_sequence: 5,
                ..
            })
        )));
    }

    #[test]
    fn outbox_state_changes_are_visible_in_history_and_not_requeued() {
        let fixture = TestState::new();
        let event = fixture.event(EventKind::TaskCompleted, Some(Outcome::Failed));
        let IngestResult::Inserted { outbox_id, .. } = fixture
            .state
            .ingest_event("local", 1, &event, timestamp(12))
            .expect("ingest")
        else {
            panic!("expected inserted")
        };
        fixture
            .state
            .finish_outbox(
                outbox_id,
                OutboxOutcome::Suppressed("quiet_hours".to_owned()),
                timestamp(13),
            )
            .expect("suppress");
        assert!(
            fixture
                .state
                .pending_outbox(None)
                .expect("outbox")
                .is_empty()
        );
        let history = fixture.state.recent_history(None).expect("history");
        let HistoryItem::Event(item) = &history[0] else {
            panic!("expected event history")
        };
        assert_eq!(item.delivery_state, OutboxState::Suppressed);
        assert_eq!(item.delivery_reason.as_deref(), Some("quiet_hours"));
    }

    #[test]
    fn clock_skew_is_stored_from_desktop_received_time() {
        let fixture = TestState::new();
        let mut event = fixture.event(EventKind::AgentQuestion, None);
        event.occurred_at = timestamp(1);
        fixture
            .state
            .ingest_event("local", 1, &event, timestamp(12))
            .expect("ingest skewed event");
        let pending = fixture.state.pending_outbox(None).expect("outbox");
        assert!(pending[0].clock_skewed);
        assert_eq!(
            pending[0]
                .received_at
                .signed_duration_since(event.occurred_at),
            ChronoDuration::hours(11)
        );
    }

    #[test]
    fn explicit_source_replacement_resets_cursor_without_deleting_history() {
        let fixture = TestState::new();
        let old_event = fixture.event(EventKind::AgentQuestion, None);
        fixture
            .state
            .ingest_event("local", 1, &old_event, timestamp(12))
            .expect("old event");
        let replacement = Uuid::new_v4();
        fixture
            .state
            .replace_source("local", replacement)
            .expect("replace source");
        assert_eq!(
            fixture
                .state
                .source("local")
                .expect("source")
                .expect("registered"),
            SourceRecord {
                source_key: "local".to_owned(),
                local_label: "My Mac".to_owned(),
                pinned_source_id: Some(replacement),
                cursor: 0,
            }
        );
        assert_eq!(
            fixture.state.recent_history(None).expect("history").len(),
            1
        );
    }
}
