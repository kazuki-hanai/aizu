import { invoke } from "@tauri-apps/api/core";
import { emitTo, listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  appViewSchema,
  bannerNotificationSchema,
  sshConnectionTestResultSchema,
  type AppView,
  type BannerNotification,
  type CompleteOnboardingRequest,
  type Preferences,
  type SshConnectionTestResult,
  type UpdatePreferencesRequest,
} from "./contracts";

export type BannerClient = {
  getBanners: () => Promise<BannerNotification[]>;
  dismiss: (id: number) => Promise<void>;
  activate: (id: number) => Promise<void>;
  acknowledgeApproval: (id: number) => Promise<void>;
  decideApproval: (id: number, decision: "allowOnce" | "deny") => Promise<void>;
  resize: (height: number) => Promise<void>;
  subscribe: (onChange: (banners: BannerNotification[]) => void) => Promise<UnlistenFn>;
};

export type BackendClient = {
  getView: () => Promise<AppView>;
  completeOnboarding: (request: CompleteOnboardingRequest) => Promise<AppView>;
  requestNotificationPermission: () => Promise<AppView>;
  sendTestNotification: () => Promise<AppView>;
  clearHistory: () => Promise<AppView>;
  setNotificationsPaused: (paused: boolean) => Promise<AppView>;
  updatePreferences: (request: UpdatePreferencesRequest) => Promise<AppView>;
  addRemoteSource: (hostAlias: string, localLabel: string) => Promise<AppView>;
  testSshConnection: (hostAlias: string) => Promise<SshConnectionTestResult>;
  removeRemoteSource: (hostAlias: string) => Promise<AppView>;
  reconnectRemoteSource: (hostAlias: string) => Promise<AppView>;
  confirmRemoteIdentity: (hostAlias: string) => Promise<AppView>;
  installCli: () => Promise<AppView>;
  configureAgents: () => Promise<AppView>;
  confirmCodexHookTrust: () => Promise<AppView>;
  subscribe: (onView: (view: AppView) => void) => Promise<UnlistenFn>;
};

const invokeView = async (
  command: string,
  args?: Record<string, unknown>,
): Promise<AppView> => {
  try {
    return appViewSchema.parse(await invokeBackend(command, args));
  } catch (error) {
    if (typeof error === "string") {
      throw new Error(error, { cause: error });
    }
    throw error;
  }
};

const invokeBackend = async (
  command: string,
  args?: Record<string, unknown>,
): Promise<unknown> => {
  if (import.meta.env.VITE_DESKTOP_E2E === "1") {
    const e2eMock = window.__wdio_mocks__?.[command];
    if (e2eMock !== undefined) {
      return e2eMock(args);
    }
    const e2eInvoke = window.__TAURI__?.core?.invoke;
    if (e2eInvoke === undefined) {
      throw new Error("The desktop E2E invoke bridge is unavailable");
    }
    return e2eInvoke(command, args);
  }
  return invoke<unknown>(command, args);
};

const tauriBackend: BackendClient = {
  getView: async () => invokeView("get_app_view"),
  completeOnboarding: async (request) =>
    invokeView("complete_onboarding", { request }),
  requestNotificationPermission: async () =>
    invokeView("request_notification_permission"),
  sendTestNotification: async () => invokeView("send_test_notification"),
  clearHistory: async () => invokeView("clear_history"),
  setNotificationsPaused: async (paused) =>
    invokeView("set_notifications_paused", { paused }),
  updatePreferences: async (request) =>
    invokeView("update_preferences", { request }),
  addRemoteSource: async (hostAlias, localLabel) =>
    invokeView("add_remote_source", { request: { hostAlias, localLabel } }),
  testSshConnection: async (hostAlias) =>
    sshConnectionTestResultSchema.parse(await invokeBackend("test_ssh_connection", { hostAlias })),
  removeRemoteSource: async (hostAlias) =>
    invokeView("remove_remote_source", { hostAlias }),
  reconnectRemoteSource: async (hostAlias) =>
    invokeView("reconnect_remote_source", { hostAlias }),
  confirmRemoteIdentity: async (hostAlias) =>
    invokeView("confirm_remote_identity", { hostAlias }),
  installCli: async () => invokeView("install_cli"),
  configureAgents: async () => {
    try {
      return await invokeView("configure_agents");
    } catch (error) {
      if (typeof error === "string" && error.startsWith("Codex and Claude Code hooks")) {
        throw new Error(error, { cause: error });
      }
      throw error;
    }
  },
  confirmCodexHookTrust: async () => invokeView("confirm_codex_hook_trust"),
  subscribe: async (onView) =>
    listen<unknown>("aizu://view-changed", (event) => {
      onView(appViewSchema.parse(event.payload));
    }),
};

export const bannerBackend: BannerClient = {
  getBanners: async () =>
    bannerNotificationSchema.array().parse(await invokeBackend("get_banners")),
  dismiss: async (id) => {
    await invokeBackend("dismiss_banner", { id });
  },
  activate: async (id) => {
    await invokeBackend("activate_banner", { id });
  },
  acknowledgeApproval: async (id) => {
    await invokeBackend("acknowledge_banner_approval", { id });
  },
  decideApproval: async (id, decision) => {
    await invokeBackend("decide_banner_approval", { id, decision });
  },
  resize: async (height) => {
    await invokeBackend("resize_banner", { height });
  },
  subscribe: async (onChange) =>
    listen<unknown>("aizu://banners-changed", (event) => {
      onChange(bannerNotificationSchema.array().parse(event.payload));
    }),
};

const defaultPreferences: Preferences = {
  language: "system",
  textSize: "standard",
  completionEnabled: true,
  questionEnabled: true,
  agentDetailsEnabled: true,
  commandApprovalsEnabled: false,
  soundEnabled: true,
  notificationDelivery: "aizuBanner",
  notificationSound: "default",
  privacyMode: "generic",
  launchAtLogin: false,
  quietHours: {
    enabled: false,
    start: "22:00",
    end: "07:00",
    questionsBypass: false,
  },
};

export const developmentView: AppView = {
  onboardingComplete: false,
  notificationPermission: "granted",
  cliStatus: "missing",
  cliVersion: null,
  protocolVersion: 1,
  appVersion: "0.1.0-dev",
  paused: false,
  trayState: "normal",
  sources: [
    {
      id: "local",
      name: "This Mac",
      kind: "local",
      status: "connected",
      detail: "Local event spool is ready",
      lastEventAt: null,
      actionRequired: null,
    },
  ],
  agentMonitors: [
    {
      agent: "codex",
      label: "Codex",
      status: "notDetected",
      hookStatus: "missing",
      version: null,
      lastSeenAt: null,
      detail: "Waiting for a verified hook event",
    },
    {
      agent: "claudeCode",
      label: "Claude Code",
      status: "notDetected",
      hookStatus: "missing",
      version: null,
      lastSeenAt: null,
      detail: "Waiting for a verified hook event",
    },
  ],
  runningAgents: [],
  history: [],
  preferences: defaultPreferences,
  lastEventAt: null,
};

const createDevelopmentBackend = (): BackendClient => {
  let view = structuredClone(developmentView);
  const listeners = new Set<(nextView: AppView) => void>();
  const publish = (): AppView => {
    const snapshot = structuredClone(view);
    listeners.forEach((listener) => listener(snapshot));
    return snapshot;
  };

  return {
    getView: async () => structuredClone(view),
    completeOnboarding: async ({ launchAtLogin }) => {
      view = {
        ...view,
        onboardingComplete: true,
        preferences: { ...view.preferences, launchAtLogin },
      };
      return publish();
    },
    requestNotificationPermission: async () => {
      view = { ...view, notificationPermission: "granted" };
      return publish();
    },
    sendTestNotification: async () => publish(),
    clearHistory: async () => {
      view = { ...view, history: [] };
      return publish();
    },
    setNotificationsPaused: async (paused) => {
      view = { ...view, paused, trayState: paused ? "paused" : "normal" };
      return publish();
    },
    updatePreferences: async (request) => {
      view = { ...view, preferences: structuredClone(request) };
      return publish();
    },
    addRemoteSource: async (hostAlias, localLabel) => {
      view = {
        ...view,
        sources: [...view.sources, {
          id: `ssh:${hostAlias}`,
          name: localLabel,
          kind: "remoteSsh",
          status: "reconnecting",
          detail: "Waiting to connect",
          lastEventAt: null,
          actionRequired: null,
        }],
      };
      return publish();
    },
    testSshConnection: async () => ({
      status: "compatible",
      message: "SSH connected and the remote Aizu CLI is compatible.",
      configResolved: true,
      reachable: true,
      protocolCompatible: true,
      remoteVersion: "0.1.0-dev",
    }),
    removeRemoteSource: async (hostAlias) => {
      view = { ...view, sources: view.sources.filter((source) => source.id !== `ssh:${hostAlias}`) };
      return publish();
    },
    reconnectRemoteSource: async (hostAlias) => {
      view = {
        ...view,
        sources: view.sources.map((source) => source.id === `ssh:${hostAlias}` ? { ...source, status: "reconnecting", detail: "Reconnect requested" } : source),
      };
      return publish();
    },
    confirmRemoteIdentity: async (hostAlias) => {
      view = { ...view, sources: view.sources.map((source) => source.id === `ssh:${hostAlias}` ? { ...source, status: "reconnecting", actionRequired: null } : source) };
      return publish();
    },
    installCli: async () => {
      view = { ...view, cliStatus: "installed", cliVersion: view.appVersion };
      return publish();
    },
    configureAgents: async () => {
      view = {
        ...view,
        cliStatus: "installed",
        cliVersion: view.appVersion,
        agentMonitors: view.agentMonitors.map((agent) => ({
          ...agent,
          hookStatus: agent.agent === "codex" ? "approvalRequired" : "configured",
          detail: agent.agent === "codex" ? "Review the installed commands in Codex" : "Required Aizu hooks configured",
        })),
      };
      return publish();
    },
    confirmCodexHookTrust: async () => {
      view = {
        ...view,
        agentMonitors: view.agentMonitors.map((agent) => agent.agent === "codex"
          ? { ...agent, hookStatus: "configured", detail: "Required Aizu hooks configured" }
          : agent),
      };
      return publish();
    },
    subscribe: async (onView) => {
      listeners.add(onView);
      return () => listeners.delete(onView);
    },
  };
};

declare global {
  // Window declaration merging requires an interface.
  // eslint-disable-next-line @typescript-eslint/consistent-type-definitions
  interface Window {
    __TAURI_INTERNALS__?: {
      invoke?: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
    };
    __wdio_mocks__?: Record<
      string,
      (args?: Record<string, unknown>) => unknown
    >;
    __aizu_e2e_emit_to__?: (target: string, event: string, payload: unknown) => Promise<void>;
    __TAURI__?: {
      core?: {
        invoke?: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
      };
    };
  }
}

if (import.meta.env.VITE_DESKTOP_E2E === "1") {
  window.__aizu_e2e_emit_to__ = (target, event, payload) => emitTo(target, event, payload);
}

export const defaultBackend =
  window.__TAURI_INTERNALS__ === undefined && import.meta.env.DEV
    ? createDevelopmentBackend()
    : tauriBackend;
