use std::{
    io::Read,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use aizu_core::{
    PROTOCOL_VERSION, SshCommandSpec, SshFailureCategory, SystemSshSource, classify_ssh_failure,
    validate_preflight_output,
};
use serde::Deserialize;

use crate::model::{SshConnectionTestResult, SshConnectionTestStatus};

const SYSTEM_SSH: &str = "/usr/bin/ssh";
const REMOTE_VERSION_COMMAND: &str = "exec \"$HOME/.local/bin/aizu\" version --json";
const REMOTE_AGENTS_COMMAND: &str = "exec \"$HOME/.local/bin/aizu\" agents --json";
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_GRACE: Duration = Duration::from_secs(1);
const MAX_STDOUT_BYTES: usize = 16 * 1024;
const MAX_STDERR_BYTES: usize = 8 * 1024;

#[derive(Debug, Deserialize)]
struct RemoteVersion {
    application: String,
    protocol: u32,
}

#[derive(Debug, Deserialize)]
struct RemoteAgentProcess {
    agent: aizu_core::AgentKind,
}

#[derive(Debug, Deserialize)]
struct RemoteAgents {
    application: String,
    agents: Vec<RemoteAgentProcess>,
}

struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    output_truncated: bool,
}

enum CaptureError {
    Spawn,
    Stopped,
    Timeout,
    Wait,
}

/// Tests SSH configuration and Aizu compatibility without registering or
/// mutating a source. All user-visible diagnostics are fixed, privacy-safe text.
#[must_use]
pub fn test_connection(host_alias: &str) -> SshConnectionTestResult {
    let Ok(source) = strict_source(Path::new(SYSTEM_SSH), host_alias) else {
        return result(
            SshConnectionTestStatus::InvalidAlias,
            "Enter a valid SSH config alias.",
            false,
            false,
            false,
            None,
        );
    };

    let preflight = match run_bounded(&source.preflight_command(), PREFLIGHT_TIMEOUT) {
        Ok(output) if output.status.success() && !output.output_truncated => output,
        Ok(output) if !output.status.success() => {
            return failure_result(classify_ssh_failure(&output.stderr), false);
        }
        Ok(_) | Err(CaptureError::Spawn | CaptureError::Stopped | CaptureError::Wait) => {
            return configuration_failure();
        }
        Err(CaptureError::Timeout) => return timed_out(false),
    };
    if validate_preflight_output(&preflight.stdout).is_err() {
        return configuration_failure();
    }

    let command = version_command(&source);
    let output = match run_bounded(&command, CONNECTION_TIMEOUT) {
        Ok(output) => output,
        Err(CaptureError::Timeout) => return timed_out(true),
        Err(CaptureError::Spawn | CaptureError::Stopped | CaptureError::Wait) => {
            return result(
                SshConnectionTestStatus::RemoteFailure,
                "The SSH connection test could not be completed.",
                true,
                false,
                false,
                None,
            );
        }
    };
    if !output.status.success() {
        return failure_result(classify_ssh_failure(&output.stderr), true);
    }
    if output.output_truncated {
        return invalid_remote_response();
    }

    parse_remote_version(&output.stdout)
}

pub fn validate_alias(host_alias: &str) -> Result<(), ()> {
    strict_source(Path::new(SYSTEM_SSH), host_alias)
        .map(|_| ())
        .map_err(|_| ())
}

/// Returns a bounded, privacy-safe process snapshot from an already configured
/// SSH source. Probe failures are diagnostic-only and never expose SSH output.
pub fn probe_remote_agents(
    host_alias: &str,
    stop: &AtomicBool,
) -> Option<Vec<aizu_core::AgentKind>> {
    let source = strict_source(Path::new(SYSTEM_SSH), host_alias).ok()?;
    let output = run_bounded_with_stop(
        &remote_command(&source, REMOTE_AGENTS_COMMAND),
        CONNECTION_TIMEOUT,
        Some(stop),
    )
    .ok()?;
    if !output.status.success() || output.output_truncated {
        return None;
    }
    parse_remote_agents(&output.stdout)
}

fn strict_source(
    executable: &Path,
    host_alias: &str,
) -> Result<SystemSshSource, aizu_core::SshConfigurationError> {
    let source = SystemSshSource::new(executable, host_alias)?;
    // Aizu accepts a concrete Host alias, not a destination, wildcard, or
    // option fragment. The alias remains a separate argv element regardless.
    if !host_alias
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        || !host_alias.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        return Err(aizu_core::SshConfigurationError::OptionLikeHostAlias);
    }
    Ok(source)
}

fn version_command(source: &SystemSshSource) -> SshCommandSpec {
    remote_command(source, REMOTE_VERSION_COMMAND)
}

fn remote_command(source: &SystemSshSource, command: &str) -> SshCommandSpec {
    let args = [
        "-T",
        "-n",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ConnectionAttempts=1",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "PermitLocalCommand=no",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain([source.host_alias().to_owned(), command.to_owned()])
    .collect();
    SshCommandSpec {
        program: source.executable().to_owned(),
        args,
    }
}

fn parse_remote_agents(stdout: &[u8]) -> Option<Vec<aizu_core::AgentKind>> {
    let report = serde_json::from_slice::<RemoteAgents>(stdout).ok()?;
    if report.application != env!("CARGO_PKG_VERSION")
        || report.agents.len() > aizu_core::MAX_PROCESS_SNAPSHOT_ENTRIES
    {
        return None;
    }
    Some(report.agents.into_iter().map(|entry| entry.agent).collect())
}

fn parse_remote_version(stdout: &[u8]) -> SshConnectionTestResult {
    let Ok(version) = serde_json::from_slice::<RemoteVersion>(stdout) else {
        return invalid_remote_response();
    };
    if !safe_version(&version.application) {
        return invalid_remote_response();
    }
    if version.protocol != PROTOCOL_VERSION {
        return result(
            SshConnectionTestStatus::IncompatibleProtocol,
            "SSH connected, but the remote Aizu protocol is incompatible.",
            true,
            true,
            false,
            Some(version.application),
        );
    }
    result(
        SshConnectionTestStatus::Compatible,
        "SSH connected and the remote Aizu CLI is compatible.",
        true,
        true,
        true,
        Some(version.application),
    )
}

fn safe_version(version: &str) -> bool {
    if version.chars().count() > 64
        || !version.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '+' | '-')
        })
    {
        return false;
    }
    let core = version
        .split_once(['-', '+'])
        .map_or(version, |(core, _)| core);
    let mut components = core.split('.');
    components.next().is_some_and(|part| {
        !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
    }) && components.next().is_some_and(|part| {
        !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
    }) && components.next().is_some_and(|part| {
        !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
    }) && components.next().is_none()
}

fn invalid_remote_response() -> SshConnectionTestResult {
    result(
        SshConnectionTestStatus::RemoteFailure,
        "SSH connected, but the remote Aizu CLI returned an invalid response.",
        true,
        true,
        false,
        None,
    )
}

fn configuration_failure() -> SshConnectionTestResult {
    result(
        SshConnectionTestStatus::ConfigurationError,
        "The SSH config alias could not be resolved safely.",
        false,
        false,
        false,
        None,
    )
}

fn timed_out(config_resolved: bool) -> SshConnectionTestResult {
    result(
        SshConnectionTestStatus::TimedOut,
        "The SSH connection test timed out.",
        config_resolved,
        false,
        false,
        None,
    )
}

fn failure_result(category: SshFailureCategory, config_resolved: bool) -> SshConnectionTestResult {
    let (status, message, reachable) = match category {
        SshFailureCategory::RetryableNetwork => (
            SshConnectionTestStatus::NetworkUnavailable,
            "The SSH host could not be reached.",
            false,
        ),
        SshFailureCategory::AuthenticationRequired => (
            SshConnectionTestStatus::AuthenticationRequired,
            "SSH authentication is required or was rejected.",
            false,
        ),
        SshFailureCategory::HostVerificationFailed => (
            SshConnectionTestStatus::HostVerificationFailed,
            "SSH host verification failed.",
            false,
        ),
        SshFailureCategory::MissingRemoteCli => (
            SshConnectionTestStatus::MissingRemoteCli,
            "SSH connected, but the Aizu CLI is not installed on the remote host.",
            true,
        ),
        SshFailureCategory::ConfigurationConflict => (
            SshConnectionTestStatus::ConfigurationError,
            "The SSH config conflicts with the Aizu connection command.",
            false,
        ),
        SshFailureCategory::RemoteFailure => (
            SshConnectionTestStatus::RemoteFailure,
            "The remote SSH command failed.",
            false,
        ),
    };
    result(status, message, config_resolved, reachable, false, None)
}

fn result(
    status: SshConnectionTestStatus,
    message: &str,
    config_resolved: bool,
    reachable: bool,
    protocol_compatible: bool,
    remote_version: Option<String>,
) -> SshConnectionTestResult {
    SshConnectionTestResult {
        status,
        message: message.to_owned(),
        config_resolved,
        reachable,
        protocol_compatible,
        remote_version,
    }
}

fn run_bounded(spec: &SshCommandSpec, timeout: Duration) -> Result<CapturedOutput, CaptureError> {
    run_bounded_with_stop(spec, timeout, None)
}

fn run_bounded_with_stop(
    spec: &SshCommandSpec,
    timeout: Duration,
    stop: Option<&AtomicBool>,
) -> Result<CapturedOutput, CaptureError> {
    let mut child = Command::new(&spec.program)
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| CaptureError::Spawn)?;
    let stdout = child.stdout.take().ok_or(CaptureError::Spawn)?;
    let stderr = child.stderr.take().ok_or(CaptureError::Spawn)?;
    let stdout_capture = Arc::new(Mutex::new((Vec::new(), false)));
    let stderr_capture = Arc::new(Mutex::new((Vec::new(), false)));
    let stdout_thread = capture_thread(stdout, Arc::clone(&stdout_capture), MAX_STDOUT_BYTES);
    let stderr_thread = capture_thread(stderr, Arc::clone(&stderr_capture), MAX_STDERR_BYTES);

    let started = Instant::now();
    let status = loop {
        if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
            terminate_child(&mut child);
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err(CaptureError::Stopped);
        }
        if started.elapsed() >= timeout {
            terminate_child(&mut child);
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err(CaptureError::Timeout);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                terminate_child(&mut child);
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(CaptureError::Wait);
            }
        }
    };
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    let (stdout, stdout_truncated) = captured(&stdout_capture);
    let (stderr, stderr_truncated) = captured(&stderr_capture);
    Ok(CapturedOutput {
        status,
        stdout,
        stderr,
        output_truncated: stdout_truncated || stderr_truncated,
    })
}

fn capture_thread(
    mut reader: impl Read + Send + 'static,
    capture: Arc<Mutex<(Vec<u8>, bool)>>,
    maximum: usize,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0; 1024];
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 {
                return;
            }
            if let Ok(mut capture) = capture.lock() {
                let remaining = maximum.saturating_sub(capture.0.len());
                capture.0.extend_from_slice(&buffer[..count.min(remaining)]);
                capture.1 |= count > remaining;
            }
        }
    })
}

fn captured(capture: &Mutex<(Vec<u8>, bool)>) -> (Vec<u8>, bool) {
    capture
        .lock()
        .map_or_else(|_| (Vec::new(), true), |value| value.clone())
}

fn terminate_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    #[cfg(unix)]
    {
        use nix::{
            sys::signal::{Signal, kill},
            unistd::Pid,
        };
        let _ = kill(
            Pid::from_raw(i32::try_from(child.id()).unwrap_or(i32::MAX)),
            Signal::SIGTERM,
        );
    }
    let started = Instant::now();
    while started.elapsed() < STOP_GRACE {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use aizu_core::{PROTOCOL_VERSION, SystemSshSource};

    use super::{
        CaptureError, REMOTE_AGENTS_COMMAND, parse_remote_agents, parse_remote_version,
        remote_command, run_bounded_with_stop, strict_source, version_command,
    };
    use crate::model::SshConnectionTestStatus;

    #[test]
    fn strict_alias_rejects_destinations_and_shell_metacharacters() {
        for alias in [
            "user@example.com",
            "host;id",
            "*.example.com",
            "-oProxyCommand=x",
        ] {
            assert!(strict_source(Path::new("/usr/bin/ssh"), alias).is_err());
        }
        assert!(strict_source(Path::new("/usr/bin/ssh"), "build-host_2.example").is_ok());
    }

    #[test]
    fn version_probe_uses_system_ssh_and_keeps_alias_in_its_own_argument() {
        let source = SystemSshSource::new("/usr/bin/ssh", "build-host").unwrap();
        let command = version_command(&source);

        assert_eq!(command.program, Path::new("/usr/bin/ssh"));
        assert!(
            command
                .args
                .iter()
                .any(|arg| arg == "StrictHostKeyChecking=yes")
        );
        assert_eq!(command.args[command.args.len() - 2], "build-host");
        assert_eq!(
            command.args.last().map(String::as_str),
            Some("exec \"$HOME/.local/bin/aizu\" version --json")
        );
    }

    #[test]
    fn agent_probe_is_fixed_and_accepts_only_bounded_current_reports() {
        let source = SystemSshSource::new("/usr/bin/ssh", "build-host").unwrap();
        let command = remote_command(&source, REMOTE_AGENTS_COMMAND);
        assert_eq!(command.args[command.args.len() - 2], "build-host");
        assert_eq!(
            command.args.last().map(String::as_str),
            Some("exec \"$HOME/.local/bin/aizu\" agents --json")
        );

        let current = format!(
            r#"{{"application":"{}","agents":[{{"agent":"codex"}},{{"agent":"claude-code"}}]}}"#,
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(
            parse_remote_agents(current.as_bytes()),
            Some(vec![
                aizu_core::AgentKind::Codex,
                aizu_core::AgentKind::ClaudeCode
            ])
        );
        assert!(parse_remote_agents(br#"{"application":"0.0.1","agents":[]}"#).is_none());
        assert!(parse_remote_agents(b"/home/example/private").is_none());
    }

    #[test]
    fn remote_version_response_requires_current_protocol() {
        let compatible = parse_remote_version(
            format!(r#"{{"application":"0.1.0","protocol":{PROTOCOL_VERSION}}}"#).as_bytes(),
        );
        assert_eq!(compatible.status, SshConnectionTestStatus::Compatible);
        assert!(compatible.config_resolved);
        assert!(compatible.reachable);
        assert!(compatible.protocol_compatible);
        assert_eq!(compatible.remote_version.as_deref(), Some("0.1.0"));

        let incompatible = parse_remote_version(br#"{"application":"0.1.0","protocol":999}"#);
        assert_eq!(
            incompatible.status,
            SshConnectionTestStatus::IncompatibleProtocol
        );
        assert!(incompatible.reachable);
        assert!(!incompatible.protocol_compatible);
    }

    #[test]
    fn invalid_remote_output_never_reaches_the_ui() {
        let invalid = parse_remote_version(b"/Users/example/private/path");
        assert_eq!(invalid.status, SshConnectionTestStatus::RemoteFailure);
        assert!(!invalid.message.contains("/Users"));
        assert!(invalid.remote_version.is_none());

        let disguised_secret = parse_remote_version(
            format!(r#"{{"application":"superSecretValue","protocol":{PROTOCOL_VERSION}}}"#)
                .as_bytes(),
        );
        assert_eq!(
            disguised_secret.status,
            SshConnectionTestStatus::RemoteFailure
        );
        assert!(disguised_secret.remote_version.is_none());
    }

    #[test]
    fn agent_probe_process_is_cancelled_without_waiting_for_timeout() {
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&stop);
        let signal_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            signal.store(true, Ordering::Release);
        });
        let command = aizu_core::SshCommandSpec {
            program: Path::new("/bin/sleep").to_owned(),
            args: vec!["5".to_owned()],
        };
        let started = Instant::now();
        assert!(matches!(
            run_bounded_with_stop(&command, Duration::from_secs(5), Some(&stop)),
            Err(CaptureError::Stopped)
        ));
        signal_thread.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
