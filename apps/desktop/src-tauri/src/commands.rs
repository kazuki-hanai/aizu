use tauri::{AppHandle, Emitter, Manager, State, Wry};
use tauri_plugin_autostart::ManagerExt;

use crate::{
    model::{
        AddRemoteSourceRequest, AppView, CompleteOnboardingRequest, Notification,
        NotificationDelivery, Preferences, SshConnectionTestResult,
    },
    state::{DesktopError, DesktopState},
    tray::TrayUi,
};

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn get_banners(app: AppHandle<Wry>) -> Result<Vec<Notification>, DesktopError> {
    crate::banner::banners(&app).map_err(DesktopError::from)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn dismiss_banner(app: AppHandle<Wry>, id: i32) -> Result<(), DesktopError> {
    crate::banner::dismiss(&app, id).map_err(DesktopError::from)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn resize_banner(app: AppHandle<Wry>, height: f64) -> Result<(), DesktopError> {
    crate::banner::resize(&app, height).map_err(DesktopError::from)
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn open_from_banner(app: AppHandle<Wry>, id: i32) -> Result<(), DesktopError> {
    crate::banner::dismiss(&app, id)?;
    crate::tray::show_main_window(&app);
    Ok(())
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
    let view = state.lock()?.update_preferences(request)?;
    if delivery == NotificationDelivery::System {
        let _ = crate::banner::clear(&app);
    }
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
    #[test]
    fn agent_setup_task_runs_off_the_calling_thread() {
        let calling_thread = std::thread::current().id();
        let worker_thread = tauri::async_runtime::block_on(super::run_agent_setup_task(|| {
            Ok(std::thread::current().id())
        }))
        .expect("blocking setup task");

        assert_ne!(calling_thread, worker_thread);
    }
}
