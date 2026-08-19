use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    num::NonZeroU32,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use aizu_core::{
    AgentKind, HookInstallError, HookStatus, MAX_AGENT_CONFIG_BYTES, MAX_PROCESS_SNAPSHOT_ENTRIES,
    ObservedAgentProcess, ProcessLifecycleState, ProcessSnapshot, ProcessSnapshotError,
    hook_configuration, install_agent_hooks, merge_hook_configuration,
    resolve_agent_configuration_path,
};
use chrono::Utc;
use sysinfo::{Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const VERSION_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_VERSION_BYTES: u64 = 4 * 1_024;

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
            agent_process_refresh_kind(),
        );
        let running: Vec<_> = self
            .system
            .processes()
            .iter()
            .filter_map(|(pid, process)| {
                if !agent_session_is_active(process, &self.system) {
                    return None;
                }
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

fn agent_session_is_active(process: &Process, system: &System) -> bool {
    #[cfg(target_os = "linux")]
    {
        session_leader_is_present(
            process.session_id().is_some(),
            process
                .session_id()
                .is_some_and(|session_id| system.process(session_id).is_some()),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (process, system);
        true
    }
}

#[cfg(any(target_os = "linux", test))]
fn session_leader_is_present(session_known: bool, leader_present: bool) -> bool {
    !session_known || leader_present
}

fn agent_process_refresh_kind() -> ProcessRefreshKind {
    // On Linux, `nothing()` still includes every task/thread as a process.
    // Aizu reports agent processes, not their worker threads.
    ProcessRefreshKind::nothing()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .without_tasks()
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

pub fn configure_hooks() -> Result<AgentHooks, HookInstallError> {
    let base = directories::BaseDirs::new().ok_or(HookInstallError::UnsafeHome)?;
    let executable = base.home_dir().join(".local/bin/aizu");
    configure_hooks_at(base.home_dir(), &executable)?;
    Ok(inspect_hooks())
}

fn configure_hooks_at(home: &Path, executable: &Path) -> Result<(), HookInstallError> {
    install_agent_hooks(home, executable, &[AgentKind::Codex, AgentKind::ClaudeCode])?;
    Ok(())
}

fn inspect_agent_hooks(agent: AgentKind) -> HookStatus {
    let Some(base) = directories::BaseDirs::new() else {
        return HookStatus::Missing;
    };
    let Ok(path) = resolve_agent_configuration_path(base.home_dir(), agent) else {
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
    configuration_status(agent, &actual, &expected, &executable)
}

fn configuration_status(
    agent: AgentKind,
    actual: &serde_json::Value,
    expected: &serde_json::Value,
    executable: &Path,
) -> HookStatus {
    if agent == AgentKind::ClaudeCode
        && actual
            .get("disableAllHooks")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    {
        return HookStatus::Missing;
    }
    if !matches!(
        merge_hook_configuration(agent, actual, executable),
        Ok(merged) if merged == *actual
    ) {
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
        ProcessMonitor, agent_process_refresh_kind, classify_agent_executable,
        classify_agent_process, configuration_status, configure_hooks_at,
        session_leader_is_present,
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
    fn agent_process_refresh_excludes_linux_tasks() {
        assert!(!agent_process_refresh_kind().tasks());
    }

    #[test]
    fn missing_linux_session_leader_is_not_an_active_agent_session() {
        assert!(session_leader_is_present(false, false));
        assert!(session_leader_is_present(true, true));
        assert!(!session_leader_is_present(true, false));
    }

    #[test]
    fn hook_status_respects_claude_disable_and_codex_approval() {
        let path = Path::new("/Users/example/.local/bin/aizu");
        let codex = hook_configuration(AgentKind::Codex, path).expect("codex hooks");
        assert_eq!(
            configuration_status(AgentKind::Codex, &codex, &codex, path),
            HookStatus::ApprovalRequired
        );

        let mut claude = hook_configuration(AgentKind::ClaudeCode, path).expect("claude hooks");
        claude["disableAllHooks"] = serde_json::Value::Bool(true);
        assert_eq!(
            configuration_status(AgentKind::ClaudeCode, &claude, &claude, path),
            HookStatus::Missing
        );
    }

    #[test]
    fn duplicate_generated_aizu_handlers_require_reconfiguration() {
        let path = Path::new("/Users/example/.local/bin/aizu");
        let expected = hook_configuration(AgentKind::Codex, path).expect("codex hooks");
        let mut actual = expected.clone();
        actual["hooks"]["Stop"]
            .as_array_mut()
            .expect("stop groups")
            .push(serde_json::json!({
                "hooks": [{
                    "type": "command",
                    "command": "'/Applications/Aizu.app/Contents/Resources/bin/aizu' hook --agent codex --event Stop",
                    "timeout": 5
                }]
            }));

        assert_eq!(
            configuration_status(AgentKind::Codex, &actual, &expected, path),
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

        let executable = home.path().join(".local/bin/aizu");
        fs::create_dir_all(executable.parent().expect("executable directory"))
            .expect("create executable directory");
        fs::write(&executable, b"aizu test executable").expect("write executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
                .expect("set executable permissions");
        }

        configure_hooks_at(home.path(), &executable)
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
