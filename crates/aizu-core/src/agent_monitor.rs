use std::collections::BTreeSet;
use std::num::NonZeroU32;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum number of process observations accepted in one platform snapshot.
///
/// Platform adapters must apply their own bounded process enumeration before
/// constructing this type. The core limit is a second line of defense against
/// unbounded desktop diagnostic state.
pub const MAX_PROCESS_SNAPSHOT_ENTRIES: usize = 128;

/// Agent integrations supported by the MVP.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    /// `OpenAI` Codex CLI.
    Codex,
    /// Anthropic Claude Code CLI.
    ClaudeCode,
}

impl AgentKind {
    /// Returns the fixed executable name used for process identification.
    #[must_use]
    pub const fn executable_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
        }
    }

    /// Returns a fixed direct-exec command for checking the installed version.
    ///
    /// Callers must launch the executable with an argument vector. This spec is
    /// not a shell command and must never be interpolated into one.
    #[must_use]
    pub const fn version_command(self) -> AgentCommandSpec {
        AgentCommandSpec {
            executable: self.executable_name(),
            arguments: &["--version"],
        }
    }

    /// Reports the setup status for one hook and whether it was observed in the
    /// agent's current configuration.
    #[must_use]
    pub const fn hook_status(self, hook: AgentHook, configured: bool) -> HookStatus {
        if !self.supports_hook(hook) {
            HookStatus::Unsupported
        } else if configured {
            HookStatus::Configured
        } else {
            HookStatus::Missing
        }
    }

    /// Returns whether the agent has a first-party adapter for a hook.
    #[must_use]
    pub const fn supports_hook(self, hook: AgentHook) -> bool {
        match (self, hook) {
            (Self::Codex | Self::ClaudeCode, AgentHook::Stop | AgentHook::PermissionRequest)
            | (Self::ClaudeCode, AgentHook::StopFailure) => true,
            (Self::Codex, AgentHook::StopFailure) => false,
        }
    }

    /// Returns the complete hook capability matrix for setup diagnostics.
    #[must_use]
    pub fn hook_capabilities(self) -> [HookCapability; AgentHook::ALL.len()] {
        AgentHook::ALL.map(|hook| HookCapability {
            hook,
            supported: self.supports_hook(hook),
        })
    }
}

/// A direct process invocation with no shell interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentCommandSpec {
    /// Executable basename resolved by the platform process adapter.
    pub executable: &'static str,
    /// Fixed arguments passed directly to the executable.
    pub arguments: &'static [&'static str],
}

/// Hooks understood by first-party MVP adapters.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum AgentHook {
    /// Successful or outcome-unknown terminal hook.
    Stop,
    /// Agent permission prompt hook.
    PermissionRequest,
    /// Claude Code failure terminal hook.
    StopFailure,
}

impl AgentHook {
    /// Stable ordering used by the desktop setup capability matrix.
    pub const ALL: [Self; 3] = [Self::Stop, Self::PermissionRequest, Self::StopFailure];
}

/// One row in an agent's hook capability matrix.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HookCapability {
    /// Hook being described.
    pub hook: AgentHook,
    /// Whether a first-party adapter supports the hook for this agent.
    pub supported: bool,
}

/// Configuration health for one agent hook.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HookStatus {
    /// The hook is supported and present in the inspected configuration.
    Configured,
    /// The hook is supported but was not present in the inspected configuration.
    Missing,
    /// Hook definitions exist, but the agent requires a user trust approval Aizu cannot inspect.
    ApprovalRequired,
    /// The first-party adapter does not support this hook for the agent.
    Unsupported,
}

/// Privacy-safe metadata about one observed agent process.
///
/// The type intentionally has no executable path, argument vector, environment,
/// working directory, terminal output, prompt, or response fields. A platform
/// adapter identifies the known executable and supplies only this metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObservedAgentProcess {
    /// Identified first-party agent executable.
    pub agent: AgentKind,
    /// Platform process identifier.
    pub process_id: NonZeroU32,
    /// Most recent lifecycle state observed by the platform adapter.
    pub state: ProcessLifecycleState,
}

impl ObservedAgentProcess {
    /// Creates an observation for a running process.
    #[must_use]
    pub const fn running(agent: AgentKind, process_id: NonZeroU32) -> Self {
        Self {
            agent,
            process_id,
            state: ProcessLifecycleState::Running,
        }
    }

    /// Applies a lifecycle transition without changing process identity.
    pub fn transition(&mut self, next: ProcessLifecycleState) -> Result<(), LifecycleError> {
        let allowed = matches!(
            (&self.state, &next),
            (ProcessLifecycleState::Running, _)
                | (
                    ProcessLifecycleState::NoLongerObserved,
                    ProcessLifecycleState::NoLongerObserved
                )
                | (
                    ProcessLifecycleState::Exited(_),
                    ProcessLifecycleState::Exited(_)
                )
        );
        if !allowed {
            return Err(LifecycleError::TerminalState {
                current: self.state.clone(),
                requested: next,
            });
        }
        if let (ProcessLifecycleState::Exited(current), ProcessLifecycleState::Exited(requested)) =
            (&self.state, &next)
            && current != requested
        {
            return Err(LifecycleError::ConflictingExit {
                current: current.clone(),
                requested: requested.clone(),
            });
        }
        self.state = next;
        Ok(())
    }
}

/// State of a process according to a platform-supplied observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub enum ProcessLifecycleState {
    /// The process was present and running at observation time.
    Running,
    /// The process disappeared without a reliable exit status.
    NoLongerObserved,
    /// The platform adapter observed how the process terminated.
    Exited(ProcessExit),
}

impl ProcessLifecycleState {
    /// Returns whether this lifecycle observation may produce task completion.
    ///
    /// This always returns `false`: only an authenticated agent hook may create
    /// `task.completed`. Process state is diagnostic context and never event
    /// synthesis input, including graceful exit with code zero.
    #[must_use]
    pub const fn synthesizes_task_completion(&self) -> bool {
        false
    }
}

/// Platform-observed process termination details.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ProcessExit {
    /// The process returned an exit status.
    ExitCode(i32),
    /// The process terminated because of a platform signal.
    Signal(NonZeroU32),
    /// The platform could not provide termination details.
    Unknown,
}

impl ProcessExit {
    /// Classifies termination for display in desktop diagnostics.
    #[must_use]
    pub const fn diagnostic(&self) -> ExitDiagnostic {
        match self {
            Self::ExitCode(0) => ExitDiagnostic::GracefulExit,
            Self::ExitCode(code) => ExitDiagnostic::NonZeroExit { code: *code },
            Self::Signal(signal) => ExitDiagnostic::Signaled { signal: *signal },
            Self::Unknown => ExitDiagnostic::Unknown,
        }
    }
}

/// Privacy-safe process exit category for desktop diagnostics.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ExitDiagnostic {
    /// The process returned exit code zero.
    GracefulExit,
    /// The process returned a nonzero exit code.
    NonZeroExit {
        /// Observed exit code.
        code: i32,
    },
    /// The process was terminated by a platform signal.
    Signaled {
        /// Observed signal number.
        signal: NonZeroU32,
    },
    /// No reliable termination detail was available.
    Unknown,
}

/// Bounded process state captured by a platform-specific adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessSnapshot {
    /// UTC instant at which the platform adapter captured this snapshot.
    pub observed_at: DateTime<Utc>,
    /// Privacy-safe observations, bounded by [`MAX_PROCESS_SNAPSHOT_ENTRIES`].
    processes: Vec<ObservedAgentProcess>,
}

impl ProcessSnapshot {
    /// Creates a bounded snapshot and rejects duplicate platform process IDs.
    pub fn new(
        observed_at: DateTime<Utc>,
        processes: Vec<ObservedAgentProcess>,
    ) -> Result<Self, ProcessSnapshotError> {
        if processes.len() > MAX_PROCESS_SNAPSHOT_ENTRIES {
            return Err(ProcessSnapshotError::TooManyProcesses {
                count: processes.len(),
                limit: MAX_PROCESS_SNAPSHOT_ENTRIES,
            });
        }

        let mut seen = BTreeSet::new();
        for process in &processes {
            if !seen.insert(process.process_id) {
                return Err(ProcessSnapshotError::DuplicateProcessId(process.process_id));
            }
        }

        Ok(Self {
            observed_at,
            processes,
        })
    }

    /// Returns the bounded process observations.
    #[must_use]
    pub fn processes(&self) -> &[ObservedAgentProcess] {
        &self.processes
    }
}

/// Invalid lifecycle transition reported by a platform adapter.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LifecycleError {
    /// A terminal observation cannot transition back to another state.
    #[error("terminal process state {current:?} cannot transition to {requested:?}")]
    TerminalState {
        /// Existing terminal state.
        current: ProcessLifecycleState,
        /// Requested replacement state.
        requested: ProcessLifecycleState,
    },
    /// Two platform observations reported different termination details.
    #[error("process exit changed from {current:?} to {requested:?}")]
    ConflictingExit {
        /// Previously observed exit.
        current: ProcessExit,
        /// Newly observed exit.
        requested: ProcessExit,
    },
}

/// Invalid platform-supplied process snapshot.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProcessSnapshotError {
    /// The snapshot exceeded the hard process count limit.
    #[error("process snapshot contains {count} entries; limit is {limit}")]
    TooManyProcesses {
        /// Supplied entry count.
        count: usize,
        /// Maximum accepted entry count.
        limit: usize,
    },
    /// The snapshot contained the same platform process ID more than once.
    #[error("process snapshot contains duplicate process id {0}")]
    DuplicateProcessId(NonZeroU32),
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn pid(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).expect("test process id must be nonzero")
    }

    #[test]
    fn codex_and_claude_use_fixed_direct_version_commands() {
        assert_eq!(
            AgentKind::Codex.version_command(),
            AgentCommandSpec {
                executable: "codex",
                arguments: &["--version"],
            }
        );
        assert_eq!(
            AgentKind::ClaudeCode.version_command(),
            AgentCommandSpec {
                executable: "claude",
                arguments: &["--version"],
            }
        );
    }

    #[test]
    fn hook_capabilities_cover_both_agents() {
        assert_eq!(
            AgentKind::Codex.hook_capabilities(),
            [
                HookCapability {
                    hook: AgentHook::Stop,
                    supported: true,
                },
                HookCapability {
                    hook: AgentHook::PermissionRequest,
                    supported: true,
                },
                HookCapability {
                    hook: AgentHook::StopFailure,
                    supported: false,
                },
            ]
        );
        assert!(
            AgentKind::ClaudeCode
                .hook_capabilities()
                .into_iter()
                .all(|capability| capability.supported)
        );
    }

    #[test]
    fn hook_status_distinguishes_missing_and_unsupported() {
        assert_eq!(
            AgentKind::ClaudeCode.hook_status(AgentHook::StopFailure, false),
            HookStatus::Missing
        );
        assert_eq!(
            AgentKind::Codex.hook_status(AgentHook::StopFailure, true),
            HookStatus::Unsupported
        );
        assert_eq!(
            AgentKind::Codex.hook_status(AgentHook::PermissionRequest, true),
            HookStatus::Configured
        );
    }

    #[test]
    fn running_process_transitions_to_terminal_diagnostic_state() {
        let mut process = ObservedAgentProcess::running(AgentKind::Codex, pid(42));
        process
            .transition(ProcessLifecycleState::Exited(ProcessExit::ExitCode(0)))
            .expect("running process may exit");
        assert_eq!(
            process.state,
            ProcessLifecycleState::Exited(ProcessExit::ExitCode(0))
        );
        assert!(matches!(
            process.transition(ProcessLifecycleState::Running),
            Err(LifecycleError::TerminalState { .. })
        ));
    }

    #[test]
    fn disappearance_is_terminal_without_claiming_an_exit() {
        let mut process = ObservedAgentProcess::running(AgentKind::ClaudeCode, pid(43));
        process
            .transition(ProcessLifecycleState::NoLongerObserved)
            .expect("running process may disappear from a later snapshot");
        assert!(matches!(
            process.transition(ProcessLifecycleState::Exited(ProcessExit::Unknown)),
            Err(LifecycleError::TerminalState { .. })
        ));
    }

    #[test]
    fn conflicting_exit_observations_are_rejected() {
        let mut process = ObservedAgentProcess::running(AgentKind::Codex, pid(44));
        process
            .transition(ProcessLifecycleState::Exited(ProcessExit::ExitCode(1)))
            .expect("first exit observation");
        assert!(matches!(
            process.transition(ProcessLifecycleState::Exited(ProcessExit::ExitCode(2))),
            Err(LifecycleError::ConflictingExit { .. })
        ));
    }

    #[test]
    fn exit_classification_is_diagnostic_only() {
        let cases = [
            (ProcessExit::ExitCode(0), ExitDiagnostic::GracefulExit),
            (
                ProcessExit::ExitCode(9),
                ExitDiagnostic::NonZeroExit { code: 9 },
            ),
            (
                ProcessExit::Signal(pid(15)),
                ExitDiagnostic::Signaled { signal: pid(15) },
            ),
            (ProcessExit::Unknown, ExitDiagnostic::Unknown),
        ];
        for (process_exit, expected) in cases {
            let state = ProcessLifecycleState::Exited(process_exit.clone());
            assert_eq!(process_exit.diagnostic(), expected);
            assert!(!state.synthesizes_task_completion());
        }
        assert!(!ProcessLifecycleState::Running.synthesizes_task_completion());
        assert!(!ProcessLifecycleState::NoLongerObserved.synthesizes_task_completion());
    }

    #[test]
    fn snapshots_are_bounded_and_reject_duplicate_process_ids() {
        let observed_at = Utc
            .with_ymd_and_hms(2026, 8, 12, 12, 0, 0)
            .single()
            .expect("valid timestamp");
        let duplicate = vec![
            ObservedAgentProcess::running(AgentKind::Codex, pid(7)),
            ObservedAgentProcess::running(AgentKind::ClaudeCode, pid(7)),
        ];
        assert_eq!(
            ProcessSnapshot::new(observed_at, duplicate),
            Err(ProcessSnapshotError::DuplicateProcessId(pid(7)))
        );

        let excessive = (1..=u32::try_from(MAX_PROCESS_SNAPSHOT_ENTRIES + 1)
            .expect("small process limit"))
            .map(|process_id| ObservedAgentProcess::running(AgentKind::Codex, pid(process_id)))
            .collect();
        assert!(matches!(
            ProcessSnapshot::new(observed_at, excessive),
            Err(ProcessSnapshotError::TooManyProcesses { .. })
        ));
    }

    #[test]
    fn serialized_observation_has_no_sensitive_process_fields() {
        let process = ObservedAgentProcess::running(AgentKind::ClaudeCode, pid(86));
        let serialized = serde_json::to_value(process).expect("serialize process observation");
        let object = serialized.as_object().expect("object");
        assert_eq!(object.len(), 3);
        assert!(object.contains_key("agent"));
        assert!(object.contains_key("process_id"));
        assert!(object.contains_key("state"));
        for prohibited in ["argv", "arguments", "path", "environment", "cwd", "output"] {
            assert!(!object.contains_key(prohibited));
        }
    }
}
