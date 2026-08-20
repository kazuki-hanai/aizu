use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use aizu_core::{
    LocalApprovalRequest, LocalApprovalResponse, MAX_LOCAL_APPROVAL_FRAME_BYTES,
    parse_strict_json_value,
};
use thiserror::Error;

const MAX_CONNECTION_HANDLERS: usize = 4;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Releases requests from older blocking Aizu CLIs back to the agent's
/// terminal-owned approval flow. Current CLIs never connect to this socket.
pub struct ApprovalBroker {
    stop: Arc<AtomicBool>,
    listener: Mutex<Option<JoinHandle<()>>>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    socket_path: Option<PathBuf>,
}

impl ApprovalBroker {
    pub fn start(socket_path: &Path) -> Self {
        let socket_path = socket_path.to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let handlers = Arc::new(Mutex::new(Vec::new()));
        let active_handlers = Arc::new(AtomicUsize::new(0));
        let listener = bind_listener(&socket_path).ok();
        let bound_path = listener.as_ref().map(|_| socket_path.clone());
        let listener_thread = listener.and_then(|listener| {
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
                                let handle = thread::Builder::new()
                                    .name("aizu-approval-request".to_owned())
                                    .spawn(move || {
                                        let _guard = guard;
                                        handle_connection(stream);
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
            stop,
            listener: Mutex::new(listener_thread),
            handlers,
            socket_path: bound_path,
        }
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
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

fn handle_connection(mut stream: std::os::unix::net::UnixStream) {
    let Ok(request) = read_request(&mut stream) else {
        return;
    };
    let _ = write_response(
        &mut stream,
        LocalApprovalResponse::Unavailable {
            request_id: request.request_id,
            presented: false,
        },
    );
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
    #[error("the local approval frame is invalid")]
    InvalidFrame,
    #[error("local approval transport failed: {0}")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::fs;

    use aizu_core::{
        AgentKind, LOCAL_APPROVAL_PROTOCOL_VERSION, LocalApprovalRequest, LocalApprovalResponse,
    };

    use super::{bind_listener, handle_connection};

    #[cfg(unix)]
    #[test]
    fn legacy_clients_are_released_to_the_terminal_without_a_decision() {
        use std::{
            io::{Read, Write},
            os::unix::net::UnixStream,
        };

        let request = LocalApprovalRequest::new(
            uuid::Uuid::new_v4(),
            AgentKind::Codex,
            "Bash".to_owned(),
            "printf 'terminal owns approval'".to_owned(),
        )
        .unwrap();
        assert_eq!(request.version, LOCAL_APPROVAL_PROTOCOL_VERSION);
        let (mut client, server) = UnixStream::pair().unwrap();
        let handler = std::thread::spawn(move || handle_connection(server));
        serde_json::to_writer(&mut client, &request).unwrap();
        client.write_all(b"\n").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        handler.join().unwrap();

        assert_eq!(
            serde_json::from_str::<LocalApprovalResponse>(response.trim()).unwrap(),
            LocalApprovalResponse::Unavailable {
                request_id: request.request_id,
                presented: false,
            }
        );
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
