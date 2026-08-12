use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use aizu_core::{
    BridgeFrame, EmitRequest, EventKind, MAX_FRAME_BYTES, Outcome, PROTOCOL_VERSION, Spool,
    SpoolError, StatePaths, Urgency, parse_strict_json_value,
};
use chrono::Utc;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{Value, json};

const FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(250);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const BRIDGE_PAGE_SIZE: usize = 256;

#[derive(Debug, Parser)]
#[command(
    name = "aizu",
    version,
    about = "Durable notifications for terminal AI agents"
)]
struct Cli {
    /// Override the per-user state directory.
    #[arg(long, global = true, value_name = "PATH")]
    state_dir: Option<PathBuf>,

    /// Override the local source display-name candidate.
    #[arg(long, global = true, value_name = "NAME")]
    display_name: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Persist a normalized agent event.
    Emit(EmitArgs),
    /// Convert a generic agent hook payload into an event.
    Hook(HookArgs),
    /// Stream durable events as the versioned NDJSON bridge protocol.
    Bridge(BridgeArgs),
    /// Inspect spool health without exposing event contents.
    Doctor(DoctorArgs),
    /// Inspect or regenerate the durable source identity.
    Identity(IdentityArgs),
    /// Print application and protocol versions.
    Version(VersionArgs),
}

#[derive(Debug, Args)]
struct EmitArgs {
    /// Event kind. Omit only when --stdin-json supplies it.
    #[arg(value_enum, required_unless_present = "stdin_json")]
    kind: Option<KindArg>,

    /// Read an `EmitRequest` JSON object from stdin.
    #[arg(long, conflicts_with = "kind")]
    stdin_json: bool,

    #[arg(long, conflicts_with = "stdin_json")]
    title: Option<String>,
    #[arg(long, conflicts_with = "stdin_json")]
    body: Option<String>,
    #[arg(long, value_enum, conflicts_with = "stdin_json")]
    outcome: Option<OutcomeArg>,
    #[arg(long, value_enum, conflicts_with = "stdin_json")]
    urgency: Option<UrgencyArg>,
    #[arg(long, conflicts_with = "stdin_json")]
    agent: Option<String>,
    #[arg(long, conflicts_with = "stdin_json")]
    session_id: Option<String>,
    #[arg(long, conflicts_with = "stdin_json")]
    occurred_at: Option<String>,
    /// Metadata encoded as a JSON object.
    #[arg(long, value_name = "JSON", conflicts_with = "stdin_json")]
    metadata: Option<String>,
    /// Print the persisted event as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct HookArgs {
    #[arg(long)]
    agent: String,
    #[arg(long, value_name = "EVENT")]
    event: String,
    /// Return non-zero on parse, validation, or persistence failure.
    #[arg(long)]
    strict: bool,
}

#[derive(Debug, Args)]
struct BridgeArgs {
    #[arg(long)]
    protocol: u32,
    #[arg(long, value_parser = parse_cursor)]
    after: i64,
    #[arg(long)]
    follow: bool,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct VersionArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct IdentityArgs {
    #[command(subcommand)]
    command: IdentityCommand,
}

#[derive(Debug, Subcommand)]
enum IdentityCommand {
    /// Replace the source identity, refusing non-empty spools by default.
    Regenerate {
        #[arg(long)]
        discard_events: bool,
        /// Confirm destructive event discard.
        #[arg(long, requires = "discard_events")]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum KindArg {
    #[value(name = "task.completed")]
    TaskCompleted,
    #[value(name = "agent.question")]
    AgentQuestion,
}

impl From<KindArg> for EventKind {
    fn from(value: KindArg) -> Self {
        match value {
            KindArg::TaskCompleted => Self::TaskCompleted,
            KindArg::AgentQuestion => Self::AgentQuestion,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OutcomeArg {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

impl From<OutcomeArg> for Outcome {
    fn from(value: OutcomeArg) -> Self {
        match value {
            OutcomeArg::Succeeded => Self::Succeeded,
            OutcomeArg::Failed => Self::Failed,
            OutcomeArg::Cancelled => Self::Cancelled,
            OutcomeArg::Unknown => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum UrgencyArg {
    Low,
    Normal,
    High,
}

impl From<UrgencyArg> for Urgency {
    fn from(value: UrgencyArg) -> Self {
        match value {
            UrgencyArg::Low => Self::Low,
            UrgencyArg::Normal => Self::Normal,
            UrgencyArg::High => Self::High,
        }
    }
}

#[derive(Serialize)]
struct PersistedEvent<'a> {
    sequence: i64,
    event: &'a aizu_core::NormalizedEvent,
}

#[derive(Serialize)]
struct VersionReport<'a> {
    application: &'a str,
    protocol: u32,
    event_schema: u32,
    database_schema: i64,
    sqlite: &'a str,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) if error.downcast_ref::<BrokenPipe>().is_some() => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("aizu: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, Box<dyn Error>> {
    let cli = Cli::parse();
    let paths = match cli.state_dir {
        Some(root) => StatePaths::new(root),
        None => StatePaths::discover()?,
    };

    match cli.command {
        Command::Version(args) => {
            print_version(args.json)?;
            Ok(0)
        }
        Command::Bridge(args) => run_bridge_command(&paths, cli.display_name, &args),
        Command::Hook(args) => run_hook_command(&paths, cli.display_name, args),
        command => {
            let spool = open_spool(&paths, cli.display_name)?;
            match command {
                Command::Emit(args) => {
                    run_emit(&spool, args)?;
                    Ok(0)
                }
                Command::Doctor(args) => {
                    print_doctor(&spool, args.json)?;
                    Ok(0)
                }
                Command::Identity(args) => run_identity(&spool, &args),
                Command::Bridge(_) | Command::Hook(_) | Command::Version(_) => {
                    unreachable!("handled above")
                }
            }
        }
    }
}

fn open_spool(paths: &StatePaths, display_name: Option<String>) -> Result<Spool, SpoolError> {
    match display_name {
        Some(name) => Spool::open_with_display_name(paths.clone(), name),
        None => Spool::open(paths.clone()),
    }
}

fn run_emit(spool: &Spool, args: EmitArgs) -> Result<(), Box<dyn Error>> {
    let (request, default_kind) = if args.stdin_json {
        let raw = read_stdin_limited(MAX_FRAME_BYTES)?;
        let request =
            serde_json::from_value::<EmitRequest>(parse_strict_json_value(&raw, MAX_FRAME_BYTES)?)?;
        (request, None)
    } else {
        let metadata = args
            .metadata
            .map(|raw| serde_json::from_str::<Value>(&raw))
            .transpose()?;
        (
            EmitRequest {
                kind: None,
                title: args.title,
                body: args.body,
                outcome: args.outcome.map(Into::into),
                urgency: args.urgency.map(Into::into),
                agent: args.agent,
                session_id: args.session_id,
                occurred_at: args.occurred_at,
                metadata,
                ignored: BTreeMap::default(),
            },
            args.kind.map(Into::into),
        )
    };
    let persisted = spool.emit(request, default_kind)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&PersistedEvent {
                sequence: persisted.sequence,
                event: &persisted.event,
            })?
        );
    } else {
        println!(
            "queued {} event {} at sequence {}",
            persisted.event.kind.as_str(),
            persisted.event.id,
            persisted.sequence
        );
    }
    Ok(())
}

fn run_hook_command(
    paths: &StatePaths,
    display_name: Option<String>,
    args: HookArgs,
) -> Result<u8, Box<dyn Error>> {
    let result = (|| -> Result<(), Box<dyn Error>> {
        let kind = parse_hook_event(&args.event)?;
        let raw = read_optional_stdin_limited(MAX_FRAME_BYTES)?;
        let mut request = if raw.is_empty() {
            EmitRequest::default()
        } else {
            serde_json::from_value::<EmitRequest>(parse_strict_json_value(&raw, MAX_FRAME_BYTES)?)?
        };
        request.kind = Some(kind);
        request.agent = Some(args.agent);
        if request.title.is_none() {
            request.title = Some(
                match kind {
                    EventKind::TaskCompleted => "Task completed",
                    EventKind::AgentQuestion => "Agent is waiting for input",
                }
                .to_owned(),
            );
        }
        let spool = open_spool(paths, display_name)?;
        spool.emit(request, Some(kind))?;
        Ok(())
    })();

    match result {
        Ok(()) => Ok(0),
        Err(error) if !args.strict => {
            eprintln!("aizu hook: notification event was not persisted: {error}");
            Ok(0)
        }
        Err(error) => Err(error),
    }
}

fn parse_hook_event(raw: &str) -> Result<EventKind, String> {
    match raw {
        "task.completed" | "completed" | "stop" | "session_end" => Ok(EventKind::TaskCompleted),
        "agent.question" | "question" | "permission" | "permission_request" | "input_required" => {
            Ok(EventKind::AgentQuestion)
        }
        _ => Err(format!("unsupported hook event {raw:?}")),
    }
}

fn run_bridge_command(
    paths: &StatePaths,
    display_name: Option<String>,
    args: &BridgeArgs,
) -> Result<u8, Box<dyn Error>> {
    let mut stdout = io::stdout().lock();
    if args.protocol != PROTOCOL_VERSION {
        write_frame(
            &mut stdout,
            &BridgeFrame::terminal_error(
                "incompatible_protocol",
                "unsupported protocol major version",
            ),
        )?;
        return Ok(2);
    }

    let spool = match open_spool(paths, display_name) {
        Ok(spool) => spool,
        Err(error) => {
            write_frame(&mut stdout, &bridge_spool_error(&error))?;
            eprintln!("aizu bridge: {error}");
            return Ok(1);
        }
    };
    if let Err(error) = spool.maintain_default() {
        write_frame(&mut stdout, &bridge_spool_error(&error))?;
        eprintln!("aizu bridge: {error}");
        return Ok(1);
    }

    match stream_bridge(&spool, args, &mut stdout) {
        Ok(code) => Ok(code),
        Err(error) => {
            let frame = error.downcast_ref::<SpoolError>().map_or_else(
                || BridgeFrame::terminal_error("internal", "bridge processing failed"),
                bridge_spool_error,
            );
            write_frame(&mut stdout, &frame)?;
            eprintln!("aizu bridge: {error}");
            Ok(1)
        }
    }
}

fn bridge_spool_error(error: &SpoolError) -> BridgeFrame {
    if error.indicates_corruption() {
        return BridgeFrame::terminal_error("spool_corrupt", "spool integrity validation failed");
    }
    match error {
        SpoolError::DatabaseTooNew { .. } | SpoolError::UnsafeSqliteVersion(_) => {
            BridgeFrame::terminal_error(
                "incompatible_database",
                "spool requires a compatible Aizu CLI",
            )
        }
        SpoolError::NetworkFilesystem(_) | SpoolError::WalUnavailable(_) => {
            BridgeFrame::terminal_error(
                "unsupported_storage",
                "spool requires a supported local filesystem",
            )
        }
        _ => BridgeFrame::terminal_error("spool_unavailable", "spool is unavailable"),
    }
}

fn stream_bridge(
    spool: &Spool,
    args: &BridgeArgs,
    stdout: &mut impl Write,
) -> Result<u8, Box<dyn Error>> {
    let initial = spool.snapshot()?;
    write_frame(
        stdout,
        &BridgeFrame::hello(
            initial.source_id,
            initial.oldest_sequence,
            initial.latest_sequence,
        ),
    )?;
    if args.after > initial.latest_sequence {
        write_frame(
            stdout,
            &BridgeFrame::terminal_error("cursor_ahead", "requested cursor is ahead of the source"),
        )?;
        return Ok(2);
    }

    let mut cursor = args.after;
    let source_id = initial.source_id;
    let mut last_frame = Instant::now();
    emit_gap_if_needed(stdout, &initial, &mut cursor)?;

    loop {
        let snapshot = spool.snapshot()?;
        if !stream_snapshot_is_valid(stdout, &snapshot, source_id, cursor)? {
            return Ok(2);
        }
        if emit_gap_if_needed(stdout, &snapshot, &mut cursor)? {
            last_frame = Instant::now();
        }

        let events = spool.events_after(cursor, Some(BRIDGE_PAGE_SIZE))?;
        let event_count = events.len();
        let mut wrote_event = false;
        for item in events {
            if item.sequence > cursor.saturating_add(1) {
                write_frame(
                    stdout,
                    &BridgeFrame::Gap {
                        requested_after: cursor,
                        oldest_sequence: Some(item.sequence),
                        lost_through_sequence: item.sequence - 1,
                    },
                )?;
            }
            write_frame(
                stdout,
                &BridgeFrame::Event {
                    sequence: item.sequence,
                    event: Box::new(item.event),
                },
            )?;
            cursor = item.sequence;
            wrote_event = true;
            last_frame = Instant::now();
        }

        if event_count == BRIDGE_PAGE_SIZE {
            continue;
        }
        let post_read_snapshot = spool.snapshot()?;
        if !stream_snapshot_is_valid(stdout, &post_read_snapshot, source_id, cursor)? {
            return Ok(2);
        }
        if cursor < post_read_snapshot.latest_sequence {
            continue;
        }
        if !args.follow {
            return Ok(0);
        }
        if !wrote_event && last_frame.elapsed() >= heartbeat_interval() {
            write_frame(
                stdout,
                &BridgeFrame::Heartbeat {
                    sent_at: Utc::now(),
                },
            )?;
            last_frame = Instant::now();
        }
        thread::sleep(follow_poll_interval());
    }
}

fn stream_snapshot_is_valid(
    writer: &mut impl Write,
    snapshot: &aizu_core::SpoolSnapshot,
    expected_source_id: uuid::Uuid,
    cursor: i64,
) -> Result<bool, Box<dyn Error>> {
    let error = if snapshot.source_id != expected_source_id {
        Some(BridgeFrame::terminal_error(
            "source_identity_changed",
            "source identity changed during the stream",
        ))
    } else if cursor > snapshot.latest_sequence {
        Some(BridgeFrame::terminal_error(
            "cursor_ahead",
            "requested cursor is ahead of the source",
        ))
    } else {
        None
    };
    if let Some(frame) = error {
        write_frame(writer, &frame)?;
        Ok(false)
    } else {
        Ok(true)
    }
}

fn emit_gap_if_needed(
    writer: &mut impl Write,
    snapshot: &aizu_core::SpoolSnapshot,
    cursor: &mut i64,
) -> Result<bool, Box<dyn Error>> {
    let gap = match snapshot.oldest_sequence {
        Some(oldest) if *cursor < oldest.saturating_sub(1) => Some(BridgeFrame::Gap {
            requested_after: *cursor,
            oldest_sequence: Some(oldest),
            lost_through_sequence: oldest - 1,
        }),
        None if *cursor < snapshot.latest_sequence => Some(BridgeFrame::Gap {
            requested_after: *cursor,
            oldest_sequence: None,
            lost_through_sequence: snapshot.latest_sequence,
        }),
        _ => None,
    };
    if let Some(BridgeFrame::Gap {
        lost_through_sequence,
        ..
    }) = gap
    {
        write_frame(
            writer,
            &BridgeFrame::Gap {
                requested_after: *cursor,
                oldest_sequence: snapshot.oldest_sequence,
                lost_through_sequence,
            },
        )?;
        *cursor = lost_through_sequence;
        return Ok(true);
    }
    Ok(false)
}

fn write_frame(writer: &mut impl Write, frame: &BridgeFrame) -> Result<(), Box<dyn Error>> {
    match frame.write_to(writer) {
        Ok(()) => Ok(()),
        Err(aizu_core::protocol::ProtocolError::Io(error))
            if error.kind() == io::ErrorKind::BrokenPipe =>
        {
            Err(Box::new(BrokenPipe))
        }
        Err(error) => Err(Box::new(error)),
    }
}

#[derive(Debug)]
struct BrokenPipe;

impl fmt::Display for BrokenPipe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bridge consumer closed the stream")
    }
}

impl Error for BrokenPipe {}

fn print_doctor(spool: &Spool, json_output: bool) -> Result<(), Box<dyn Error>> {
    let report = spool.doctor()?;
    if json_output {
        println!("{}", serde_json::to_string(&report)?);
    } else {
        println!("healthy: {}", report.healthy);
        println!("state directory: {}", report.state_dir.display());
        println!("database: {}", report.database_path.display());
        println!("SQLite: {}", report.sqlite_version);
        println!("journal mode: {}", report.journal_mode);
        println!("schema: {}", report.schema_version);
        println!("source: {}", report.source_id);
        println!("events: {}", report.event_count);
        println!("payload bytes: {}", report.payload_bytes);
        println!(
            "sequence range: {}..={}",
            report
                .oldest_sequence
                .map_or_else(|| "empty".to_owned(), |value| value.to_string()),
            report.latest_sequence
        );
    }
    Ok(())
}

fn run_identity(spool: &Spool, args: &IdentityArgs) -> Result<u8, Box<dyn Error>> {
    match &args.command {
        IdentityCommand::Regenerate {
            discard_events,
            yes,
            json: json_output,
        } => {
            let result = spool.regenerate_identity(*discard_events, *yes)?;
            if *json_output {
                println!(
                    "{}",
                    json!({
                        "old_source_id": result.old_source_id,
                        "new_source_id": result.new_source_id,
                        "discarded_events": result.discarded_events,
                        "backup_path": result
                            .backup_path
                            .as_ref()
                            .map(|path| path.to_string_lossy().into_owned()),
                    })
                );
            } else {
                println!(
                    "regenerated source identity {} -> {}",
                    result.old_source_id, result.new_source_id
                );
                if let Some(path) = result.backup_path {
                    println!("backup: {}", path.display());
                }
            }
            Ok(0)
        }
    }
}

fn print_version(json_output: bool) -> Result<(), Box<dyn Error>> {
    if json_output {
        println!(
            "{}",
            serde_json::to_string(&VersionReport {
                application: env!("CARGO_PKG_VERSION"),
                protocol: PROTOCOL_VERSION,
                event_schema: aizu_core::event::SCHEMA_VERSION,
                database_schema: aizu_core::DATABASE_SCHEMA_VERSION,
                sqlite: aizu_core::sqlite_version(),
            })?
        );
    } else {
        println!("aizu {}", env!("CARGO_PKG_VERSION"));
    }
    Ok(())
}

fn read_stdin_limited(maximum: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take(u64::try_from(maximum + 1)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(format!("stdin exceeds the {maximum}-byte limit").into());
    }
    if bytes.is_empty() {
        return Err("stdin JSON is empty".into());
    }
    Ok(bytes)
}

fn read_optional_stdin_limited(maximum: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    if io::stdin().is_terminal() {
        return Ok(Vec::new());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .lock()
        .take(u64::try_from(maximum + 1)?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(format!("stdin exceeds the {maximum}-byte limit").into());
    }
    Ok(bytes)
}

fn parse_cursor(raw: &str) -> Result<i64, String> {
    let value = raw
        .parse::<i64>()
        .map_err(|_| "cursor must be an integer".to_owned())?;
    if value < 0 {
        Err("cursor must be non-negative".to_owned())
    } else {
        Ok(value)
    }
}

fn heartbeat_interval() -> Duration {
    debug_duration_override("AIZU_TEST_HEARTBEAT_MS").unwrap_or(HEARTBEAT_INTERVAL)
}

fn follow_poll_interval() -> Duration {
    debug_duration_override("AIZU_TEST_POLL_MS").unwrap_or(FOLLOW_POLL_INTERVAL)
}

fn debug_duration_override(name: &str) -> Option<Duration> {
    if cfg!(debug_assertions) {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|milliseconds| *milliseconds > 0)
            .map(Duration::from_millis)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_compatibility_errors_are_non_retryable_bridge_frames() {
        for error in [
            SpoolError::DatabaseTooNew {
                found: 2,
                supported: 1,
            },
            SpoolError::UnsafeSqliteVersion("3.51.2".into()),
        ] {
            assert!(matches!(
                bridge_spool_error(&error),
                BridgeFrame::Error { code, .. } if code == "incompatible_database"
            ));
        }
        assert!(matches!(
            bridge_spool_error(&SpoolError::HighWatermarkBehind {
                high_watermark: 0,
                newest_sequence: 1,
            }),
            BridgeFrame::Error { code, .. } if code == "spool_corrupt"
        ));
        for error in [
            SpoolError::WalUnavailable("delete".into()),
            SpoolError::NetworkFilesystem("nfs".into()),
        ] {
            assert!(matches!(
                bridge_spool_error(&error),
                BridgeFrame::Error { code, .. } if code == "unsupported_storage"
            ));
        }
    }
}
