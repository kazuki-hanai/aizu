use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

use aizu_core::{
    DesktopState as CoreDesktopState, EventKind as CoreEventKind, HistoryItem, NotificationPolicy,
    OutboxState as CoreOutboxState, Outcome as CoreOutcome, PipelineReport,
    QuietHours as CoreQuietHours, Spool, StatePaths, dispatch_outbox, ingest_spool,
    safe_agent_excerpt,
};
use chrono::{Local, Timelike, Utc};
use thiserror::Error;

use crate::{
    cli_diagnostic::CliDiagnostic,
    model::{
        AgentKind, AgentMonitorView, AgentRuntimeStatus, AppView, CliStatus, DeliveryStatus,
        EventKind, HistoryEvent, HookStatus, Notification, NotificationDelivery, PermissionStatus,
        Preferences, RunningAgentView, SourceActionRequired, SourceKind, SourceStatus, SourceView,
        TaskOutcome, TrayState,
    },
    notifier::{Notifier, NotifyError},
    store::{RemoteSourceConfig, SettingsStore, StoreError, StoredSettings},
};

#[derive(Debug, Error)]
pub enum DesktopError {
    #[error(transparent)]
    Notify(#[from] NotifyError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("desktop state lock is unavailable")]
    Lock,
    #[error("quiet hour time must be in HH:MM format")]
    InvalidQuietHours,
    #[error("quiet hour start and end must be different")]
    EmptyQuietHours,
    #[error("launch-at-login could not be changed: {0}")]
    Autostart(String),
    #[error("SSH source configuration is invalid: {0}")]
    SshConfiguration(#[from] aizu_core::SshConfigurationError),
    #[error("SSH source configuration is invalid: enter a valid SSH config alias")]
    InvalidSshAlias,
    #[error("an SSH source with alias {0:?} already exists")]
    DuplicateRemoteSource(String),
    #[error("at most {0} remote SSH sources may be configured")]
    RemoteSourceLimit(usize),
    #[error("SSH source {0:?} was not found")]
    RemoteSourceNotFound(String),
    #[error("local spool error: {0}")]
    Spool(#[from] aizu_core::SpoolError),
    #[error("desktop database error: {0}")]
    Desktop(#[from] aizu_core::DesktopError),
    #[error("local notification pipeline error: {0}")]
    Pipeline(#[from] aizu_core::PipelineError),
    #[error("the bundled Aizu CLI is unavailable")]
    CliBundleUnavailable,
    #[error("the CLI install directory could not be prepared")]
    CliInstallDirectory,
    #[error("the CLI could not be installed safely")]
    CliInstall,
    #[error("Codex and Claude Code hooks could not be configured safely: {0}")]
    AgentConfiguration(String),
    #[error("Codex and Claude Code setup stopped unexpectedly; restart Aizu before trying again")]
    AgentSetupTask,
    #[error("Codex and Claude Code hooks and the Aizu CLI must be configured first")]
    AgentSetupIncomplete,
    #[error(
        "notifications are disabled; enable Aizu in System Settings > Notifications, then try again"
    )]
    NotificationPermissionDisabled,
}

impl serde::Serialize for DesktopError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub struct AppService {
    notifier: Arc<dyn Notifier>,
    store: SettingsStore,
    settings: StoredSettings,
    notification_permission: PermissionStatus,
    sources: Vec<SourceView>,
    history: Vec<HistoryEvent>,
    agent_monitors: Vec<AgentMonitorView>,
    running_agents: Vec<RunningAgentView>,
    cli_status: CliStatus,
    cli_version: Option<String>,
    spool: Option<Spool>,
    state_paths: StatePaths,
    desktop: CoreDesktopState,
    pending_remote_identities: BTreeMap<String, uuid::Uuid>,
    remote_connection_epochs: BTreeMap<String, u64>,
}

impl AppService {
    pub fn new(
        notifier: Arc<dyn Notifier>,
        store: SettingsStore,
        paths: &StatePaths,
    ) -> Result<Self, DesktopError> {
        let settings = store.load()?;
        if settings.remote_sources.len() > crate::remote_worker::MAX_REMOTE_SOURCES {
            return Err(DesktopError::RemoteSourceLimit(
                crate::remote_worker::MAX_REMOTE_SOURCES,
            ));
        }
        let notification_permission =
            if settings.preferences.notification_delivery == NotificationDelivery::AizuBanner {
                PermissionStatus::Granted
            } else {
                notifier.permission_status()?
            };
        let cli_diagnostic = crate::cli_diagnostic::inspect(env!("CARGO_PKG_VERSION"))
            .unwrap_or(CliDiagnostic::Missing);
        let (cli_status, cli_version) = cli_status(&cli_diagnostic);
        let spool = (cli_status == CliStatus::Installed)
            .then(|| Spool::open(paths.clone()))
            .transpose()?;
        let desktop = CoreDesktopState::open(paths.desktop_db())?;
        let remote_connection_epochs = settings
            .remote_sources
            .iter()
            .map(|source| (source.host_alias.clone(), 0))
            .collect();
        let mut service = Self {
            notifier,
            store,
            settings,
            notification_permission,
            sources: vec![SourceView {
                id: "local".to_owned(),
                name: "This Mac".to_owned(),
                kind: SourceKind::Local,
                status: SourceStatus::Reconnecting,
                detail: "Waiting for the local source worker".to_owned(),
                last_event_at: None,
                action_required: None,
            }],
            history: Vec::new(),
            agent_monitors: default_agent_monitors(),
            running_agents: Vec::new(),
            cli_status,
            cli_version,
            spool,
            state_paths: paths.clone(),
            desktop,
            pending_remote_identities: BTreeMap::new(),
            remote_connection_epochs,
        };
        service.refresh_history()?;
        service.sync_remote_source_views();
        Ok(service)
    }

    #[cfg(feature = "desktop-e2e")]
    pub fn prepare_e2e_state(&mut self) -> Result<(), DesktopError> {
        self.cli_status = CliStatus::Installed;
        self.cli_version = Some(env!("CARGO_PKG_VERSION").to_owned());
        self.spool = Some(Spool::open(self.state_paths.clone())?);
        for monitor in &mut self.agent_monitors {
            monitor.hook_status = HookStatus::Configured;
            "E2E hook fixture configured".clone_into(&mut monitor.detail);
        }
        Ok(())
    }

    pub fn view(&self) -> AppView {
        AppView {
            onboarding_complete: self.settings.onboarding_complete,
            notification_permission: self.notification_permission,
            cli_status: self.cli_status,
            cli_version: self.cli_version.clone(),
            protocol_version: 1,
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            paused: self.settings.paused,
            tray_state: self.tray_state(),
            sources: self.sources.clone(),
            agent_monitors: self.agent_monitors.clone(),
            running_agents: self.running_agents.clone(),
            history: self.history.clone(),
            preferences: self.settings.preferences.clone(),
            last_event_at: self.history.first().map(|event| event.occurred_at.clone()),
        }
    }

    pub fn poll_local_pipeline(&mut self) -> Result<(AppView, bool), DesktopError> {
        if self.settings.preferences.notification_delivery == NotificationDelivery::AizuBanner {
            self.notification_permission = PermissionStatus::Granted;
        } else if let Ok(permission) = self.notifier.permission_status() {
            self.notification_permission = permission;
        }
        let now = Utc::now();
        let recovered_permission_events =
            if self.notification_permission == PermissionStatus::Granted {
                self.desktop.requeue_permission_suppressed(now)?
            } else {
                0
            };
        let ingest = if let Some(spool) = self.spool.as_ref() {
            ingest_spool(spool, &self.desktop, "local", "This Mac", now)?
        } else {
            PipelineReport::default()
        };
        let policy = self.notification_policy();
        let local_now = Local::now();
        let local_minute = local_now
            .hour()
            .checked_mul(60)
            .and_then(|minutes| minutes.checked_add(local_now.minute()))
            .and_then(|minutes| u16::try_from(minutes).ok());
        let adapter = PipelineNotifier {
            notifier: self.notifier.as_ref(),
            delivery: self.settings.preferences.notification_delivery,
            language: self.settings.preferences.language,
            text_size: self.settings.preferences.text_size,
            sound: self
                .settings
                .preferences
                .sound_enabled
                .then_some(self.settings.preferences.notification_sound),
        };
        let dispatched = if self.notification_permission == PermissionStatus::NotDetermined {
            PipelineReport::default()
        } else {
            dispatch_outbox(&self.desktop, &adapter, &policy, now, local_minute)?
        };
        let previous_source_status = self.sources[0].status;
        let pipeline_changed = ingest.ingested > 0
            || recovered_permission_events > 0
            || ingest.duplicates > 0
            || ingest.gaps > 0
            || dispatched.delivered > 0
            || dispatched.suppressed > 0
            || dispatched.retryable_failures > 0
            || dispatched.terminal_failures > 0
            || dispatched.deferred > 0;
        if self.spool.is_some() {
            self.sources[0].status = SourceStatus::Connected;
            "Local spool synchronized".clone_into(&mut self.sources[0].detail);
        } else {
            self.sources[0].status = SourceStatus::Disabled;
            "Compatible Aizu CLI required".clone_into(&mut self.sources[0].detail);
        }
        let changed = pipeline_changed || previous_source_status != self.sources[0].status;
        self.refresh_history()?;
        self.sources[0].last_event_at = self.history.first().map(|event| event.occurred_at.clone());
        Ok((self.view(), changed))
    }

    pub fn record_pipeline_error(&mut self, detail: &str) -> AppView {
        self.sources[0].status = SourceStatus::Error;
        self.sources[0].detail = sanitize_diagnostic(detail);
        self.view()
    }

    pub fn update_process_snapshot(&mut self, snapshot: &aizu_core::ProcessSnapshot) -> bool {
        let previous = self.agent_monitors.clone();
        let previous_running = self.running_agents.clone();
        let mut codex_count = 0usize;
        let mut claude_count = 0usize;
        let mut running_agents: Vec<_> = snapshot
            .processes()
            .iter()
            .filter(|process| matches!(process.state, aizu_core::ProcessLifecycleState::Running))
            .map(|process| {
                let agent = match process.agent {
                    aizu_core::AgentKind::Codex => AgentKind::Codex,
                    aizu_core::AgentKind::ClaudeCode => AgentKind::ClaudeCode,
                };
                let (name, count) = match agent {
                    AgentKind::Codex => {
                        codex_count += 1;
                        ("Codex", codex_count)
                    }
                    AgentKind::ClaudeCode => {
                        claude_count += 1;
                        ("Claude Code", claude_count)
                    }
                };
                RunningAgentView {
                    agent,
                    label: format!("{name} {count}"),
                    source_id: "local".to_owned(),
                    source_name: "This Mac".to_owned(),
                    source_kind: SourceKind::Local,
                }
            })
            .collect();
        running_agents.extend(
            self.running_agents
                .iter()
                .filter(|agent| agent.source_kind == SourceKind::RemoteSsh)
                .cloned(),
        );
        self.running_agents = running_agents;
        for agent in [
            aizu_core::AgentKind::Codex,
            aizu_core::AgentKind::ClaudeCode,
        ] {
            let observations: Vec<_> = snapshot
                .processes()
                .iter()
                .filter(|process| process.agent == agent)
                .collect();
            if observations.is_empty() {
                continue;
            }
            let kind = match agent {
                aizu_core::AgentKind::Codex => AgentKind::Codex,
                aizu_core::AgentKind::ClaudeCode => AgentKind::ClaudeCode,
            };
            if let Some(monitor) = self
                .agent_monitors
                .iter_mut()
                .find(|item| item.agent == kind)
            {
                if observations.iter().any(|process| {
                    matches!(process.state, aizu_core::ProcessLifecycleState::Running)
                }) {
                    if monitor.status != AgentRuntimeStatus::Waiting {
                        monitor.status = AgentRuntimeStatus::Running;
                    }
                    "Known agent process detected".clone_into(&mut monitor.detail);
                } else {
                    if monitor.status != AgentRuntimeStatus::Waiting {
                        monitor.status = AgentRuntimeStatus::NotDetected;
                    }
                    "Agent process no longer observed".clone_into(&mut monitor.detail);
                }
                monitor.last_seen_at = Some(snapshot.observed_at.to_rfc3339());
            }
        }
        previous != self.agent_monitors || previous_running != self.running_agents
    }

    pub fn update_remote_agents(
        &mut self,
        host_alias: &str,
        connection_epoch: u64,
        agents: &[aizu_core::AgentKind],
    ) -> bool {
        if self.remote_connection_epochs.get(host_alias) != Some(&connection_epoch) {
            return false;
        }
        let source_id = format!("ssh:{host_alias}");
        let Some(source) = self
            .sources
            .iter()
            .find(|source| source.id == source_id && source.status == SourceStatus::Connected)
        else {
            return false;
        };
        let source_name = source.name.clone();
        let previous = self.running_agents.clone();
        self.running_agents
            .retain(|agent| agent.source_id != source_id);
        let mut codex_count = 0usize;
        let mut claude_count = 0usize;
        self.running_agents.extend(agents.iter().map(|agent| {
            let (agent, name, count) = match agent {
                aizu_core::AgentKind::Codex => {
                    codex_count += 1;
                    (AgentKind::Codex, "Codex", codex_count)
                }
                aizu_core::AgentKind::ClaudeCode => {
                    claude_count += 1;
                    (AgentKind::ClaudeCode, "Claude Code", claude_count)
                }
            };
            RunningAgentView {
                agent,
                label: format!("{name} {count}"),
                source_id: source_id.clone(),
                source_name: source_name.clone(),
                source_kind: SourceKind::RemoteSsh,
            }
        }));
        previous != self.running_agents
    }

    pub fn update_cli_diagnostic(&mut self, diagnostic: &CliDiagnostic) -> bool {
        let (status, version) = cli_status(diagnostic);
        let changed = status != self.cli_status || version != self.cli_version;
        self.cli_status = status;
        self.cli_version = version;
        changed
    }

    pub fn update_agent_versions(
        &mut self,
        versions: &crate::process_monitor::AgentVersions,
    ) -> bool {
        let previous = self.agent_monitors.clone();
        for (agent, version) in versions {
            let kind = match agent {
                aizu_core::AgentKind::Codex => AgentKind::Codex,
                aizu_core::AgentKind::ClaudeCode => AgentKind::ClaudeCode,
            };
            if let Some(monitor) = self
                .agent_monitors
                .iter_mut()
                .find(|monitor| monitor.agent == kind)
            {
                monitor.version.clone_from(version);
            }
        }
        previous != self.agent_monitors
    }

    pub fn update_agent_hooks(&mut self, hooks: crate::process_monitor::AgentHooks) -> bool {
        let previous = self.agent_monitors.clone();
        for (agent, status) in &hooks {
            let kind = match agent {
                aizu_core::AgentKind::Codex => AgentKind::Codex,
                aizu_core::AgentKind::ClaudeCode => AgentKind::ClaudeCode,
            };
            let status = match status {
                aizu_core::HookStatus::Configured => HookStatus::Configured,
                aizu_core::HookStatus::Missing => HookStatus::Missing,
                aizu_core::HookStatus::ApprovalRequired
                    if self.settings.codex_hook_trust_confirmed =>
                {
                    HookStatus::Configured
                }
                aizu_core::HookStatus::ApprovalRequired => HookStatus::ApprovalRequired,
                aizu_core::HookStatus::Unsupported => HookStatus::Unsupported,
            };
            if let Some(monitor) = self
                .agent_monitors
                .iter_mut()
                .find(|monitor| monitor.agent == kind)
            {
                monitor.hook_status = status;
                if status == HookStatus::Configured
                    && monitor.status == AgentRuntimeStatus::NotDetected
                {
                    "Required Aizu hooks configured".clone_into(&mut monitor.detail);
                }
            }
        }
        previous != self.agent_monitors
    }

    pub fn add_remote_source(
        &mut self,
        host_alias: String,
        local_label: String,
    ) -> Result<AppView, DesktopError> {
        crate::ssh_connection_test::validate_alias(&host_alias)
            .map_err(|()| DesktopError::InvalidSshAlias)?;
        if self
            .settings
            .remote_sources
            .iter()
            .any(|source| source.host_alias == host_alias)
        {
            return Err(DesktopError::DuplicateRemoteSource(host_alias));
        }
        if self.settings.remote_sources.len() >= crate::remote_worker::MAX_REMOTE_SOURCES {
            return Err(DesktopError::RemoteSourceLimit(
                crate::remote_worker::MAX_REMOTE_SOURCES,
            ));
        }
        self.desktop
            .register_source(&format!("ssh:{host_alias}"), &local_label)?;
        self.settings.remote_sources.push(RemoteSourceConfig {
            host_alias: host_alias.clone(),
            local_label,
            reconnect_generation: 0,
        });
        match self.remote_connection_epochs.entry(host_alias) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(0);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                *entry.get_mut() = entry.get().wrapping_add(1);
            }
        }
        self.persist()?;
        self.sync_remote_source_views();
        Ok(self.view())
    }

    pub fn remove_remote_source(&mut self, host_alias: &str) -> Result<AppView, DesktopError> {
        if !self
            .settings
            .remote_sources
            .iter()
            .any(|source| source.host_alias == host_alias)
        {
            return Err(DesktopError::RemoteSourceNotFound(host_alias.to_owned()));
        }
        let previous_settings = self.settings.clone();
        self.settings
            .remote_sources
            .retain(|source| source.host_alias != host_alias);
        if let Err(error) = self.persist() {
            self.settings = previous_settings;
            return Err(error);
        }
        if let Err(error) = self
            .desktop
            .release_source_identity(&format!("ssh:{host_alias}"))
        {
            self.settings = previous_settings;
            self.persist()?;
            return Err(error.into());
        }
        self.invalidate_remote_agent_snapshot(host_alias);
        self.running_agents
            .retain(|agent| agent.source_id != format!("ssh:{host_alias}"));
        self.sync_remote_source_views();
        Ok(self.view())
    }

    pub fn reconnect_remote_source(&mut self, host_alias: &str) -> Result<AppView, DesktopError> {
        let source = self
            .settings
            .remote_sources
            .iter_mut()
            .find(|source| source.host_alias == host_alias)
            .ok_or_else(|| DesktopError::RemoteSourceNotFound(host_alias.to_owned()))?;
        source.reconnect_generation = source.reconnect_generation.saturating_add(1);
        self.persist()?;
        self.set_remote_status(
            host_alias,
            SourceStatus::Reconnecting,
            "Reconnect requested",
        );
        Ok(self.view())
    }

    pub fn reconnect_all_remote_sources(&mut self) -> Result<AppView, DesktopError> {
        let blocked: std::collections::HashSet<&str> = self
            .sources
            .iter()
            .filter(|source| {
                source.action_required == Some(SourceActionRequired::ConfirmIdentityChange)
            })
            .filter_map(|source| source.id.strip_prefix("ssh:"))
            .collect();
        let aliases: Vec<String> = self
            .settings
            .remote_sources
            .iter_mut()
            .filter(|source| !blocked.contains(source.host_alias.as_str()))
            .map(|source| {
                source.reconnect_generation = source.reconnect_generation.saturating_add(1);
                source.host_alias.clone()
            })
            .collect();
        if aliases.is_empty() {
            return Ok(self.view());
        }
        self.persist()?;
        for alias in aliases {
            self.set_remote_status(&alias, SourceStatus::Reconnecting, "Reconnect requested");
        }
        Ok(self.view())
    }

    pub fn clear_history(&mut self) -> Result<AppView, DesktopError> {
        self.desktop.clear_history()?;
        self.refresh_history()?;
        Ok(self.view())
    }

    pub fn confirm_remote_identity(&mut self, host_alias: &str) -> Result<AppView, DesktopError> {
        let identity = self
            .pending_remote_identities
            .remove(host_alias)
            .ok_or_else(|| DesktopError::RemoteSourceNotFound(host_alias.to_owned()))?;
        self.desktop
            .replace_source(&format!("ssh:{host_alias}"), identity)?;
        let source = self
            .settings
            .remote_sources
            .iter_mut()
            .find(|source| source.host_alias == host_alias)
            .ok_or_else(|| DesktopError::RemoteSourceNotFound(host_alias.to_owned()))?;
        source.reconnect_generation = source.reconnect_generation.saturating_add(1);
        self.persist()?;
        self.set_remote_status(
            host_alias,
            SourceStatus::Reconnecting,
            "Identity change accepted; reconnecting",
        );
        if let Some(view) = self
            .sources
            .iter_mut()
            .find(|source| source.id == format!("ssh:{host_alias}"))
        {
            view.action_required = None;
        }
        Ok(self.view())
    }

    pub fn remote_sources(&self) -> Vec<RemoteSourceConfig> {
        self.settings.remote_sources.clone()
    }

    pub fn connected_remote_agent_sources(&self) -> Vec<(String, u64)> {
        self.sources
            .iter()
            .filter(|source| source.status == SourceStatus::Connected)
            .filter_map(|source| {
                let host_alias = source.id.strip_prefix("ssh:")?;
                let epoch = self.remote_connection_epochs.get(host_alias)?;
                Some((host_alias.to_owned(), *epoch))
            })
            .collect()
    }

    pub fn core_desktop(&self) -> CoreDesktopState {
        self.desktop.clone()
    }

    pub fn maintain_history(&mut self) -> Result<bool, DesktopError> {
        let report = self.desktop.maintain_history(Utc::now())?;
        if report.events_pruned == 0 && report.gaps_pruned == 0 {
            return Ok(false);
        }
        self.refresh_history()?;
        Ok(true)
    }

    pub fn set_remote_status(
        &mut self,
        host_alias: &str,
        status: SourceStatus,
        detail: &str,
    ) -> bool {
        let Some(source_index) = self
            .sources
            .iter()
            .position(|source| source.id == format!("ssh:{host_alias}"))
        else {
            return false;
        };
        let safe_detail = sanitize_diagnostic(detail);
        let was_connected = self.sources[source_index].status == SourceStatus::Connected;
        if was_connected && status != SourceStatus::Connected {
            self.invalidate_remote_agent_snapshot(host_alias);
        }
        let source = &mut self.sources[source_index];
        let mut changed = source.status != status || source.detail != safe_detail;
        source.status = status;
        source.detail = safe_detail;
        if status != SourceStatus::Connected {
            let previous_count = self.running_agents.len();
            self.running_agents
                .retain(|agent| agent.source_id != format!("ssh:{host_alias}"));
            changed |= previous_count != self.running_agents.len();
        }
        if changed {
            let _ = self.refresh_history();
        }
        changed
    }

    pub fn require_remote_identity_confirmation(
        &mut self,
        host_alias: &str,
        identity: uuid::Uuid,
    ) -> bool {
        self.pending_remote_identities
            .insert(host_alias.to_owned(), identity);
        let mut changed = self.set_remote_status(
            host_alias,
            SourceStatus::Error,
            "Remote spool identity changed",
        );
        let Some(source) = self
            .sources
            .iter_mut()
            .find(|source| source.id == format!("ssh:{host_alias}"))
        else {
            return false;
        };
        changed |= source.action_required != Some(SourceActionRequired::ConfirmIdentityChange);
        source.action_required = Some(SourceActionRequired::ConfirmIdentityChange);
        changed
    }

    fn invalidate_remote_agent_snapshot(&mut self, host_alias: &str) {
        let epoch = self
            .remote_connection_epochs
            .entry(host_alias.to_owned())
            .or_insert(0);
        *epoch = epoch.wrapping_add(1);
        self.running_agents
            .retain(|agent| agent.source_id != format!("ssh:{host_alias}"));
    }

    pub fn request_notification_permission(&mut self) -> Result<AppView, DesktopError> {
        self.notification_permission = if self.settings.preferences.notification_delivery
            == NotificationDelivery::AizuBanner
        {
            PermissionStatus::Granted
        } else {
            self.notifier.request_permission()?
        };
        Ok(self.view())
    }

    pub fn send_test_notification(&mut self) -> Result<AppView, DesktopError> {
        self.notification_permission = if self.settings.preferences.notification_delivery
            == NotificationDelivery::AizuBanner
        {
            PermissionStatus::Granted
        } else {
            self.notifier.permission_status()?
        };
        if self.notification_permission != PermissionStatus::Granted {
            return Err(DesktopError::NotificationPermissionDisabled);
        }
        let japanese = self.settings.preferences.language.prefers_japanese();
        let delivery = self.settings.preferences.notification_delivery;
        self.notifier.notify(&Notification {
            // A manual test is a new user action each time. Reusing a stable
            // identifier makes macOS replace the prior notification and may
            // play only its sound without presenting another banner.
            id: stable_notification_id(&format!("aizu-test-notification-{}", uuid::Uuid::new_v4())),
            title: if japanese {
                "Aizu テスト通知"
            } else {
                "Aizu test notification"
            }
            .to_owned(),
            body: if japanese {
                if delivery == NotificationDelivery::AizuBanner {
                    "Aizuバナーを利用できます。"
                } else {
                    "macOS通知を利用できます。"
                }
            } else if delivery == NotificationDelivery::AizuBanner {
                "Aizu Banner is ready."
            } else {
                "macOS notifications are ready."
            }
            .to_owned(),
            sound: self
                .settings
                .preferences
                .sound_enabled
                .then_some(self.settings.preferences.notification_sound),
            delivery,
            language: self.settings.preferences.language,
            text_size: self.settings.preferences.text_size,
            can_activate_terminal: false,
            approval: None,
            activation: None,
        })?;
        Ok(self.view())
    }

    pub fn install_cli(&mut self, source: &Path) -> Result<AppView, DesktopError> {
        if !matches!(
            crate::cli_diagnostic::inspect_path(source, env!("CARGO_PKG_VERSION")),
            Ok(CliDiagnostic::Installed { .. })
        ) {
            return Err(DesktopError::CliInstall);
        }
        let current_diagnostic = crate::cli_diagnostic::inspect(env!("CARGO_PKG_VERSION"))
            .map_err(|_| DesktopError::CliInstall)?;
        let base = directories::BaseDirs::new().ok_or(DesktopError::CliInstallDirectory)?;
        let directory = base.home_dir().join(".local/bin");
        std::fs::create_dir_all(&directory).map_err(|_| DesktopError::CliInstallDirectory)?;
        let replace_managed = replace_managed_cli(&current_diagnostic);
        aizu_core::install_cli(source, directory.join("aizu"), replace_managed)
            .map_err(|_| DesktopError::CliInstall)?;
        let diagnostic = crate::cli_diagnostic::inspect(env!("CARGO_PKG_VERSION"))
            .map_err(|_| DesktopError::CliInstall)?;
        self.update_cli_diagnostic(&diagnostic);
        if self.cli_status != CliStatus::Installed {
            return Err(DesktopError::CliInstall);
        }
        self.spool = Some(Spool::open(self.state_paths.clone())?);
        Ok(self.view())
    }

    pub fn configure_agent_hooks(&mut self) -> Result<AppView, DesktopError> {
        let hooks = crate::process_monitor::configure_hooks()
            .map_err(|error| DesktopError::AgentConfiguration(error.to_string()))?;
        self.settings.codex_hook_trust_confirmed = false;
        self.persist()?;
        self.update_agent_hooks(hooks);
        Ok(self.view())
    }

    pub fn confirm_codex_hook_trust(&mut self) -> Result<AppView, DesktopError> {
        let codex = self
            .agent_monitors
            .iter_mut()
            .find(|monitor| monitor.agent == AgentKind::Codex)
            .ok_or(DesktopError::AgentSetupIncomplete)?;
        if codex.hook_status != HookStatus::ApprovalRequired {
            return Err(DesktopError::AgentSetupIncomplete);
        }
        codex.hook_status = HookStatus::Configured;
        "Required Aizu hooks configured".clone_into(&mut codex.detail);
        self.settings.codex_hook_trust_confirmed = true;
        self.persist()?;
        Ok(self.view())
    }

    pub fn complete_onboarding(&mut self, launch_at_login: bool) -> Result<AppView, DesktopError> {
        if self.cli_status != CliStatus::Installed
            || self
                .agent_monitors
                .iter()
                .any(|monitor| monitor.hook_status != HookStatus::Configured)
        {
            return Err(DesktopError::AgentSetupIncomplete);
        }
        self.settings.onboarding_complete = true;
        self.settings.preferences.launch_at_login = launch_at_login;
        self.persist()?;
        Ok(self.view())
    }

    pub fn set_paused(&mut self, paused: bool) -> Result<AppView, DesktopError> {
        self.settings.paused = paused;
        self.persist()?;
        Ok(self.view())
    }

    pub fn update_preferences(
        &mut self,
        preferences: Preferences,
    ) -> Result<AppView, DesktopError> {
        validate_preferences(&preferences)?;
        let notification_permission =
            if preferences.notification_delivery == NotificationDelivery::AizuBanner {
                PermissionStatus::Granted
            } else {
                self.notifier.permission_status()?
            };
        self.settings.preferences = preferences;
        self.notification_permission = notification_permission;
        self.persist()?;
        Ok(self.view())
    }

    fn persist(&self) -> Result<(), DesktopError> {
        self.store.save(&self.settings).map_err(DesktopError::from)
    }

    fn notification_policy(&self) -> NotificationPolicy {
        let quiet_hours = self
            .settings
            .preferences
            .quiet_hours
            .enabled
            .then(|| CoreQuietHours {
                start_minute: clock_minute(&self.settings.preferences.quiet_hours.start),
                end_minute: clock_minute(&self.settings.preferences.quiet_hours.end),
                questions_bypass: self.settings.preferences.quiet_hours.questions_bypass,
            });
        NotificationPolicy {
            paused: self.settings.paused,
            completion_enabled: self.settings.preferences.completion_enabled,
            question_enabled: self.settings.preferences.question_enabled,
            agent_details_enabled: self.settings.preferences.agent_details_enabled,
            quiet_hours,
            ..NotificationPolicy::default()
        }
    }

    fn refresh_history(&mut self) -> Result<(), DesktopError> {
        self.history = self
            .desktop
            .recent_history(Some(100))?
            .into_iter()
            .filter_map(map_history_item)
            .collect();
        merge_agent_history(&mut self.agent_monitors, &self.history);
        Ok(())
    }

    fn sync_remote_source_views(&mut self) {
        self.sources
            .retain(|source| source.kind == SourceKind::Local);
        self.sources.extend(
            self.settings
                .remote_sources
                .iter()
                .map(|source| SourceView {
                    id: format!("ssh:{}", source.host_alias),
                    name: source.local_label.clone(),
                    kind: SourceKind::RemoteSsh,
                    status: SourceStatus::Reconnecting,
                    detail: "Waiting to connect".to_owned(),
                    last_event_at: None,
                    action_required: None,
                }),
        );
        self.running_agents.retain(|agent| {
            agent.source_kind == SourceKind::Local
                || self
                    .settings
                    .remote_sources
                    .iter()
                    .any(|source| agent.source_id == format!("ssh:{}", source.host_alias))
        });
    }

    fn tray_state(&self) -> TrayState {
        if self.settings.paused {
            return TrayState::Paused;
        }
        if matches!(
            self.notification_permission,
            PermissionStatus::Denied | PermissionStatus::AlertsDisabled
        ) || self
            .sources
            .iter()
            .any(|source| source.status == SourceStatus::Error)
        {
            return TrayState::Error;
        }
        if self
            .agent_monitors
            .iter()
            .any(|monitor| monitor.status == AgentRuntimeStatus::Waiting)
        {
            return TrayState::Attention;
        }
        TrayState::Normal
    }
}

struct PipelineNotifier<'a> {
    notifier: &'a dyn Notifier,
    delivery: NotificationDelivery,
    language: crate::model::LanguagePreference,
    text_size: crate::model::TextSize,
    sound: Option<crate::model::NotificationSound>,
}

impl aizu_core::Notifier for PipelineNotifier<'_> {
    fn notify(
        &self,
        notification: &aizu_core::PreparedNotification,
    ) -> Result<(), aizu_core::NotifyError> {
        match self.delivery {
            NotificationDelivery::AizuBanner => {}
            NotificationDelivery::System => match self.notifier.permission_status() {
                Ok(
                    PermissionStatus::Denied
                    | PermissionStatus::NotDetermined
                    | PermissionStatus::AlertsDisabled,
                ) => {
                    return Err(aizu_core::NotifyError::PermissionDenied);
                }
                Ok(PermissionStatus::Granted) => {}
                Err(_) => return Err(aizu_core::NotifyError::Retryable),
            },
        }
        let activation = notification.activation.clone();
        self.notifier
            .notify(&Notification {
                id: stable_notification_id(&notification.identifier),
                title: notification.title.clone(),
                body: notification.body.clone(),
                sound: self.sound,
                delivery: self.delivery,
                language: self.language,
                text_size: self.text_size,
                can_activate_terminal: activation.is_some(),
                approval: None,
                activation,
            })
            .map_err(|_| aizu_core::NotifyError::Retryable)
    }
}

fn map_history_item(item: HistoryItem) -> Option<HistoryEvent> {
    let HistoryItem::Event(item) = item else {
        let HistoryItem::Gap(gap) = item else {
            return None;
        };
        return Some(HistoryEvent {
            id: format!("gap-{}", gap.row_id),
            kind: EventKind::DeliveryGap,
            title: "Some remote events are no longer available".to_owned(),
            summary: None,
            source_name: gap.source_label,
            occurred_at: gap.received_at.to_rfc3339(),
            delivery_status: DeliveryStatus::Failed,
            outcome: None,
            adapter: None,
        });
    };
    let kind = match item.event.kind {
        CoreEventKind::TaskCompleted => EventKind::TaskCompleted,
        CoreEventKind::AgentQuestion => EventKind::AgentQuestion,
    };
    let outcome = item.event.outcome.map(|outcome| match outcome {
        CoreOutcome::Succeeded => TaskOutcome::Succeeded,
        CoreOutcome::Failed => TaskOutcome::Failed,
        CoreOutcome::Cancelled => TaskOutcome::Cancelled,
        CoreOutcome::Unknown => TaskOutcome::Unknown,
    });
    let delivery_status = match item.delivery_state {
        CoreOutboxState::Pending | CoreOutboxState::FailedRetryable => DeliveryStatus::Pending,
        CoreOutboxState::Delivered => DeliveryStatus::Delivered,
        CoreOutboxState::Suppressed => DeliveryStatus::Suppressed,
        CoreOutboxState::FailedTerminal => DeliveryStatus::Failed,
    };
    let adapter = item
        .event
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.get("aizu_adapter"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let summary = matches!(
        (item.event.source.agent.as_str(), adapter.as_deref()),
        ("codex", Some("codex-v1")) | ("claude-code", Some("claude-code-v1"))
    )
    .then(|| item.event.body.as_deref().and_then(safe_agent_excerpt))
    .flatten();
    Some(HistoryEvent {
        id: item.event.id.to_string(),
        kind,
        title: item.event.title,
        summary,
        source_name: item.source_label,
        occurred_at: item.event.occurred_at.to_rfc3339(),
        delivery_status,
        outcome,
        adapter,
    })
}

fn derive_agent_monitors(history: &[HistoryEvent]) -> Vec<AgentMonitorView> {
    let mut monitors = default_agent_monitors();
    for monitor in &mut monitors {
        let adapter = match monitor.agent {
            AgentKind::Codex => "codex-v1",
            AgentKind::ClaudeCode => "claude-code-v1",
        };
        if let Some(event) = history
            .iter()
            .find(|event| event.adapter.as_deref() == Some(adapter))
        {
            monitor.status = match event.kind {
                EventKind::AgentQuestion => AgentRuntimeStatus::Waiting,
                EventKind::TaskCompleted if matches!(event.outcome, Some(TaskOutcome::Failed)) => {
                    AgentRuntimeStatus::Error
                }
                EventKind::TaskCompleted => AgentRuntimeStatus::Completed,
                EventKind::DeliveryGap => continue,
            };
            monitor.last_seen_at = Some(event.occurred_at.clone());
            "Verified hook event received".clone_into(&mut monitor.detail);
        }
    }
    monitors
}

fn merge_agent_history(monitors: &mut [AgentMonitorView], history: &[HistoryEvent]) {
    for derived in derive_agent_monitors(history) {
        let Some(observed_at) = derived.last_seen_at.as_deref() else {
            continue;
        };
        let Some(monitor) = monitors
            .iter_mut()
            .find(|monitor| monitor.agent == derived.agent)
        else {
            continue;
        };
        if monitor
            .last_seen_at
            .as_deref()
            .is_none_or(|last_seen| observed_at > last_seen)
        {
            monitor.status = derived.status;
            monitor.last_seen_at = derived.last_seen_at;
            monitor.detail = derived.detail;
        }
    }
}

fn stable_notification_id(identifier: &str) -> i32 {
    const OFFSET: u32 = 2_166_136_261;
    const PRIME: u32 = 16_777_619;
    let hash = identifier.as_bytes().iter().fold(OFFSET, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(PRIME)
    });
    i32::from_ne_bytes(hash.to_ne_bytes())
}

fn cli_status(diagnostic: &CliDiagnostic) -> (CliStatus, Option<String>) {
    match diagnostic {
        CliDiagnostic::Missing => (CliStatus::Missing, None),
        CliDiagnostic::Installed { version } => (CliStatus::Installed, Some(version.clone())),
        CliDiagnostic::VersionMismatch { version } => (CliStatus::VersionMismatch, version.clone()),
    }
}

const fn replace_managed_cli(diagnostic: &CliDiagnostic) -> bool {
    matches!(
        diagnostic,
        CliDiagnostic::Installed { .. } | CliDiagnostic::VersionMismatch { version: Some(_) }
    )
}

fn clock_minute(value: &str) -> u16 {
    value
        .split_once(':')
        .and_then(|(hours, minutes)| {
            Some(hours.parse::<u16>().ok()?.saturating_mul(60) + minutes.parse::<u16>().ok()?)
        })
        .unwrap_or_default()
}

fn sanitize_diagnostic(detail: &str) -> String {
    let summary = detail.split(':').next().unwrap_or("local pipeline error");
    summary
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect()
}

fn default_agent_monitors() -> Vec<AgentMonitorView> {
    [
        (AgentKind::Codex, "Codex"),
        (AgentKind::ClaudeCode, "Claude Code"),
    ]
    .into_iter()
    .map(|(agent, label)| AgentMonitorView {
        agent,
        label: label.to_owned(),
        status: AgentRuntimeStatus::NotDetected,
        hook_status: HookStatus::Missing,
        version: None,
        last_seen_at: None,
        detail: "Waiting for a verified hook event".to_owned(),
    })
    .collect()
}

pub struct DesktopState(Mutex<AppService>);

impl DesktopState {
    pub const fn new(service: AppService) -> Self {
        Self(Mutex::new(service))
    }

    pub fn lock(&self) -> Result<MutexGuard<'_, AppService>, DesktopError> {
        self.0.lock().map_err(|_| DesktopError::Lock)
    }

    pub fn try_lock(&self) -> Result<Option<MutexGuard<'_, AppService>>, DesktopError> {
        match self.0.try_lock() {
            Ok(state) => Ok(Some(state)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Poisoned(_)) => Err(DesktopError::Lock),
        }
    }
}

fn validate_preferences(preferences: &Preferences) -> Result<(), DesktopError> {
    let quiet = &preferences.quiet_hours;
    if !valid_clock_time(&quiet.start) || !valid_clock_time(&quiet.end) {
        return Err(DesktopError::InvalidQuietHours);
    }
    if quiet.enabled && quiet.start == quiet.end {
        return Err(DesktopError::EmptyQuietHours);
    }
    Ok(())
}

fn valid_clock_time(value: &str) -> bool {
    let Some((hour, minute)) = value.split_once(':') else {
        return false;
    };
    hour.len() == 2
        && minute.len() == 2
        && hour.parse::<u8>().is_ok_and(|hour| hour < 24)
        && minute.parse::<u8>().is_ok_and(|minute| minute < 60)
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, sync::Arc, time::SystemTime};

    use aizu_core::{
        HistoryGap, HistoryItem, ObservedAgentProcess, PreparedNotification, ProcessSnapshot,
        StatePaths,
    };
    use chrono::Utc;

    use crate::{
        model::{
            AgentKind, AgentRuntimeStatus, DeliveryStatus, EventKind, HistoryEvent,
            LanguagePreference, NotificationDelivery, NotificationSound, PermissionStatus,
            SourceKind, SourceStatus, TaskOutcome, TrayState,
        },
        notifier::FakeNotifier,
        store::SettingsStore,
    };

    use super::{
        AppService, DesktopState, PipelineNotifier, derive_agent_monitors, map_history_item,
        replace_managed_cli, stable_notification_id,
    };

    fn service(notifier: Arc<FakeNotifier>) -> AppService {
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("test clock should be valid")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("aizu-service-{suffix}"));
        let store = SettingsStore::new(directory.join("settings.json"));
        AppService::new(notifier, store, &StatePaths::new(directory.join("state")))
            .expect("service should initialize")
    }

    #[test]
    fn tray_access_does_not_wait_for_busy_desktop_state() {
        let state = DesktopState::new(service(FakeNotifier::with_permission(
            PermissionStatus::Granted,
        )));
        let _busy = state.lock().expect("desktop state lock");

        assert!(state.try_lock().expect("non-blocking lock check").is_none());
    }

    #[test]
    fn fake_notifier_receives_test_notification() {
        let notifier = FakeNotifier::with_permission(PermissionStatus::Granted);
        let mut service = service(notifier.clone());
        let mut preferences = service.view().preferences;
        preferences.notification_sound = NotificationSound::Bloom;
        service
            .update_preferences(preferences)
            .expect("sound choice should persist");

        service
            .send_test_notification()
            .expect("test notification should be scheduled");

        let notifications = notifier.notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].title, "Aizu test notification");
        assert_eq!(notifications[0].sound, Some(NotificationSound::Bloom));
        assert_eq!(notifications[0].delivery, NotificationDelivery::AizuBanner);
    }

    #[test]
    fn repeated_test_notifications_use_distinct_native_identifiers() {
        let notifier = FakeNotifier::with_permission(PermissionStatus::Granted);
        let mut service = service(notifier.clone());

        service
            .send_test_notification()
            .expect("first test notification should be scheduled");
        service
            .send_test_notification()
            .expect("second test notification should be scheduled");

        let notifications = notifier.notifications();
        assert_eq!(notifications.len(), 2);
        assert_ne!(notifications[0].id, notifications[1].id);
    }

    #[test]
    fn japanese_preference_localizes_the_test_notification() {
        let notifier = FakeNotifier::with_permission(PermissionStatus::Granted);
        let mut service = service(notifier.clone());
        let mut preferences = service.view().preferences;
        preferences.language = LanguagePreference::Japanese;
        service
            .update_preferences(preferences)
            .expect("language choice should persist");

        service
            .send_test_notification()
            .expect("test notification should be scheduled");

        let notifications = notifier.notifications();
        assert_eq!(notifications[0].title, "Aizu テスト通知");
        assert_eq!(notifications[0].body, "Aizuバナーを利用できます。");
    }

    #[test]
    fn test_notification_requires_granted_permission() {
        let notifier = FakeNotifier::with_permission(PermissionStatus::Denied);
        let mut service = service(notifier.clone());
        let mut preferences = service.view().preferences;
        preferences.notification_delivery = NotificationDelivery::System;
        service
            .update_preferences(preferences)
            .expect("system notification choice should persist");

        assert!(matches!(
            service.send_test_notification(),
            Err(super::DesktopError::NotificationPermissionDisabled)
        ));
        assert!(notifier.notifications().is_empty());
    }

    #[test]
    fn aizu_banner_does_not_require_native_notification_permission() {
        let notifier = FakeNotifier::with_permission(PermissionStatus::Denied);
        let mut service = service(notifier.clone());

        service
            .send_test_notification()
            .expect("Aizu Banner should not depend on macOS notification permission");

        let notifications = notifier.notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].delivery, NotificationDelivery::AizuBanner);
        assert_eq!(
            service.view().notification_permission,
            PermissionStatus::Granted
        );
    }

    #[test]
    fn paused_state_has_priority_in_tray_state_machine() {
        let notifier = FakeNotifier::with_permission(PermissionStatus::Denied);
        let mut service = service(notifier);

        service.set_paused(true).expect("pause should persist");

        assert_eq!(service.view().tray_state, TrayState::Paused);
    }

    #[test]
    fn every_running_agent_process_is_exposed_without_pid_or_arguments() {
        let notifier = FakeNotifier::with_permission(PermissionStatus::Granted);
        let mut service = service(notifier);
        let snapshot = ProcessSnapshot::new(
            Utc::now(),
            vec![
                ObservedAgentProcess::running(
                    aizu_core::AgentKind::Codex,
                    NonZeroU32::new(101).unwrap(),
                ),
                ObservedAgentProcess::running(
                    aizu_core::AgentKind::Codex,
                    NonZeroU32::new(202).unwrap(),
                ),
                ObservedAgentProcess::running(
                    aizu_core::AgentKind::ClaudeCode,
                    NonZeroU32::new(303).unwrap(),
                ),
            ],
        )
        .expect("valid snapshot");

        assert!(service.update_process_snapshot(&snapshot));
        let view = service.view();
        assert_eq!(
            view.running_agents
                .iter()
                .map(|agent| agent.label.as_str())
                .collect::<Vec<_>>(),
            ["Codex 1", "Codex 2", "Claude Code 1"]
        );
        let serialized = serde_json::to_string(&view.running_agents).expect("serialize agents");
        assert!(!serialized.contains("101"));
        assert!(!serialized.contains("202"));
        assert!(!serialized.contains("303"));
    }

    #[test]
    fn connected_remote_agent_processes_are_listed_with_their_source() {
        let notifier = FakeNotifier::with_permission(PermissionStatus::Granted);
        let mut service = service(notifier);
        service
            .add_remote_source("remote-host".to_owned(), "Remote host".to_owned())
            .expect("remote source should register");
        assert!(service.set_remote_status("remote-host", SourceStatus::Connected, "Connected"));
        let first_epoch = service.connected_remote_agent_sources()[0].1;
        assert!(service.update_remote_agents(
            "remote-host",
            first_epoch,
            &[
                aizu_core::AgentKind::Codex,
                aizu_core::AgentKind::ClaudeCode,
            ]
        ));

        let view = service.view();
        assert_eq!(view.running_agents.len(), 2);
        assert!(view.running_agents.iter().all(|agent| {
            agent.source_id == "ssh:remote-host"
                && agent.source_name == "Remote host"
                && agent.source_kind == SourceKind::RemoteSsh
        }));

        assert!(service.set_remote_status(
            "remote-host",
            SourceStatus::Reconnecting,
            "Connection interrupted; retrying"
        ));
        assert!(service.view().running_agents.is_empty());

        assert!(service.set_remote_status("remote-host", SourceStatus::Connected, "Connected"));
        let reconnected_epoch = service.connected_remote_agent_sources()[0].1;
        assert_ne!(first_epoch, reconnected_epoch);
        assert!(!service.update_remote_agents(
            "remote-host",
            first_epoch,
            &[aizu_core::AgentKind::Codex]
        ));
        assert!(service.view().running_agents.is_empty());
        assert!(service.update_remote_agents(
            "remote-host",
            reconnected_epoch,
            &[aizu_core::AgentKind::ClaudeCode]
        ));
        assert_eq!(service.view().running_agents.len(), 1);
    }

    #[test]
    fn only_a_verified_aizu_cli_enables_replacement() {
        assert!(replace_managed_cli(
            &crate::cli_diagnostic::CliDiagnostic::Installed {
                version: "0.1.0".to_owned(),
            }
        ));
        assert!(replace_managed_cli(
            &crate::cli_diagnostic::CliDiagnostic::VersionMismatch {
                version: Some("0.0.9".to_owned()),
            }
        ));
        assert!(!replace_managed_cli(
            &crate::cli_diagnostic::CliDiagnostic::Missing
        ));
        assert!(!replace_managed_cli(
            &crate::cli_diagnostic::CliDiagnostic::VersionMismatch { version: None }
        ));
    }

    #[test]
    fn reconnect_all_skips_sources_waiting_for_identity_confirmation() {
        let notifier = FakeNotifier::with_permission(PermissionStatus::Granted);
        let mut service = service(notifier);
        service
            .add_remote_source("build-host".to_owned(), "Build".to_owned())
            .expect("first source should register");
        service
            .add_remote_source("review-host".to_owned(), "Review".to_owned())
            .expect("second source should register");
        assert!(service.require_remote_identity_confirmation("review-host", uuid::Uuid::new_v4()));

        service
            .reconnect_all_remote_sources()
            .expect("reconnect should persist");

        let generations: std::collections::BTreeMap<_, _> = service
            .remote_sources()
            .into_iter()
            .map(|source| (source.host_alias, source.reconnect_generation))
            .collect();
        assert_eq!(generations["build-host"], 1);
        assert_eq!(generations["review-host"], 0);
    }

    #[cfg(unix)]
    #[test]
    fn failed_source_removal_keeps_settings_and_identity_pin() {
        use std::os::unix::fs::symlink;

        let notifier = FakeNotifier::with_permission(PermissionStatus::Granted);
        let mut service = service(notifier);
        service
            .add_remote_source("remote-host".to_owned(), "Remote host".to_owned())
            .expect("remote source should register");
        let source_id = uuid::Uuid::new_v4();
        service
            .desktop
            .pin_source("ssh:remote-host", source_id)
            .expect("identity should pin");

        let settings_path = service.store.path.clone();
        std::fs::remove_file(&settings_path).expect("settings should be removable");
        symlink("missing-settings-target", &settings_path).expect("failure symlink should exist");

        assert!(matches!(
            service.remove_remote_source("remote-host"),
            Err(super::DesktopError::Store(_))
        ));
        assert!(
            service
                .remote_sources()
                .iter()
                .any(|source| source.host_alias == "remote-host")
        );
        assert_eq!(
            service
                .desktop
                .source("ssh:remote-host")
                .expect("source should load")
                .expect("source should remain")
                .pinned_source_id,
            Some(source_id)
        );
    }

    #[test]
    fn system_notifications_are_stable_and_retain_trusted_terminal_actions() {
        let notifier = FakeNotifier::with_permission(PermissionStatus::Granted);
        let adapter = PipelineNotifier {
            notifier: notifier.as_ref(),
            delivery: NotificationDelivery::System,
            language: LanguagePreference::English,
            text_size: crate::model::TextSize::Large,
            sound: Some(crate::model::NotificationSound::Pulse),
        };
        let prepared = PreparedNotification {
            identifier: format!(
                "aizu-event-{}-{}",
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4()
            ),
            title: "Agent is waiting for input".to_owned(),
            body: "Agent is waiting for input on This Mac".to_owned(),
            activation: Some(aizu_core::TerminalActivation {
                application: aizu_core::TerminalApplication::Iterm2,
                application_session: Some("w0t0p0:ABCD".to_owned()),
                tmux: None,
            }),
        };

        aizu_core::Notifier::notify(&adapter, &prepared).expect("notification should schedule");

        let notifications = notifier.notifications();
        assert_eq!(
            notifications[0].id,
            stable_notification_id(&prepared.identifier)
        );
        assert_eq!(
            notifications[0].sound,
            Some(crate::model::NotificationSound::Pulse)
        );
        assert_eq!(notifications[0].text_size, crate::model::TextSize::Large);
        assert!(notifications[0].can_activate_terminal);
        assert!(notifications[0].activation.is_some());

        let banner_notifier = FakeNotifier::with_permission(PermissionStatus::NotDetermined);
        let banner_adapter = PipelineNotifier {
            notifier: banner_notifier.as_ref(),
            delivery: NotificationDelivery::AizuBanner,
            language: LanguagePreference::English,
            text_size: crate::model::TextSize::Large,
            sound: None,
        };
        aizu_core::Notifier::notify(&banner_adapter, &prepared)
            .expect("Aizu Banner should schedule without native permission");
        let banners = banner_notifier.notifications();
        assert!(banners[0].can_activate_terminal);
        assert!(banners[0].activation.is_some());
    }

    #[test]
    fn history_gap_is_exposed_without_remote_detail() {
        let occurred_at = Utc::now();
        let event = map_history_item(HistoryItem::Gap(HistoryGap {
            row_id: 7,
            source_key: "ssh:work".to_owned(),
            source_label: "Work host".to_owned(),
            lost_from_sequence: 2,
            lost_through_sequence: 8,
            received_at: occurred_at,
        }))
        .expect("gap should be visible");

        assert_eq!(event.title, "Some remote events are no longer available");
        assert_eq!(event.source_name, "Work host");
    }

    #[test]
    fn agent_failure_state_uses_task_outcome_not_notification_delivery() {
        let event = |outcome, delivery_status| HistoryEvent {
            id: uuid::Uuid::new_v4().to_string(),
            kind: EventKind::TaskCompleted,
            title: "private title".to_owned(),
            summary: None,
            source_name: "Local".to_owned(),
            occurred_at: Utc::now().to_rfc3339(),
            delivery_status,
            outcome,
            adapter: Some("claude-code-v1".to_owned()),
        };
        let failed =
            derive_agent_monitors(&[event(Some(TaskOutcome::Failed), DeliveryStatus::Delivered)]);
        assert_eq!(
            failed
                .iter()
                .find(|monitor| monitor.agent == AgentKind::ClaudeCode)
                .expect("claude monitor")
                .status,
            AgentRuntimeStatus::Error
        );
        let successful =
            derive_agent_monitors(&[event(Some(TaskOutcome::Succeeded), DeliveryStatus::Failed)]);
        assert_eq!(
            successful
                .iter()
                .find(|monitor| monitor.agent == AgentKind::ClaudeCode)
                .expect("claude monitor")
                .status,
            AgentRuntimeStatus::Completed
        );
    }
}
