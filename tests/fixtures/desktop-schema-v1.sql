CREATE TABLE desktop_schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);
CREATE TABLE desktop_sources (
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
    lost_through_sequence INTEGER NOT NULL CHECK (lost_through_sequence >= lost_from_sequence),
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
INSERT INTO desktop_schema_migrations(version, applied_at)
VALUES (1, '2026-01-01T00:00:00Z');
INSERT INTO desktop_sources(
    source_key,
    local_label,
    pinned_source_id,
    cursor,
    created_at,
    updated_at
) VALUES (
    'local',
    'Fixture Mac',
    NULL,
    0,
    '2026-01-01T00:00:00Z',
    '2026-01-01T00:00:00Z'
);
