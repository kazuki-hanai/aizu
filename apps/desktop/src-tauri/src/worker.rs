use std::{
    sync::mpsc::{self, Receiver},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::{
    cli_diagnostic,
    process_monitor::{self, ProcessMonitor},
    remote_worker::RemoteFleet,
    state::DesktopState,
    tray::TrayUi,
};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_secs(2);
const VERSION_POLL_INTERVAL: Duration = Duration::from_mins(1);
const HISTORY_MAINTENANCE_INTERVAL: Duration = Duration::from_mins(10);

pub struct LocalWorker {
    stop: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl LocalWorker {
    pub fn start(app: AppHandle<Wry>) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("aizu-local-worker".to_owned())
            .spawn(move || run(&app, &worker_stop))?;
        Ok(Self {
            stop,
            thread: Mutex::new(Some(thread)),
        })
    }

    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut handle) = self.thread.lock()
            && let Some(handle) = handle.take()
        {
            let _ = handle.join();
        }
    }
}

impl Drop for LocalWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[allow(clippy::too_many_lines)]
fn run(app: &AppHandle<Wry>, stop: &AtomicBool) {
    let mut process_monitor = ProcessMonitor::new();
    let mut remote_fleet = RemoteFleet::new();
    let mut version_probe: Option<Receiver<process_monitor::AgentVersions>> = None;
    let mut cli_probe: Option<Receiver<cli_diagnostic::CliDiagnostic>> = None;
    let mut last_version_poll = Instant::now()
        .checked_sub(VERSION_POLL_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_process_poll = Instant::now()
        .checked_sub(PROCESS_POLL_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut last_history_maintenance = Instant::now()
        .checked_sub(HISTORY_MAINTENANCE_INTERVAL)
        .unwrap_or_else(Instant::now);
    while !stop.load(Ordering::Acquire) {
        #[cfg(not(feature = "desktop-e2e"))]
        let diagnostic_poll_due = last_process_poll.elapsed() >= PROCESS_POLL_INTERVAL;
        #[cfg(feature = "desktop-e2e")]
        let diagnostic_poll_due = false;
        let history_maintenance_due =
            last_history_maintenance.elapsed() >= HISTORY_MAINTENANCE_INTERVAL;
        let snapshot = diagnostic_poll_due
            .then(|| {
                last_process_poll = Instant::now();
                process_monitor.snapshot().ok()
            })
            .flatten();
        if diagnostic_poll_due && cli_probe.is_none() {
            let (sender, receiver) = mpsc::sync_channel(1);
            if thread::Builder::new()
                .name("aizu-cli-version-probe".to_owned())
                .spawn(move || {
                    let diagnostic = cli_diagnostic::inspect(env!("CARGO_PKG_VERSION"))
                        .unwrap_or(cli_diagnostic::CliDiagnostic::Missing);
                    let _ = sender.send(diagnostic);
                })
                .is_ok()
            {
                cli_probe = Some(receiver);
            }
        }
        let cli_diagnostic = receive_probe(&mut cli_probe);
        if version_probe.is_none() && last_version_poll.elapsed() >= VERSION_POLL_INTERVAL {
            let (sender, receiver) = mpsc::sync_channel(1);
            if thread::Builder::new()
                .name("aizu-agent-version-probe".to_owned())
                .spawn(move || {
                    let _ = sender.send(process_monitor::inspect_versions());
                })
                .is_ok()
            {
                version_probe = Some(receiver);
                last_version_poll = Instant::now();
            }
        }
        let agent_versions = receive_probe(&mut version_probe);
        let agent_hooks = diagnostic_poll_due.then(process_monitor::inspect_hooks);
        let result = app.state::<DesktopState>().lock().map(|mut state| {
            let (view, mut changed) = state.poll_local_pipeline().unwrap_or_else(|error| {
                let view = state.record_pipeline_error(&error.to_string());
                (view, true)
            });
            if let Some(snapshot) = snapshot.as_ref() {
                changed |= state.update_process_snapshot(snapshot);
            }
            if let Some(diagnostic) = cli_diagnostic.as_ref() {
                changed |= state.update_cli_diagnostic(diagnostic);
            }
            if let Some(versions) = agent_versions.as_ref() {
                changed |= state.update_agent_versions(versions);
            }
            if let Some(hooks) = agent_hooks.as_ref() {
                changed |= state.update_agent_hooks(*hooks);
            }
            if history_maintenance_due {
                let maintained = state.maintain_history().unwrap_or(false);
                last_history_maintenance = if maintained {
                    Instant::now()
                        .checked_sub(HISTORY_MAINTENANCE_INTERVAL)
                        .unwrap_or_else(Instant::now)
                } else {
                    Instant::now()
                };
                changed |= maintained;
            }
            remote_fleet.sync(
                &state.remote_sources(),
                &state.core_desktop(),
                &state.connected_remote_agent_sources(),
            );
            for update in remote_fleet.updates() {
                changed |= if let Some(identity) = update.replacement_identity {
                    state.require_remote_identity_confirmation(&update.host_alias, identity)
                } else {
                    state.set_remote_status(&update.host_alias, update.status, update.detail)
                };
            }
            for update in remote_fleet.agent_updates() {
                changed |= state.update_remote_agents(
                    &update.host_alias,
                    update.connection_epoch,
                    &update.agents,
                );
            }
            (if changed { state.view() } else { view }, changed)
        });

        if let Ok((view, changed)) = result
            && changed
        {
            if let Some(tray) = app.try_state::<TrayUi>() {
                let _ = tray.sync_from_state(app);
            }
            let _ = app.emit("aizu://view-changed", view);
        }
        let _ = crate::banner::request_present(app);
        thread::sleep(POLL_INTERVAL);
    }
}

fn receive_probe<T>(probe: &mut Option<Receiver<T>>) -> Option<T> {
    match probe.as_ref()?.try_recv() {
        Ok(value) => {
            *probe = None;
            Some(value)
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            *probe = None;
            None
        }
        Err(mpsc::TryRecvError::Empty) => None,
    }
}
