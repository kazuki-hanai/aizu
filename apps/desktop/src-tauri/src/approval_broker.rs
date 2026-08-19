use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use aizu_core::{
    ApprovalDecision, LocalApprovalRequest, LocalApprovalResponse, MAX_LOCAL_APPROVAL_FRAME_BYTES,
    parse_strict_json_value,
};
use tauri::{AppHandle, Manager, Wry};
use thiserror::Error;

use crate::{
    model::{AgentKind, ApprovalPresentation, Notification, NotificationDelivery, Preferences},
    state::DesktopState,
};

const MAX_PENDING_APPROVALS: usize = 1;
const MAX_CONNECTION_HANDLERS: usize = 4;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(2);
const DECISION_TIMEOUT: Duration = Duration::from_secs(45);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

struct PendingApproval {
    request_id: uuid::Uuid,
    sender: SyncSender<LocalApprovalResponse>,
    frontend_rendered: bool,
    window_shown: bool,
}

impl PendingApproval {
    fn presented(&self) -> bool {
        self.frontend_rendered && self.window_shown
    }
}

#[derive(Default)]
struct ApprovalRegistry {
    pending: Mutex<BTreeMap<i32, PendingApproval>>,
    next_banner_id: AtomicI32,
}

impl ApprovalRegistry {
    fn register(
        &self,
        request_id: uuid::Uuid,
        sender: SyncSender<LocalApprovalResponse>,
    ) -> Result<i32, BrokerError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| BrokerError::StateUnavailable)?;
        if pending.len() >= MAX_PENDING_APPROVALS {
            return Err(BrokerError::Busy);
        }
        let id = self
            .next_banner_id
            .fetch_sub(1, Ordering::AcqRel)
            .saturating_sub(1);
        pending.insert(
            id,
            PendingApproval {
                request_id,
                sender,
                frontend_rendered: false,
                window_shown: false,
            },
        );
        Ok(id)
    }

    fn mark_frontend_rendered(&self, id: i32) -> Result<bool, BrokerError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| BrokerError::StateUnavailable)?;
        let Some(pending) = pending.get_mut(&id) else {
            return Ok(false);
        };
        pending.frontend_rendered = true;
        Ok(true)
    }

    fn mark_window_shown(&self, id: i32) -> Result<bool, BrokerError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| BrokerError::StateUnavailable)?;
        let Some(pending) = pending.get_mut(&id) else {
            return Ok(false);
        };
        pending.window_shown = true;
        Ok(true)
    }

    fn complete(&self, id: i32, decision: ApprovalDecision) -> Result<bool, BrokerError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| BrokerError::StateUnavailable)?;
        if !pending.get(&id).is_some_and(PendingApproval::presented) {
            return Ok(false);
        }
        let pending = pending.remove(&id);
        let Some(pending) = pending else {
            return Ok(false);
        };
        let _ = pending.sender.send(LocalApprovalResponse::Decision {
            request_id: pending.request_id,
            decision,
        });
        Ok(true)
    }

    fn cancel(&self, id: i32) -> Result<bool, BrokerError> {
        let pending = self
            .pending
            .lock()
            .map_err(|_| BrokerError::StateUnavailable)?
            .remove(&id);
        let Some(pending) = pending else {
            return Ok(false);
        };
        let _ = pending.sender.send(LocalApprovalResponse::Unavailable {
            request_id: pending.request_id,
            presented: pending.presented(),
        });
        Ok(true)
    }

    fn expire(&self, id: i32, request_id: uuid::Uuid) -> Result<Option<bool>, BrokerError> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| BrokerError::StateUnavailable)?;
        if pending.get(&id).map(|entry| entry.request_id) != Some(request_id) {
            return Ok(None);
        }
        Ok(pending.remove(&id).map(|pending| pending.presented()))
    }

    fn cancel_all(&self) -> Vec<i32> {
        let pending = self
            .pending
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        let ids = pending.keys().copied().collect();
        for (_, pending) in pending {
            let _ = pending.sender.send(LocalApprovalResponse::Unavailable {
                request_id: pending.request_id,
                presented: pending.presented(),
            });
        }
        ids
    }
}

pub struct ApprovalBroker {
    registry: Arc<ApprovalRegistry>,
    stop: Arc<AtomicBool>,
    listener: Mutex<Option<JoinHandle<()>>>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    socket_path: Option<PathBuf>,
}

impl ApprovalBroker {
    pub fn start(app: AppHandle<Wry>, socket_path: &Path) -> Self {
        let socket_path = socket_path.to_path_buf();
        let registry = Arc::new(ApprovalRegistry::default());
        let stop = Arc::new(AtomicBool::new(false));
        let handlers = Arc::new(Mutex::new(Vec::new()));
        let active_handlers = Arc::new(AtomicUsize::new(0));
        let listener = bind_listener(&socket_path).ok();
        let bound_path = listener.as_ref().map(|_| socket_path.clone());
        let listener_thread = listener.and_then(|listener| {
            let registry = Arc::clone(&registry);
            let stop = Arc::clone(&stop);
            let handlers_for_thread = Arc::clone(&handlers);
            thread::Builder::new()
                .name("aizu-approval-listener".to_owned())
                .spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        reap_handlers(&handlers_for_thread);
                        match listener.accept() {
                            Ok((stream, _)) => {
                                if active_handlers.load(Ordering::Acquire)
                                    >= MAX_CONNECTION_HANDLERS
                                {
                                    continue;
                                }
                                active_handlers.fetch_add(1, Ordering::AcqRel);
                                let guard = ActiveHandlerGuard(Arc::clone(&active_handlers));
                                let app = app.clone();
                                let registry = Arc::clone(&registry);
                                let stop = Arc::clone(&stop);
                                let handle = thread::Builder::new()
                                    .name("aizu-approval-request".to_owned())
                                    .spawn(move || {
                                        let _guard = guard;
                                        handle_connection(&app, &registry, &stop, stream);
                                    });
                                if let Ok(handle) = handle
                                    && let Ok(mut handlers) = handlers_for_thread.lock()
                                {
                                    handlers.push(handle);
                                }
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                thread::sleep(Duration::from_millis(25));
                            }
                            Err(_) => thread::sleep(Duration::from_millis(100)),
                        }
                    }
                    reap_handlers(&handlers_for_thread);
                })
                .ok()
        });
        Self {
            registry,
            stop,
            listener: Mutex::new(listener_thread),
            handlers,
            socket_path: bound_path,
        }
    }

    pub fn decide(&self, id: i32, decision: ApprovalDecision) -> Result<bool, BrokerError> {
        self.registry.complete(id, decision)
    }

    pub fn mark_frontend_rendered(&self, id: i32) -> Result<bool, BrokerError> {
        self.registry.mark_frontend_rendered(id)
    }

    pub fn mark_window_shown(&self, id: i32) -> Result<bool, BrokerError> {
        self.registry.mark_window_shown(id)
    }

    pub fn cancel(&self, id: i32) -> Result<bool, BrokerError> {
        self.registry.cancel(id)
    }

    pub fn cancel_all(&self) -> Vec<i32> {
        self.registry.cancel_all()
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.registry.cancel_all();
        if let Ok(mut listener) = self.listener.lock()
            && let Some(listener) = listener.take()
        {
            let _ = listener.join();
        }
        if let Ok(mut handlers) = self.handlers.lock() {
            for handler in handlers.drain(..) {
                let _ = handler.join();
            }
        }
        if let Some(path) = self.socket_path.as_ref() {
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for ApprovalBroker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct ActiveHandlerGuard(Arc<AtomicUsize>);

impl Drop for ActiveHandlerGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle_connection(
    app: &AppHandle<Wry>,
    registry: &ApprovalRegistry,
    stop: &AtomicBool,
    mut stream: std::os::unix::net::UnixStream,
) {
    let Ok(request) = read_request(&mut stream) else {
        return;
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let Ok(id) = registry.register(request.request_id, sender) else {
        let _ = write_response(
            &mut stream,
            LocalApprovalResponse::Unavailable {
                request_id: request.request_id,
                presented: false,
            },
        );
        return;
    };
    let preferences = if let Ok(state) = app.state::<DesktopState>().lock() {
        state.view().preferences
    } else {
        let _ = registry.expire(id, request.request_id);
        let _ = write_response(
            &mut stream,
            LocalApprovalResponse::Unavailable {
                request_id: request.request_id,
                presented: false,
            },
        );
        return;
    };
    if !preferences.command_approvals_enabled {
        let _ = registry.expire(id, request.request_id);
        let _ = write_response(
            &mut stream,
            LocalApprovalResponse::Unavailable {
                request_id: request.request_id,
                presented: false,
            },
        );
        return;
    }
    let notification = approval_notification(&request, &preferences, id);
    if crate::banner::show(app, &notification).is_err() {
        let _ = crate::banner::dismiss(app, id);
        let _ = registry.expire(id, request.request_id);
        let _ = write_response(
            &mut stream,
            LocalApprovalResponse::Unavailable {
                request_id: request.request_id,
                presented: false,
            },
        );
        return;
    }
    let deadline = Instant::now() + DECISION_TIMEOUT;
    let response = loop {
        if stop.load(Ordering::Acquire) || Instant::now() >= deadline {
            break None;
        }
        match receiver.recv_timeout(STOP_POLL_INTERVAL) {
            Ok(response) => break Some(response),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break None,
        }
    };
    let response = response.unwrap_or_else(|| expiry_response(registry, id, request.request_id));
    let _ = registry.expire(id, request.request_id);
    // The handler also owns cleanup so a cancellation that races banner
    // presentation cannot leave an orphaned approval visible.
    let _ = crate::banner::dismiss(app, id);
    let _ = write_response(&mut stream, response);
}

fn expiry_response(
    registry: &ApprovalRegistry,
    id: i32,
    request_id: uuid::Uuid,
) -> LocalApprovalResponse {
    let presented = registry
        .expire(id, request_id)
        .ok()
        .flatten()
        .unwrap_or(false);
    LocalApprovalResponse::Unavailable {
        request_id,
        presented,
    }
}

fn approval_notification(
    request: &LocalApprovalRequest,
    preferences: &Preferences,
    id: i32,
) -> Notification {
    let japanese = preferences.language.prefers_japanese();
    let agent = match request.agent {
        aizu_core::AgentKind::Codex => AgentKind::Codex,
        aizu_core::AgentKind::ClaudeCode => AgentKind::ClaudeCode,
    };
    let agent_label = match agent {
        AgentKind::Codex => "Codex",
        AgentKind::ClaudeCode => "Claude Code",
    };
    Notification {
        id,
        title: if japanese {
            format!("{agent_label} が実行許可を求めています")
        } else {
            format!("{agent_label} requests permission")
        },
        body: if japanese {
            "内容を確認して選択してください。"
        } else {
            "Review the exact command before choosing."
        }
        .to_owned(),
        sound: preferences
            .sound_enabled
            .then_some(preferences.notification_sound),
        delivery: NotificationDelivery::AizuBanner,
        language: preferences.language,
        text_size: preferences.text_size,
        can_activate_terminal: false,
        approval: Some(ApprovalPresentation {
            agent,
            tool_name: request.tool_name.clone(),
            command: request.command.clone(),
        }),
        activation: None,
    }
}

fn read_request(
    stream: &mut std::os::unix::net::UnixStream,
) -> Result<LocalApprovalRequest, BrokerError> {
    stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut frame = Vec::new();
    let mut byte = [0_u8; 1];
    while frame.len() <= MAX_LOCAL_APPROVAL_FRAME_BYTES {
        match stream.read(&mut byte)? {
            0 => return Err(BrokerError::InvalidFrame),
            1 if byte[0] == b'\n' => break,
            1 => frame.push(byte[0]),
            _ => unreachable!(),
        }
    }
    if frame.is_empty() || frame.len() > MAX_LOCAL_APPROVAL_FRAME_BYTES {
        return Err(BrokerError::InvalidFrame);
    }
    let value = parse_strict_json_value(&frame, MAX_LOCAL_APPROVAL_FRAME_BYTES)
        .map_err(|_| BrokerError::InvalidFrame)?;
    let request: LocalApprovalRequest =
        serde_json::from_value(value).map_err(|_| BrokerError::InvalidFrame)?;
    request.validate().map_err(|_| BrokerError::InvalidFrame)?;
    Ok(request)
}

fn write_response(
    stream: &mut std::os::unix::net::UnixStream,
    response: LocalApprovalResponse,
) -> Result<(), BrokerError> {
    serde_json::to_writer(&mut *stream, &response).map_err(|_| BrokerError::InvalidFrame)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn bind_listener(path: &Path) -> io::Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "approval socket has no parent")
    })?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_socket() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "approval socket path is not a socket",
            ));
        }
        Ok(_) => match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "approval broker is already running",
                ));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                fs::remove_file(path)?;
            }
            Err(error) => return Err(error),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = std::os::unix::net::UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn reap_handlers(handlers: &Mutex<Vec<JoinHandle<()>>>) {
    let Ok(mut handlers) = handlers.lock() else {
        return;
    };
    let mut index = 0;
    while index < handlers.len() {
        if handlers[index].is_finished() {
            let handle = handlers.swap_remove(index);
            let _ = handle.join();
        } else {
            index += 1;
        }
    }
}

#[derive(Debug, Error)]
pub enum BrokerError {
    #[error("local approval state is unavailable")]
    StateUnavailable,
    #[error("another local approval is already pending")]
    Busy,
    #[error("the local approval frame is invalid")]
    InvalidFrame,
    #[error("local approval transport failed: {0}")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::mpsc};

    use aizu_core::{ApprovalDecision, LocalApprovalResponse};

    use super::{ApprovalRegistry, bind_listener, expiry_response};

    #[test]
    fn decisions_are_one_shot_and_preserve_the_request_identifier() {
        let registry = ApprovalRegistry::default();
        let request_id = uuid::Uuid::new_v4();
        let (sender, receiver) = mpsc::sync_channel(1);
        let id = registry.register(request_id, sender).unwrap();

        assert!(registry.mark_window_shown(id).unwrap());
        assert!(registry.mark_frontend_rendered(id).unwrap());
        assert!(registry.complete(id, ApprovalDecision::AllowOnce).unwrap());
        assert!(!registry.complete(id, ApprovalDecision::Deny).unwrap());
        assert_eq!(
            receiver.recv().unwrap(),
            LocalApprovalResponse::Decision {
                request_id,
                decision: ApprovalDecision::AllowOnce,
            }
        );
    }

    #[test]
    fn only_one_command_approval_can_wait_at_a_time() {
        let registry = ApprovalRegistry::default();
        let (first_sender, _first_receiver) = mpsc::sync_channel(1);
        registry
            .register(uuid::Uuid::new_v4(), first_sender)
            .unwrap();
        let (second_sender, _second_receiver) = mpsc::sync_channel(1);

        assert!(
            registry
                .register(uuid::Uuid::new_v4(), second_sender)
                .is_err()
        );
    }

    #[test]
    fn cancellation_reports_presentation_without_fabricating_a_denial() {
        let registry = ApprovalRegistry::default();
        let request_id = uuid::Uuid::new_v4();
        let (sender, receiver) = mpsc::sync_channel(1);
        let id = registry.register(request_id, sender).unwrap();

        assert!(registry.cancel(id).unwrap());
        assert!(!registry.cancel(id).unwrap());
        assert_eq!(
            receiver.recv().unwrap(),
            LocalApprovalResponse::Unavailable {
                request_id,
                presented: false,
            }
        );

        let presented_request_id = uuid::Uuid::new_v4();
        let (sender, receiver) = mpsc::sync_channel(1);
        let id = registry.register(presented_request_id, sender).unwrap();
        assert!(registry.mark_window_shown(id).unwrap());
        assert!(registry.mark_frontend_rendered(id).unwrap());
        assert!(registry.cancel(id).unwrap());
        assert_eq!(
            receiver.recv().unwrap(),
            LocalApprovalResponse::Unavailable {
                request_id: presented_request_id,
                presented: true,
            }
        );
    }

    #[test]
    fn only_a_presented_request_can_be_approved() {
        let registry = ApprovalRegistry::default();
        let request_id = uuid::Uuid::new_v4();
        let (sender, _receiver) = mpsc::sync_channel(1);
        let id = registry.register(request_id, sender).unwrap();

        assert!(registry.mark_frontend_rendered(id).unwrap());
        assert!(!registry.complete(id, ApprovalDecision::AllowOnce).unwrap());
        assert!(registry.mark_window_shown(id).unwrap());
        assert!(registry.complete(id, ApprovalDecision::AllowOnce).unwrap());
    }

    #[test]
    fn a_frontend_ack_without_a_successful_window_show_is_not_presented() {
        let registry = ApprovalRegistry::default();
        let request_id = uuid::Uuid::new_v4();
        let (sender, _receiver) = mpsc::sync_channel(1);
        let id = registry.register(request_id, sender).unwrap();
        assert!(registry.mark_frontend_rendered(id).unwrap());

        assert_eq!(
            expiry_response(&registry, id, request_id),
            LocalApprovalResponse::Unavailable {
                request_id,
                presented: false,
            }
        );
        assert!(!registry.complete(id, ApprovalDecision::AllowOnce).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn socket_binding_rejects_non_socket_nodes_and_sets_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("approval.sock");
        fs::write(&path, b"do not replace").unwrap();
        assert!(bind_listener(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"do not replace");

        fs::remove_file(&path).unwrap();
        let listener = bind_listener(&path).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            bind_listener(&path).unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse
        );
        drop(listener);
        bind_listener(&path).expect("a stale socket should be replaced safely");
    }
}
