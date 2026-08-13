#[cfg(any(test, feature = "desktop-e2e"))]
use std::sync::Arc;
#[cfg(any(test, feature = "desktop-e2e"))]
use std::sync::Mutex;

#[cfg(not(target_os = "macos"))]
use tauri::plugin::PermissionState as TauriPermissionState;
use tauri::{AppHandle, Wry};
#[cfg(not(target_os = "macos"))]
use tauri_plugin_notification::NotificationExt;
use thiserror::Error;

use crate::model::{Notification, NotificationDelivery, PermissionStatus};

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("notification permission check failed: {0}")]
    Permission(String),
    #[error("native notification scheduling failed: {0}")]
    Scheduling(String),
}

pub trait Notifier: Send + Sync {
    fn permission_status(&self) -> Result<PermissionStatus, NotifyError>;
    fn request_permission(&self) -> Result<PermissionStatus, NotifyError>;
    fn notify(&self, notification: &Notification) -> Result<(), NotifyError>;
}

pub struct SystemNotifier {
    app: AppHandle<Wry>,
}

impl SystemNotifier {
    pub fn new(app: AppHandle<Wry>) -> Self {
        Self { app }
    }
}

impl Notifier for SystemNotifier {
    fn permission_status(&self) -> Result<PermissionStatus, NotifyError> {
        system_permission_status(&self.app)
    }

    fn request_permission(&self) -> Result<PermissionStatus, NotifyError> {
        request_system_permission(&self.app)
    }

    fn notify(&self, notification: &Notification) -> Result<(), NotifyError> {
        if notification.delivery == NotificationDelivery::AizuBanner {
            return crate::banner::show(&self.app, notification);
        }
        #[cfg(target_os = "macos")]
        return show_system_notification(&self.app, notification);
        #[cfg(not(target_os = "macos"))]
        show_system_notification(&self.app, notification)
    }
}

#[cfg(target_os = "macos")]
fn system_permission_status(_app: &AppHandle<Wry>) -> Result<PermissionStatus, NotifyError> {
    let settings = mac_usernotifications::blocking::get_notification_settings()
        .map_err(|error| NotifyError::Permission(error.to_string()))?;
    Ok(map_macos_settings(settings))
}

#[cfg(target_os = "macos")]
fn request_system_permission(app: &AppHandle<Wry>) -> Result<PermissionStatus, NotifyError> {
    mac_usernotifications::blocking::request_auth()
        .map_err(|error| NotifyError::Permission(error.to_string()))?;
    system_permission_status(app)
}

#[cfg(target_os = "macos")]
const fn map_macos_permission(
    permission: mac_usernotifications::AuthorizationStatus,
) -> PermissionStatus {
    use mac_usernotifications::AuthorizationStatus;
    match permission {
        AuthorizationStatus::Authorized
        | AuthorizationStatus::Provisional
        | AuthorizationStatus::Ephemeral => PermissionStatus::Granted,
        AuthorizationStatus::Denied => PermissionStatus::Denied,
        AuthorizationStatus::NotDetermined | AuthorizationStatus::Unknown => {
            PermissionStatus::NotDetermined
        }
    }
}

#[cfg(target_os = "macos")]
const fn map_macos_settings(
    settings: mac_usernotifications::NotificationSettings,
) -> PermissionStatus {
    use mac_usernotifications::NotificationSettingStatus;
    match map_macos_permission(settings.authorization_status) {
        PermissionStatus::Granted
            if !matches!(settings.alert_enabled, NotificationSettingStatus::Enabled) =>
        {
            PermissionStatus::AlertsDisabled
        }
        permission => permission,
    }
}

#[cfg(not(target_os = "macos"))]
fn system_permission_status(app: &AppHandle<Wry>) -> Result<PermissionStatus, NotifyError> {
    app.notification()
        .permission_state()
        .map(map_permission)
        .map_err(|error| NotifyError::Permission(error.to_string()))
}

#[cfg(not(target_os = "macos"))]
fn request_system_permission(app: &AppHandle<Wry>) -> Result<PermissionStatus, NotifyError> {
    app.notification()
        .request_permission()
        .map(map_permission)
        .map_err(|error| NotifyError::Permission(error.to_string()))
}

#[cfg(target_os = "macos")]
fn show_system_notification(
    _app: &AppHandle<Wry>,
    notification: &Notification,
) -> Result<(), NotifyError> {
    let builder = build_macos_notification(notification);

    // The macOS UN backend returns only after Notification Center accepts the
    // request. The durable outbox must not be completed before that boundary.
    builder
        .show()
        .map(drop)
        .map_err(|error| NotifyError::Scheduling(error.to_string()))
}

#[cfg(target_os = "macos")]
fn build_macos_notification(notification: &Notification) -> notify_rust::Notification {
    let mut builder = notify_rust::Notification::new();
    builder
        .appname("Aizu")
        .id(format!("aizu-{:08x}", notification.id.cast_unsigned()))
        .summary(&notification.title)
        .body(&notification.body)
        .interruption_level(notify_rust::InterruptionLevel::Active);

    if let Some(sound) = notification.sound {
        builder.sound_name(native_sound_name(sound));
    }

    builder
}

#[cfg(not(target_os = "macos"))]
fn show_system_notification(
    app: &AppHandle<Wry>,
    notification: &Notification,
) -> Result<(), NotifyError> {
    let mut builder = app
        .notification()
        .builder()
        .id(notification.id)
        .title(&notification.title)
        .body(&notification.body);

    if let Some(sound) = notification.sound {
        builder = builder.sound(native_sound_name(sound));
    }

    builder
        .show()
        .map_err(|error| NotifyError::Scheduling(error.to_string()))
}

#[cfg(target_os = "macos")]
const fn native_sound_name(sound: crate::model::NotificationSound) -> &'static str {
    match sound {
        crate::model::NotificationSound::Default => "aizu-pop.wav",
        crate::model::NotificationSound::Glass => "Glass",
        crate::model::NotificationSound::Ping => "Ping",
        crate::model::NotificationSound::Pop => "Pop",
        crate::model::NotificationSound::Hero => "Hero",
    }
}

#[cfg(not(target_os = "macos"))]
const fn native_sound_name(sound: crate::model::NotificationSound) -> &'static str {
    match sound {
        crate::model::NotificationSound::Default => "default",
        crate::model::NotificationSound::Glass => "Glass",
        crate::model::NotificationSound::Ping => "Ping",
        crate::model::NotificationSound::Pop => "Pop",
        crate::model::NotificationSound::Hero => "Hero",
    }
}

#[cfg(not(target_os = "macos"))]
const fn map_permission(permission: TauriPermissionState) -> PermissionStatus {
    match permission {
        TauriPermissionState::Granted => PermissionStatus::Granted,
        TauriPermissionState::Denied => PermissionStatus::Denied,
        TauriPermissionState::Prompt | TauriPermissionState::PromptWithRationale => {
            PermissionStatus::NotDetermined
        }
    }
}

#[cfg(any(test, feature = "desktop-e2e"))]
pub struct FakeNotifier {
    permission: Mutex<PermissionStatus>,
    notifications: Mutex<Vec<Notification>>,
}

#[cfg(any(test, feature = "desktop-e2e"))]
impl FakeNotifier {
    pub fn with_permission(permission: PermissionStatus) -> Arc<Self> {
        Arc::new(Self {
            permission: Mutex::new(permission),
            notifications: Mutex::new(Vec::new()),
        })
    }

    #[cfg(any(test, feature = "desktop-e2e"))]
    pub fn notifications(&self) -> Vec<Notification> {
        self.notifications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[cfg(any(test, feature = "desktop-e2e"))]
impl Notifier for FakeNotifier {
    fn permission_status(&self) -> Result<PermissionStatus, NotifyError> {
        Ok(*self
            .permission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    fn request_permission(&self) -> Result<PermissionStatus, NotifyError> {
        let mut permission = self
            .permission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *permission = PermissionStatus::Granted;
        Ok(*permission)
    }

    fn notify(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.notifications
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(notification.clone());
        Ok(())
    }
}

#[cfg(feature = "desktop-e2e")]
pub struct E2eNotifier {
    app: AppHandle<Wry>,
    recorder: Arc<FakeNotifier>,
}

#[cfg(feature = "desktop-e2e")]
impl E2eNotifier {
    pub fn new(app: AppHandle<Wry>, recorder: Arc<FakeNotifier>) -> Arc<Self> {
        Arc::new(Self { app, recorder })
    }
}

#[cfg(feature = "desktop-e2e")]
impl Notifier for E2eNotifier {
    fn permission_status(&self) -> Result<PermissionStatus, NotifyError> {
        self.recorder.permission_status()
    }

    fn request_permission(&self) -> Result<PermissionStatus, NotifyError> {
        self.recorder.request_permission()
    }

    fn notify(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.recorder.notify(notification)?;
        if notification.delivery == NotificationDelivery::AizuBanner {
            crate::banner::show(&self.app, notification)?;
        }
        Ok(())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use mac_usernotifications::{
        AuthorizationStatus, NotificationSettingStatus, NotificationSettings,
    };

    use super::{
        PermissionStatus, build_macos_notification, map_macos_permission, map_macos_settings,
        native_sound_name,
    };

    #[test]
    fn macos_permission_statuses_preserve_denial_and_prompt_state() {
        assert_eq!(
            map_macos_permission(AuthorizationStatus::NotDetermined),
            PermissionStatus::NotDetermined
        );
        assert_eq!(
            map_macos_permission(AuthorizationStatus::Denied),
            PermissionStatus::Denied
        );
        for authorized in [
            AuthorizationStatus::Authorized,
            AuthorizationStatus::Provisional,
            AuthorizationStatus::Ephemeral,
        ] {
            assert_eq!(map_macos_permission(authorized), PermissionStatus::Granted);
        }
    }

    #[test]
    fn authorized_macos_notifications_require_banner_alerts() {
        let settings = NotificationSettings {
            authorization_status: AuthorizationStatus::Authorized,
            alert_enabled: NotificationSettingStatus::Disabled,
            badge_enabled: NotificationSettingStatus::Disabled,
            sound_enabled: NotificationSettingStatus::Enabled,
            lock_screen_enabled: NotificationSettingStatus::Disabled,
            notification_center_enabled: NotificationSettingStatus::Enabled,
        };

        assert_eq!(
            map_macos_settings(settings),
            PermissionStatus::AlertsDisabled
        );
    }

    #[test]
    fn macos_notifications_are_delivered_immediately() {
        let notification = crate::model::Notification {
            id: 42,
            title: "Ready".to_owned(),
            body: "The task completed.".to_owned(),
            sound: Some(crate::model::NotificationSound::Glass),
            delivery: crate::model::NotificationDelivery::System,
            language: crate::model::LanguagePreference::English,
            text_size: crate::model::TextSize::Standard,
        };

        let native = build_macos_notification(&notification);

        assert_eq!(native.timeout, notify_rust::Timeout::Default);
    }

    #[test]
    fn default_macos_notification_uses_the_bundled_aizu_pop() {
        assert_eq!(
            native_sound_name(crate::model::NotificationSound::Default),
            "aizu-pop.wav"
        );
    }
}
