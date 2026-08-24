use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::Preferences;

const CURRENT_SETTINGS_VERSION: u32 = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSettings {
    #[serde(default)]
    pub settings_version: u32,
    pub onboarding_complete: bool,
    pub paused: bool,
    pub preferences: Preferences,
    pub remote_sources: Vec<RemoteSourceConfig>,
    #[serde(default)]
    pub codex_hook_trust_confirmed: bool,
}

impl Default for StoredSettings {
    fn default() -> Self {
        Self {
            settings_version: CURRENT_SETTINGS_VERSION,
            onboarding_complete: false,
            paused: false,
            preferences: Preferences::default(),
            remote_sources: Vec::new(),
            codex_hook_trust_confirmed: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSourceConfig {
    pub host_alias: String,
    pub local_label: String,
    #[serde(default)]
    pub reconnect_generation: u64,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("settings directory could not be created: {0}")]
    CreateDirectory(io::Error),
    #[error("settings could not be read: {0}")]
    Read(io::Error),
    #[error("settings are not valid JSON: {0}")]
    Decode(serde_json::Error),
    #[error("settings version {found} is newer than the maximum supported version {supported}")]
    UnsupportedVersion { found: u64, supported: u32 },
    #[error("settings could not be encoded: {0}")]
    Encode(serde_json::Error),
    #[error("settings temporary file could not be written: {0}")]
    Write(io::Error),
    #[error("settings file could not be replaced atomically: {0}")]
    Replace(io::Error),
}

pub struct SettingsStore {
    pub(crate) path: PathBuf,
}

impl SettingsStore {
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<StoredSettings, StoreError> {
        reject_symlink(&self.path).map_err(StoreError::Read)?;
        match fs::read(&self.path) {
            Ok(bytes) => {
                // Read the version envelope before decoding the current complete
                // shape. A future version may legitimately remove today's required
                // fields, but it should still get the accurate version error.
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(StoreError::Decode)?;
                let found_version = value
                    .get("settingsVersion")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                if found_version > u64::from(CURRENT_SETTINGS_VERSION) {
                    return Err(StoreError::UnsupportedVersion {
                        found: found_version,
                        supported: CURRENT_SETTINGS_VERSION,
                    });
                }
                let mut settings: StoredSettings =
                    serde_json::from_value(value).map_err(StoreError::Decode)?;
                if settings.settings_version < CURRENT_SETTINGS_VERSION {
                    // Pre-versioned settings cannot distinguish the old `false`
                    // default from an intentional privacy opt-out. Preserve the
                    // stored value; only a missing field receives the new `true`
                    // serde default.
                    //
                    // Command approvals were briefly persisted as default-on in
                    // version 1 but never formed part of the terminal-default
                    // product contract. Require an explicit opt-in after upgrade.
                    if settings.settings_version < 2 {
                        settings.preferences.command_approvals_enabled = false;
                    }
                    if settings.settings_version < 6 {
                        settings.preferences.approval_display =
                            settings.preferences.notification_display;
                    }
                    settings.settings_version = CURRENT_SETTINGS_VERSION;
                    self.save(&settings)?;
                }
                Ok(settings)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(StoredSettings::default()),
            Err(error) => Err(StoreError::Read(error)),
        }
    }

    pub fn save(&self, settings: &StoredSettings) -> Result<(), StoreError> {
        let parent = self.path.parent().ok_or_else(|| {
            StoreError::CreateDirectory(io::Error::new(
                io::ErrorKind::InvalidInput,
                "settings path has no parent",
            ))
        })?;
        fs::create_dir_all(parent).map_err(StoreError::CreateDirectory)?;
        set_private_directory(parent).map_err(StoreError::CreateDirectory)?;
        reject_symlink(&self.path).map_err(StoreError::Write)?;

        let bytes = serde_json::to_vec_pretty(settings).map_err(StoreError::Encode)?;
        let temporary = parent.join(format!(".settings-{}.tmp", uuid::Uuid::new_v4()));
        reject_symlink(&temporary).map_err(StoreError::Write)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(StoreError::Write)?;
        set_private_file(&temporary).map_err(StoreError::Write)?;
        if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(StoreError::Write(error));
        }
        drop(file);
        let result = fs::rename(&temporary, &self.path).map_err(StoreError::Replace);
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result?;
        set_private_file(&self.path).map_err(StoreError::Write)
    }
}

fn reject_symlink(path: &std::path::Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "settings path must not be a symbolic link",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn set_private_file(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file(_path: &std::path::Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_directory(path: &std::path::Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn set_private_directory(_path: &std::path::Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::{CURRENT_SETTINGS_VERSION, SettingsStore, StoredSettings};

    fn temporary_settings_path(test_name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("test clock should be after Unix epoch")
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("aizu-desktop-store-{test_name}-{suffix}"));
        let path = directory.join("settings.json");
        fs::create_dir_all(&directory).expect("temporary settings directory");
        (directory, path)
    }

    #[test]
    fn persists_settings_without_resetting_defaults() {
        let (directory, path) = temporary_settings_path("persist");
        let store = SettingsStore::new(path);
        let mut expected = StoredSettings {
            onboarding_complete: true,
            ..StoredSettings::default()
        };
        expected.preferences.launch_at_login = true;

        store.save(&expected).expect("settings should save");
        let actual = store.load().expect("settings should load");

        assert!(actual.onboarding_complete);
        assert!(actual.preferences.launch_at_login);
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn versions_pre_versioned_settings_without_overriding_agent_details_opt_out() {
        let (directory, path) = temporary_settings_path("migrate-agent-details");
        // Full shape written by the pre-versioned v0.1.0-dev.1 settings model.
        fs::write(
            &path,
            serde_json::json!({
                "onboardingComplete": true,
                "paused": false,
                "preferences": {
                    "language": "system",
                    "textSize": "standard",
                    "completionEnabled": true,
                    "questionEnabled": true,
                    "agentDetailsEnabled": false,
                    "soundEnabled": true,
                    "notificationDelivery": "aizuBanner",
                    "notificationSound": "default",
                    "privacyMode": "generic",
                    "launchAtLogin": false,
                    "quietHours": {
                        "enabled": false,
                        "start": "22:00",
                        "end": "07:00",
                        "questionsBypass": false
                    }
                },
                "remoteSources": [],
                "codexHookTrustConfirmed": false
            })
            .to_string(),
        )
        .expect("write old settings fixture");

        let store = SettingsStore::new(path.clone());
        let migrated = store.load().expect("old settings migrate");
        assert_eq!(migrated.settings_version, CURRENT_SETTINGS_VERSION);
        assert!(!migrated.preferences.agent_details_enabled);
        assert!(!migrated.preferences.command_approvals_enabled);
        assert!(migrated.preferences.center_approval_dialogs);

        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read migrated settings"))
                .expect("valid persisted JSON");
        assert_eq!(
            persisted
                .get("settingsVersion")
                .and_then(serde_json::Value::as_u64),
            Some(u64::from(CURRENT_SETTINGS_VERSION))
        );
        assert_eq!(
            persisted
                .pointer("/preferences/agentDetailsEnabled")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            persisted
                .pointer("/preferences/commandApprovalsEnabled")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn pre_versioned_missing_agent_details_uses_the_new_true_default() {
        let (directory, path) = temporary_settings_path("default-agent-details");
        let fixture = serde_json::json!({
            "onboardingComplete": true,
            "paused": false,
            "preferences": {
                "language": "system",
                "textSize": "standard",
                "completionEnabled": true,
                "questionEnabled": true,
                "soundEnabled": true,
                "notificationDelivery": "aizuBanner",
                "notificationSound": "default",
                "privacyMode": "generic",
                "launchAtLogin": false,
                "quietHours": {
                    "enabled": false,
                    "start": "22:00",
                    "end": "07:00",
                    "questionsBypass": false
                }
            },
            "remoteSources": [],
            "codexHookTrustConfirmed": false
        });
        fs::write(&path, fixture.to_string()).expect("write old settings fixture");

        let store = SettingsStore::new(path);
        let migrated = store.load().expect("old settings migrate");

        assert_eq!(migrated.settings_version, CURRENT_SETTINGS_VERSION);
        assert!(migrated.preferences.agent_details_enabled);
        assert!(!migrated.preferences.command_approvals_enabled);
        assert!(migrated.preferences.center_approval_dialogs);
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn version_one_command_approvals_require_a_fresh_opt_in() {
        let (directory, path) = temporary_settings_path("migrate-command-approvals");
        let settings = StoredSettings {
            settings_version: 1,
            preferences: crate::model::Preferences {
                command_approvals_enabled: true,
                ..crate::model::Preferences::default()
            },
            ..StoredSettings::default()
        };
        fs::write(
            &path,
            serde_json::to_vec(&settings).expect("encode v1 settings"),
        )
        .expect("write v1 settings");

        let store = SettingsStore::new(path.clone());
        let migrated = store.load().expect("v1 settings migrate");

        assert_eq!(migrated.settings_version, CURRENT_SETTINGS_VERSION);
        assert!(!migrated.preferences.command_approvals_enabled);
        let persisted: StoredSettings =
            serde_json::from_slice(&fs::read(&path).expect("read migrated settings"))
                .expect("decode migrated settings");
        assert!(!persisted.preferences.command_approvals_enabled);
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn current_version_preserves_command_approval_opt_in() {
        let (directory, path) = temporary_settings_path("preserve-command-approvals");
        let mut settings = StoredSettings::default();
        settings.preferences.command_approvals_enabled = true;
        let store = SettingsStore::new(path);
        store.save(&settings).expect("save current settings");

        let loaded = store.load().expect("load current settings");
        assert_eq!(loaded.settings_version, CURRENT_SETTINGS_VERSION);
        assert!(loaded.preferences.command_approvals_enabled);
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn version_two_preserves_approval_opt_in_and_defaults_centering_on() {
        let (directory, path) = temporary_settings_path("migrate-approval-centering");
        let mut fixture = serde_json::to_value(StoredSettings::default())
            .expect("encode current settings fixture");
        fixture["settingsVersion"] = serde_json::json!(2);
        fixture["preferences"]["commandApprovalsEnabled"] = serde_json::json!(true);
        fixture["preferences"]
            .as_object_mut()
            .expect("preferences object")
            .remove("centerApprovalDialogs");
        fs::write(
            &path,
            serde_json::to_vec(&fixture).expect("encode v2 settings"),
        )
        .expect("write v2 settings");

        let store = SettingsStore::new(path.clone());
        let migrated = store.load().expect("v2 settings migrate");

        assert_eq!(migrated.settings_version, CURRENT_SETTINGS_VERSION);
        assert!(migrated.preferences.command_approvals_enabled);
        assert!(migrated.preferences.center_approval_dialogs);
        let persisted: StoredSettings =
            serde_json::from_slice(&fs::read(&path).expect("read migrated settings"))
                .expect("decode migrated settings");
        assert!(persisted.preferences.command_approvals_enabled);
        assert!(persisted.preferences.center_approval_dialogs);
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn version_three_defaults_and_persists_the_primary_notification_display() {
        let (directory, path) = temporary_settings_path("migrate-notification-display");
        let mut fixture = serde_json::to_value(StoredSettings::default())
            .expect("encode current settings fixture");
        fixture["settingsVersion"] = serde_json::json!(3);
        fixture["preferences"]
            .as_object_mut()
            .expect("preferences object")
            .remove("notificationDisplay");
        fs::write(
            &path,
            serde_json::to_vec(&fixture).expect("encode v3 settings"),
        )
        .expect("write v3 settings");

        let store = SettingsStore::new(path.clone());
        let migrated = store.load().expect("v3 settings migrate");

        assert_eq!(migrated.settings_version, CURRENT_SETTINGS_VERSION);
        assert_eq!(
            migrated.preferences.notification_display,
            crate::model::NotificationDisplay::Primary
        );
        let persisted: StoredSettings =
            serde_json::from_slice(&fs::read(&path).expect("read migrated settings"))
                .expect("decode migrated settings");
        assert_eq!(
            persisted.preferences.notification_display,
            crate::model::NotificationDisplay::Primary
        );
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn version_four_preserves_the_selected_notification_display() {
        let (directory, path) = temporary_settings_path("migrate-secondary-display");
        let mut fixture = serde_json::to_value(StoredSettings::default())
            .expect("encode current settings fixture");
        fixture["settingsVersion"] = serde_json::json!(4);
        fixture["preferences"]["notificationDisplay"] = serde_json::json!("pointer");
        fs::write(
            &path,
            serde_json::to_vec(&fixture).expect("encode v4 settings"),
        )
        .expect("write v4 settings");

        let store = SettingsStore::new(path.clone());
        let migrated = store.load().expect("v4 settings migrate");

        assert_eq!(migrated.settings_version, CURRENT_SETTINGS_VERSION);
        assert_eq!(
            migrated.preferences.notification_display,
            crate::model::NotificationDisplay::Pointer
        );
        assert_eq!(
            migrated.preferences.approval_display,
            crate::model::NotificationDisplay::Pointer
        );
        let persisted: StoredSettings =
            serde_json::from_slice(&fs::read(&path).expect("read migrated settings"))
                .expect("decode migrated settings");
        assert_eq!(persisted.settings_version, CURRENT_SETTINGS_VERSION);
        assert_eq!(
            persisted.preferences.notification_display,
            crate::model::NotificationDisplay::Pointer
        );
        assert_eq!(
            persisted.preferences.approval_display,
            crate::model::NotificationDisplay::Pointer
        );
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn version_five_copies_the_existing_display_to_approval_display() {
        let (directory, path) = temporary_settings_path("migrate-approval-display");
        let mut fixture = serde_json::to_value(StoredSettings::default())
            .expect("encode current settings fixture");
        fixture["settingsVersion"] = serde_json::json!(5);
        fixture["preferences"]["notificationDisplay"] = serde_json::json!("focusedWindow");
        fixture["preferences"]
            .as_object_mut()
            .expect("preferences object")
            .remove("approvalDisplay");
        fs::write(
            &path,
            serde_json::to_vec(&fixture).expect("encode v5 settings"),
        )
        .expect("write v5 settings");

        let store = SettingsStore::new(path.clone());
        let migrated = store.load().expect("v5 settings migrate");

        assert_eq!(migrated.settings_version, CURRENT_SETTINGS_VERSION);
        assert_eq!(
            migrated.preferences.notification_display,
            crate::model::NotificationDisplay::FocusedWindow
        );
        assert_eq!(
            migrated.preferences.approval_display,
            crate::model::NotificationDisplay::FocusedWindow
        );
        let persisted: StoredSettings =
            serde_json::from_slice(&fs::read(&path).expect("read migrated settings"))
                .expect("decode migrated settings");
        assert_eq!(
            persisted.preferences.approval_display,
            crate::model::NotificationDisplay::FocusedWindow
        );
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn current_version_persists_independent_display_preferences() {
        let (directory, path) = temporary_settings_path("independent-displays");
        let mut settings = StoredSettings::default();
        settings.preferences.notification_display = crate::model::NotificationDisplay::Secondary;
        settings.preferences.approval_display = crate::model::NotificationDisplay::Pointer;
        let store = SettingsStore::new(path);
        store.save(&settings).expect("save independent displays");

        let loaded = store.load().expect("load independent displays");
        assert_eq!(
            loaded.preferences.notification_display,
            crate::model::NotificationDisplay::Secondary
        );
        assert_eq!(
            loaded.preferences.approval_display,
            crate::model::NotificationDisplay::Pointer
        );
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn preserves_an_explicit_agent_details_opt_out_after_migration() {
        let (directory, path) = temporary_settings_path("preserve-opt-out");
        let mut settings = StoredSettings::default();
        settings.preferences.agent_details_enabled = false;
        let store = SettingsStore::new(path);
        store.save(&settings).expect("save versioned opt-out");

        let loaded = store.load().expect("load versioned settings");
        assert_eq!(loaded.settings_version, CURRENT_SETTINGS_VERSION);
        assert!(!loaded.preferences.agent_details_enabled);
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }

    #[test]
    fn rejects_a_future_settings_version_before_decoding_its_shape() {
        let (directory, path) = temporary_settings_path("future-version");
        let future_version = u64::from(CURRENT_SETTINGS_VERSION) + 1;
        fs::write(
            &path,
            serde_json::json!({
                "settingsVersion": future_version,
                "futureShape": true
            })
            .to_string(),
        )
        .expect("write future settings");
        let store = SettingsStore::new(path);

        let error = store.load().expect_err("future settings must be rejected");
        assert!(matches!(
            error,
            super::StoreError::UnsupportedVersion {
                found,
                supported: CURRENT_SETTINGS_VERSION
            } if found == future_version
        ));
        fs::remove_dir_all(directory).expect("temporary directory should be removable");
    }
}
