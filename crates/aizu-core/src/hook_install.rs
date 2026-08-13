//! Safe installation of first-party agent hook configuration files.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use thiserror::Error;

use crate::protocol::ProtocolError;
use crate::{
    AgentKind, IntegrationError, hook_configuration, merge_hook_configuration,
    parse_strict_json_value,
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

/// Maximum accepted size of one agent configuration file.
pub const MAX_AGENT_CONFIG_BYTES: usize = 128 * 1_024;

const TEMP_CREATE_ATTEMPTS: usize = 128;
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
/// Every existing JSON document is parsed and merged before any file is
/// changed. Each changed file is then replaced atomically from a private
/// same-directory staging file. Unrelated keys and hook handlers are retained.
pub fn install_agent_hooks(
    home: &Path,
    executable: &Path,
    agents: &[AgentKind],
) -> Result<Vec<HookInstallResult>, HookInstallError> {
    validate_executable(executable)?;
    let mut updates = Vec::with_capacity(agents.len());
    for &agent in agents {
        let path = resolve_agent_configuration_path(home, agent)?;
        let existed = path.try_exists().map_err(|source| HookInstallError::Io {
            operation: "inspect an agent configuration",
            source,
        })?;
        let existing = read_configuration(&path)?;
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
            outcome,
        });
    }

    for update in &updates {
        if update.outcome != HookInstallOutcome::AlreadyConfigured {
            write_configuration(&update.path, &update.bytes)?;
        }
    }

    Ok(updates
        .into_iter()
        .map(|update| HookInstallResult {
            agent: update.agent,
            outcome: update.outcome,
        })
        .collect())
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
    let canonical_home = fs::canonicalize(home).map_err(|source| HookInstallError::Io {
        operation: "inspect the user home directory",
        source,
    })?;
    if !canonical_home.is_dir() {
        return Err(HookInstallError::UnsafeHome);
    }
    let requested = agent_configuration_path(&canonical_home, agent);
    resolve_configuration_path(&canonical_home, &requested)
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
    outcome: HookInstallOutcome,
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

fn read_configuration(path: &Path) -> Result<serde_json::Value, HookInstallError> {
    reject_symlink(path)?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({}));
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
    parse_strict_json_value(&bytes, MAX_AGENT_CONFIG_BYTES).map_err(Into::into)
}

fn write_configuration(path: &Path, bytes: &[u8]) -> Result<(), HookInstallError> {
    let parent = path.parent().ok_or(HookInstallError::UnsafePath)?;
    let parent_existed = parent.exists();
    fs::create_dir_all(parent).map_err(|source| HookInstallError::Io {
        operation: "create an agent configuration directory",
        source,
    })?;
    reject_symlink(parent)?;
    if !parent.is_dir() {
        return Err(HookInstallError::UnsafePath);
    }
    #[cfg(unix)]
    if !parent_existed {
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|source| {
            HookInstallError::Io {
                operation: "secure an agent configuration directory",
                source,
            }
        })?;
    }
    validate_configuration_directory(parent)?;
    reject_symlink(path)?;

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

fn validate_configuration_directory(path: &Path) -> Result<(), HookInstallError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| HookInstallError::Io {
        operation: "inspect an agent configuration directory",
        source,
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(HookInstallError::UnsafePath);
    }
    #[cfg(unix)]
    if metadata.mode() & 0o022 != 0 {
        return Err(HookInstallError::UnsafePath);
    }
    Ok(())
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
