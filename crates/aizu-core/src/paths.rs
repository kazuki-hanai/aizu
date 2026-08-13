use std::path::{Path, PathBuf};

use directories::BaseDirs;

use crate::spool::SpoolError;

/// Filesystem locations used by the CLI-side durable spool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatePaths {
    root: PathBuf,
}

impl StatePaths {
    /// Builds paths under an explicitly selected root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolves the default per-user state directory.
    pub fn discover() -> Result<Self, SpoolError> {
        if let Some(path) = std::env::var_os("AIZU_STATE_DIR")
            && !path.is_empty()
        {
            return Ok(Self::new(path));
        }

        let base = BaseDirs::new().ok_or(SpoolError::StateDirectoryUnavailable)?;
        #[cfg(target_os = "macos")]
        let root = base.data_dir().join("Aizu");
        #[cfg(not(target_os = "macos"))]
        let root = base
            .state_dir()
            .unwrap_or_else(|| base.data_local_dir())
            .join("aizu");

        Ok(Self::new(root))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn spool_db(&self) -> PathBuf {
        self.root.join("spool.sqlite3")
    }

    #[must_use]
    pub fn desktop_db(&self) -> PathBuf {
        self.root.join("desktop.sqlite3")
    }

    #[must_use]
    pub fn identity_backup_dir(&self) -> PathBuf {
        self.root.join("backups")
    }
}
