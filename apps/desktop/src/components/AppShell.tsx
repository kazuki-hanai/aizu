import {
  Activity,
  AlertTriangle,
  Bell,
  BellOff,
  Cable,
  CheckCircle2,
  CircleHelp,
  Clock3,
  Inbox,
  Laptop,
  ListChecks,
  LoaderCircle,
  Languages,
  MessageSquareText,
  Plus,
  RefreshCw,
  Settings2,
  ShieldCheck,
  SlidersHorizontal,
  Type,
  Trash2,
  Volume2,
  Wrench,
  X,
} from "lucide-react";
import { useCallback, useMemo, useRef, useState, type KeyboardEvent } from "react";

import { formatTimestamp } from "../lib/format";
import type { AgentMonitor, AppView, Preferences, RunningAgent, SourceView, SshConnectionTestResult } from "../lib/contracts";
import { messages, resolveLanguage } from "../lib/i18n";
import { AgentIcon } from "./AgentIcon";
import { BrandMark } from "./BrandMark";
import { StatusBadge } from "./StatusBadge";

type ViewName = "agents" | "sources" | "settings";
type AppMessages = ReturnType<typeof messages>;

const localizedState = (value: string, t: AppMessages): string => {
  switch (value) {
    case "normal": return t.normal;
    case "attention": return t.attention;
    case "paused": return t.paused;
    case "error": return t.error;
    case "connected": return t.connected;
    case "reconnecting": return t.reconnecting;
    case "disabled": return t.disabled;
    case "pending": return t.pending;
    case "delivered": return t.delivered;
    case "suppressed": return t.suppressed;
    case "failed": return t.failed;
    case "configured": return t.configured;
    case "missing": return t.missing;
    case "approvalRequired": return t.approvalRequired;
    case "unsupported": return t.unsupported;
    default: return t.unknown;
  }
};

type AppShellProps = {
  view: AppView;
  busy: boolean;
  onPauseChange: (paused: boolean) => Promise<void>;
  onSendTest: () => Promise<void>;
  onClearHistory: () => Promise<void>;
  onUpdatePreferences: (preferences: Preferences) => Promise<void>;
  onAddRemoteSource: (hostAlias: string, localLabel: string) => Promise<boolean>;
  onTestRemoteConnection: (hostAlias: string) => Promise<SshConnectionTestResult>;
  onRemoveRemoteSource: (hostAlias: string) => Promise<void>;
  onReconnectRemoteSource: (hostAlias: string) => Promise<void>;
  onConfirmRemoteIdentity: (hostAlias: string) => Promise<void>;
  onInstallCli: () => Promise<void>;
  onConfigureAgents: () => Promise<void>;
  onConfirmCodexTrust: () => Promise<void>;
};

const sourceTone = (source: SourceView) => {
  if (source.status === "connected") return "success" as const;
  if (source.status === "reconnecting") return "warning" as const;
  if (source.status === "error") return "danger" as const;
  return "muted" as const;
};

export function AppShell({
  view,
  busy,
  onPauseChange,
  onSendTest,
  onClearHistory,
  onUpdatePreferences,
  onAddRemoteSource,
  onTestRemoteConnection,
  onRemoveRemoteSource,
  onReconnectRemoteSource,
  onConfirmRemoteIdentity,
  onInstallCli,
  onConfigureAgents,
  onConfirmCodexTrust,
}: AppShellProps) {
  const [activeView, setActiveView] = useState<ViewName>("agents");
  const t = messages(view.preferences.language);
  const locale = resolveLanguage(view.preferences.language);
  const heading = t[activeView];

  const content = useMemo(() => {
    switch (activeView) {
      case "sources":
        return <SourcesView appVersion={view.appVersion} busy={busy} cliStatus={view.cliStatus} cliVersion={view.cliVersion} locale={locale} onAdd={onAddRemoteSource} onConfirmIdentity={onConfirmRemoteIdentity} onInstallCli={onInstallCli} onReconnect={onReconnectRemoteSource} onRemove={onRemoveRemoteSource} onTest={onTestRemoteConnection} sources={view.sources} t={t} />;
      case "settings":
        return (
          <SettingsView
            busy={busy}
            muted={view.paused}
            onChange={onUpdatePreferences}
            onMuteChange={onPauseChange}
            preferences={view.preferences}
            onSendTest={onSendTest}
            t={t}
          />
        );
      case "agents":
        return (
          <AgentsView
            busy={busy}
            onClearActivity={onClearHistory}
            onConfigureAgents={onConfigureAgents}
            onConfirmCodexTrust={onConfirmCodexTrust}
            locale={locale}
            t={t}
            view={view}
          />
        );
    }
  }, [
    activeView,
    busy,
    onClearHistory,
    onUpdatePreferences,
    onAddRemoteSource,
    onTestRemoteConnection,
    onRemoveRemoteSource,
    onReconnectRemoteSource,
    onConfirmRemoteIdentity,
    onInstallCli,
    onConfigureAgents,
    onConfirmCodexTrust,
    onPauseChange,
    onSendTest,
    locale,
    t,
    view,
  ]);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="sidebar__brand">
          <BrandMark small />
          <div>
            <strong>Aizu</strong>
            <span>{t.agentAlerts}</span>
          </div>
        </div>

        <nav aria-label={t.mainNavigation} className="sidebar__nav">
          <NavButton
            active={activeView === "agents"}
            icon={Activity}
            label={t.agents}
            onClick={() => setActiveView("agents")}
          />
          <NavButton
            active={activeView === "sources"}
            icon={Laptop}
            label={t.sources}
            onClick={() => setActiveView("sources")}
          />
          <NavButton
            active={activeView === "settings"}
            icon={Settings2}
            label={t.settings}
            onClick={() => setActiveView("settings")}
          />
        </nav>

      </aside>

      <main className="main-panel">
        <header className="topbar">
          <h1>{heading}</h1>
        </header>
        <div className="content-area">{content}</div>
      </main>
    </div>
  );
}

type NavButtonProps = {
  active: boolean;
  icon: typeof Activity;
  label: string;
  onClick: () => void;
};

function NavButton({ active, icon: Icon, label, onClick }: NavButtonProps) {
  return (
    <button
      aria-current={active ? "page" : undefined}
      className={active ? "nav-button is-active" : "nav-button"}
      onClick={onClick}
      type="button"
    >
      <Icon aria-hidden="true" size={18} />
      {label}
    </button>
  );
}

type AgentsViewProps = {
  busy: boolean;
  view: AppView;
  onClearActivity: () => Promise<void>;
  onConfigureAgents: () => Promise<void>;
  onConfirmCodexTrust: () => Promise<void>;
  locale: string;
  t: AppMessages;
};

function AgentsView({ busy, locale, view, onClearActivity, onConfigureAgents, onConfirmCodexTrust, t }: AgentsViewProps) {
  const runningSourceCount = new Set(view.runningAgents.map((agent) => agent.sourceId)).size;

  return (
    <div className="agents-layout">
      <section className="section-block">
        <div className="section-heading">
          <div>
            <h2>{t.runningAgents}</h2>
            <p>{view.runningAgents.length === 0 ? t.noAgentsRunning : t.processCount(view.runningAgents.length, runningSourceCount)}</p>
          </div>
          {view.agentMonitors.some((agent) => agent.hookStatus === "approvalRequired") ? (
            <button className="button button--secondary" disabled={busy} onClick={() => void onConfirmCodexTrust()} type="button">
              <ShieldCheck aria-hidden="true" size={16} />
              {t.confirmCodex}
            </button>
          ) : view.agentMonitors.some((agent) => agent.hookStatus === "missing") ? (
            <button className="button button--secondary" disabled={busy} onClick={() => void onConfigureAgents()} type="button">
              <Wrench aria-hidden="true" size={16} />
              {t.setUpAgents}
            </button>
          ) : null}
        </div>
        <div className="agent-list">
          {view.runningAgents.length === 0 ? (
            <div className="agent-list__empty">{t.startAgents}</div>
          ) : view.runningAgents.map((agent) => (
            <RunningAgentRow
              agent={agent}
              key={`${agent.sourceId}:${agent.label}`}
              monitor={agent.sourceKind === "local" ? view.agentMonitors.find((monitor) => monitor.agent === agent.agent) : undefined}
              t={t}
            />
          ))}
        </div>
      </section>

      <ActivityView busy={busy} locale={locale} onClear={onClearActivity} t={t} view={view} />
    </div>
  );
}

function RunningAgentRow({ agent, monitor, t }: { agent: RunningAgent; monitor: AgentMonitor | undefined; t: AppMessages }) {
  return (
    <article className="agent-row">
      <AgentIcon agent={agent.agent} />
      <div className="agent-row__identity">
        <strong>{agent.label}</strong>
        <span>{agent.sourceKind === "local" ? t.local : agent.sourceName}</span>
      </div>
      <div className="agent-row__detail">
        <span>{agent.sourceKind === "remoteSsh" ? t.runningViaSsh : t.currentlyRunning}</span>
        <small>{agent.sourceKind === "remoteSsh" ? t.remoteDetected : `${monitor?.version ?? t.versionUnknown} · ${t.hook} ${monitor === undefined ? t.unknown : localizedState(monitor.hookStatus, t)}`}</small>
      </div>
      <StatusBadge label={t.running} tone="success" />
    </article>
  );
}

type SourcesViewProps = {
  appVersion: string;
  busy: boolean;
  cliStatus: AppView["cliStatus"];
  cliVersion: string | null;
  sources: SourceView[];
  onAdd: (hostAlias: string, localLabel: string) => Promise<boolean>;
  onTest: (hostAlias: string) => Promise<SshConnectionTestResult>;
  onRemove: (hostAlias: string) => Promise<void>;
  onReconnect: (hostAlias: string) => Promise<void>;
  onConfirmIdentity: (hostAlias: string) => Promise<void>;
  onInstallCli: () => Promise<void>;
  locale: string;
  t: AppMessages;
};

function SourcesView({ appVersion, busy, cliStatus, cliVersion, locale, sources, onAdd, onRemove, onReconnect, onConfirmIdentity, onInstallCli, onTest, t }: SourcesViewProps) {
  const [addSourceOpen, setAddSourceOpen] = useState(false);
  const [hostAlias, setHostAlias] = useState("");
  const [localLabel, setLocalLabel] = useState("");
  const [connectionResult, setConnectionResult] = useState<SshConnectionTestResult | null>(null);
  const [testingConnection, setTestingConnection] = useState(false);
  const addSourceButton = useRef<HTMLButtonElement>(null);
  const sourceDialog = useRef<HTMLElement>(null);
  const testGeneration = useRef(0);

  const closeAddSource = useCallback(() => {
    testGeneration.current += 1;
    setAddSourceOpen(false);
    setHostAlias("");
    setLocalLabel("");
    setConnectionResult(null);
    setTestingConnection(false);
    window.requestAnimationFrame(() => addSourceButton.current?.focus());
  }, []);

  const handleDialogKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      closeAddSource();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = Array.from(sourceDialog.current?.querySelectorAll<HTMLElement>(
      "button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex='-1'])",
    ) ?? []);
    const first = focusable.at(0);
    const last = focusable.at(-1);
    if (!first || !last) {
      event.preventDefault();
      return;
    }
    const active = document.activeElement;
    if (event.shiftKey && (active === first || !sourceDialog.current?.contains(active))) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (active === last || !sourceDialog.current?.contains(active))) {
      event.preventDefault();
      first.focus();
    }
  };

  const testConnection = async () => {
    const generation = testGeneration.current + 1;
    testGeneration.current = generation;
    setConnectionResult(null);
    setTestingConnection(true);
    try {
      const result = await onTest(hostAlias);
      if (testGeneration.current === generation) setConnectionResult(result);
    } catch {
      if (testGeneration.current === generation) {
        setConnectionResult({
          status: "remoteFailure",
          message: t.sshTestError,
          configResolved: false,
          reachable: false,
          protocolCompatible: false,
          remoteVersion: null,
        });
      }
    } finally {
      if (testGeneration.current === generation) setTestingConnection(false);
    }
  };

  return (
    <div className="sources-layout">
    <section className="section-block">
      <div className="section-heading">
        <div>
          <h2>{t.configuredSources}</h2>
          <p>{t.sourcesDescription}</p>
        </div>
        <button
          aria-label={t.addSshSource}
          className="icon-button"
          disabled={busy}
          onClick={() => setAddSourceOpen(true)}
          ref={addSourceButton}
          title={t.addSshSource}
          type="button"
        >
          <Plus aria-hidden="true" size={18} />
        </button>
      </div>
      {cliStatus === "installed" ? null : (
        <div className="source-repair">
          <AlertTriangle aria-hidden="true" size={18} />
          <div>
            <strong>{cliStatus === "missing" ? t.cliMissing : t.cliUpdateRequired}</strong>
            <span>{cliStatus === "missing" ? t.cliMissingHelp : t.cliVersionMismatch(cliVersion ?? t.unknown, appVersion)}</span>
          </div>
          <button className="button button--secondary" disabled={busy} onClick={() => void onInstallCli()} type="button">
            <Wrench aria-hidden="true" size={15} />
            {cliStatus === "missing" ? t.installCli : t.updateCli}
          </button>
        </div>
      )}
      <div className="source-list">
        {sources.map((source) => (
          <div className="source-manage-row" key={source.id}>
            <SourceRow locale={locale} source={source} t={t} />
            {source.kind === "remoteSsh" ? <div className="source-actions">
              {source.actionRequired === "confirmIdentityChange" ? <button className="button button--warning" disabled={busy} onClick={() => void onConfirmIdentity(source.id.slice(4))} type="button">{t.confirmIdentity}</button> : null}
              <button className="icon-button" disabled={busy} onClick={() => void onReconnect(source.id.slice(4))} title={`${t.reconnect} ${source.name}`} type="button"><RefreshCw aria-hidden="true" size={15} /><span className="sr-only">{t.reconnect} {source.name}</span></button>
              <button className="text-button" disabled={busy} onClick={() => void onRemove(source.id.slice(4))} type="button">{t.remove}</button>
            </div> : null}
          </div>
        ))}
      </div>
    </section>
    {addSourceOpen ? (
      <div className="dialog-backdrop" role="presentation">
        <section aria-labelledby="add-ssh-source-title" aria-modal="true" className="source-dialog" onKeyDown={handleDialogKeyDown} ref={sourceDialog} role="dialog">
          <div className="source-dialog__header">
            <div>
              <h2 id="add-ssh-source-title">{t.addSshSource}</h2>
              <p>{t.sshAliasHelp}</p>
            </div>
            <button aria-label={t.close} className="icon-button" disabled={busy} onClick={closeAddSource} title={t.close} type="button">
              <X aria-hidden="true" size={18} />
            </button>
          </div>
          <form className="source-form" onSubmit={(event) => { event.preventDefault(); void onAdd(hostAlias, localLabel).then((added) => { if (added) closeAddSource(); }); }}>
            <label>{t.sshAlias}<input autoComplete="off" autoFocus maxLength={255} onChange={(event) => { testGeneration.current += 1; setHostAlias(event.target.value); setConnectionResult(null); setTestingConnection(false); }} required value={hostAlias} /></label>
            <label>{t.localLabel}<input autoComplete="off" maxLength={200} onChange={(event) => setLocalLabel(event.target.value)} required value={localLabel} /></label>
            <div className="source-form__actions">
              <button className="button button--secondary" disabled={busy || testingConnection || hostAlias.length === 0} onClick={() => void testConnection()} type="button">
                {testingConnection ? <LoaderCircle aria-hidden="true" className="spin" size={15} /> : <Cable aria-hidden="true" size={15} />}
                {testingConnection ? t.testing : t.testConnection}
              </button>
              <button className="button button--primary" disabled={busy || testingConnection} type="submit">{t.addSource}</button>
            </div>
          </form>
          {connectionResult === null ? null : (
            <div className={`connection-test-result connection-test-result--${connectionResult.status === "compatible" ? "success" : "error"}`} role={connectionResult.status === "compatible" ? "status" : "alert"}>
              {connectionResult.status === "compatible" ? <CheckCircle2 aria-hidden="true" size={17} /> : <AlertTriangle aria-hidden="true" size={17} />}
              <div>
                <strong>{connectionResult.message}</strong>
                {connectionResult.remoteVersion === null ? null : <span>{t.remoteAizu} {connectionResult.remoteVersion}</span>}
              </div>
            </div>
          )}
        </section>
      </div>
    ) : null}
    </div>
  );
}

function SourceRow({ locale, source, t }: { locale: string; source: SourceView; t: AppMessages }) {
  return (
    <article className="source-row">
      <span className="source-row__icon" aria-hidden="true"><Laptop size={19} /></span>
      <div className="source-row__identity">
        <strong>{source.kind === "local" ? t.thisMac : source.name}</strong>
        <span>{source.kind === "local" ? t.local : t.remoteSsh}</span>
      </div>
      <div className="source-row__detail">
        <span>{source.status === "error" ? source.detail : localizedState(source.status, t)}</span>
        <small>{source.lastEventAt === null ? t.noEventsYet : formatTimestamp(source.lastEventAt, locale, t.timeUnavailable)}</small>
      </div>
      <StatusBadge label={localizedState(source.status, t)} tone={sourceTone(source)} />
    </article>
  );
}

function ActivityView({ busy, locale, onClear, t, view }: { busy: boolean; locale: string; onClear: () => Promise<void>; t: AppMessages; view: AppView }) {
  if (view.history.length === 0) {
    return (
      <section className="section-block empty-state">
        <Inbox aria-hidden="true" size={30} />
        <h2>{t.noRecentActivity}</h2>
      </section>
    );
  }

  return (
    <section className="section-block activity-block">
      <div className="section-heading">
        <div><h2>{t.recentActivity}</h2></div>
        <button
          aria-label={t.clearRecentActivity}
          className="icon-button"
          disabled={busy}
          onClick={() => {
            if (window.confirm(t.clearConfirm)) {
              void onClear();
            }
          }}
          title={t.clearRecentActivity}
          type="button"
        >
          <Trash2 aria-hidden="true" size={16} />
        </button>
      </div>
      <div className="history-list">
        {view.history.slice(0, 5).map((event) => (
          <article
            className="history-row"
            key={event.id}
          >
            <span className="history-row__icon" aria-hidden="true">
              {event.kind === "agentQuestion" ? <CircleHelp size={18} /> : event.kind === "deliveryGap" ? <AlertTriangle size={18} /> : <CheckCircle2 size={18} />}
            </span>
            <div className="history-row__body">
              <strong>{event.title}</strong>
              {view.preferences.agentDetailsEnabled && event.summary !== null ? <span className="history-row__summary" title={event.summary}>{event.summary}</span> : null}
              <div className="history-row__meta">
                <span>{event.sourceName}</span>
                <span aria-hidden="true">·</span>
                <time dateTime={event.occurredAt}>{formatTimestamp(event.occurredAt, locale, t.timeUnavailable)}</time>
              </div>
            </div>
            {event.deliveryStatus === "delivered" ? (
              <span className="sr-only">{t.delivered}</span>
            ) : (
              <div className="history-row__status">
                <StatusBadge
                  label={localizedState(event.deliveryStatus, t)}
                  tone={event.deliveryStatus === "failed" ? "danger" : event.deliveryStatus === "pending" ? "warning" : "muted"}
                />
              </div>
            )}
          </article>
        ))}
      </div>
    </section>
  );
}

type SettingsViewProps = {
  busy: boolean;
  muted: boolean;
  onChange: (preferences: Preferences) => Promise<void>;
  onMuteChange: (muted: boolean) => Promise<void>;
  preferences: Preferences;
  onSendTest: () => Promise<void>;
  t: AppMessages;
};

function SettingsView({ busy, muted, onChange, onMuteChange, preferences, onSendTest, t }: SettingsViewProps) {
  const toggle = (key: "completionEnabled" | "questionEnabled" | "agentDetailsEnabled" | "launchAtLogin") => {
    void onChange({ ...preferences, [key]: !preferences[key] });
  };
  const sounds: readonly { label: string; value: Preferences["notificationSound"] | "off" }[] = [
    { label: t.sounds.off, value: "off" },
    { label: t.sounds.default, value: "default" },
    { label: t.sounds.glass, value: "glass" },
    { label: t.sounds.ping, value: "ping" },
    { label: t.sounds.pop, value: "pop" },
    { label: t.sounds.hero, value: "hero" },
  ];

  return (
    <section className="settings-layout">
      <div className="settings-title"><Bell size={18} aria-hidden="true" /><h2>{t.notifications}</h2></div>
      <div className="settings-list">
        <label className="setting-row setting-row--select">
          <Languages aria-hidden="true" size={17} />
          <span>{t.language}</span>
          <select
            aria-label={t.language}
            disabled={busy}
            value={preferences.language}
            onChange={(event) => {
              const language = event.target.value;
              if (language === "system" || language === "ja" || language === "en") {
                void onChange({ ...preferences, language });
              }
            }}
          >
            <option value="system">{t.languageSystem}</option>
            <option value="ja">{t.languageJapanese}</option>
            <option value="en">{t.languageEnglish}</option>
          </select>
        </label>
        <label className="setting-row setting-row--select">
          <Type aria-hidden="true" size={17} />
          <span>{t.textSize}</span>
          <select
            aria-label={t.textSize}
            disabled={busy}
            value={preferences.textSize}
            onChange={(event) => {
              const textSize = event.target.value;
              if (textSize === "small" || textSize === "standard" || textSize === "large") {
                void onChange({ ...preferences, textSize });
              }
            }}
          >
            <option value="small">{t.textSizeSmall}</option>
            <option value="standard">{t.textSizeStandard}</option>
            <option value="large">{t.textSizeLarge}</option>
          </select>
        </label>
        <SettingToggle checked={muted} disabled={busy} icon={BellOff} label={t.muteNotifications} onChange={() => void onMuteChange(!muted)} />
        <SettingToggle checked={preferences.completionEnabled} disabled={busy} icon={ListChecks} label={t.taskCompletion} onChange={() => toggle("completionEnabled")} />
        <SettingToggle checked={preferences.questionEnabled} disabled={busy} icon={CircleHelp} label={t.agentQuestions} onChange={() => toggle("questionEnabled")} />
        <label className="setting-row setting-row--select">
          <Bell aria-hidden="true" size={17} />
          <span>{t.notificationStyle}</span>
          <select
            aria-label={t.notificationStyle}
            disabled={busy}
            value={preferences.notificationDelivery}
            onChange={(event) => {
              const notificationDelivery = event.target.value;
              if (notificationDelivery === "aizuBanner" || notificationDelivery === "system") {
                void onChange({ ...preferences, notificationDelivery });
              }
            }}
          >
            <option value="aizuBanner">{t.aizuBanner}</option>
            <option value="system">{t.macosNotifications}</option>
          </select>
        </label>
        <label className="setting-row setting-row--select">
          <Volume2 aria-hidden="true" size={17} />
          <span>{t.notificationSound}</span>
          <select
            aria-label={t.notificationSound}
            disabled={busy}
            value={preferences.soundEnabled ? preferences.notificationSound : "off"}
            onChange={(event) => {
              const selected = sounds.find((sound) => sound.value === event.target.value);
              if (!selected) return;
              if (selected.value === "off") {
                void onChange({ ...preferences, soundEnabled: false });
              } else {
                void onChange({ ...preferences, soundEnabled: true, notificationSound: selected.value });
              }
            }}
          >
            {sounds.map((sound) => <option key={sound.value} value={sound.value}>{sound.label}</option>)}
          </select>
        </label>
        <div className="setting-row setting-row--command">
          <Bell aria-hidden="true" size={17} />
          <span>{t.testNotification}</span>
          <button aria-label={t.sendTestNotification} className="button button--secondary" disabled={busy} onClick={() => void onSendTest()} type="button">{t.sendTest}</button>
        </div>
      </div>

      <details className="settings-advanced">
        <summary><SlidersHorizontal aria-hidden="true" size={17} /><span>{t.advanced}</span></summary>
        <div className="settings-advanced__content">
          <SettingToggle checked={preferences.agentDetailsEnabled} disabled={busy} icon={MessageSquareText} label={t.showAgentDetails} onChange={() => toggle("agentDetailsEnabled")} />
          <SettingToggle checked={preferences.launchAtLogin} disabled={busy} icon={Clock3} label={t.launchAtLogin} onChange={() => toggle("launchAtLogin")} />
          <SettingToggle checked={preferences.quietHours.enabled} disabled={busy} icon={Clock3} label={t.muteOnSchedule} onChange={() => void onChange({ ...preferences, quietHours: { ...preferences.quietHours, enabled: !preferences.quietHours.enabled } })} />
          {preferences.quietHours.enabled ? <>
            <div className="time-controls">
              <label>{t.start}<input disabled={busy} type="time" value={preferences.quietHours.start} onChange={(event) => void onChange({ ...preferences, quietHours: { ...preferences.quietHours, start: event.target.value } })} /></label>
              <label>{t.end}<input disabled={busy} type="time" value={preferences.quietHours.end} onChange={(event) => void onChange({ ...preferences, quietHours: { ...preferences.quietHours, end: event.target.value } })} /></label>
            </div>
            <SettingToggle checked={preferences.quietHours.questionsBypass} disabled={busy} icon={CircleHelp} label={t.allowQuestionsMuted} onChange={() => void onChange({ ...preferences, quietHours: { ...preferences.quietHours, questionsBypass: !preferences.quietHours.questionsBypass } })} />
          </> : null}
        </div>
      </details>
    </section>
  );
}

type SettingToggleProps = {
  checked: boolean;
  disabled?: boolean;
  icon: typeof Activity;
  label: string;
  onChange: () => void;
};

function SettingToggle({ checked, disabled = false, icon: Icon, label, onChange }: SettingToggleProps) {
  return (
    <div className="setting-row">
      <Icon aria-hidden="true" size={17} />
      <span>{label}</span>
      <button aria-checked={checked} aria-label={label} className="switch" disabled={disabled} onClick={onChange} role="switch" type="button"><span /></button>
    </div>
  );
}
