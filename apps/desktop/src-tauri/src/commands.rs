use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow, Wry};
use tauri_plugin_autostart::ManagerExt;

use crate::{
    model::{
        AddRemoteSourceRequest, AppView, ApprovalAction, CompleteOnboardingRequest, Notification,
        NotificationDelivery, Preferences, SshConnectionTestResult,
    },
    state::{DesktopError, DesktopState},
    tray::TrayUi,
};

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_banners(
    app: AppHandle<Wry>,
    window: WebviewWindow<Wry>,
) -> Result<Vec<Notification>, DesktopError> {
    ensure_banner_caller(window.label())?;
    crate::banner::banners(&app).map_err(DesktopError::from)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn dismiss_banner(
    app: AppHandle<Wry>,
    window: WebviewWindow<Wry>,
    id: i32,
) -> Result<(), DesktopError> {
    ensure_banner_caller(window.label())?;
    if let Some(broker) = app.try_state::<crate::approval_broker::ApprovalBroker>() {
        let cancelled = broker
            .cancel(id)
            .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()))?;
        if cancelled {
            cleanup_after_committed_approval(|| crate::banner::dismiss(&app, id));
            return Ok(());
        }
    }
    crate::banner::dismiss(&app, id).map_err(DesktopError::from)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn acknowledge_banner_approval(
    app: AppHandle<Wry>,
    window: WebviewWindow<Wry>,
    id: i32,
) -> Result<(), DesktopError> {
    ensure_banner_caller(window.label())?;
    if !crate::banner::has_approval(&app, id)? {
        return Err(approval_unavailable());
    }
    let broker = app.state::<crate::approval_broker::ApprovalBroker>();
    if !broker
        .mark_frontend_rendered(id)
        .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()))?
    {
        return Err(approval_unavailable());
    }
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn decide_banner_approval(
    app: AppHandle<Wry>,
    window: WebviewWindow<Wry>,
    id: i32,
    decision: ApprovalAction,
) -> Result<(), DesktopError> {
    ensure_banner_caller(window.label())?;
    let decision = match decision {
        ApprovalAction::AllowOnce => aizu_core::ApprovalDecision::AllowOnce,
        ApprovalAction::Deny => aizu_core::ApprovalDecision::Deny,
    };
    let broker = app.state::<crate::approval_broker::ApprovalBroker>();
    if !broker
        .decide(id, decision)
        .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()))?
    {
        return Err(approval_unavailable());
    }
    cleanup_after_committed_approval(|| crate::banner::dismiss(&app, id));
    Ok(())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub async fn activate_banner(
    app: AppHandle<Wry>,
    window: WebviewWindow<Wry>,
    id: i32,
) -> Result<(), DesktopError> {
    ensure_banner_caller(window.label())?;
    let (claim, target) = crate::banner::claim_activation(&app, id)?;
    let result =
        tauri::async_runtime::spawn_blocking(move || crate::terminal_activation::activate(&target))
            .await
            .map_err(|error| {
                crate::notifier::NotifyError::Scheduling(format!(
                    "terminal activation task stopped unexpectedly: {error}"
                ))
            })
            .and_then(|result| {
                result.map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()))
            });
    if let Err(error) = result {
        let _ = crate::banner::cancel_activation(&app, &claim);
        return Err(error.into());
    }
    crate::banner::complete_activation(&app, &claim).map_err(DesktopError::from)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn resize_banner(
    app: AppHandle<Wry>,
    window: WebviewWindow<Wry>,
    height: f64,
) -> Result<(), DesktopError> {
    ensure_banner_caller(window.label())?;
    crate::banner::resize(&app, height).map_err(DesktopError::from)
}

fn ensure_banner_caller(window_label: &str) -> Result<(), DesktopError> {
    if window_label == crate::banner::BANNER_WINDOW {
        return Ok(());
    }
    Err(crate::notifier::NotifyError::Scheduling(
        "Aizu banner data and actions are available only from the banner window".to_owned(),
    )
    .into())
}

fn approval_unavailable() -> DesktopError {
    crate::notifier::NotifyError::Scheduling(
        "this command approval is no longer available".to_owned(),
    )
    .into()
}

fn cleanup_after_committed_approval(
    cleanup: impl FnOnce() -> Result<(), crate::notifier::NotifyError>,
) {
    let _ = cleanup();
}

#[cfg(feature = "desktop-e2e")]
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_e2e_notifications(
    notifier: State<'_, std::sync::Arc<crate::notifier::FakeNotifier>>,
) -> Vec<crate::model::Notification> {
    notifier.notifications()
}

#[cfg(feature = "desktop-e2e")]
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_e2e_banners(app: AppHandle<Wry>) -> Result<Vec<Notification>, DesktopError> {
    crate::banner::banners(&app).map_err(DesktopError::from)
}

#[cfg(feature = "desktop-e2e")]
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_e2e_banner_window_state(app: AppHandle<Wry>) -> Result<(bool, bool, u32), DesktopError> {
    let window = app
        .get_webview_window(crate::banner::BANNER_WINDOW)
        .ok_or_else(|| {
            crate::notifier::NotifyError::Scheduling("banner window is unavailable".to_owned())
        })?;
    let visible = window
        .is_visible()
        .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()))?;
    let focused = window
        .is_focused()
        .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()))?;
    let height = window
        .inner_size()
        .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()))?
        .height;
    Ok((visible, focused, height))
}

#[cfg(feature = "desktop-e2e")]
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_e2e_banner_monitor_state(app: AppHandle<Wry>) -> Result<(usize, bool), DesktopError> {
    let window = app
        .get_webview_window(crate::banner::BANNER_WINDOW)
        .ok_or_else(|| {
            crate::notifier::NotifyError::Scheduling("banner window is unavailable".to_owned())
        })?;
    let monitors = window
        .available_monitors()
        .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()))?;
    let current = window
        .current_monitor()
        .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()))?
        .ok_or_else(|| {
            crate::notifier::NotifyError::Scheduling("current display is unavailable".to_owned())
        })?;
    let primary = window
        .primary_monitor()
        .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()))?
        .ok_or_else(|| {
            crate::notifier::NotifyError::Scheduling("primary display is unavailable".to_owned())
        })?;
    let current_is_primary =
        current.position() == primary.position() && current.size() == primary.size();
    Ok((monitors.len(), current_is_primary))
}

#[cfg(feature = "desktop-e2e")]
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn show_e2e_terminal_banner(app: AppHandle<Wry>) -> Result<(), DesktopError> {
    crate::banner::show(
        &app,
        &Notification {
            id: 8_675_309,
            title: "Codex task completed".to_owned(),
            body: "## Completed\n\n- All tests passed\n- Run next:\n\n```sh\nmise run check\n```"
                .to_owned(),
            sound: None,
            delivery: NotificationDelivery::AizuBanner,
            language: crate::model::LanguagePreference::English,
            text_size: crate::model::TextSize::Standard,
            can_activate_terminal: true,
            approval: None,
            activation: Some(aizu_core::TerminalActivation {
                application: aizu_core::TerminalApplication::Iterm2,
                application_session: Some("w0t0p0:E2E".to_owned()),
                tmux: None,
            }),
        },
    )?;
    Ok(())
}

#[cfg(feature = "desktop-e2e")]
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn show_e2e_scrollable_banners(app: AppHandle<Wry>) -> Result<(), DesktopError> {
    for index in 1..=3 {
        let lines = (1..=18)
            .map(|line| format!("- Notification {index}, line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = if index == 3 {
            "The final short notification remains visible after the long notifications close."
                .to_owned()
        } else {
            format!("## Full notification {index}\n\n{lines}")
        };
        crate::banner::show(
            &app,
            &Notification {
                id: 8_675_400 + index,
                title: format!("Scrollable notification {index}"),
                body,
                sound: None,
                delivery: NotificationDelivery::AizuBanner,
                language: crate::model::LanguagePreference::English,
                text_size: crate::model::TextSize::Standard,
                can_activate_terminal: false,
                approval: None,
                activation: None,
            },
        )?;
    }
    Ok(())
}

#[cfg(feature = "desktop-e2e")]
#[tauri::command]
pub fn get_e2e_terminal_activation_count() -> usize {
    crate::terminal_activation::e2e_activation_count()
}

#[cfg(feature = "desktop-e2e")]
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn hide_e2e_main_window(app: AppHandle<Wry>) -> Result<(), DesktopError> {
    let window = app.get_webview_window("main").ok_or_else(|| {
        crate::notifier::NotifyError::Scheduling("main window is unavailable".to_owned())
    })?;
    window
        .hide()
        .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()).into())
}

#[cfg(feature = "desktop-e2e")]
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn is_e2e_main_window_visible(app: AppHandle<Wry>) -> Result<bool, DesktopError> {
    let window = app.get_webview_window("main").ok_or_else(|| {
        crate::notifier::NotifyError::Scheduling("main window is unavailable".to_owned())
    })?;
    window
        .is_visible()
        .map_err(|error| crate::notifier::NotifyError::Scheduling(error.to_string()).into())
}

#[cfg(feature = "desktop-e2e")]
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn set_e2e_remote_status(
    state: State<'_, DesktopState>,
    host_alias: String,
    status: crate::model::SourceStatus,
) -> Result<AppView, DesktopError> {
    let mut state = state.lock()?;
    state.set_remote_status(&host_alias, status, "E2E remote fixture");
    Ok(state.view())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub async fn test_ssh_connection(host_alias: String) -> SshConnectionTestResult {
    tauri::async_runtime::spawn_blocking(move || {
        crate::ssh_connection_test::test_connection(&host_alias)
    })
    .await
    .unwrap_or_else(|_| SshConnectionTestResult {
        status: crate::model::SshConnectionTestStatus::RemoteFailure,
        message: "The SSH connection test could not be completed.".to_owned(),
        config_resolved: false,
        reachable: false,
        protocol_compatible: false,
        remote_version: None,
    })
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_app_view(state: State<'_, DesktopState>) -> Result<AppView, DesktopError> {
    Ok(state.lock()?.view())
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn request_notification_permission(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
) -> Result<AppView, DesktopError> {
    let view = state.lock()?.request_notification_permission()?;
    publish(&app, &view);
    Ok(view)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn send_test_notification(state: State<'_, DesktopState>) -> Result<AppView, DesktopError> {
    state.lock()?.send_test_notification()
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn clear_history(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
) -> Result<AppView, DesktopError> {
    let view = state.lock()?.clear_history()?;
    publish(&app, &view);
    Ok(view)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn set_notifications_paused(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
    paused: bool,
) -> Result<AppView, DesktopError> {
    let view = state.lock()?.set_paused(paused)?;
    publish(&app, &view);
    Ok(view)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn complete_onboarding(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
    request: CompleteOnboardingRequest,
) -> Result<AppView, DesktopError> {
    set_autostart(&app, request.launch_at_login)?;
    let view = state.lock()?.complete_onboarding(request.launch_at_login)?;
    publish(&app, &view);
    Ok(view)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn update_preferences(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
    request: Preferences,
) -> Result<AppView, DesktopError> {
    let previous_launch_at_login = state.lock()?.view().preferences.launch_at_login;
    if previous_launch_at_login != request.launch_at_login {
        set_autostart(&app, request.launch_at_login)?;
    }
    let delivery = request.notification_delivery;
    let approvals_enabled = request.command_approvals_enabled;
    let view = state.lock()?.update_preferences(request)?;
    if !approvals_enabled
        && let Some(broker) = app.try_state::<crate::approval_broker::ApprovalBroker>()
    {
        for id in broker.cancel_all() {
            let _ = crate::banner::dismiss(&app, id);
        }
    }
    if delivery == NotificationDelivery::System {
        let _ = crate::banner::clear_passive(&app);
    } else {
        // Preferences are already durably committed. Banner presentation is
        // retried independently and must not turn that commit into an error.
        let _ = crate::banner::update_text_size(&app, view.preferences.text_size);
    }
    // The preference is already durable. Reposition any current approval as a
    // best-effort presentation refresh without replaying its sound or decision.
    let _ = crate::banner::update_notification_display(&app, view.preferences.notification_display);
    let _ =
        crate::banner::update_approval_centering(&app, view.preferences.center_approval_dialogs);
    publish(&app, &view);
    Ok(view)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn add_remote_source(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
    request: AddRemoteSourceRequest,
) -> Result<AppView, DesktopError> {
    let view = state
        .lock()?
        .add_remote_source(request.host_alias, request.local_label)?;
    publish(&app, &view);
    Ok(view)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn remove_remote_source(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
    host_alias: String,
) -> Result<AppView, DesktopError> {
    let view = state.lock()?.remove_remote_source(&host_alias)?;
    publish(&app, &view);
    Ok(view)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn reconnect_remote_source(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
    host_alias: String,
) -> Result<AppView, DesktopError> {
    let view = state.lock()?.reconnect_remote_source(&host_alias)?;
    publish(&app, &view);
    Ok(view)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn confirm_remote_identity(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
    host_alias: String,
) -> Result<AppView, DesktopError> {
    let view = state.lock()?.confirm_remote_identity(&host_alias)?;
    publish(&app, &view);
    Ok(view)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn install_cli(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
) -> Result<AppView, DesktopError> {
    let source = bundled_cli_path(&app)?;
    let view = state.lock()?.install_cli(&source)?;
    publish(&app, &view);
    Ok(view)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub async fn configure_agents(app: AppHandle<Wry>) -> Result<AppView, DesktopError> {
    let worker_app = app.clone();
    run_agent_setup_task(move || {
        let source = bundled_cli_path(&worker_app)?;
        let state = worker_app.state::<DesktopState>();
        let mut state = state.lock()?;
        state.install_cli(&source)?;
        state.configure_agent_hooks()?;
        Ok(())
    })
    .await?;
    let state = app.state::<DesktopState>();
    let state = state.lock()?;
    let view = state.view();
    let _ = app.emit("aizu://view-changed", &view);
    drop(state);
    sync_tray(&app);
    Ok(view)
}

async fn run_agent_setup_task<T, F>(task: F) -> Result<T, DesktopError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, DesktopError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|_| DesktopError::AgentSetupTask)?
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn confirm_codex_hook_trust(
    app: AppHandle<Wry>,
    state: State<'_, DesktopState>,
) -> Result<AppView, DesktopError> {
    let view = state.lock()?.confirm_codex_hook_trust()?;
    publish(&app, &view);
    Ok(view)
}

fn bundled_cli_path(app: &AppHandle<Wry>) -> Result<std::path::PathBuf, DesktopError> {
    let resource = app
        .path()
        .resource_dir()
        .ok()
        .map(|directory| directory.join("bin/aizu"));
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(|parent| parent.join("aizu")));
    resource
        .into_iter()
        .chain(sibling)
        .find(|candidate| candidate.is_file())
        .ok_or(DesktopError::CliBundleUnavailable)
}

fn set_autostart(app: &AppHandle<Wry>, enabled: bool) -> Result<(), DesktopError> {
    let result = if enabled {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    result.map_err(|error| DesktopError::Autostart(error.to_string()))
}

fn publish(app: &AppHandle<Wry>, view: &AppView) {
    sync_tray(app);
    let _ = app.emit("aizu://view-changed", view);
}

fn sync_tray(app: &AppHandle<Wry>) {
    if let Some(tray) = app.try_state::<TrayUi>() {
        let _ = tray.sync_from_state(app);
    }
}

#[cfg(test)]
mod tests {
    use crate::notifier::NotifyError;

    #[test]
    fn agent_setup_task_runs_off_the_calling_thread() {
        let calling_thread = std::thread::current().id();
        let worker_thread = tauri::async_runtime::block_on(super::run_agent_setup_task(|| {
            Ok(std::thread::current().id())
        }))
        .expect("blocking setup task");

        assert_ne!(calling_thread, worker_thread);
    }

    #[test]
    fn banner_data_and_actions_reject_non_banner_callers() {
        assert!(super::ensure_banner_caller(crate::banner::BANNER_WINDOW).is_ok());
        assert!(super::ensure_banner_caller("main").is_err());
    }

    #[test]
    fn a_committed_approval_decision_or_cancellation_ignores_cleanup_failure() {
        let cleanup_ran = std::cell::Cell::new(false);
        super::cleanup_after_committed_approval(|| {
            cleanup_ran.set(true);
            Err(NotifyError::Scheduling("forced cleanup failure".to_owned()))
        });

        assert!(cleanup_ran.get());
    }
}
