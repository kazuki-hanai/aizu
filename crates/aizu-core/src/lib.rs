//! Shared event, storage, and bridge protocol implementation for Aizu.

pub mod event;
pub mod paths;
pub mod protocol;
pub mod spool;

pub use event::{
    EmitRequest, EventKind, NormalizedEvent, Outcome, Source, Urgency, ValidationError,
};
pub use paths::StatePaths;
pub use protocol::{
    BridgeFrame, BridgeStreamValidator, FrameDecoder, PROTOCOL_VERSION, ParsedBridgeFrame,
    parse_frame_line, parse_strict_json_value,
};
pub use spool::{
    DATABASE_SCHEMA_VERSION, DoctorReport, IdentityRegeneration, MaintenanceReport,
    RetentionPolicy, Spool, SpoolError, SpoolEvent, SpoolSnapshot,
};

/// Maximum serialized normalized event size.
pub const MAX_EVENT_BYTES: usize = 65_536;
/// Maximum serialized bridge frame size, excluding the trailing newline.
pub const MAX_FRAME_BYTES: usize = 131_072;

/// Version of the `SQLite` library linked into this binary.
#[must_use]
pub fn sqlite_version() -> &'static str {
    rusqlite::version()
}
