//! Safe installation of first-party agent hook configuration files.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

use crate::protocol::ProtocolError;
use crate::{
    AgentKind, HookStatus, IntegrationError, hook_configuration, hook_configuration_status,
    merge_hook_configuration, parse_strict_json_value,
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

/// Maximum accepted size of one agent configuration file.
pub const MAX_AGENT_CONFIG_BYTES: usize = 128 * 1_024;

const TEMP_CREATE_ATTEMPTS: usize = 128;
const INSTALL_LOCK_DIRECTORY: &str = ".aizu";
const INSTALL_LOCK_FILE: &str = "hooks.lock";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Result of installing hooks for one agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookInstallOutcome {
    /// A new agent configuration file was created.
    Created,
    /// An existing configuration was updated while retaining unrelated values.
    Updated,
    /// The required hooks were already installed.
    AlreadyConfigured,
}

/// Result of installing hooks for one first-party agent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HookInstallResult {
    /// Agent whose configuration was inspected.
    pub agent: AgentKind,
    /// Filesystem operation that was required.
    pub outcome: HookInstallOutcome,
}

/// Failures produced while validating or atomically updating agent hooks.
#[derive(Debug, Error)]
pub enum HookInstallError {
    #[error("the user home directory is unavailable or unsafe")]
    UnsafeHome,
    #[error("the Aizu hook executable is not a regular executable file")]
    ExecutableNotRegular,
    #[error("the Aizu hook executable must not be a symlink")]
    ExecutableIsSymlink,
    #[error("an agent configuration path is unsafe")]
    UnsafePath,
    #[error(
        "the ~/{directory} directory is writable by group or others; run `chmod go-w ~/{directory}` and retry"
    )]
    InsecureDirectoryPermissions { directory: &'static str },
    #[error("an agent configuration exceeds the size limit")]
    TooLarge,
    #[error("an agent configuration is not valid JSON")]
    Json(#[from] serde_json::Error),
    #[error("an agent configuration is not valid JSON")]
    StrictJson(#[from] ProtocolError),
    #[error(
        "Claude Code has disableAllHooks enabled; leave it unchanged and enable hooks explicitly before installing Aizu"
    )]
    ClaudeHooksDisabled,
    #[error("an agent configuration has an incompatible hook structure")]
    Integration(#[from] IntegrationError),
    #[error("an agent configuration changed while Aizu was preparing the update")]
    ConcurrentModification,
    #[error("another Aizu hook installation is already running")]
    InstallBusy,
    #[error("could not allocate a temporary hook configuration file")]
    TemporaryFileExhausted,
    #[error("{operation} failed: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

/// Installs first-party hooks for the selected agents beneath `home`.
///
/// Every existing JSON document and target directory is validated before any
/// agent configuration is changed. A per-user lock serializes cooperating Aizu
/// installers, and each changed file is replaced atomically from a private
/// same-directory staging file. Unrelated keys and hook handlers are retained.
///
/// Agent processes and editors do not participate in Aizu's lock. The installer
/// re-reads all inputs while holding the lock and compares each file again just
/// before replacement, but callers should still avoid editing these files while
/// installation is running.
pub fn install_agent_hooks(
    home: &Path,
    executable: &Path,
    agents: &[AgentKind],
) -> Result<Vec<HookInstallResult>, HookInstallError> {
    validate_executable(executable)?;
    let canonical_home = canonical_home(home)?;

    // Fail on malformed JSON and unsafe targets before creating even Aizu's
    // installer lock directory.
    let initial = prepare_updates(&canonical_home, executable, agents)?;
    preflight_update_targets(&canonical_home, &initial)?;

    let _lock = HookInstallLock::acquire(&canonical_home)?;
    let updates = prepare_updates(&canonical_home, executable, agents)?;
    preflight_update_targets(&canonical_home, &updates)?;
    let mut created_directories = create_update_directories(&canonical_home, &updates)?;

    for update in &updates {
        if update.outcome != HookInstallOutcome::AlreadyConfigured {
            write_configuration(
                &canonical_home,
                &update.path,
                &update.bytes,
                update.original_bytes.as_deref(),
            )?;
        }
    }
    created_directories.disarm();

    Ok(updates
        .into_iter()
        .map(|update| HookInstallResult {
            agent: update.agent,
            outcome: update.outcome,
        })
        .collect())
}

/// Inspects the current user's first-party hook configuration without changing
/// any file. Invalid, unsafe, missing, or oversized configurations are reported
/// as missing and no configuration content or path is returned.
#[must_use]
pub fn inspect_agent_hooks(home: &Path, executable: &Path) -> [(AgentKind, HookStatus); 2] {
    [AgentKind::Codex, AgentKind::ClaudeCode]
        .map(|agent| (agent, inspect_agent_hook(home, executable, agent)))
}

fn inspect_agent_hook(home: &Path, executable: &Path, agent: AgentKind) -> HookStatus {
    let Ok(path) = resolve_agent_configuration_path(home, agent) else {
        return HookStatus::Missing;
    };
    let Ok(bytes) = fs::read(path) else {
        return HookStatus::Missing;
    };
    if bytes.len() > MAX_AGENT_CONFIG_BYTES {
        return HookStatus::Missing;
    }
    let Ok(actual) = parse_strict_json_value(&bytes, MAX_AGENT_CONFIG_BYTES) else {
        return HookStatus::Missing;
    };
    hook_configuration_status(agent, &actual, executable)
}

fn prepare_updates(
    home: &Path,
    executable: &Path,
    agents: &[AgentKind],
) -> Result<Vec<PreparedUpdate>, HookInstallError> {
    let mut updates = Vec::with_capacity(agents.len());
    for &agent in agents {
        if updates
            .iter()
            .any(|update: &PreparedUpdate| update.agent == agent)
        {
            continue;
        }
        let path = resolve_agent_configuration_path(home, agent)?;
        let ConfigurationSnapshot {
            value: existing,
            bytes: original_bytes,
        } = read_configuration(&path)?;
        let existed = original_bytes.is_some();
        if agent == AgentKind::ClaudeCode
            && existing
                .get("disableAllHooks")
                .is_some_and(|value| value == true)
        {
            return Err(HookInstallError::ClaudeHooksDisabled);
        }
        let merged = merge_hook_configuration(agent, &existing, executable)?;
        let bytes = serialize_configuration(&merged)?;
        let permission_update = existed && configuration_permissions_need_update(&path)?;
        let outcome = if merged == existing && !permission_update {
            HookInstallOutcome::AlreadyConfigured
        } else if existed {
            HookInstallOutcome::Updated
        } else {
            HookInstallOutcome::Created
        };
        updates.push(PreparedUpdate {
            agent,
            path,
            bytes,
            original_bytes,
            outcome,
        });
    }
    Ok(updates)
}

fn validate_executable(path: &Path) -> Result<(), HookInstallError> {
    // Keep path syntax errors consistent with the read-only generator.
    hook_configuration(AgentKind::Codex, path)?;
    let metadata = fs::symlink_metadata(path).map_err(|source| HookInstallError::Io {
        operation: "inspect the Aizu hook executable",
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(HookInstallError::ExecutableIsSymlink);
    }
    if !metadata.is_file() {
        return Err(HookInstallError::ExecutableNotRegular);
    }
    #[cfg(unix)]
    if metadata.mode() & 0o111 == 0 {
        return Err(HookInstallError::ExecutableNotRegular);
    }
    Ok(())
}

/// Resolves one supported agent's configuration without allowing the target
/// to escape the supplied user home directory.
pub fn resolve_agent_configuration_path(
    home: &Path,
    agent: AgentKind,
) -> Result<PathBuf, HookInstallError> {
    let canonical_home = canonical_home(home)?;
    let requested = agent_configuration_path(&canonical_home, agent);
    resolve_configuration_path(&canonical_home, &requested)
}

fn canonical_home(home: &Path) -> Result<PathBuf, HookInstallError> {
    let canonical_home = fs::canonicalize(home).map_err(|source| HookInstallError::Io {
        operation: "inspect the user home directory",
        source,
    })?;
    if !canonical_home.is_dir() {
        return Err(HookInstallError::UnsafeHome);
    }
    Ok(canonical_home)
}

/// Returns the fixed per-user configuration path for one supported agent.
#[must_use]
pub fn agent_configuration_path(home: &Path, agent: AgentKind) -> PathBuf {
    match agent {
        AgentKind::Codex => home.join(".codex/hooks.json"),
        AgentKind::ClaudeCode => home.join(".claude/settings.json"),
    }
}

struct PreparedUpdate {
    agent: AgentKind,
    path: PathBuf,
    bytes: Vec<u8>,
    original_bytes: Option<Vec<u8>>,
    outcome: HookInstallOutcome,
}

fn preflight_update_targets(
    home: &Path,
    updates: &[PreparedUpdate],
) -> Result<(), HookInstallError> {
    for update in updates {
        let parent = update.path.parent().ok_or(HookInstallError::UnsafePath)?;
        preflight_configuration_directory(home, parent)?;
        reject_symlink(&update.path)?;
        validate_existing_owner(home, &update.path)?;
    }
    Ok(())
}

fn preflight_configuration_directory(
    home: &Path,
    directory: &Path,
) -> Result<(), HookInstallError> {
    if !directory.starts_with(home) || directory == home {
        return Err(HookInstallError::UnsafePath);
    }
    if directory.exists() {
        return validate_configuration_directory(home, directory);
    }

    let mut current = directory;
    while current != home {
        match fs::symlink_metadata(current) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(HookInstallError::UnsafePath);
                }
                validate_existing_owner(home, current)?;
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                current = current.parent().ok_or(HookInstallError::UnsafePath)?;
            }
            Err(source) => {
                return Err(HookInstallError::Io {
                    operation: "inspect an agent configuration directory",
                    source,
                });
            }
        }
    }
    validate_existing_owner(home, home)
}

fn create_update_directories(
    home: &Path,
    updates: &[PreparedUpdate],
) -> Result<CreatedDirectories, HookInstallError> {
    let mut created = CreatedDirectories {
        paths: Vec::new(),
        armed: true,
    };
    for update in updates {
        if update.outcome == HookInstallOutcome::AlreadyConfigured {
            continue;
        }
        let parent = update.path.parent().ok_or(HookInstallError::UnsafePath)?;
        if !parent.exists() {
            create_private_directory(parent)?;
            created.paths.push(parent.to_path_buf());
        }
    }
    for update in updates {
        if update.outcome != HookInstallOutcome::AlreadyConfigured {
            validate_configuration_directory(
                home,
                update.path.parent().ok_or(HookInstallError::UnsafePath)?,
            )?;
        }
    }
    Ok(created)
}

fn create_private_directory(path: &Path) -> Result<(), HookInstallError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|source| HookInstallError::Io {
            operation: "create an agent configuration directory",
            source,
        })?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        HookInstallError::Io {
            operation: "secure an agent configuration directory",
            source,
        }
    })?;
    Ok(())
}

struct CreatedDirectories {
    paths: Vec<PathBuf>,
    armed: bool,
}

impl CreatedDirectories {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CreatedDirectories {
    fn drop(&mut self) {
        if self.armed {
            for path in self.paths.iter().rev() {
                let _ = fs::remove_dir(path);
            }
        }
    }
}

struct HookInstallLock {
    file: File,
}

impl HookInstallLock {
    fn acquire(home: &Path) -> Result<Self, HookInstallError> {
        let directory = home.join(INSTALL_LOCK_DIRECTORY);
        reject_symlink(&directory)?;
        if !directory.exists() {
            create_private_directory(&directory)?;
        }
        validate_configuration_directory(home, &directory)?;

        let path = directory.join(INSTALL_LOCK_FILE);
        reject_symlink(&path)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            options
                .mode(0o600)
                .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed());
        }
        let file = options.open(&path).map_err(|source| HookInstallError::Io {
            operation: "open the hook installation lock",
            source,
        })?;
        validate_lock_file(home, &file)?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            HookInstallError::Io {
                operation: "secure the hook installation lock",
                source,
            }
        })?;
        file.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => HookInstallError::InstallBusy,
            TryLockError::Error(source) => HookInstallError::Io {
                operation: "lock hook installation",
                source,
            },
        })?;
        Ok(Self { file })
    }
}

impl Drop for HookInstallLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn validate_lock_file(home: &Path, file: &File) -> Result<(), HookInstallError> {
    let metadata = file.metadata().map_err(|source| HookInstallError::Io {
        operation: "inspect the hook installation lock",
        source,
    })?;
    if !metadata.is_file() {
        return Err(HookInstallError::UnsafePath);
    }
    #[cfg(unix)]
    {
        if metadata.nlink() != 1
            || metadata.uid()
                != fs::metadata(home)
                    .map_err(|source| HookInstallError::Io {
                        operation: "inspect the user home directory",
                        source,
                    })?
                    .uid()
        {
            return Err(HookInstallError::UnsafePath);
        }
    }
    Ok(())
}

fn resolve_configuration_path(home: &Path, path: &Path) -> Result<PathBuf, HookInstallError> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        && fs::metadata(path).is_err()
    {
        return Err(HookInstallError::UnsafePath);
    }
    let resolved = match fs::canonicalize(path) {
        Ok(resolved) => {
            if !resolved.is_file() {
                return Err(HookInstallError::UnsafePath);
            }
            resolved
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(HookInstallError::UnsafePath)?;
            match fs::canonicalize(parent) {
                Ok(parent) => parent.join(path.file_name().ok_or(HookInstallError::UnsafePath)?),
                Err(parent_error) if parent_error.kind() == io::ErrorKind::NotFound => {
                    path.to_path_buf()
                }
                Err(source) => {
                    return Err(HookInstallError::Io {
                        operation: "resolve an agent configuration directory",
                        source,
                    });
                }
            }
        }
        Err(source) => {
            return Err(HookInstallError::Io {
                operation: "resolve an agent configuration",
                source,
            });
        }
    };
    if !resolved.starts_with(home) {
        return Err(HookInstallError::UnsafePath);
    }
    validate_existing_owner(home, &resolved)?;
    Ok(resolved)
}

fn validate_existing_owner(home: &Path, path: &Path) -> Result<(), HookInstallError> {
    #[cfg(unix)]
    {
        let owner = fs::metadata(home)
            .map_err(|source| HookInstallError::Io {
                operation: "inspect the user home directory",
                source,
            })?
            .uid();
        let existing = path
            .ancestors()
            .find(|ancestor| ancestor.exists())
            .ok_or(HookInstallError::UnsafePath)?;
        if fs::metadata(existing)
            .map_err(|source| HookInstallError::Io {
                operation: "inspect an agent configuration owner",
                source,
            })?
            .uid()
            != owner
        {
            return Err(HookInstallError::UnsafePath);
        }
    }
    Ok(())
}

struct ConfigurationSnapshot {
    value: serde_json::Value,
    bytes: Option<Vec<u8>>,
}

fn read_configuration(path: &Path) -> Result<ConfigurationSnapshot, HookInstallError> {
    let Some(bytes) = read_configuration_bytes(path)? else {
        return Ok(ConfigurationSnapshot {
            value: serde_json::json!({}),
            bytes: None,
        });
    };
    let value = parse_strict_json_value(&bytes, MAX_AGENT_CONFIG_BYTES)?;
    Ok(ConfigurationSnapshot {
        value,
        bytes: Some(bytes),
    })
}

fn read_configuration_bytes(path: &Path) -> Result<Option<Vec<u8>>, HookInstallError> {
    reject_symlink(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(source) => {
            return Err(HookInstallError::Io {
                operation: "read an agent configuration",
                source,
            });
        }
    };
    let mut bytes = Vec::new();
    file.take((MAX_AGENT_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| HookInstallError::Io {
            operation: "read an agent configuration",
            source,
        })?;
    if bytes.len() > MAX_AGENT_CONFIG_BYTES {
        return Err(HookInstallError::TooLarge);
    }
    Ok(Some(bytes))
}

fn write_configuration(
    home: &Path,
    path: &Path,
    bytes: &[u8],
    original_bytes: Option<&[u8]>,
) -> Result<(), HookInstallError> {
    let parent = path.parent().ok_or(HookInstallError::UnsafePath)?;
    validate_configuration_directory(home, parent)?;
    reject_symlink(path)?;
    ensure_configuration_unchanged(path, original_bytes)?;

    let (temporary_path, mut temporary_file) = create_temporary(parent)?;
    let mut temporary = TemporaryFile::new(temporary_path);
    let result = (|| {
        temporary_file
            .write_all(bytes)
            .map_err(|source| HookInstallError::Io {
                operation: "write a staged agent configuration",
                source,
            })?;
        temporary_file
            .sync_all()
            .map_err(|source| HookInstallError::Io {
                operation: "sync a staged agent configuration",
                source,
            })?;
        drop(temporary_file);
        reject_symlink(path)?;
        ensure_configuration_unchanged(path, original_bytes)?;
        fs::rename(temporary.path(), path).map_err(|source| HookInstallError::Io {
            operation: "replace an agent configuration",
            source,
        })?;
        temporary.disarm();
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            HookInstallError::Io {
                operation: "secure an agent configuration",
                source,
            }
        })?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary.path());
    }
    result
}

fn ensure_configuration_unchanged(
    path: &Path,
    expected: Option<&[u8]>,
) -> Result<(), HookInstallError> {
    let current = read_configuration_bytes(path)?;
    if current.as_deref() == expected {
        Ok(())
    } else {
        Err(HookInstallError::ConcurrentModification)
    }
}

fn serialize_configuration(configuration: &serde_json::Value) -> Result<Vec<u8>, HookInstallError> {
    let mut bytes = serde_json::to_vec_pretty(configuration)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_AGENT_CONFIG_BYTES {
        return Err(HookInstallError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn configuration_permissions_need_update(path: &Path) -> Result<bool, HookInstallError> {
    let metadata = fs::metadata(path).map_err(|source| HookInstallError::Io {
        operation: "inspect agent configuration permissions",
        source,
    })?;
    Ok(metadata.mode() & 0o777 != 0o600)
}

#[cfg(not(unix))]
fn configuration_permissions_need_update(_path: &Path) -> Result<bool, HookInstallError> {
    Ok(false)
}

fn validate_configuration_directory(home: &Path, path: &Path) -> Result<(), HookInstallError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| HookInstallError::Io {
        operation: "inspect an agent configuration directory",
        source,
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(HookInstallError::UnsafePath);
    }
    #[cfg(unix)]
    {
        let home_owner = fs::metadata(home)
            .map_err(|source| HookInstallError::Io {
                operation: "inspect the user home directory",
                source,
            })?
            .uid();
        validate_unix_directory_security(
            configuration_directory_name(home, path),
            metadata.mode(),
            metadata.uid(),
            home_owner,
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn validate_unix_directory_security(
    directory: Option<&'static str>,
    mode: u32,
    owner: u32,
    home_owner: u32,
) -> Result<(), HookInstallError> {
    if owner != home_owner {
        return Err(HookInstallError::UnsafePath);
    }
    if mode & 0o022 != 0 {
        return Err(HookInstallError::InsecureDirectoryPermissions {
            directory: directory.ok_or(HookInstallError::UnsafePath)?,
        });
    }
    Ok(())
}

fn configuration_directory_name(home: &Path, path: &Path) -> Option<&'static str> {
    if path == home.join(".codex") {
        Some(".codex")
    } else if path == home.join(".claude") {
        Some(".claude")
    } else if path == home.join(INSTALL_LOCK_DIRECTORY) {
        Some(INSTALL_LOCK_DIRECTORY)
    } else {
        None
    }
}

fn create_temporary(parent: &Path) -> Result<(PathBuf, File), HookInstallError> {
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = OsString::from(".aizu-hooks-");
        name.push(format!("{}-{sequence}.tmp", std::process::id()));
        let path = parent.join(name);
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(HookInstallError::Io {
                    operation: "create a staged agent configuration",
                    source,
                });
            }
        }
    }
    Err(HookInstallError::TemporaryFileExhausted)
}

fn reject_symlink(path: &Path) -> Result<(), HookInstallError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(HookInstallError::UnsafePath),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(HookInstallError::Io {
            operation: "inspect an agent configuration path",
            source,
        }),
    }
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> Result<(), HookInstallError> {
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| HookInstallError::Io {
            operation: "sync an agent configuration directory",
            source,
        })
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<(), HookInstallError> {
    Ok(())
}

struct TemporaryFile {
    path: PathBuf,
    armed: bool,
}

impl TemporaryFile {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    fn executable(home: &Path) -> PathBuf {
        let path = home.join(".local/bin/aizu");
        fs::create_dir_all(path.parent().unwrap()).expect("executable directory");
        fs::write(&path, b"aizu test executable").expect("test executable");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
        path
    }

    #[test]
    fn installs_both_agents_preserves_existing_hooks_and_is_idempotent() {
        let home = tempfile::tempdir().expect("temporary home");
        fs::create_dir(home.path().join(".codex")).expect("Codex directory");
        fs::write(
            home.path().join(".codex/hooks.json"),
            serde_json::to_vec(&json!({
                "existing": true,
                "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "other"}]}]}
            }))
            .expect("serialize fixture"),
        )
        .expect("Codex configuration");

        let executable = executable(home.path());
        let first = install_agent_hooks(
            home.path(),
            &executable,
            &[AgentKind::Codex, AgentKind::ClaudeCode],
        )
        .expect("install hooks");
        assert_eq!(first[0].outcome, HookInstallOutcome::Updated);
        assert_eq!(first[1].outcome, HookInstallOutcome::Created);

        let codex: serde_json::Value = serde_json::from_slice(
            &fs::read(home.path().join(".codex/hooks.json")).expect("read Codex configuration"),
        )
        .expect("parse Codex configuration");
        assert_eq!(codex["existing"], true);
        assert_eq!(codex["hooks"]["Stop"].as_array().unwrap().len(), 2);
        let claude: serde_json::Value = serde_json::from_slice(
            &fs::read(home.path().join(".claude/settings.json"))
                .expect("read Claude configuration"),
        )
        .expect("parse Claude configuration");
        assert!(claude["hooks"]["StopFailure"].is_array());

        let second = install_agent_hooks(
            home.path(),
            &executable,
            &[AgentKind::Codex, AgentKind::ClaudeCode],
        )
        .expect("repeat install");
        assert!(
            second
                .iter()
                .all(|result| result.outcome == HookInstallOutcome::AlreadyConfigured)
        );

        assert_eq!(
            inspect_agent_hooks(home.path(), &executable),
            [
                (AgentKind::Codex, HookStatus::ApprovalRequired),
                (AgentKind::ClaudeCode, HookStatus::Configured),
            ]
        );
    }

    #[test]
    fn read_only_hook_inspection_fails_closed() {
        let home = tempfile::tempdir().expect("temporary home");
        fs::create_dir(home.path().join(".claude")).expect("Claude directory");
        fs::write(
            home.path().join(".claude/settings.json"),
            br#"{"disableAllHooks":true,"private":"do-not-report"}"#,
        )
        .expect("Claude configuration");
        let executable = executable(home.path());

        assert_eq!(
            inspect_agent_hooks(home.path(), &executable),
            [
                (AgentKind::Codex, HookStatus::Missing),
                (AgentKind::ClaudeCode, HookStatus::Missing),
            ]
        );
    }

    #[test]
    fn explicit_claude_install_does_not_override_global_hook_disable() {
        let home = tempfile::tempdir().expect("temporary home");
        fs::create_dir(home.path().join(".claude")).expect("Claude directory");
        fs::write(
            home.path().join(".claude/settings.json"),
            br#"{"disableAllHooks":true,"theme":"dark"}"#,
        )
        .expect("Claude configuration");

        let executable = executable(home.path());
        assert!(matches!(
            install_agent_hooks(home.path(), &executable, &[AgentKind::ClaudeCode],),
            Err(HookInstallError::ClaudeHooksDisabled)
        ));
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(home.path().join(".claude/settings.json"))
                .expect("read Claude configuration"),
        )
        .expect("parse Claude configuration");
        assert_eq!(value["disableAllHooks"], true);
        assert_eq!(value["theme"], "dark");
        assert!(value.get("hooks").is_none());
    }

    #[test]
    fn malformed_or_oversized_configuration_is_unchanged() {
        let home = tempfile::tempdir().expect("temporary home");
        fs::create_dir(home.path().join(".codex")).expect("Codex directory");
        let path = home.path().join(".codex/hooks.json");
        fs::write(&path, b"not json").expect("malformed configuration");
        let executable = executable(home.path());
        assert!(matches!(
            install_agent_hooks(home.path(), &executable, &[AgentKind::Codex]),
            Err(HookInstallError::StrictJson(_))
        ));
        assert_eq!(fs::read(&path).expect("read unchanged file"), b"not json");

        fs::write(&path, vec![b' '; MAX_AGENT_CONFIG_BYTES + 1]).expect("oversized configuration");
        assert!(matches!(
            install_agent_hooks(home.path(), &executable, &[AgentKind::Codex]),
            Err(HookInstallError::TooLarge)
        ));
        assert_eq!(
            fs::metadata(path).expect("unchanged oversized file").len(),
            (MAX_AGENT_CONFIG_BYTES + 1) as u64
        );
    }

    #[test]
    fn duplicate_json_keys_are_rejected_without_modification() {
        let home = tempfile::tempdir().expect("temporary home");
        fs::create_dir(home.path().join(".codex")).expect("Codex directory");
        let path = home.path().join(".codex/hooks.json");
        let original = br#"{"hooks":{},"hooks":{}}"#;
        fs::write(&path, original).expect("duplicate-key configuration");
        let executable = executable(home.path());
        assert!(matches!(
            install_agent_hooks(home.path(), &executable, &[AgentKind::Codex]),
            Err(HookInstallError::StrictJson(_))
        ));
        assert_eq!(fs::read(path).expect("unchanged configuration"), original);
    }

    #[test]
    fn stale_prepared_update_does_not_overwrite_a_concurrent_change() {
        let home = tempfile::tempdir().expect("temporary home");
        let directory = home.path().join(".codex");
        fs::create_dir(&directory).expect("Codex directory");
        let path = directory.join("hooks.json");
        let original = b"{}\n";
        fs::write(&path, original).expect("initial configuration");
        let executable = executable(home.path());
        let merged =
            merge_hook_configuration(AgentKind::Codex, &serde_json::json!({}), &executable)
                .expect("merge configuration");
        let bytes = serialize_configuration(&merged).expect("serialize configuration");

        let concurrent = br#"{"updatedByAgent":true}"#;
        fs::write(&path, concurrent).expect("concurrent update");
        assert!(matches!(
            write_configuration(home.path(), &path, &bytes, Some(original)),
            Err(HookInstallError::ConcurrentModification)
        ));
        assert_eq!(fs::read(path).expect("preserved update"), concurrent);
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_second_directory_does_not_modify_first_configuration() {
        let home = tempfile::tempdir().expect("temporary home");
        let codex_directory = home.path().join(".codex");
        let claude_directory = home.path().join(".claude");
        fs::create_dir(&codex_directory).expect("Codex directory");
        fs::create_dir(&claude_directory).expect("Claude directory");
        let codex_path = codex_directory.join("hooks.json");
        let original = br#"{"existing":true}"#;
        fs::write(&codex_path, original).expect("Codex configuration");
        fs::write(claude_directory.join("settings.json"), b"{}\n").expect("Claude configuration");
        fs::set_permissions(&claude_directory, fs::Permissions::from_mode(0o777))
            .expect("unsafe Claude permissions");

        let executable = executable(home.path());
        let error = install_agent_hooks(
            home.path(),
            &executable,
            &[AgentKind::Codex, AgentKind::ClaudeCode],
        )
        .expect_err("unsafe Claude permissions");
        assert!(matches!(
            error,
            HookInstallError::InsecureDirectoryPermissions {
                directory: ".claude"
            }
        ));
        assert_eq!(
            error.to_string(),
            "the ~/.claude directory is writable by group or others; run `chmod go-w ~/.claude` and retry"
        );
        assert_eq!(
            fs::read(codex_path).expect("unchanged Codex configuration"),
            original
        );
        assert!(!home.path().join(INSTALL_LOCK_DIRECTORY).exists());
    }

    #[cfg(unix)]
    #[test]
    fn foreign_owner_takes_precedence_over_writable_mode_diagnostic() {
        assert!(matches!(
            validate_unix_directory_security(Some(".codex"), 0o775, 0, 1_000),
            Err(HookInstallError::UnsafePath)
        ));
        assert!(matches!(
            validate_unix_directory_security(Some(".codex"), 0o775, 1_000, 1_000),
            Err(HookInstallError::InsecureDirectoryPermissions {
                directory: ".codex"
            })
        ));
    }

    #[test]
    fn malformed_second_configuration_creates_no_first_agent_directory() {
        let home = tempfile::tempdir().expect("temporary home");
        let claude_directory = home.path().join(".claude");
        fs::create_dir(&claude_directory).expect("Claude directory");
        fs::write(claude_directory.join("settings.json"), b"not json")
            .expect("malformed Claude configuration");

        let executable = executable(home.path());
        assert!(matches!(
            install_agent_hooks(
                home.path(),
                &executable,
                &[AgentKind::Codex, AgentKind::ClaudeCode],
            ),
            Err(HookInstallError::StrictJson(_))
        ));
        assert!(!home.path().join(".codex").exists());
        assert!(!home.path().join(INSTALL_LOCK_DIRECTORY).exists());
    }

    #[test]
    fn concurrent_aizu_install_returns_busy_without_changing_configuration() {
        let home = tempfile::tempdir().expect("temporary home");
        let codex_directory = home.path().join(".codex");
        fs::create_dir(&codex_directory).expect("Codex directory");
        let path = codex_directory.join("hooks.json");
        let original = b"{}\n";
        fs::write(&path, original).expect("Codex configuration");
        let executable = executable(home.path());
        let canonical = canonical_home(home.path()).expect("canonical home");
        let _lock = HookInstallLock::acquire(&canonical).expect("installation lock");

        assert!(matches!(
            install_agent_hooks(home.path(), &executable, &[AgentKind::Codex]),
            Err(HookInstallError::InstallBusy)
        ));
        assert_eq!(
            fs::read(path).expect("unchanged Codex configuration"),
            original
        );
    }

    #[cfg(unix)]
    #[test]
    fn configured_file_permissions_are_repaired_and_new_files_are_private() {
        let home = tempfile::tempdir().expect("temporary home");
        let codex_directory = home.path().join(".codex");
        fs::create_dir(&codex_directory).expect("Codex directory");
        let executable = executable(home.path());
        let configured =
            merge_hook_configuration(AgentKind::Codex, &serde_json::json!({}), &executable)
                .expect("configured hooks");
        let path = codex_directory.join("hooks.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&configured).expect("serialize hooks"),
        )
        .expect("Codex configuration");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("public permissions");

        let result = install_agent_hooks(home.path(), &executable, &[AgentKind::Codex])
            .expect("repair permissions");
        assert_eq!(result[0].outcome, HookInstallOutcome::Updated);
        assert_eq!(fs::metadata(&path).expect("metadata").mode() & 0o777, 0o600);

        install_agent_hooks(home.path(), &executable, &[AgentKind::ClaudeCode])
            .expect("create Claude configuration");
        let claude_directory = home.path().join(".claude");
        assert_eq!(
            fs::metadata(&claude_directory).expect("metadata").mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(claude_directory.join("settings.json"))
                .expect("metadata")
                .mode()
                & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn safe_home_symlink_is_preserved_and_external_or_dangling_links_are_rejected() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().expect("temporary home");
        let dotfiles = home.path().join(".dotfiles/claude");
        let claude = home.path().join(".claude");
        fs::create_dir_all(&dotfiles).expect("dotfiles directory");
        fs::create_dir(&claude).expect("Claude directory");
        let target = dotfiles.join("settings.json");
        fs::write(&target, b"{}\n").expect("target configuration");
        let link = claude.join("settings.json");
        symlink(&target, &link).expect("configuration symlink");

        let executable = executable(home.path());
        install_agent_hooks(home.path(), &executable, &[AgentKind::ClaudeCode])
            .expect("install through safe symlink");
        assert!(
            fs::symlink_metadata(&link)
                .expect("link remains")
                .file_type()
                .is_symlink()
        );
        let installed: serde_json::Value =
            serde_json::from_slice(&fs::read(&target).expect("read target")).expect("parse target");
        assert!(installed["hooks"]["Stop"].is_array());

        fs::remove_file(&link).expect("remove safe link");
        let external = tempfile::tempdir().expect("external directory");
        let external_target = external.path().join("settings.json");
        fs::write(&external_target, b"{}\n").expect("external target");
        symlink(&external_target, &link).expect("external symlink");
        assert!(matches!(
            install_agent_hooks(home.path(), &executable, &[AgentKind::ClaudeCode]),
            Err(HookInstallError::UnsafePath)
        ));

        fs::remove_file(&link).expect("remove external link");
        symlink(home.path().join("missing.json"), &link).expect("dangling symlink");
        assert!(matches!(
            install_agent_hooks(home.path(), &executable, &[AgentKind::ClaudeCode]),
            Err(HookInstallError::UnsafePath)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn missing_non_executable_and_symlinked_executables_are_rejected() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().expect("temporary home");
        let path = home.path().join("aizu");
        assert!(matches!(
            install_agent_hooks(home.path(), &path, &[AgentKind::Codex]),
            Err(HookInstallError::Io { .. })
        ));
        fs::write(&path, b"not executable").expect("plain file");
        assert!(matches!(
            install_agent_hooks(home.path(), &path, &[AgentKind::Codex]),
            Err(HookInstallError::ExecutableNotRegular)
        ));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
        let link = home.path().join("aizu-link");
        symlink(&path, &link).expect("executable symlink");
        assert!(matches!(
            install_agent_hooks(home.path(), &link, &[AgentKind::Codex]),
            Err(HookInstallError::ExecutableIsSymlink)
        ));
    }
}
