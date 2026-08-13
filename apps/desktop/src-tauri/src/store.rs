use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::Preferences;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSettings {
    pub onboarding_complete: bool,
    pub paused: bool,
    pub preferences: Preferences,
    pub remote_sources: Vec<RemoteSourceConfig>,
    #[serde(default)]
    pub codex_hook_trust_confirmed: bool,
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
    #[error("settings could not be encoded: {0}")]
    Encode(serde_json::Error),
    #[error("settings temporary file could not be written: {0}")]
    Write(io::Error),
    #[error("settings file could not be replaced atomically: {0}")]
    Replace(io::Error),
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<StoredSettings, StoreError> {
        reject_symlink(&self.path).map_err(StoreError::Read)?;
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(StoreError::Decode),
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

    use super::{SettingsStore, StoredSettings};

    #[test]
    fn persists_settings_without_resetting_defaults() {
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("test clock should be after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("aizu-desktop-store-{suffix}"));
        let path = directory.join("settings.json");
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
}
