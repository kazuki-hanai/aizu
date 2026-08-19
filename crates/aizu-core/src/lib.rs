//! Shared event, storage, and bridge protocol implementation for Aizu.

pub mod adapter;
pub mod agent_monitor;
pub mod approval;
pub mod desktop;
pub mod event;
pub mod hook_install;
pub mod install;
pub mod integration;
pub mod notification;
pub mod paths;
pub mod pipeline;
pub mod protocol;
pub mod remote;
pub mod spool;
pub mod ssh;
pub mod terminal_activation;

pub use adapter::{
    AdapterError, AgentAdapter, ClaudeCodeAdapter, CodexAdapter, safe_agent_excerpt,
};
pub use agent_monitor::{
    AgentCommandSpec, AgentHook, AgentKind, ExitDiagnostic, HookCapability, HookStatus,
    LifecycleError, MAX_PROCESS_SNAPSHOT_ENTRIES, ObservedAgentProcess, ProcessExit,
    ProcessLifecycleState, ProcessSnapshot, ProcessSnapshotError,
};
pub use approval::{
    ApprovalDecision, ApprovalError, LOCAL_APPROVAL_PRESENTED_METADATA_KEY,
    LOCAL_APPROVAL_PROTOCOL_VERSION, LocalApprovalRequest, LocalApprovalResponse,
    MAX_LOCAL_APPROVAL_COMMAND_BYTES, MAX_LOCAL_APPROVAL_FRAME_BYTES,
    MAX_LOCAL_APPROVAL_TOOL_BYTES, local_approval_request_from_hook,
};
pub use desktop::{
    DESKTOP_DATABASE_SCHEMA_VERSION, DesktopError, DesktopState, GapResult, HistoryEvent,
    HistoryGap, HistoryItem, HistoryMaintenanceReport, IngestResult, OutboxItem, OutboxOutcome,
    OutboxState, PinResult, SourceRecord, SourceRegistration,
};
pub use event::{
    EmitRequest, EventKind, NormalizedEvent, Outcome, Source, Urgency, ValidationError,
};
pub use hook_install::{
    HookInstallError, HookInstallOutcome, HookInstallResult, MAX_AGENT_CONFIG_BYTES,
    agent_configuration_path, install_agent_hooks, resolve_agent_configuration_path,
};
pub use install::{InstallError, InstallOutcome, install_cli};
pub use integration::{IntegrationError, hook_configuration, merge_hook_configuration};
pub use notification::{
    BacklogPlan, CLOCK_SKEW_THRESHOLD, NotificationContext, NotificationDecision,
    NotificationPolicy, PreparedNotification, QuietHours, SuppressionReason, aggregate_backlog,
    aggregate_backlog_count, is_clock_skewed,
};
pub use paths::StatePaths;
pub use pipeline::{
    Notifier, NotifyError, PipelineError, PipelineReport, dispatch_outbox, ingest_spool,
};
pub use protocol::{
    BridgeFrame, BridgeStreamValidator, FrameDecoder, PROTOCOL_VERSION, ParsedBridgeFrame,
    parse_frame_line, parse_strict_json_value,
};
pub use remote::{
    BRIDGE_STALE_TIMEOUT, BRIDGE_STARTUP_TIMEOUT, BoundedBridgeStderr, MAX_CAPTURED_STDERR_BYTES,
    ReconnectDisposition, RemoteBridgeConsumer, RemoteConsumerError, RemoteDiagnostic,
    RemoteStreamReport, RemoteTermination,
};
pub use spool::{
    DATABASE_SCHEMA_VERSION, DoctorReport, IdentityRegeneration, MaintenanceReport,
    RetentionPolicy, Spool, SpoolError, SpoolEvent, SpoolSnapshot,
};
pub use ssh::{
    SshCommandSpec, SshConfigurationError, SshFailureCategory, SystemSshSource,
    classify_ssh_failure, validate_host_alias, validate_preflight_output,
};
pub use terminal_activation::{
    TERMINAL_ACTIVATION_METADATA_KEY, TerminalActivation, TerminalApplication, TmuxActivation,
    remove_terminal_activation_metadata,
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
