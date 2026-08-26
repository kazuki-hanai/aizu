import type { UnlistenFn } from "@tauri-apps/api/event";

import { developmentView } from "../lib/backend";
import type { BackendClient } from "../lib/backend";
import type { AppView } from "../lib/contracts";

export const makeView = (override: Partial<AppView> = {}): AppView => ({
  ...structuredClone(developmentView),
  ...override,
});

export const makeBackend = (initialView: AppView): BackendClient => {
  let view = structuredClone(initialView);
  const subscribe = async (): Promise<UnlistenFn> => () => undefined;

  return {
    getView: async () => structuredClone(view),
    completeOnboarding: async ({ launchAtLogin }) => {
      view = {
        ...view,
        onboardingComplete: true,
        preferences: { ...view.preferences, launchAtLogin },
      };
      return structuredClone(view);
    },
    requestNotificationPermission: async () => {
      view = { ...view, notificationPermission: "granted" };
      return structuredClone(view);
    },
    sendTestNotification: async () => structuredClone(view),
    clearHistory: async () => {
      view = { ...view, history: [] };
      return structuredClone(view);
    },
    setNotificationsPaused: async (paused) => {
      view = { ...view, paused, trayState: paused ? "paused" : "normal" };
      return structuredClone(view);
    },
    updatePreferences: async (preferences) => {
      view = { ...view, preferences: structuredClone(preferences) };
      return structuredClone(view);
    },
    addRemoteSource: async (hostAlias, localLabel) => {
      view = { ...view, sources: [...view.sources, { id: `ssh:${hostAlias}`, name: localLabel, kind: "remoteSsh", status: "reconnecting", detail: "Waiting to connect", lastEventAt: null, actionRequired: null }] };
      return structuredClone(view);
    },
    testSshConnection: async () => ({
      status: "compatible",
      message: "SSH connected and the remote Aizu CLI is compatible.",
      configResolved: true,
      reachable: true,
      protocolCompatible: true,
      remoteVersion: "0.1.0",
      integrations: [
        { agent: "codex", status: "approvalRequired" },
        { agent: "claudeCode", status: "configured" },
      ],
    }),
    removeRemoteSource: async (hostAlias) => {
      view = { ...view, sources: view.sources.filter((source) => source.id !== `ssh:${hostAlias}`) };
      return structuredClone(view);
    },
    reconnectRemoteSource: async (hostAlias) => {
      view = { ...view, sources: view.sources.map((source) => source.id === `ssh:${hostAlias}` ? { ...source, status: "reconnecting" } : source) };
      return structuredClone(view);
    },
    confirmRemoteIdentity: async (hostAlias) => {
      view = { ...view, sources: view.sources.map((source) => source.id === `ssh:${hostAlias}` ? { ...source, actionRequired: null } : source) };
      return structuredClone(view);
    },
    installCli: async () => {
      view = { ...view, cliStatus: "installed", cliVersion: view.appVersion };
      return structuredClone(view);
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
      return structuredClone(view);
    },
    confirmCodexHookTrust: async () => {
      view = {
        ...view,
        agentMonitors: view.agentMonitors.map((agent) => agent.agent === "codex"
          ? { ...agent, hookStatus: "configured", detail: "Required Aizu hooks configured" }
          : agent),
      };
      return structuredClone(view);
    },
    subscribe,
  };
};
