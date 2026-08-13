use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionStatus {
    NotDetermined,
    Granted,
    Denied,
    AlertsDisabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrayState {
    Normal,
    Attention,
    Paused,
    Error,
}

impl TrayState {
    pub fn tooltip(self, language: LanguagePreference) -> &'static str {
        match (self, language.prefers_japanese()) {
            (Self::Normal, true) => "Aizu - 監視中",
            (Self::Attention, true) => "Aizu - 確認が必要",
            (Self::Paused, true) => "Aizu - 通知をミュート中",
            (Self::Error, true) => "Aizu - 対応が必要",
            (Self::Normal, _) => "Aizu - monitoring",
            (Self::Attention, _) => "Aizu - attention needed",
            (Self::Paused, _) => "Aizu - notifications muted",
            (Self::Error, _) => "Aizu - action required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    Local,
    RemoteSsh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceStatus {
    Connected,
    Reconnecting,
    Error,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    pub status: SourceStatus,
    pub detail: String,
    pub last_event_at: Option<String>,
    pub action_required: Option<SourceActionRequired>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceActionRequired {
    ConfirmIdentityChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SshConnectionTestStatus {
    Compatible,
    InvalidAlias,
    ConfigurationError,
    NetworkUnavailable,
    AuthenticationRequired,
    HostVerificationFailed,
    MissingRemoteCli,
    IncompatibleProtocol,
    TimedOut,
    RemoteFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConnectionTestResult {
    pub status: SshConnectionTestStatus,
    pub message: String,
    pub config_resolved: bool,
    pub reachable: bool,
    pub protocol_compatible: bool,
    pub remote_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    TaskCompleted,
    AgentQuestion,
    DeliveryGap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DeliveryStatus {
    Pending,
    Delivered,
    Suppressed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEvent {
    pub id: String,
    pub kind: EventKind,
    pub title: String,
    pub summary: Option<String>,
    pub source_name: String,
    pub occurred_at: String,
    pub delivery_status: DeliveryStatus,
    pub outcome: Option<TaskOutcome>,
    #[serde(skip_serializing)]
    pub adapter: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentKind {
    Codex,
    ClaudeCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentRuntimeStatus {
    NotDetected,
    Running,
    Waiting,
    Completed,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HookStatus {
    Configured,
    Missing,
    ApprovalRequired,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMonitorView {
    pub agent: AgentKind,
    pub label: String,
    pub status: AgentRuntimeStatus,
    pub hook_status: HookStatus,
    pub version: Option<String>,
    pub last_seen_at: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningAgentView {
    pub agent: AgentKind,
    pub label: String,
    pub source_id: String,
    pub source_name: String,
    pub source_kind: SourceKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuietHours {
    pub enabled: bool,
    pub start: String,
    pub end: String,
    pub questions_bypass: bool,
}

impl Default for QuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            start: "22:00".to_owned(),
            end: "07:00".to_owned(),
            questions_bypass: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
pub struct Preferences {
    #[serde(default)]
    pub language: LanguagePreference,
    #[serde(default)]
    pub text_size: TextSize,
    pub completion_enabled: bool,
    pub question_enabled: bool,
    #[serde(default)]
    pub agent_details_enabled: bool,
    pub sound_enabled: bool,
    #[serde(default)]
    pub notification_delivery: NotificationDelivery,
    #[serde(default)]
    pub notification_sound: NotificationSound,
    pub privacy_mode: PrivacyMode,
    pub launch_at_login: bool,
    pub quiet_hours: QuietHours,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            language: LanguagePreference::default(),
            text_size: TextSize::default(),
            completion_enabled: true,
            question_enabled: true,
            agent_details_enabled: false,
            sound_enabled: true,
            notification_delivery: NotificationDelivery::default(),
            notification_sound: NotificationSound::default(),
            privacy_mode: PrivacyMode::Generic,
            launch_at_login: false,
            quiet_hours: QuietHours::default(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextSize {
    Small,
    #[default]
    Standard,
    Large,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NotificationDelivery {
    #[default]
    AizuBanner,
    System,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LanguagePreference {
    #[default]
    System,
    #[serde(rename = "ja")]
    Japanese,
    #[serde(rename = "en")]
    English,
}

impl LanguagePreference {
    pub fn prefers_japanese(self) -> bool {
        match self {
            Self::Japanese => true,
            Self::English => false,
            Self::System => sys_locale::get_locale()
                .is_some_and(|locale| locale.to_ascii_lowercase().starts_with("ja")),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NotificationSound {
    #[default]
    Default,
    Glass,
    Ping,
    Pop,
    Hero,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PrivacyMode {
    #[serde(alias = "titles")]
    Generic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CliStatus {
    Installed,
    Missing,
    VersionMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppView {
    pub onboarding_complete: bool,
    pub notification_permission: PermissionStatus,
    pub cli_status: CliStatus,
    pub cli_version: Option<String>,
    pub protocol_version: u16,
    pub app_version: String,
    pub paused: bool,
    pub tray_state: TrayState,
    pub sources: Vec<SourceView>,
    pub agent_monitors: Vec<AgentMonitorView>,
    pub running_agents: Vec<RunningAgentView>,
    pub history: Vec<HistoryEvent>,
    pub preferences: Preferences,
    pub last_event_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteOnboardingRequest {
    pub launch_at_login: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddRemoteSourceRequest {
    pub host_alias: String,
    pub local_label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub id: i32,
    pub title: String,
    pub body: String,
    pub sound: Option<NotificationSound>,
    pub delivery: NotificationDelivery,
    pub language: LanguagePreference,
    pub text_size: TextSize,
}

#[cfg(test)]
mod tests {
    use super::{
        AppView, CliStatus, LanguagePreference, NotificationSound, PermissionStatus, Preferences,
        TextSize, TrayState,
    };

    #[test]
    fn app_view_uses_frontend_contract_field_names() {
        let view = AppView {
            onboarding_complete: false,
            notification_permission: PermissionStatus::NotDetermined,
            cli_status: CliStatus::Missing,
            cli_version: None,
            protocol_version: 1,
            app_version: "0.1.0".to_owned(),
            paused: false,
            tray_state: TrayState::Normal,
            sources: Vec::new(),
            agent_monitors: Vec::new(),
            running_agents: Vec::new(),
            history: Vec::new(),
            preferences: Preferences::default(),
            last_event_at: None,
        };

        let value = serde_json::to_value(view).expect("view should serialize");
        assert_eq!(value["notificationPermission"], "notDetermined");
        assert_eq!(value["cliStatus"], "missing");
        assert_eq!(value["preferences"]["privacyMode"], "generic");
        assert_eq!(value["preferences"]["notificationSound"], "default");
        assert_eq!(value["preferences"]["notificationDelivery"], "aizuBanner");
        assert_eq!(value["preferences"]["language"], "system");
        assert_eq!(value["preferences"]["textSize"], "standard");
    }

    #[test]
    fn preferences_from_older_settings_default_new_fields() {
        let preferences: Preferences = serde_json::from_value(serde_json::json!({
            "completionEnabled": true,
            "questionEnabled": true,
            "soundEnabled": true,
            "privacyMode": "generic",
            "launchAtLogin": false,
            "quietHours": {
                "enabled": false,
                "start": "22:00",
                "end": "07:00",
                "questionsBypass": false
            }
        }))
        .expect("older preferences should remain readable");

        assert_eq!(preferences.notification_sound, NotificationSound::Default);
        assert_eq!(
            preferences.notification_delivery,
            super::NotificationDelivery::AizuBanner
        );
        assert_eq!(preferences.language, LanguagePreference::System);
        assert_eq!(preferences.text_size, TextSize::Standard);
    }
}
