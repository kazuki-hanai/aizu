#[cfg(target_os = "macos")]
use std::collections::{BTreeMap, VecDeque};
#[cfg(any(test, feature = "desktop-e2e", target_os = "macos"))]
use std::sync::{Arc, Mutex};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(not(target_os = "macos"))]
use tauri::plugin::PermissionState as TauriPermissionState;
use tauri::{AppHandle, Wry};
#[cfg(not(target_os = "macos"))]
use tauri_plugin_notification::NotificationExt;
use thiserror::Error;

use crate::model::{Notification, NotificationDelivery, PermissionStatus};
#[cfg(target_os = "macos")]
use user_notify_reborn::{NotifyManager, NotifyManagerExt, NotifyResponseAction};

#[cfg(target_os = "macos")]
const MAX_SYSTEM_ACTIVATIONS: usize = 64;
#[cfg(target_os = "macos")]
const RESPONSE_HANDLER_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("notification permission check failed: {0}")]
    Permission(String),
    #[error("native notification scheduling failed: {0}")]
    Scheduling(String),
    #[error("native notification response handler setup failed: {0}")]
    ResponseHandler(String),
}

pub trait Notifier: Send + Sync {
    fn permission_status(&self) -> Result<PermissionStatus, NotifyError>;
    fn request_permission(&self) -> Result<PermissionStatus, NotifyError>;
    fn notify(&self, notification: &Notification) -> Result<(), NotifyError>;
}

pub struct SystemNotifier {
    app: AppHandle<Wry>,
    #[cfg(target_os = "macos")]
    response_state: Arc<Mutex<SystemResponseState>>,
}

impl SystemNotifier {
    #[must_use]
    pub fn new(app: AppHandle<Wry>) -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                response_state: Arc::new(Mutex::new(SystemResponseState::new(
                    optional_response_handler(|| initialize_system_response_handler(&app)),
                ))),
                app,
            }
        }
        #[cfg(not(target_os = "macos"))]
        Self { app }
    }

    #[cfg(target_os = "macos")]
    fn request_response_handler(&self) {
        let now = Instant::now();
        {
            let mut state = self
                .response_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.begin_retry(now) {
                return;
            }
        }

        let app = self.app.clone();
        let response_state = Arc::clone(&self.response_state);
        if let Err(error) = self.app.run_on_main_thread(move || {
            let result = initialize_system_response_handler(&app);
            response_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .finish_retry(result, Instant::now());
        }) {
            self.response_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .finish_retry(
                    Err(NotifyError::ResponseHandler(error.to_string())),
                    Instant::now(),
                );
        }
    }
}

#[cfg(target_os = "macos")]
fn optional_response_handler<T>(initialize: impl FnOnce() -> Result<T, NotifyError>) -> Option<T> {
    initialize().ok()
}

#[cfg(target_os = "macos")]
struct SystemResponseHandler {
    responses: Arc<Mutex<SystemActivationRegistry>>,
    _manager: NotifyManager,
}

#[cfg(target_os = "macos")]
struct SystemResponseState {
    handler: Option<SystemResponseHandler>,
    retry_in_progress: bool,
    retry_after: Instant,
}

#[cfg(target_os = "macos")]
impl SystemResponseState {
    fn new(handler: Option<SystemResponseHandler>) -> Self {
        Self {
            handler,
            retry_in_progress: false,
            retry_after: Instant::now(),
        }
    }

    fn begin_retry(&mut self, now: Instant) -> bool {
        if self.handler.is_some() || self.retry_in_progress || now < self.retry_after {
            return false;
        }
        self.retry_in_progress = true;
        true
    }

    fn finish_retry(&mut self, result: Result<SystemResponseHandler, NotifyError>, now: Instant) {
        self.retry_in_progress = false;
        match result {
            Ok(handler) => self.handler = Some(handler),
            Err(_) => self.retry_after = now + RESPONSE_HANDLER_RETRY_DELAY,
        }
    }

    fn responses(&self) -> Option<Arc<Mutex<SystemActivationRegistry>>> {
        self.handler
            .as_ref()
            .map(|handler| Arc::clone(&handler.responses))
    }
}

#[cfg(target_os = "macos")]
fn system_response_registry(
    state: &Mutex<SystemResponseState>,
    actionable: bool,
) -> Result<Option<Arc<Mutex<SystemActivationRegistry>>>, NotifyError> {
    let responses = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .responses();
    if actionable && responses.is_none() {
        return Err(NotifyError::ResponseHandler(
            "native notification actions are temporarily unavailable".to_owned(),
        ));
    }
    Ok(responses)
}

#[cfg(target_os = "macos")]
fn initialize_system_response_handler(
    app: &AppHandle<Wry>,
) -> Result<SystemResponseHandler, NotifyError> {
    // mac-usernotifications installs its delegate on first use. The app-owned
    // response delegate must be installed afterwards so it remains final.
    system_permission_status(app)?;

    let responses = Arc::new(Mutex::new(SystemActivationRegistry::default()));
    let manager = NotifyManager::try_new("dev.aizu.desktop", None)
        .map_err(|error| NotifyError::ResponseHandler(error.to_string()))?;
    let callback_responses = Arc::clone(&responses);
    manager
        .register(
            Box::new(move |response| {
                if let Some(activation) = take_system_response(
                    &callback_responses,
                    &response.notification_id,
                    &response.action,
                ) {
                    drop(tauri::async_runtime::spawn_blocking(move || {
                        let _ = crate::terminal_activation::activate(&activation);
                    }));
                }
            }),
            Vec::new(),
        )
        .map_err(|error| NotifyError::ResponseHandler(error.to_string()))?;

    Ok(SystemResponseHandler {
        responses,
        _manager: manager,
    })
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
struct SystemActivationEntry {
    token: u64,
    activation: aizu_core::TerminalActivation,
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct SystemActivationRegistry {
    entries: BTreeMap<String, SystemActivationEntry>,
    order: VecDeque<(String, u64)>,
    next_token: u64,
}

#[cfg(target_os = "macos")]
impl SystemActivationRegistry {
    fn replace(
        &mut self,
        identifier: &str,
        activation: Option<aizu_core::TerminalActivation>,
    ) -> Option<u64> {
        self.entries.remove(identifier);
        self.order.retain(|(stored, _)| stored != identifier);
        let activation = activation?;

        while self.entries.len() >= MAX_SYSTEM_ACTIVATIONS {
            let Some((oldest, token)) = self.order.pop_front() else {
                break;
            };
            if self
                .entries
                .get(&oldest)
                .is_some_and(|entry| entry.token == token)
            {
                self.entries.remove(&oldest);
            }
        }

        self.next_token = self.next_token.wrapping_add(1).max(1);
        let token = self.next_token;
        self.entries.insert(
            identifier.to_owned(),
            SystemActivationEntry { token, activation },
        );
        self.order.push_back((identifier.to_owned(), token));
        Some(token)
    }

    fn remove_if(&mut self, identifier: &str, token: u64) {
        if self
            .entries
            .get(identifier)
            .is_some_and(|entry| entry.token == token)
        {
            self.entries.remove(identifier);
            self.order
                .retain(|(stored, stored_token)| stored != identifier || *stored_token != token);
        }
    }

    fn take(&mut self, identifier: &str) -> Option<aizu_core::TerminalActivation> {
        let entry = self.entries.remove(identifier)?;
        self.order.retain(|(stored, _)| stored != identifier);
        Some(entry.activation)
    }
}

#[cfg(target_os = "macos")]
fn take_system_response(
    responses: &Mutex<SystemActivationRegistry>,
    identifier: &str,
    action: &NotifyResponseAction,
) -> Option<aizu_core::TerminalActivation> {
    let activation = responses
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take(identifier);
    matches!(action, NotifyResponseAction::Default)
        .then_some(activation)
        .flatten()
}

impl Notifier for SystemNotifier {
    fn permission_status(&self) -> Result<PermissionStatus, NotifyError> {
        system_permission_status(&self.app)
    }

    fn request_permission(&self) -> Result<PermissionStatus, NotifyError> {
        let permission = request_system_permission(&self.app)?;
        #[cfg(target_os = "macos")]
        self.request_response_handler();
        Ok(permission)
    }

    fn notify(&self, notification: &Notification) -> Result<(), NotifyError> {
        if notification.delivery == NotificationDelivery::AizuBanner {
            return crate::banner::show(&self.app, notification);
        }
        #[cfg(target_os = "macos")]
        {
            self.request_response_handler();
            let responses =
                system_response_registry(&self.response_state, notification.activation.is_some())?;
            show_system_notification(responses.as_deref(), notification)
        }
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
    responses: Option<&Mutex<SystemActivationRegistry>>,
    notification: &Notification,
) -> Result<(), NotifyError> {
    let identifier = macos_notification_id(notification.id);
    let token = responses.and_then(|responses| {
        responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(&identifier, notification.activation.clone())
    });
    let scheduled = mac_usernotifications::blocking::send(build_macos_notification(notification));
    if let (Err(error), Some((responses, token))) = (&scheduled, responses.zip(token)) {
        responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove_if(&identifier, token);
        return Err(NotifyError::Scheduling(error.to_string()));
    }
    scheduled
        .map(drop)
        .map_err(|error| NotifyError::Scheduling(error.to_string()))
}

#[cfg(target_os = "macos")]
fn build_macos_notification(notification: &Notification) -> mac_usernotifications::Notification {
    let identifier = macos_notification_id(notification.id);
    let mut builder = mac_usernotifications::Notification::new()
        .id(&identifier)
        .title(&notification.title)
        .message(&notification.body)
        .interruption_level(mac_usernotifications::InterruptionLevel::Active);

    if let Some(sound) = notification.sound {
        builder = builder.sound(native_sound_name(sound));
    }
    builder
}

#[cfg(target_os = "macos")]
fn macos_notification_id(id: i32) -> String {
    format!("aizu-{:08x}", id.cast_unsigned())
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
    sound.asset_name()
}

#[cfg(not(target_os = "macos"))]
const fn native_sound_name(_sound: crate::model::NotificationSound) -> &'static str {
    "default"
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
        MAX_SYSTEM_ACTIVATIONS, PermissionStatus, SystemActivationRegistry, SystemResponseState,
        build_macos_notification, macos_notification_id, map_macos_permission, map_macos_settings,
        native_sound_name, optional_response_handler, system_response_registry,
        take_system_response,
    };
    use user_notify_reborn::NotifyResponseAction;

    fn iterm_target(session: &str) -> aizu_core::TerminalActivation {
        aizu_core::TerminalActivation {
            application: aizu_core::TerminalApplication::Iterm2,
            application_session: Some(session.to_owned()),
            tmux: None,
        }
    }

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
    fn response_handler_failure_does_not_abort_notifier_startup() {
        let handler = optional_response_handler(|| {
            Err::<u8, _>(super::NotifyError::ResponseHandler(
                "unavailable".to_owned(),
            ))
        });

        assert!(handler.is_none());
    }

    #[test]
    fn actionable_system_notification_waits_for_response_handler_recovery() {
        let state = std::sync::Mutex::new(SystemResponseState::new(None));

        assert!(system_response_registry(&state, false).unwrap().is_none());
        assert!(matches!(
            system_response_registry(&state, true),
            Err(super::NotifyError::ResponseHandler(_))
        ));
    }

    #[test]
    fn response_handler_retries_are_coalesced_and_time_gated() {
        let mut state = SystemResponseState::new(None);
        let now = std::time::Instant::now();

        assert!(state.begin_retry(now));
        assert!(!state.begin_retry(now));
        state.finish_retry(
            Err(super::NotifyError::ResponseHandler(
                "unavailable".to_owned(),
            )),
            now,
        );
        assert!(!state.begin_retry(now));
        assert!(state.begin_retry(now + super::RESPONSE_HANDLER_RETRY_DELAY));
    }

    #[test]
    fn macos_notifications_use_bundled_aizu_sounds() {
        for (sound, asset) in [
            (crate::model::NotificationSound::Default, "aizu-pop.wav"),
            (crate::model::NotificationSound::Chime, "aizu-chime.wav"),
            (crate::model::NotificationSound::Pulse, "aizu-pulse.wav"),
            (crate::model::NotificationSound::Bloom, "aizu-bloom.wav"),
        ] {
            assert_eq!(native_sound_name(sound), asset);
        }
    }

    #[test]
    fn macos_notifications_use_stable_ids_without_per_notification_waiters() {
        let notification = crate::model::Notification {
            id: 42,
            title: "Ready".to_owned(),
            body: "The task completed.".to_owned(),
            sound: Some(crate::model::NotificationSound::Chime),
            delivery: crate::model::NotificationDelivery::System,
            language: crate::model::LanguagePreference::English,
            text_size: crate::model::TextSize::Standard,
            can_activate_terminal: false,
            approval: None,
            activation: None,
        };

        let _native = build_macos_notification(&notification);

        assert_eq!(macos_notification_id(notification.id), "aizu-0000002a");
    }

    #[test]
    fn system_activation_registry_is_globally_bounded_and_keeps_newest_targets() {
        let mut registry = SystemActivationRegistry::default();
        for index in 0..=MAX_SYSTEM_ACTIVATIONS {
            let identifier = format!("aizu-{index:08x}");
            registry.replace(&identifier, Some(iterm_target(&format!("session-{index}"))));
        }

        assert_eq!(registry.entries.len(), MAX_SYSTEM_ACTIVATIONS);
        assert!(registry.take("aizu-00000000").is_none());
        assert_eq!(
            registry
                .take(&format!("aizu-{MAX_SYSTEM_ACTIVATIONS:08x}"))
                .and_then(|target| target.application_session),
            Some(format!("session-{MAX_SYSTEM_ACTIVATIONS}"))
        );
    }

    #[test]
    fn failed_old_schedule_cannot_remove_a_newer_replacement() {
        let mut registry = SystemActivationRegistry::default();
        let old = registry
            .replace("aizu-0000002a", Some(iterm_target("old")))
            .expect("old token");
        registry
            .replace("aizu-0000002a", Some(iterm_target("new")))
            .expect("new token");

        registry.remove_if("aizu-0000002a", old);

        assert_eq!(
            registry
                .take("aizu-0000002a")
                .and_then(|target| target.application_session),
            Some("new".to_owned())
        );
    }

    #[test]
    fn non_actionable_replacement_clears_a_stale_target() {
        let mut registry = SystemActivationRegistry::default();
        registry.replace("aizu-0000002a", Some(iterm_target("old")));

        assert!(registry.replace("aizu-0000002a", None).is_none());
        assert!(registry.take("aizu-0000002a").is_none());
    }

    #[test]
    fn only_default_response_consumes_and_returns_an_activation() {
        let registry = std::sync::Mutex::new(SystemActivationRegistry::default());
        registry
            .lock()
            .expect("registry")
            .replace("aizu-0000002a", Some(iterm_target("dismissed")));

        assert!(
            take_system_response(&registry, "aizu-0000002a", &NotifyResponseAction::Dismiss,)
                .is_none()
        );
        assert!(
            take_system_response(&registry, "aizu-0000002a", &NotifyResponseAction::Default,)
                .is_none()
        );

        registry
            .lock()
            .expect("registry")
            .replace("aizu-0000002a", Some(iterm_target("clicked")));
        assert_eq!(
            take_system_response(&registry, "aizu-0000002a", &NotifyResponseAction::Default,)
                .and_then(|target| target.application_session),
            Some("clicked".to_owned())
        );
    }
}
