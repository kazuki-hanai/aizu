#![cfg_attr(feature = "desktop-e2e", allow(dead_code, unused_imports))]

mod banner;
mod cli_diagnostic;
mod commands;
mod model;
mod notifier;
mod process_monitor;
mod remote_worker;
mod ssh_connection_test;
mod state;
mod store;
mod terminal_activation;
mod tray;
mod worker;

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
    sync::Arc,
};

use tauri::{Manager, RunEvent, WindowEvent};

#[cfg(feature = "desktop-e2e")]
use crate::commands::{
    get_e2e_notifications, get_e2e_terminal_activation_count, hide_e2e_main_window,
    is_e2e_main_window_visible, set_e2e_remote_status, show_e2e_terminal_banner,
};
use crate::{
    commands::{
        activate_banner, add_remote_source, clear_history, complete_onboarding, configure_agents,
        confirm_codex_hook_trust, confirm_remote_identity, dismiss_banner, get_app_view,
        get_banners, install_cli, reconnect_remote_source, remove_remote_source,
        request_notification_permission, resize_banner, send_test_notification,
        set_notifications_paused, test_ssh_connection, update_preferences,
    },
    state::{AppService, DesktopState},
    store::SettingsStore,
};

struct WorkerLease(File);

impl WorkerLease {
    fn acquire(state_root: &Path) -> io::Result<Self> {
        fs::create_dir_all(state_root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            fs::set_permissions(state_root, fs::Permissions::from_mode(0o700))?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .custom_flags(nix::libc::O_NOFOLLOW)
                .open(state_root.join("desktop-worker.lock"))?;
            File::try_lock(&file)?;
            Ok(Self(file))
        }
        #[cfg(not(unix))]
        {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(state_root.join("desktop-worker.lock"))?;
            File::try_lock(&file)?;
            Ok(Self(file))
        }
    }
}

impl Drop for WorkerLease {
    fn drop(&mut self) {
        let _ = File::unlock(&self.0);
    }
}

fn desktop_builder() -> tauri::Builder<tauri::Wry> {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(
            |app, _args, _working_directory| {
                tray::show_main_window(app);
            },
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::Builder::new().build());
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    builder
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = desktop_builder();
    #[cfg(feature = "desktop-e2e")]
    let builder = builder
        .plugin(tauri_plugin_wdio::init())
        .plugin(tauri_plugin_wdio_webdriver::init());
    let app = builder
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            banner::setup(app);

            let state_paths = aizu_core::StatePaths::discover()?;
            #[cfg(feature = "desktop-e2e")]
            let settings_path = state_paths.root().join("settings.json");
            #[cfg(not(feature = "desktop-e2e"))]
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let worker_lease = WorkerLease::acquire(state_paths.root())?;
            #[cfg(feature = "desktop-e2e")]
            let notification_recorder =
                notifier::FakeNotifier::with_permission(model::PermissionStatus::NotDetermined);
            #[cfg(not(feature = "desktop-e2e"))]
            let notifier = Arc::new(notifier::SystemNotifier::new(app.handle().clone())?);
            #[cfg(feature = "desktop-e2e")]
            let service_notifier: Arc<dyn notifier::Notifier> =
                notifier::E2eNotifier::new(app.handle().clone(), notification_recorder.clone());
            #[cfg(not(feature = "desktop-e2e"))]
            let service_notifier: Arc<dyn notifier::Notifier> = notifier;
            let mut service = AppService::new(
                service_notifier,
                SettingsStore::new(settings_path),
                &state_paths,
            )?;
            #[cfg(feature = "desktop-e2e")]
            service.prepare_e2e_state()?;
            let _ = service.poll_local_pipeline();
            let initial_view = service.view();
            app.manage(DesktopState::new(service));
            #[cfg(feature = "desktop-e2e")]
            app.manage(notification_recorder);
            app.manage(worker_lease);
            app.manage(tray::setup(app, &initial_view)?);
            app.manage(worker::LocalWorker::start(app.handle().clone())?);

            if let Some(window) = app.get_webview_window("main") {
                let window_to_hide = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window_to_hide.hide();
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_banners,
            dismiss_banner,
            activate_banner,
            resize_banner,
            get_app_view,
            complete_onboarding,
            request_notification_permission,
            send_test_notification,
            clear_history,
            set_notifications_paused,
            update_preferences,
            add_remote_source,
            remove_remote_source,
            reconnect_remote_source,
            test_ssh_connection,
            confirm_remote_identity,
            install_cli,
            configure_agents,
            confirm_codex_hook_trust,
            #[cfg(feature = "desktop-e2e")]
            get_e2e_notifications,
            #[cfg(feature = "desktop-e2e")]
            get_e2e_terminal_activation_count,
            #[cfg(feature = "desktop-e2e")]
            hide_e2e_main_window,
            #[cfg(feature = "desktop-e2e")]
            is_e2e_main_window_visible,
            #[cfg(feature = "desktop-e2e")]
            set_e2e_remote_status,
            #[cfg(feature = "desktop-e2e")]
            show_e2e_terminal_banner,
        ])
        .build(tauri::generate_context!())
        .expect("Aizu desktop runtime failed");
    app.run(|app, event| {
        if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
            app.state::<worker::LocalWorker>().shutdown();
        }
    });
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::WorkerLease;

    #[test]
    fn desktop_worker_lease_rejects_a_second_owner() {
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("test clock should be after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("aizu-worker-lease-{suffix}"));
        let first = WorkerLease::acquire(&directory).expect("first worker should acquire lease");

        assert!(WorkerLease::acquire(&directory).is_err());

        drop(first);
        WorkerLease::acquire(&directory).expect("lease should be released after worker exits");
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }
}
