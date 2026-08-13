use std::{
    collections::BTreeMap,
    io::Read,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use aizu_core::{
    BoundedBridgeStderr, DesktopError as CoreDesktopError, DesktopState as CoreDesktopState,
    ReconnectDisposition, RemoteBridgeConsumer, RemoteConsumerError, SshCommandSpec,
    SshFailureCategory, SystemSshSource, validate_preflight_output,
};
use chrono::Utc;

use crate::{model::SourceStatus, store::RemoteSourceConfig};

const IO_CHUNK_BYTES: usize = 8 * 1_024;
const PREFLIGHT_LIMIT: usize = 128 * 1_024;
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(12);
const STOP_GRACE: Duration = Duration::from_secs(1);
const UPDATE_CHANNEL_CAPACITY: usize = 32;
const STDOUT_CHANNEL_CAPACITY: usize = 8;
const AGENT_PROBE_INTERVAL: Duration = Duration::from_secs(5);
pub const MAX_REMOTE_SOURCES: usize = 32;

#[derive(Debug)]
pub struct RemoteUpdate {
    pub host_alias: String,
    pub status: SourceStatus,
    pub detail: &'static str,
    pub replacement_identity: Option<uuid::Uuid>,
}

#[derive(Debug)]
pub struct RemoteAgentUpdate {
    pub host_alias: String,
    pub connection_epoch: u64,
    pub agents: Vec<aizu_core::AgentKind>,
}

pub struct RemoteFleet {
    jobs: BTreeMap<String, RemoteJob>,
    updates_tx: SyncSender<RemoteUpdate>,
    updates_rx: Receiver<RemoteUpdate>,
    agent_probes: BTreeMap<String, JoinHandle<()>>,
    agent_updates_tx: SyncSender<RemoteAgentUpdate>,
    agent_updates_rx: Receiver<RemoteAgentUpdate>,
    agent_probe_stop: Arc<AtomicBool>,
    last_agent_probe: Instant,
}

impl RemoteFleet {
    pub fn new() -> Self {
        let (updates_tx, updates_rx) = mpsc::sync_channel(UPDATE_CHANNEL_CAPACITY);
        let (agent_updates_tx, agent_updates_rx) = mpsc::sync_channel(UPDATE_CHANNEL_CAPACITY);
        Self {
            jobs: BTreeMap::new(),
            updates_tx,
            updates_rx,
            agent_probes: BTreeMap::new(),
            agent_updates_tx,
            agent_updates_rx,
            agent_probe_stop: Arc::new(AtomicBool::new(false)),
            last_agent_probe: Instant::now()
                .checked_sub(AGENT_PROBE_INTERVAL)
                .unwrap_or_else(Instant::now),
        }
    }

    pub fn sync(
        &mut self,
        configs: &[RemoteSourceConfig],
        desktop: &CoreDesktopState,
        connected_agent_sources: &[(String, u64)],
    ) {
        let removed: Vec<_> = self
            .jobs
            .keys()
            .filter(|alias| {
                !configs
                    .iter()
                    .any(|config| config.host_alias.as_str() == alias.as_str())
            })
            .cloned()
            .collect();
        for alias in removed {
            if let Some(job) = self.jobs.remove(&alias) {
                job.stop_and_join();
            }
        }

        for config in configs.iter().take(MAX_REMOTE_SOURCES) {
            let restart = self
                .jobs
                .get(&config.host_alias)
                .is_some_and(|job| job.generation != config.reconnect_generation || job.finished());
            if restart && let Some(job) = self.jobs.remove(&config.host_alias) {
                job.stop_and_join();
            }
            if !self.jobs.contains_key(&config.host_alias)
                && let Ok(job) =
                    RemoteJob::spawn(config.clone(), desktop.clone(), self.updates_tx.clone())
            {
                self.jobs.insert(config.host_alias.clone(), job);
            }
        }
        self.reap_agent_probes();
        if self.last_agent_probe.elapsed() >= AGENT_PROBE_INTERVAL {
            let mut spawned_probe = false;
            for (host_alias, connection_epoch) in
                connected_agent_sources.iter().take(MAX_REMOTE_SOURCES)
            {
                if !self.jobs.contains_key(host_alias) || self.agent_probes.contains_key(host_alias)
                {
                    continue;
                }
                let alias = host_alias.clone();
                let probe_alias = alias.clone();
                let connection_epoch = *connection_epoch;
                let updates = self.agent_updates_tx.clone();
                let stop = Arc::clone(&self.agent_probe_stop);
                if let Ok(thread) = thread::Builder::new()
                    .name(format!("aizu-ssh-agents-{}", safe_thread_name(&alias)))
                    .spawn(move || {
                        if let Some(agents) =
                            crate::ssh_connection_test::probe_remote_agents(&probe_alias, &stop)
                        {
                            let _ = updates.try_send(RemoteAgentUpdate {
                                host_alias: probe_alias,
                                connection_epoch,
                                agents,
                            });
                        }
                    })
                {
                    self.agent_probes.insert(alias, thread);
                    spawned_probe = true;
                }
            }
            if spawned_probe {
                self.last_agent_probe = Instant::now();
            }
        }
    }

    pub fn updates(&self) -> impl Iterator<Item = RemoteUpdate> + '_ {
        self.updates_rx.try_iter()
    }

    pub fn agent_updates(&self) -> impl Iterator<Item = RemoteAgentUpdate> + '_ {
        self.agent_updates_rx.try_iter()
    }

    fn reap_agent_probes(&mut self) {
        let finished: Vec<_> = self
            .agent_probes
            .iter()
            .filter(|(_, thread)| thread.is_finished())
            .map(|(alias, _)| alias.clone())
            .collect();
        for alias in finished {
            if let Some(thread) = self.agent_probes.remove(&alias) {
                let _ = thread.join();
            }
        }
    }
}

impl Drop for RemoteFleet {
    fn drop(&mut self) {
        self.agent_probe_stop.store(true, Ordering::Release);
        for (_, job) in std::mem::take(&mut self.jobs) {
            job.stop_and_join();
        }
        for (_, probe) in std::mem::take(&mut self.agent_probes) {
            let _ = probe.join();
        }
    }
}

struct RemoteJob {
    generation: u64,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RemoteJob {
    fn spawn(
        config: RemoteSourceConfig,
        desktop: CoreDesktopState,
        updates: SyncSender<RemoteUpdate>,
    ) -> std::io::Result<Self> {
        let generation = config.reconnect_generation;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let name = format!("aizu-ssh-{}", safe_thread_name(&config.host_alias));
        let thread = thread::Builder::new()
            .name(name)
            .spawn(move || run_remote(&config, &desktop, &updates, &worker_stop))?;
        Ok(Self {
            generation,
            stop,
            thread: Some(thread),
        })
    }

    fn finished(&self) -> bool {
        self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn stop_and_join(mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_remote(
    config: &RemoteSourceConfig,
    desktop: &CoreDesktopState,
    updates: &SyncSender<RemoteUpdate>,
    stop: &AtomicBool,
) {
    let Ok(source) = SystemSshSource::new("/usr/bin/ssh", config.host_alias.clone()) else {
        send_update(
            updates,
            &config.host_alias,
            SourceStatus::Error,
            "Invalid SSH alias",
        );
        return;
    };
    let mut attempt = 0;
    while !stop.load(Ordering::Acquire) {
        send_update(
            updates,
            &config.host_alias,
            SourceStatus::Reconnecting,
            "Connecting",
        );
        match bridge_once(&source, config, desktop, stop, updates) {
            BridgeResult::Stopped => return,
            BridgeResult::Retry { was_connected } => {
                if was_connected {
                    attempt = 0;
                }
                send_update(
                    updates,
                    &config.host_alias,
                    SourceStatus::Reconnecting,
                    "Connection interrupted; retrying",
                );
                if wait_for_stop(stop, source.reconnect_delay(attempt)) {
                    return;
                }
                attempt = attempt.saturating_add(1);
            }
            BridgeResult::UserAction(detail) => {
                let update = RemoteUpdate {
                    host_alias: config.host_alias.clone(),
                    status: SourceStatus::Error,
                    detail,
                    replacement_identity: None,
                };
                if !send_required_update(updates, update, stop) {
                    return;
                }
                while !stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(100));
                }
                return;
            }
            BridgeResult::IdentityChanged(identity) => {
                let update = RemoteUpdate {
                    host_alias: config.host_alias.clone(),
                    status: SourceStatus::Error,
                    detail: "Remote spool identity changed",
                    replacement_identity: Some(identity),
                };
                if !send_required_update(updates, update, stop) {
                    return;
                }
                while !stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(100));
                }
                return;
            }
        }
    }
}

enum BridgeResult {
    Stopped,
    Retry { was_connected: bool },
    UserAction(&'static str),
    IdentityChanged(uuid::Uuid),
}

#[allow(clippy::too_many_lines)]
fn bridge_once(
    source: &SystemSshSource,
    config: &RemoteSourceConfig,
    desktop: &CoreDesktopState,
    stop: &AtomicBool,
    updates: &SyncSender<RemoteUpdate>,
) -> BridgeResult {
    match run_preflight(&source.preflight_command(), stop) {
        Ok(output) if validate_preflight_output(&output).is_ok() => {}
        Ok(_) => return BridgeResult::UserAction("SSH config conflicts with Aizu"),
        Err(CaptureError::Stopped) => return BridgeResult::Stopped,
        Err(CaptureError::Failure(category)) if category.retry_automatically() => {
            return BridgeResult::Retry {
                was_connected: false,
            };
        }
        Err(CaptureError::Failure(category)) => {
            return BridgeResult::UserAction(ssh_detail(category));
        }
    }

    let source_key = format!("ssh:{}", config.host_alias);
    let Ok(mut consumer) = RemoteBridgeConsumer::open(
        desktop.clone(),
        &source_key,
        &config.local_label,
        Utc::now(),
    ) else {
        return BridgeResult::UserAction("Remote source state requires attention");
    };
    let Ok(command) = source.bridge_command(consumer.cursor()) else {
        return BridgeResult::UserAction("Remote cursor is invalid");
    };
    let Ok(mut child) = spawn_command(&command) else {
        return BridgeResult::Retry {
            was_connected: false,
        };
    };
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return BridgeResult::Retry {
            was_connected: false,
        };
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_child(&mut child);
        return BridgeResult::Retry {
            was_connected: false,
        };
    };
    let (stdout_tx, stdout_rx) = mpsc::sync_channel(STDOUT_CHANNEL_CAPACITY);
    let io_stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&io_stop);
    let stdout_thread = thread::spawn(move || read_chunks(stdout, &stdout_tx, &reader_stop));
    let captured_stderr = Arc::new(Mutex::new(BoundedBridgeStderr::default()));
    let stderr_capture = Arc::clone(&captured_stderr);
    let stderr_thread = thread::spawn(move || capture_stderr(stderr, &stderr_capture));
    let mut connected = false;
    let result = loop {
        if stop.load(Ordering::Acquire) {
            terminate_child(&mut child);
            break BridgeResult::Stopped;
        }
        if consumer.timeout_at(Utc::now()).is_some() {
            terminate_child(&mut child);
            break BridgeResult::Retry {
                was_connected: connected,
            };
        }
        match stdout_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ReaderMessage::Chunk(chunk)) => match consumer.push_stdout(&chunk, Utc::now()) {
                Ok(report) => {
                    if consumer.handshake_complete() && !connected {
                        connected = true;
                        send_update(
                            updates,
                            &config.host_alias,
                            SourceStatus::Connected,
                            "Connected",
                        );
                    }
                    if let Some(termination) = report.termination {
                        terminate_child(&mut child);
                        break match termination.disposition {
                            ReconnectDisposition::Retry => BridgeResult::Retry {
                                was_connected: connected,
                            },
                            ReconnectDisposition::UserActionRequired => {
                                BridgeResult::UserAction("Remote Aizu requires attention")
                            }
                        };
                    }
                }
                Err(error) if identity_change(&error).is_some() => {
                    terminate_child(&mut child);
                    break BridgeResult::IdentityChanged(
                        identity_change(&error).expect("identity error matched"),
                    );
                }
                Err(error) => {
                    terminate_child(&mut child);
                    break match error.reconnect_disposition() {
                        ReconnectDisposition::Retry => BridgeResult::Retry {
                            was_connected: connected,
                        },
                        ReconnectDisposition::UserActionRequired => {
                            BridgeResult::UserAction("Remote identity or protocol changed")
                        }
                    };
                }
            },
            Ok(ReaderMessage::Eof) => {
                let termination = consumer.finish_stdout().ok();
                break if termination.is_some_and(|item| {
                    item.disposition == ReconnectDisposition::UserActionRequired
                }) {
                    BridgeResult::UserAction("Remote Aizu requires attention")
                } else {
                    BridgeResult::Retry {
                        was_connected: connected,
                    }
                };
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break BridgeResult::Retry {
                    was_connected: connected,
                };
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if child.try_wait().ok().flatten().is_some() {
                    break BridgeResult::Retry {
                        was_connected: connected,
                    };
                }
            }
        }
    };
    terminate_child(&mut child);
    io_stop.store(true, Ordering::Release);
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    if matches!(result, BridgeResult::Retry { .. })
        && let Ok(stderr) = captured_stderr.lock()
        && let Some(category) = retry_stderr_override(&stderr)
    {
        return BridgeResult::UserAction(ssh_detail(category));
    }
    result
}

fn identity_change(error: &RemoteConsumerError) -> Option<uuid::Uuid> {
    match error {
        RemoteConsumerError::Desktop(CoreDesktopError::SourceIdentityChanged {
            actual, ..
        })
        | RemoteConsumerError::Protocol(
            aizu_core::protocol::ProtocolError::SourceIdentityMismatch { actual, .. },
        ) => Some(*actual),
        _ => None,
    }
}

fn retry_stderr_override(stderr: &BoundedBridgeStderr) -> Option<SshFailureCategory> {
    if stderr.bytes_seen() == 0 {
        return None;
    }
    let category = stderr.classify();
    if category.retry_automatically() {
        None
    } else {
        Some(category)
    }
}

enum ReaderMessage {
    Chunk(Vec<u8>),
    Eof,
}

fn read_chunks(mut reader: impl Read, sender: &SyncSender<ReaderMessage>, stop: &AtomicBool) {
    loop {
        let mut buffer = vec![0; IO_CHUNK_BYTES];
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => {
                let _ = send_reader_message(sender, ReaderMessage::Eof, stop);
                return;
            }
            Ok(count) => {
                buffer.truncate(count);
                if !send_reader_message(sender, ReaderMessage::Chunk(buffer), stop) {
                    return;
                }
            }
        }
    }
}

fn send_reader_message(
    sender: &SyncSender<ReaderMessage>,
    mut message: ReaderMessage,
    stop: &AtomicBool,
) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(message) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                message = returned;
                thread::sleep(Duration::from_millis(10));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn capture_stderr(mut reader: impl Read, capture: &Mutex<BoundedBridgeStderr>) {
    let mut buffer = [0; 1024];
    while let Ok(count) = reader.read(&mut buffer) {
        if count == 0 {
            break;
        }
        if let Ok(mut capture) = capture.lock() {
            capture.push(&buffer[..count]);
        }
    }
}

enum CaptureError {
    Stopped,
    Failure(SshFailureCategory),
}

fn run_preflight(command: &SshCommandSpec, stop: &AtomicBool) -> Result<Vec<u8>, CaptureError> {
    let mut child = spawn_command(command)
        .map_err(|_| CaptureError::Failure(SshFailureCategory::RemoteFailure))?;
    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return Err(CaptureError::Failure(SshFailureCategory::RemoteFailure));
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_child(&mut child);
        return Err(CaptureError::Failure(SshFailureCategory::RemoteFailure));
    };
    let stdout_capture = Arc::new(Mutex::new(Vec::new()));
    let output = Arc::clone(&stdout_capture);
    let stdout_thread = thread::spawn(move || capture_bytes(stdout, &output, PREFLIGHT_LIMIT));
    let stderr_capture = Arc::new(Mutex::new(BoundedBridgeStderr::default()));
    let errors = Arc::clone(&stderr_capture);
    let stderr_thread = thread::spawn(move || capture_stderr(stderr, &errors));
    let started = Instant::now();
    let result = loop {
        if stop.load(Ordering::Acquire) {
            terminate_child(&mut child);
            break Err(CaptureError::Stopped);
        }
        if started.elapsed() >= PREFLIGHT_TIMEOUT {
            terminate_child(&mut child);
            break Err(CaptureError::Failure(SshFailureCategory::RetryableNetwork));
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(_)) => {
                let category = stderr_capture
                    .lock()
                    .map_or(SshFailureCategory::RemoteFailure, |value| value.classify());
                break Err(CaptureError::Failure(category));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => break Err(CaptureError::Failure(SshFailureCategory::RemoteFailure)),
        }
    };
    terminate_child(&mut child);
    let _ = stdout_thread.join();
    let _ = stderr_thread.join();
    result.map(|()| {
        stdout_capture
            .lock()
            .map_or_else(|_| Vec::new(), |value| value.clone())
    })
}

fn capture_bytes(mut reader: impl Read, capture: &Mutex<Vec<u8>>, maximum: usize) {
    let mut buffer = [0; 1024];
    while let Ok(count) = reader.read(&mut buffer) {
        if count == 0 {
            break;
        }
        if let Ok(mut capture) = capture.lock() {
            let remaining = maximum.saturating_add(1).saturating_sub(capture.len());
            capture.extend_from_slice(&buffer[..count.min(remaining)]);
        }
    }
}

fn spawn_command(spec: &SshCommandSpec) -> std::io::Result<Child> {
    Command::new(&spec.program)
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
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

fn wait_for_stop(stop: &AtomicBool, duration: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < duration {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

fn send_update(
    sender: &SyncSender<RemoteUpdate>,
    alias: &str,
    status: SourceStatus,
    detail: &'static str,
) {
    let _ = sender.try_send(RemoteUpdate {
        host_alias: alias.to_owned(),
        status,
        detail,
        replacement_identity: None,
    });
}

fn send_required_update(
    sender: &SyncSender<RemoteUpdate>,
    mut update: RemoteUpdate,
    stop: &AtomicBool,
) -> bool {
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        match sender.try_send(update) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                update = returned;
                thread::sleep(Duration::from_millis(10));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

const fn ssh_detail(category: SshFailureCategory) -> &'static str {
    match category {
        SshFailureCategory::RetryableNetwork => "Network unavailable",
        SshFailureCategory::AuthenticationRequired => "SSH authentication required",
        SshFailureCategory::HostVerificationFailed => "SSH host verification failed",
        SshFailureCategory::MissingRemoteCli => "Aizu CLI is missing on the remote host",
        SshFailureCategory::ConfigurationConflict => "SSH config conflicts with Aizu",
        SshFailureCategory::RemoteFailure => "Remote SSH command failed",
    }
}

fn safe_thread_name(alias: &str) -> String {
    alias
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(24)
        .collect()
}

#[cfg(test)]
mod tests {
    use aizu_core::{
        BoundedBridgeStderr, DesktopError as CoreDesktopError, RemoteConsumerError,
        SshFailureCategory, protocol::ProtocolError,
    };
    use uuid::Uuid;

    use super::{identity_change, retry_stderr_override};

    #[test]
    fn both_identity_error_layers_reach_explicit_confirmation() {
        let expected = Uuid::parse_str("7a4881c7-c667-47dc-b544-f98a46ab17ca").unwrap();
        let actual = Uuid::parse_str("5a4881c7-c667-47dc-b544-f98a46ab17ca").unwrap();
        let protocol = RemoteConsumerError::Protocol(ProtocolError::SourceIdentityMismatch {
            expected,
            actual,
        });
        let desktop = RemoteConsumerError::Desktop(CoreDesktopError::SourceIdentityChanged {
            source_key: "ssh:build".to_owned(),
            expected,
            actual,
        });

        assert_eq!(identity_change(&protocol), Some(actual));
        assert_eq!(identity_change(&desktop), Some(actual));
    }

    #[test]
    fn empty_or_retryable_stderr_preserves_automatic_reconnect() {
        let mut stderr = BoundedBridgeStderr::default();
        assert_eq!(retry_stderr_override(&stderr), None);

        stderr.push(b"connection reset by peer");
        assert_eq!(retry_stderr_override(&stderr), None);

        let mut terminal = BoundedBridgeStderr::default();
        terminal.push(b"permission denied");
        assert_eq!(
            retry_stderr_override(&terminal),
            Some(SshFailureCategory::AuthenticationRequired)
        );
    }
}
