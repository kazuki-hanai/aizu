use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use aizu_core::{
    AgentKind, HookStatus, MAX_PROCESS_SNAPSHOT_ENTRIES, ObservedAgentProcess,
    ProcessLifecycleState, ProcessSnapshot, ProcessSnapshotError, hook_configuration,
    merge_hook_configuration,
};
use chrono::Utc;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use thiserror::Error;

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_VERSION_BYTES: u64 = 4 * 1_024;
const MAX_AGENT_CONFIG_BYTES: usize = 128 * 1_024;

pub type AgentVersions = [(AgentKind, Option<String>); 2];
pub type AgentHooks = [(AgentKind, HookStatus); 2];

pub struct ProcessMonitor {
    system: System,
    previous: BTreeMap<NonZeroU32, AgentKind>,
}

pub fn inspect_versions() -> AgentVersions {
    [AgentKind::Codex, AgentKind::ClaudeCode].map(|agent| (agent, inspect_version(agent)))
}

fn inspect_version(agent: AgentKind) -> Option<String> {
    let spec = agent.version_command();
    let mut child = Command::new(spec.executable)
        .args(spec.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(MAX_VERSION_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()
            .map(|_| bytes)
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().ok()? {
            break status;
        }
        if started.elapsed() >= VERSION_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let bytes = reader.join().ok()??;
    if !status.success() || bytes.len() > usize::try_from(MAX_VERSION_BYTES).ok()? {
        return None;
    }
    let output = String::from_utf8(bytes).ok()?;
    let line = output.lines().next()?.trim();
    let sanitized: String = line
        .chars()
        .filter(|character| !character.is_control())
        .take(100)
        .collect();
    (!sanitized.is_empty()).then_some(sanitized)
}

impl ProcessMonitor {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            previous: BTreeMap::new(),
        }
    }

    pub fn snapshot(&mut self) -> Result<ProcessSnapshot, ProcessSnapshotError> {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_exe(UpdateKind::OnlyIfNotSet),
        );
        let running: Vec<_> = self
            .system
            .processes()
            .iter()
            .filter_map(|(pid, process)| {
                let agent = classify_agent_process(process.name(), process.exe())?;
                Some(ObservedAgentProcess::running(
                    agent,
                    NonZeroU32::new(pid.as_u32())?,
                ))
            })
            .take(MAX_PROCESS_SNAPSHOT_ENTRIES)
            .collect();
        let current: BTreeMap<_, _> = running
            .iter()
            .map(|process| (process.process_id, process.agent))
            .collect();
        let mut processes = running;
        for (pid, agent) in &self.previous {
            if processes.len() == MAX_PROCESS_SNAPSHOT_ENTRIES || current.contains_key(pid) {
                continue;
            }
            let mut process = ObservedAgentProcess::running(*agent, *pid);
            process
                .transition(ProcessLifecycleState::NoLongerObserved)
                .expect("running process may disappear");
            processes.push(process);
        }
        self.previous = current;
        ProcessSnapshot::new(Utc::now(), processes)
    }
}

fn classify_agent_process(name: &std::ffi::OsStr, executable: Option<&Path>) -> Option<AgentKind> {
    if executable.is_some_and(is_embedded_app_executable) {
        return None;
    }
    classify_agent_executable(name).or_else(|| {
        executable
            .and_then(Path::file_name)
            .and_then(classify_agent_executable)
    })
}

fn is_embedded_app_executable(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|component| component.to_ascii_lowercase().ends_with(".app"))
    })
}

fn classify_agent_executable(name: &std::ffi::OsStr) -> Option<AgentKind> {
    let name = name.to_str()?;
    let normalized = name.strip_suffix(".exe").unwrap_or(name);
    if normalized.eq_ignore_ascii_case("codex") {
        Some(AgentKind::Codex)
    } else if normalized.eq_ignore_ascii_case("claude") {
        Some(AgentKind::ClaudeCode)
    } else {
        None
    }
}

pub fn inspect_hooks() -> AgentHooks {
    [AgentKind::Codex, AgentKind::ClaudeCode].map(|agent| (agent, inspect_agent_hooks(agent)))
}

pub fn configure_hooks() -> Result<AgentHooks, AgentHookInstallError> {
    let base = directories::BaseDirs::new().ok_or(AgentHookInstallError::HomeUnavailable)?;
    let executable = base.home_dir().join(".local/bin/aizu");
    configure_hooks_at(base.home_dir(), &executable)?;
    Ok(inspect_hooks())
}

fn configure_hooks_at(home: &Path, executable: &Path) -> Result<(), AgentHookInstallError> {
    let mut updates = Vec::new();
    for agent in [AgentKind::Codex, AgentKind::ClaudeCode] {
        let path = resolve_configuration_path(home, &configuration_path(home, agent))?;
        let existing = read_configuration(&path)?;
        let merged = merge_hook_configuration(agent, &existing, executable)?;
        updates.push((path, merged));
    }
    for (path, configuration) in updates {
        write_configuration(&path, &configuration)?;
    }
    Ok(())
}

fn inspect_agent_hooks(agent: AgentKind) -> HookStatus {
    let Some(base) = directories::BaseDirs::new() else {
        return HookStatus::Missing;
    };
    let Ok(path) =
        resolve_configuration_path(base.home_dir(), &configuration_path(base.home_dir(), agent))
    else {
        return HookStatus::Missing;
    };
    let Ok(bytes) = fs::read(path) else {
        return HookStatus::Missing;
    };
    if bytes.len() > MAX_AGENT_CONFIG_BYTES {
        return HookStatus::Missing;
    }
    let Ok(actual) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return HookStatus::Missing;
    };
    let executable = base.home_dir().join(".local/bin/aizu");
    let Ok(expected) = hook_configuration(agent, &executable) else {
        return HookStatus::Missing;
    };
    configuration_status(agent, &actual, &expected)
}

fn configuration_status(
    agent: AgentKind,
    actual: &serde_json::Value,
    expected: &serde_json::Value,
) -> HookStatus {
    if agent == AgentKind::ClaudeCode
        && actual
            .get("disableAllHooks")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    {
        return HookStatus::Missing;
    }
    let Some(expected_hooks) = expected.get("hooks").and_then(serde_json::Value::as_object) else {
        return HookStatus::Missing;
    };
    let Some(actual_hooks) = actual.get("hooks").and_then(serde_json::Value::as_object) else {
        return HookStatus::Missing;
    };
    let configured = expected_hooks.iter().all(|(event, groups)| {
        let expected_handlers = handlers(groups);
        actual_hooks.get(event).is_some_and(|actual_groups| {
            expected_handlers
                .iter()
                .all(|handler| handlers(actual_groups).contains(handler))
        })
    });
    if configured && agent == AgentKind::Codex {
        HookStatus::ApprovalRequired
    } else if configured {
        HookStatus::Configured
    } else {
        HookStatus::Missing
    }
}

fn configuration_path(home: &Path, agent: AgentKind) -> PathBuf {
    match agent {
        AgentKind::Codex => home.join(".codex/hooks.json"),
        AgentKind::ClaudeCode => home.join(".claude/settings.json"),
    }
}

fn resolve_configuration_path(home: &Path, path: &Path) -> Result<PathBuf, AgentHookInstallError> {
    let canonical_home = fs::canonicalize(home)?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        && fs::metadata(path).is_err()
    {
        return Err(AgentHookInstallError::UnsafePath);
    }
    let resolved = match fs::canonicalize(path) {
        Ok(resolved) => {
            if !resolved.is_file() {
                return Err(AgentHookInstallError::UnsafePath);
            }
            resolved
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(AgentHookInstallError::UnsafePath)?;
            match fs::canonicalize(parent) {
                Ok(parent) => {
                    parent.join(path.file_name().ok_or(AgentHookInstallError::UnsafePath)?)
                }
                Err(parent_error) if parent_error.kind() == std::io::ErrorKind::NotFound => {
                    path.to_path_buf()
                }
                Err(parent_error) => return Err(AgentHookInstallError::Io(parent_error)),
            }
        }
        Err(error) => return Err(AgentHookInstallError::Io(error)),
    };
    if !resolved.starts_with(&canonical_home) {
        return Err(AgentHookInstallError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = fs::metadata(&canonical_home)?.uid();
        let owner_path = resolved
            .ancestors()
            .find(|ancestor| ancestor.exists())
            .ok_or(AgentHookInstallError::UnsafePath)?;
        if fs::metadata(owner_path)?.uid() != owner {
            return Err(AgentHookInstallError::UnsafePath);
        }
    }
    Ok(resolved)
}

fn read_configuration(path: &Path) -> Result<serde_json::Value, AgentHookInstallError> {
    reject_symlink(path)?;
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({}));
        }
        Err(error) => return Err(AgentHookInstallError::Io(error)),
    };
    if bytes.len() > MAX_AGENT_CONFIG_BYTES {
        return Err(AgentHookInstallError::TooLarge);
    }
    serde_json::from_slice(&bytes).map_err(AgentHookInstallError::Json)
}

fn write_configuration(
    path: &Path,
    configuration: &serde_json::Value,
) -> Result<(), AgentHookInstallError> {
    let parent = path.parent().ok_or(AgentHookInstallError::UnsafePath)?;
    fs::create_dir_all(parent)?;
    reject_symlink(parent)?;
    if !parent.is_dir() {
        return Err(AgentHookInstallError::UnsafePath);
    }
    let bytes = serde_json::to_vec_pretty(configuration)?;
    if bytes.len() > MAX_AGENT_CONFIG_BYTES {
        return Err(AgentHookInstallError::TooLarge);
    }
    let temporary = parent.join(format!(".aizu-hooks-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        OpenOptions::new().read(true).open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(AgentHookInstallError::Io)
}

fn reject_symlink(path: &Path) -> Result<(), AgentHookInstallError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(AgentHookInstallError::UnsafePath),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AgentHookInstallError::Io(error)),
    }
}

#[derive(Debug, Error)]
pub enum AgentHookInstallError {
    #[error("the user home directory is unavailable")]
    HomeUnavailable,
    #[error("an agent configuration path is unsafe")]
    UnsafePath,
    #[error("an agent configuration exceeds the size limit")]
    TooLarge,
    #[error("an agent configuration file could not be accessed")]
    Io(#[from] std::io::Error),
    #[error("an agent configuration is not valid JSON")]
    Json(#[from] serde_json::Error),
    #[error("an agent configuration has an incompatible hook structure")]
    Integration(#[from] aizu_core::IntegrationError),
}

fn handlers(groups: &serde_json::Value) -> Vec<serde_json::Value> {
    groups
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(serde_json::Value::as_array))
        .flatten()
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use aizu_core::{AgentKind, HookStatus, hook_configuration};

    use super::{
        ProcessMonitor, classify_agent_executable, classify_agent_process, configuration_status,
        configure_hooks_at,
    };

    #[test]
    fn agent_executable_matching_is_exact_and_case_insensitive() {
        assert_eq!(
            classify_agent_executable(std::ffi::OsStr::new("Claude")),
            Some(AgentKind::ClaudeCode)
        );
        assert_eq!(
            classify_agent_executable(std::ffi::OsStr::new("codex.exe")),
            Some(AgentKind::Codex)
        );
        assert_eq!(
            classify_agent_executable(std::ffi::OsStr::new("claude-helper")),
            None
        );
        assert_eq!(
            classify_agent_executable(std::ffi::OsStr::new("node")),
            None
        );
    }

    #[test]
    fn app_embedded_codex_helpers_are_not_reported_as_agents() {
        assert_eq!(
            classify_agent_process(
                std::ffi::OsStr::new("codex"),
                Some(Path::new(
                    "/Applications/ChatGPT.app/Contents/Resources/codex",
                )),
            ),
            None
        );
        assert_eq!(
            classify_agent_process(
                std::ffi::OsStr::new("codex"),
                Some(Path::new("/Users/example/.local/bin/codex")),
            ),
            Some(AgentKind::Codex)
        );
    }

    #[test]
    fn process_snapshot_is_bounded_and_privacy_safe() {
        let snapshot = ProcessMonitor::new()
            .snapshot()
            .expect("system process snapshot should be readable");
        assert!(snapshot.processes().len() <= aizu_core::MAX_PROCESS_SNAPSHOT_ENTRIES);
    }

    #[test]
    fn hook_status_respects_claude_disable_and_codex_approval() {
        let path = Path::new("/Users/example/.local/bin/aizu");
        let codex = hook_configuration(AgentKind::Codex, path).expect("codex hooks");
        assert_eq!(
            configuration_status(AgentKind::Codex, &codex, &codex),
            HookStatus::ApprovalRequired
        );

        let mut claude = hook_configuration(AgentKind::ClaudeCode, path).expect("claude hooks");
        claude["disableAllHooks"] = serde_json::Value::Bool(true);
        assert_eq!(
            configuration_status(AgentKind::ClaudeCode, &claude, &claude),
            HookStatus::Missing
        );
    }

    #[cfg(unix)]
    #[test]
    fn home_owned_configuration_symlink_resolves_without_replacing_the_link() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().expect("temporary home");
        let claude = home.path().join(".claude");
        let dotfiles = home.path().join(".dotfiles/claude");
        fs::create_dir_all(&claude).expect("create Claude directory");
        fs::create_dir_all(&dotfiles).expect("create dotfiles directory");
        let target = dotfiles.join("settings.json");
        fs::write(&target, b"{}\n").expect("write target");
        let link = claude.join("settings.json");
        symlink(&target, &link).expect("create settings symlink");

        fs::create_dir_all(home.path().join(".codex")).expect("create Codex directory");
        fs::write(
            home.path().join(".codex/hooks.json"),
            br#"{"existingCodex":true}"#,
        )
        .expect("write Codex settings");
        fs::write(&target, br#"{"existingClaude":true}"#).expect("write Claude settings");

        configure_hooks_at(home.path(), &home.path().join(".local/bin/aizu"))
            .expect("safe linked settings should be merged");

        assert!(
            fs::symlink_metadata(&link)
                .expect("link remains")
                .file_type()
                .is_symlink()
        );
        let codex: serde_json::Value = serde_json::from_slice(
            &fs::read(home.path().join(".codex/hooks.json")).expect("read Codex settings"),
        )
        .expect("parse Codex settings");
        let claude: serde_json::Value =
            serde_json::from_slice(&fs::read(&target).expect("read linked Claude settings"))
                .expect("parse Claude settings");
        assert_eq!(codex["existingCodex"], true);
        assert!(codex["hooks"]["Stop"].is_array());
        assert_eq!(claude["existingClaude"], true);
        assert!(claude["hooks"]["StopFailure"].is_array());
    }
}
