use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rusqlite::{
    Connection, MAIN_DB, OptionalExtension, Transaction, TransactionBehavior, params, types::Type,
};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::event::{EmitRequest, EventKind, NormalizedEvent, ValidationError, format_timestamp};
use crate::paths::StatePaths;

pub const DATABASE_SCHEMA_VERSION: i64 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_PAGE_SIZE: usize = 256;
const MAINTENANCE_BATCH_SIZE: usize = 1_000;
const MAINTENANCE_INTERVAL: ChronoDuration = ChronoDuration::hours(24);

#[derive(Clone, Debug)]
pub struct Spool {
    paths: StatePaths,
    display_name: String,
}

impl Spool {
    pub fn open(paths: StatePaths) -> Result<Self, SpoolError> {
        let display_name = default_display_name();
        Self::open_with_display_name(paths, display_name)
    }

    pub fn open_with_display_name(
        paths: StatePaths,
        display_name: impl Into<String>,
    ) -> Result<Self, SpoolError> {
        create_private_directory(paths.root())?;
        create_private_database_file(&paths.spool_db())?;
        let spool = Self {
            paths,
            display_name: display_name.into(),
        };
        spool.initialize()?;
        Ok(spool)
    }

    #[must_use]
    pub fn paths(&self) -> &StatePaths {
        &self.paths
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn emit(
        &self,
        request: EmitRequest,
        default_kind: Option<EventKind>,
    ) -> Result<SpoolEvent, SpoolError> {
        let now = Utc::now();
        if self.maintenance_due(now)? {
            self.maintain(now, RetentionPolicy::default())?;
        }
        self.ensure_maintenance_inactive()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.ensure_maintenance_inactive()?;
        let source_id = source_id_in_transaction(&transaction)?;

        let event = request.normalize(source_id, self.display_name.clone(), default_kind)?;
        let sequence = next_sequence(&transaction)?;
        let payload = event.to_json()?;
        let inserted_at = format_timestamp(Utc::now());
        transaction.execute(
            "INSERT INTO events (
                sequence, event_id, schema_version, kind, occurred_at, payload_json, inserted_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                sequence,
                event.id.to_string(),
                i64::from(event.schema_version),
                event.kind.as_str(),
                format_timestamp(event.occurred_at),
                payload,
                inserted_at,
            ],
        )?;
        transaction.commit()?;
        self.apply_file_permissions()?;

        Ok(SpoolEvent { sequence, event })
    }

    pub fn snapshot(&self) -> Result<SpoolSnapshot, SpoolError> {
        let connection = self.connection()?;
        let (source_id, latest_sequence): (String, i64) = connection.query_row(
            "SELECT source_id, high_watermark FROM source_identity WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let oldest_sequence =
            connection.query_row("SELECT MIN(sequence) FROM events", [], |row| row.get(0))?;

        Ok(SpoolSnapshot {
            source_id: parse_uuid_column(source_id, 0)?,
            oldest_sequence,
            latest_sequence,
        })
    }

    pub fn events_after(
        &self,
        after: i64,
        limit: Option<usize>,
    ) -> Result<Vec<SpoolEvent>, SpoolError> {
        if after < 0 {
            return Err(SpoolError::InvalidCursor(after));
        }
        let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE);
        if !(1..=10_000).contains(&limit) {
            return Err(SpoolError::InvalidPageSize);
        }
        let limit = i64::try_from(limit).map_err(|_| SpoolError::InvalidPageSize)?;
        let connection = self.connection()?;
        let spool_source_id: String = connection.query_row(
            "SELECT source_id FROM source_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        let spool_source_id = parse_uuid_column(spool_source_id, 0)?;
        let mut statement = connection.prepare(
            "SELECT sequence, event_id, schema_version, kind, occurred_at, payload_json
             FROM events
             WHERE sequence > ?1
             ORDER BY sequence ASC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![after, limit], |row| {
            let sequence: i64 = row.get(0)?;
            let event_id: String = row.get(1)?;
            let schema_version: i64 = row.get(2)?;
            let kind: String = row.get(3)?;
            let occurred_at: String = row.get(4)?;
            let payload: String = row.get(5)?;
            let event = serde_json::from_str::<NormalizedEvent>(&payload).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(error))
            })?;
            Ok((
                SpoolEvent { sequence, event },
                event_id,
                schema_version,
                kind,
                occurred_at,
            ))
        })?;

        let mut events = Vec::new();
        for row in rows {
            let (item, event_id, schema_version, kind, occurred_at) = row?;
            item.event.validate()?;
            let mismatch = if item.event.id.to_string() != event_id {
                Some("event_id")
            } else if i64::from(item.event.schema_version) != schema_version {
                Some("schema_version")
            } else if item.event.kind.as_str() != kind {
                Some("kind")
            } else if format_timestamp(item.event.occurred_at) != occurred_at {
                Some("occurred_at")
            } else if item.event.source.source_id != spool_source_id {
                Some("source_id")
            } else {
                None
            };
            if let Some(field) = mismatch {
                return Err(SpoolError::StoredEventMismatch {
                    sequence: item.sequence,
                    field,
                });
            }
            events.push(item);
        }
        Ok(events)
    }

    pub fn doctor(&self) -> Result<DoctorReport, SpoolError> {
        let connection = self.connection()?;
        let sqlite_version: String =
            connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
        let journal_mode: String =
            connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        let schema_version = database_schema_version(&connection)?;
        let snapshot = self.snapshot()?;
        let event_count: i64 =
            connection.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        let payload_bytes: i64 = connection.query_row(
            "SELECT COALESCE(SUM(LENGTH(CAST(payload_json AS BLOB))), 0) FROM events",
            [],
            |row| row.get(0),
        )?;

        Ok(DoctorReport {
            healthy: true,
            state_dir: self.paths.root().to_path_buf(),
            database_path: self.paths.spool_db(),
            sqlite_version,
            journal_mode,
            schema_version,
            source_id: snapshot.source_id,
            event_count,
            payload_bytes,
            oldest_sequence: snapshot.oldest_sequence,
            latest_sequence: snapshot.latest_sequence,
        })
    }

    /// Applies bounded age, count, and payload-byte retention without
    /// resetting the source high-watermark.
    pub fn maintain(
        &self,
        now: DateTime<Utc>,
        policy: RetentionPolicy,
    ) -> Result<MaintenanceReport, SpoolError> {
        policy.validate()?;
        self.ensure_maintenance_inactive()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.ensure_maintenance_inactive()?;
        let report = prune_retention_batch(&transaction, now, policy)?;
        transaction.commit()?;

        if report.deleted_events > 0 {
            connection.execute_batch(
                "PRAGMA wal_checkpoint(PASSIVE);
                 PRAGMA incremental_vacuum(256);",
            )?;
        }
        Ok(report)
    }

    pub fn maintain_default(&self) -> Result<MaintenanceReport, SpoolError> {
        self.maintain(Utc::now(), RetentionPolicy::default())
    }

    pub fn regenerate_identity(
        &self,
        discard_events: bool,
        confirmed: bool,
    ) -> Result<IdentityRegeneration, SpoolError> {
        if discard_events && !confirmed {
            return Err(SpoolError::ConfirmationRequired);
        }
        let _lock = MaintenanceLock::acquire(self.paths.root())?;
        let mut connection = self.connection()?;
        let barrier = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        barrier.commit()?;
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let event_count: i64 =
            connection.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))?;
        if event_count > 0 && !discard_events {
            return Err(SpoolError::IdentityHasEvents(event_count));
        }
        let backup_path = if event_count > 0 {
            Some(self.backup_database(&connection)?)
        } else {
            None
        };
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        if discard_events {
            transaction.execute("DELETE FROM events", [])?;
        }
        let old_source_id = source_id_in_transaction(&transaction)?;
        let new_source_id = Uuid::new_v4();
        transaction.execute(
            "UPDATE source_identity
             SET source_id = ?1, high_watermark = 0, created_at = ?2
             WHERE singleton = 1",
            params![new_source_id.to_string(), format_timestamp(Utc::now())],
        )?;
        transaction.commit()?;
        self.apply_file_permissions()?;

        Ok(IdentityRegeneration {
            old_source_id,
            new_source_id,
            discarded_events: if discard_events { event_count } else { 0 },
            backup_path,
        })
    }

    fn initialize(&self) -> Result<(), SpoolError> {
        let mut connection = Connection::open(self.paths.spool_db())?;
        ensure_safe_sqlite_version(&connection)?;
        reject_newer_database(&connection)?;
        configure_connection(&connection)?;

        let transaction = connection.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
             );",
        )?;
        let existing_version: Option<i64> = transaction
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .optional()?
            .flatten();
        if existing_version.unwrap_or(0) > DATABASE_SCHEMA_VERSION {
            return Err(SpoolError::DatabaseTooNew {
                found: existing_version.unwrap_or_default(),
                supported: DATABASE_SCHEMA_VERSION,
            });
        }
        if existing_version.unwrap_or(0) < 1 {
            apply_migration_v1(&transaction)?;
        }
        ensure_source_identity(&transaction)?;
        transaction.commit()?;
        self.apply_file_permissions()?;
        Ok(())
    }

    fn connection(&self) -> Result<Connection, SpoolError> {
        let connection = Connection::open(self.paths.spool_db())?;
        ensure_safe_sqlite_version(&connection)?;
        reject_newer_database(&connection)?;
        configure_connection(&connection)?;
        Ok(connection)
    }

    fn backup_database(&self, source: &Connection) -> Result<PathBuf, SpoolError> {
        let backup_dir = self.paths.identity_backup_dir();
        create_private_directory(&backup_dir)?;
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        let backup = backup_dir.join(format!("spool-before-identity-{timestamp}.sqlite3"));
        source.backup(MAIN_DB, &backup, None)?;
        set_private_file(&backup)?;
        Ok(backup)
    }

    fn apply_file_permissions(&self) -> Result<(), SpoolError> {
        set_private_file(&self.paths.spool_db())?;
        for suffix in ["-wal", "-shm"] {
            let mut path = self.paths.spool_db().into_os_string();
            path.push(suffix);
            let path = PathBuf::from(path);
            if path.exists() {
                set_private_file(&path)?;
            }
        }
        Ok(())
    }

    fn ensure_maintenance_inactive(&self) -> Result<(), SpoolError> {
        if self.paths.root().join("maintenance.lock").exists() {
            Err(SpoolError::MaintenanceBusy)
        } else {
            Ok(())
        }
    }

    fn maintenance_due(&self, now: DateTime<Utc>) -> Result<bool, SpoolError> {
        let connection = self.connection()?;
        let raw: Option<String> = connection.query_row(
            "SELECT last_maintenance_at FROM source_identity WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        let Some(raw) = raw else {
            return Ok(true);
        };
        let last = DateTime::parse_from_rfc3339(&raw)
            .map_err(|source| SpoolError::InvalidStoredTimestamp { raw, source })?
            .with_timezone(&Utc);
        let elapsed = now.signed_duration_since(last);
        Ok(elapsed < ChronoDuration::zero() || elapsed >= MAINTENANCE_INTERVAL)
    }
}

fn prune_retention_batch(
    transaction: &Transaction<'_>,
    now: DateTime<Utc>,
    policy: RetentionPolicy,
) -> Result<MaintenanceReport, SpoolError> {
    let (mut remaining_events, mut remaining_payload_bytes): (i64, i64) = transaction.query_row(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(CAST(payload_json AS BLOB))), 0)
             FROM events",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let cutoff = now
        .checked_sub_signed(policy.max_age)
        .ok_or(SpoolError::InvalidRetentionPolicy)?;
    let mut deleted_events = 0_i64;
    let mut deleted_payload_bytes = 0_i64;

    for _ in 0..policy.batch_size {
        let candidate = next_retention_candidate(
            transaction,
            cutoff,
            remaining_events,
            remaining_payload_bytes,
            policy,
        )?;
        let Some((sequence, inserted_at, payload_bytes)) = candidate else {
            break;
        };
        let inserted_at = DateTime::parse_from_rfc3339(&inserted_at)
            .map_err(|source| SpoolError::InvalidStoredTimestamp {
                raw: inserted_at,
                source,
            })?
            .with_timezone(&Utc);
        if inserted_at >= cutoff
            && remaining_events <= policy.max_events
            && remaining_payload_bytes <= policy.max_payload_bytes
        {
            break;
        }
        transaction.execute("DELETE FROM events WHERE sequence = ?1", params![sequence])?;
        deleted_events += 1;
        deleted_payload_bytes += payload_bytes;
        remaining_events -= 1;
        remaining_payload_bytes -= payload_bytes;
    }

    let expired_exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE inserted_at < ?1)",
        params![format_timestamp(cutoff)],
        |row| row.get(0),
    )?;
    let more_required = remaining_events > policy.max_events
        || remaining_payload_bytes > policy.max_payload_bytes
        || expired_exists;
    transaction.execute(
        "UPDATE source_identity
         SET last_maintenance_at = ?1
         WHERE singleton = 1",
        params![if more_required {
            None
        } else {
            Some(format_timestamp(now))
        }],
    )?;

    Ok(MaintenanceReport {
        deleted_events,
        deleted_payload_bytes,
        remaining_events,
        remaining_payload_bytes,
        more_required,
    })
}

fn next_retention_candidate(
    transaction: &Transaction<'_>,
    cutoff: DateTime<Utc>,
    remaining_events: i64,
    remaining_payload_bytes: i64,
    policy: RetentionPolicy,
) -> Result<Option<(i64, String, i64)>, SpoolError> {
    Ok(transaction
        .query_row(
            "SELECT sequence, inserted_at, LENGTH(CAST(payload_json AS BLOB))
             FROM events
             WHERE inserted_at < ?1 OR ?2 > ?3 OR ?4 > ?5
             ORDER BY CASE WHEN inserted_at < ?1 THEN 0 ELSE 1 END, sequence ASC
             LIMIT 1",
            params![
                format_timestamp(cutoff),
                remaining_events,
                policy.max_events,
                remaining_payload_bytes,
                policy.max_payload_bytes,
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?)
}

fn configure_connection(connection: &Connection) -> Result<(), SpoolError> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = FULL;
         PRAGMA auto_vacuum = INCREMENTAL;",
    )?;
    let journal_mode: String =
        connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(SpoolError::WalUnavailable(journal_mode));
    }
    Ok(())
}

fn ensure_safe_sqlite_version(connection: &Connection) -> Result<(), SpoolError> {
    let version: String = connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    if sqlite_version_is_safe(&version) {
        Ok(())
    } else {
        Err(SpoolError::UnsafeSqliteVersion(version))
    }
}

fn sqlite_version_is_safe(version: &str) -> bool {
    let mut numbers = version
        .split('.')
        .take(3)
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let parsed = (
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
        numbers.next().unwrap_or(0),
    );
    parsed >= (3, 51, 3)
        || (parsed.0 == 3 && parsed.1 == 50 && parsed.2 >= 7)
        || (parsed.0 == 3 && parsed.1 == 44 && parsed.2 >= 6)
}

fn apply_migration_v1(transaction: &Transaction<'_>) -> Result<(), SpoolError> {
    transaction.execute_batch(
        "CREATE TABLE events (
            sequence INTEGER PRIMARY KEY CHECK (sequence > 0),
            event_id TEXT NOT NULL UNIQUE,
            schema_version INTEGER NOT NULL,
            kind TEXT NOT NULL,
            occurred_at TEXT NOT NULL,
            payload_json TEXT NOT NULL,
            inserted_at TEXT NOT NULL
         );
         CREATE TABLE source_identity (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            source_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            high_watermark INTEGER NOT NULL DEFAULT 0 CHECK (high_watermark >= 0),
            last_maintenance_at TEXT
         );
         INSERT INTO schema_migrations (version, applied_at) VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));",
    )?;
    Ok(())
}

fn ensure_source_identity(transaction: &Transaction<'_>) -> Result<(), SpoolError> {
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_identity WHERE singleton = 1)",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        transaction.execute(
            "INSERT INTO source_identity (
                singleton, source_id, created_at, high_watermark, last_maintenance_at
             ) VALUES (1, ?1, ?2, 0, ?2)",
            params![Uuid::new_v4().to_string(), format_timestamp(Utc::now())],
        )?;
    }
    Ok(())
}

fn next_sequence(transaction: &Transaction<'_>) -> Result<i64, SpoolError> {
    let changed = transaction.execute(
        "UPDATE source_identity
         SET high_watermark = high_watermark + 1
         WHERE singleton = 1 AND high_watermark < ?1",
        params![i64::MAX],
    )?;
    if changed != 1 {
        return Err(SpoolError::SequenceExhausted);
    }
    let sequence: i64 = transaction.query_row(
        "SELECT high_watermark FROM source_identity WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    if sequence <= 0 {
        return Err(SpoolError::SequenceExhausted);
    }
    Ok(sequence)
}

fn source_id_in_transaction(transaction: &Transaction<'_>) -> Result<Uuid, SpoolError> {
    let raw: String = transaction.query_row(
        "SELECT source_id FROM source_identity WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    parse_uuid_column(raw, 0)
}

fn parse_uuid_column(raw: String, column: usize) -> Result<Uuid, SpoolError> {
    Uuid::parse_str(&raw).map_err(|error| SpoolError::InvalidStoredUuid {
        column,
        raw,
        source: error,
    })
}

fn database_schema_version(connection: &Connection) -> Result<i64, SpoolError> {
    let table_exists: bool = connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
            WHERE type = 'table' AND name = 'schema_migrations'
         )",
        [],
        |row| row.get(0),
    )?;
    if !table_exists {
        return Ok(0);
    }
    Ok(connection
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get::<_, Option<i64>>(0)
        })?
        .unwrap_or(0))
}

fn reject_newer_database(connection: &Connection) -> Result<(), SpoolError> {
    let schema_version = database_schema_version(connection)?;
    if schema_version > DATABASE_SCHEMA_VERSION {
        Err(SpoolError::DatabaseTooNew {
            found: schema_version,
            supported: DATABASE_SCHEMA_VERSION,
        })
    } else {
        Ok(())
    }
}

fn default_display_name() -> String {
    hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "local".to_owned())
}

fn create_private_directory(path: &Path) -> Result<(), SpoolError> {
    if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(SpoolError::UnsafeStatePath(path.to_path_buf()));
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn create_private_database_file(path: &Path) -> Result<(), SpoolError> {
    if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(SpoolError::UnsafeStatePath(path.to_path_buf()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
    }
    set_private_file(path)
}

fn set_private_file(path: &Path) -> Result<(), SpoolError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

struct MaintenanceLock {
    path: PathBuf,
}

impl MaintenanceLock {
    fn acquire(root: &Path) -> Result<Self, SpoolError> {
        let path = root.join("maintenance.lock");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    SpoolError::MaintenanceBusy
                } else {
                    SpoolError::Io(error)
                }
            })?;
        set_private_file(&path)?;
        Ok(Self { path })
    }
}

impl Drop for MaintenanceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpoolEvent {
    pub sequence: i64,
    pub event: NormalizedEvent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpoolSnapshot {
    pub source_id: Uuid,
    pub oldest_sequence: Option<i64>,
    pub latest_sequence: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorReport {
    pub healthy: bool,
    #[serde(serialize_with = "serialize_path_lossy")]
    pub state_dir: PathBuf,
    #[serde(serialize_with = "serialize_path_lossy")]
    pub database_path: PathBuf,
    pub sqlite_version: String,
    pub journal_mode: String,
    pub schema_version: i64,
    pub source_id: Uuid,
    pub event_count: i64,
    pub payload_bytes: i64,
    pub oldest_sequence: Option<i64>,
    pub latest_sequence: i64,
}

fn serialize_path_lossy<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&path.to_string_lossy())
}

#[derive(Clone, Copy, Debug)]
pub struct RetentionPolicy {
    pub max_age: ChronoDuration,
    pub max_events: i64,
    pub max_payload_bytes: i64,
    pub batch_size: usize,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_age: ChronoDuration::days(30),
            max_events: 100_000,
            max_payload_bytes: 256 * 1024 * 1024,
            batch_size: MAINTENANCE_BATCH_SIZE,
        }
    }
}

impl RetentionPolicy {
    fn validate(self) -> Result<(), SpoolError> {
        if self.max_age < ChronoDuration::zero()
            || self.max_events < 0
            || self.max_payload_bytes < 0
            || self.batch_size == 0
            || self.batch_size > 10_000
        {
            return Err(SpoolError::InvalidRetentionPolicy);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MaintenanceReport {
    pub deleted_events: i64,
    pub deleted_payload_bytes: i64,
    pub remaining_events: i64,
    pub remaining_payload_bytes: i64,
    pub more_required: bool,
}

#[derive(Clone, Debug)]
pub struct IdentityRegeneration {
    pub old_source_id: Uuid,
    pub new_source_id: Uuid,
    pub discarded_events: i64,
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum SpoolError {
    #[error("the per-user state directory could not be determined")]
    StateDirectoryUnavailable,
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("event validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("bundled SQLite version {0} does not include the required WAL fix")]
    UnsafeSqliteVersion(String),
    #[error("SQLite WAL mode is unavailable (journal_mode={0})")]
    WalUnavailable(String),
    #[error("database schema version {found} is newer than supported version {supported}")]
    DatabaseTooNew { found: i64, supported: i64 },
    #[error("stored UUID in column {column} is invalid: {raw}")]
    InvalidStoredUuid {
        column: usize,
        raw: String,
        #[source]
        source: uuid::Error,
    },
    #[error("sequence counter is exhausted")]
    SequenceExhausted,
    #[error("cursor must be non-negative, got {0}")]
    InvalidCursor(i64),
    #[error("page size is invalid")]
    InvalidPageSize,
    #[error("identity regeneration requires --yes when discarding events")]
    ConfirmationRequired,
    #[error("identity cannot be regenerated while {0} events remain")]
    IdentityHasEvents(i64),
    #[error("another maintenance operation is already active")]
    MaintenanceBusy,
    #[error("state path must not be a symbolic link: {0}")]
    UnsafeStatePath(PathBuf),
    #[error("retention policy is invalid")]
    InvalidRetentionPolicy,
    #[error("stored timestamp is invalid: {raw}")]
    InvalidStoredTimestamp {
        raw: String,
        #[source]
        source: chrono::ParseError,
    },
    #[error("stored event at sequence {sequence} does not match its {field} index")]
    StoredEventMismatch { sequence: i64, field: &'static str },
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use tempfile::TempDir;

    use super::*;
    use crate::event::Outcome;

    fn spool() -> (TempDir, Spool) {
        let directory = TempDir::new().unwrap();
        let spool =
            Spool::open_with_display_name(StatePaths::new(directory.path()), "test-host").unwrap();
        (directory, spool)
    }

    #[test]
    fn sequences_are_monotonic_and_snapshot_is_durable() {
        let (_directory, spool) = spool();
        let first = spool
            .emit(
                EmitRequest {
                    title: Some("First".into()),
                    ..EmitRequest::default()
                },
                Some(EventKind::TaskCompleted),
            )
            .unwrap();
        let second = spool
            .emit(
                EmitRequest {
                    title: Some("Second".into()),
                    outcome: Some(Outcome::Succeeded),
                    ..EmitRequest::default()
                },
                Some(EventKind::TaskCompleted),
            )
            .unwrap();

        assert_eq!((first.sequence, second.sequence), (1, 2));
        assert_eq!(spool.snapshot().unwrap().oldest_sequence, Some(1));
        assert_eq!(spool.snapshot().unwrap().latest_sequence, 2);
        assert_eq!(spool.events_after(1, None).unwrap(), vec![second]);
    }

    #[test]
    fn concurrent_emit_allocates_unique_sequences() {
        let (_directory, spool) = spool();
        let spool = Arc::new(spool);
        let mut threads = Vec::new();
        for index in 0..20 {
            let spool = Arc::clone(&spool);
            threads.push(thread::spawn(move || {
                spool
                    .emit(
                        EmitRequest {
                            title: Some(format!("Event {index}")),
                            ..EmitRequest::default()
                        },
                        Some(EventKind::AgentQuestion),
                    )
                    .unwrap()
                    .sequence
            }));
        }
        let mut sequences: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        sequences.sort_unstable();
        assert_eq!(sequences, (1..=20).collect::<Vec<_>>());
    }

    #[test]
    fn identity_regeneration_refuses_nonempty_spool_without_discard() {
        let (_directory, spool) = spool();
        spool
            .emit(
                EmitRequest {
                    title: Some("Question".into()),
                    ..EmitRequest::default()
                },
                Some(EventKind::AgentQuestion),
            )
            .unwrap();

        assert!(matches!(
            spool.regenerate_identity(false, false).unwrap_err(),
            SpoolError::IdentityHasEvents(1)
        ));
        assert!(matches!(
            spool.regenerate_identity(true, false).unwrap_err(),
            SpoolError::ConfirmationRequired
        ));
        let regenerated = spool.regenerate_identity(true, true).unwrap();
        assert_eq!(regenerated.discarded_events, 1);
        let backup_path = regenerated.backup_path.unwrap();
        assert!(backup_path.exists());
        let backup = Connection::open(backup_path).unwrap();
        let backup_event_count: i64 = backup
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        let backup_source_id: String = backup
            .query_row(
                "SELECT source_id FROM source_identity WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(backup_event_count, 1);
        assert_eq!(
            Uuid::parse_str(&backup_source_id).unwrap(),
            regenerated.old_source_id
        );
        assert_eq!(spool.snapshot().unwrap().latest_sequence, 0);
    }

    #[test]
    fn recognizes_fixed_sqlite_release_lines() {
        assert!(sqlite_version_is_safe("3.51.3"));
        assert!(sqlite_version_is_safe("3.52.0"));
        assert!(sqlite_version_is_safe("3.50.7"));
        assert!(sqlite_version_is_safe("3.44.6"));
        assert!(!sqlite_version_is_safe("3.51.2"));
        assert!(!sqlite_version_is_safe("3.50.6"));
        assert!(!sqlite_version_is_safe("3.44.5"));
    }

    #[test]
    fn maintenance_prunes_without_resetting_high_watermark() {
        let (_directory, spool) = spool();
        for index in 0..3 {
            spool
                .emit(
                    EmitRequest {
                        title: Some(format!("Event {index}")),
                        ..EmitRequest::default()
                    },
                    Some(EventKind::AgentQuestion),
                )
                .unwrap();
        }

        let report = spool
            .maintain(
                Utc::now(),
                RetentionPolicy {
                    max_events: 1,
                    ..RetentionPolicy::default()
                },
            )
            .unwrap();
        assert_eq!(report.deleted_events, 2);
        let snapshot = spool.snapshot().unwrap();
        assert_eq!(snapshot.oldest_sequence, Some(3));
        assert_eq!(snapshot.latest_sequence, 3);

        let next = spool
            .emit(
                EmitRequest {
                    title: Some("Next".into()),
                    ..EmitRequest::default()
                },
                Some(EventKind::AgentQuestion),
            )
            .unwrap();
        assert_eq!(next.sequence, 4);
    }

    #[test]
    fn maintenance_lock_blocks_emit() {
        let (_directory, spool) = spool();
        fs::write(spool.paths.root().join("maintenance.lock"), b"locked").unwrap();

        assert!(matches!(
            spool
                .emit(
                    EmitRequest {
                        title: Some("Question".into()),
                        ..EmitRequest::default()
                    },
                    Some(EventKind::AgentQuestion),
                )
                .unwrap_err(),
            SpoolError::MaintenanceBusy
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_state_root() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().unwrap();
        let target = directory.path().join("target");
        fs::create_dir(&target).unwrap();
        let linked = directory.path().join("linked");
        symlink(&target, &linked).unwrap();

        assert!(matches!(
            Spool::open(StatePaths::new(linked)).unwrap_err(),
            SpoolError::UnsafeStatePath(_)
        ));
    }

    #[test]
    fn source_identity_is_stable_across_reopen() {
        let directory = TempDir::new().unwrap();
        let paths = StatePaths::new(directory.path());
        let first = Spool::open_with_display_name(paths.clone(), "one")
            .unwrap()
            .snapshot()
            .unwrap()
            .source_id;
        let second = Spool::open_with_display_name(paths, "two")
            .unwrap()
            .snapshot()
            .unwrap()
            .source_id;
        assert_eq!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn state_permissions_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let (directory, spool) = spool();
        assert_eq!(
            fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(spool.paths.spool_db())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn maintenance_finds_expired_events_after_newer_sequences() {
        let (_directory, spool) = spool();
        for index in 0..2 {
            spool
                .emit(
                    EmitRequest {
                        title: Some(format!("Event {index}")),
                        ..EmitRequest::default()
                    },
                    Some(EventKind::AgentQuestion),
                )
                .unwrap();
        }
        let connection = spool.connection().unwrap();
        connection
            .execute(
                "UPDATE events
                 SET inserted_at = ?1
                 WHERE sequence = 2",
                params![format_timestamp(Utc::now() - ChronoDuration::days(31))],
            )
            .unwrap();
        drop(connection);

        let report = spool
            .maintain(Utc::now(), RetentionPolicy::default())
            .unwrap();
        assert_eq!(report.deleted_events, 1);
        assert_eq!(
            spool
                .events_after(0, None)
                .unwrap()
                .into_iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn detects_index_and_payload_mismatch() {
        let (_directory, spool) = spool();
        spool
            .emit(
                EmitRequest {
                    title: Some("Question".into()),
                    ..EmitRequest::default()
                },
                Some(EventKind::AgentQuestion),
            )
            .unwrap();
        let connection = spool.connection().unwrap();
        connection
            .execute(
                "UPDATE events SET event_id = ?1 WHERE sequence = 1",
                params![Uuid::now_v7().to_string()],
            )
            .unwrap();
        drop(connection);

        assert!(matches!(
            spool.events_after(0, None).unwrap_err(),
            SpoolError::StoredEventMismatch {
                sequence: 1,
                field: "event_id"
            }
        ));
    }

    #[test]
    fn future_maintenance_timestamp_does_not_disable_retention_forever() {
        let (_directory, spool) = spool();
        let connection = spool.connection().unwrap();
        connection
            .execute(
                "UPDATE source_identity SET last_maintenance_at = ?1 WHERE singleton = 1",
                params![format_timestamp(Utc::now() + ChronoDuration::days(1))],
            )
            .unwrap();
        drop(connection);

        assert!(spool.maintenance_due(Utc::now()).unwrap());
    }

    #[test]
    fn refuses_newer_database_schema_without_removing_it() {
        let (directory, spool) = spool();
        let connection = spool.connection().unwrap();
        connection
            .execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (2, ?1)",
                params![format_timestamp(Utc::now())],
            )
            .unwrap();
        connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        drop(connection);
        drop(spool);
        let database = directory.path().join("spool.sqlite3");
        let before = fs::read(&database).unwrap();

        assert!(matches!(
            Spool::open(StatePaths::new(directory.path())).unwrap_err(),
            SpoolError::DatabaseTooNew {
                found: 2,
                supported: 1
            }
        ));
        assert_eq!(fs::read(&database).unwrap(), before);
        let connection = Connection::open(database).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, 2);
    }

    #[test]
    fn corrupt_database_is_preserved_for_recovery() {
        let directory = TempDir::new().unwrap();
        let database = directory.path().join("spool.sqlite3");
        let original = b"not a SQLite database";
        fs::write(&database, original).unwrap();

        assert!(Spool::open(StatePaths::new(directory.path())).is_err());
        assert_eq!(fs::read(database).unwrap(), original);
    }

    #[test]
    fn sequence_exhaustion_does_not_insert_an_event() {
        let (_directory, spool) = spool();
        let connection = spool.connection().unwrap();
        connection
            .execute(
                "UPDATE source_identity SET high_watermark = ?1 WHERE singleton = 1",
                params![i64::MAX],
            )
            .unwrap();
        drop(connection);

        assert!(matches!(
            spool
                .emit(
                    EmitRequest {
                        title: Some("Question".into()),
                        ..EmitRequest::default()
                    },
                    Some(EventKind::AgentQuestion),
                )
                .unwrap_err(),
            SpoolError::SequenceExhausted
        ));
        assert!(spool.events_after(0, None).unwrap().is_empty());
        assert_eq!(spool.snapshot().unwrap().latest_sequence, i64::MAX);
    }

    #[test]
    fn rejects_invalid_cursor_and_page_sizes() {
        let (_directory, spool) = spool();
        assert!(matches!(
            spool.events_after(-1, None).unwrap_err(),
            SpoolError::InvalidCursor(-1)
        ));
        assert!(matches!(
            spool.events_after(0, Some(0)).unwrap_err(),
            SpoolError::InvalidPageSize
        ));
        assert!(matches!(
            spool.events_after(0, Some(10_001)).unwrap_err(),
            SpoolError::InvalidPageSize
        ));
    }

    #[test]
    fn bounded_maintenance_reports_remaining_work() {
        let (_directory, spool) = spool();
        for index in 0..3 {
            spool
                .emit(
                    EmitRequest {
                        title: Some(format!("Event {index}")),
                        ..EmitRequest::default()
                    },
                    Some(EventKind::AgentQuestion),
                )
                .unwrap();
        }
        let policy = RetentionPolicy {
            max_events: 0,
            batch_size: 1,
            ..RetentionPolicy::default()
        };
        let first = spool.maintain(Utc::now(), policy).unwrap();
        assert_eq!(first.deleted_events, 1);
        assert!(first.more_required);
        assert!(spool.maintenance_due(Utc::now()).unwrap());
        let second = spool.maintain(Utc::now(), policy).unwrap();
        assert_eq!(second.deleted_events, 1);
        assert!(second.more_required);
        let third = spool.maintain(Utc::now(), policy).unwrap();
        assert_eq!(third.deleted_events, 1);
        assert!(!third.more_required);
    }
}
