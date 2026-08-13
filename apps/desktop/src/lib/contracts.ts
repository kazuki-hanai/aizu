import { z } from "zod";

export const permissionStatusSchema = z.enum([
  "notDetermined",
  "granted",
  "denied",
  "alertsDisabled",
]);

export const trayStateSchema = z.enum([
  "normal",
  "attention",
  "paused",
  "error",
]);

export const sourceSchema = z.object({
  id: z.string(),
  name: z.string(),
  kind: z.enum(["local", "remoteSsh"]),
  status: z.enum(["connected", "reconnecting", "error", "disabled"]),
  detail: z.string(),
  lastEventAt: z.string().nullable(),
  actionRequired: z.enum(["confirmIdentityChange"]).nullable(),
});

export const sshConnectionTestResultSchema = z.object({
  status: z.enum([
    "compatible",
    "invalidAlias",
    "configurationError",
    "networkUnavailable",
    "authenticationRequired",
    "hostVerificationFailed",
    "missingRemoteCli",
    "incompatibleProtocol",
    "timedOut",
    "remoteFailure",
  ]),
  message: z.string(),
  configResolved: z.boolean(),
  reachable: z.boolean(),
  protocolCompatible: z.boolean(),
  remoteVersion: z.string().nullable(),
});

export const historyEventSchema = z.object({
  id: z.string(),
  kind: z.enum(["taskCompleted", "agentQuestion", "deliveryGap"]),
  title: z.string(),
  summary: z.string().nullable(),
  sourceName: z.string(),
  occurredAt: z.string(),
  deliveryStatus: z.enum(["pending", "delivered", "suppressed", "failed"]),
  outcome: z.enum(["succeeded", "failed", "cancelled", "unknown"]).nullable(),
});

export const agentMonitorSchema = z.object({
  agent: z.enum(["codex", "claudeCode"]),
  label: z.string(),
  status: z.enum(["notDetected", "running", "waiting", "completed", "error"]),
  hookStatus: z.enum(["configured", "missing", "approvalRequired", "unsupported"]),
  version: z.string().nullable(),
  lastSeenAt: z.string().nullable(),
  detail: z.string(),
});

export const runningAgentSchema = z.object({
  agent: z.enum(["codex", "claudeCode"]),
  label: z.string(),
  sourceId: z.string(),
  sourceName: z.string(),
  sourceKind: z.enum(["local", "remoteSsh"]),
});

export const quietHoursSchema = z.object({
  enabled: z.boolean(),
  start: z.string().regex(/^([01]\d|2[0-3]):[0-5]\d$/u),
  end: z.string().regex(/^([01]\d|2[0-3]):[0-5]\d$/u),
  questionsBypass: z.boolean(),
});

export const preferencesSchema = z.object({
  language: z.enum(["system", "ja", "en"]).default("system"),
  completionEnabled: z.boolean(),
  questionEnabled: z.boolean(),
  agentDetailsEnabled: z.boolean().default(false),
  soundEnabled: z.boolean(),
  notificationDelivery: z.enum(["aizuBanner", "system"]).default("aizuBanner"),
  notificationSound: z.enum(["default", "glass", "ping", "pop", "hero"]),
  privacyMode: z.literal("generic"),
  launchAtLogin: z.boolean(),
  quietHours: quietHoursSchema,
});

export const bannerNotificationSchema = z.object({
  id: z.number().int(),
  title: z.string(),
  body: z.string(),
  sound: z.enum(["default", "glass", "ping", "pop", "hero"]).nullable(),
  delivery: z.enum(["aizuBanner", "system"]),
  language: z.enum(["system", "ja", "en"]),
});

export const appViewSchema = z.object({
  onboardingComplete: z.boolean(),
  notificationPermission: permissionStatusSchema,
  cliStatus: z.enum(["installed", "missing", "versionMismatch"]),
  cliVersion: z.string().nullable(),
  protocolVersion: z.number().int().positive(),
  appVersion: z.string(),
  paused: z.boolean(),
  trayState: trayStateSchema,
  sources: z.array(sourceSchema),
  agentMonitors: z.array(agentMonitorSchema),
  runningAgents: z.array(runningAgentSchema),
  history: z.array(historyEventSchema),
  preferences: preferencesSchema,
  lastEventAt: z.string().nullable(),
});

export type PermissionStatus = z.infer<typeof permissionStatusSchema>;
export type TrayState = z.infer<typeof trayStateSchema>;
export type SourceView = z.infer<typeof sourceSchema>;
export type SshConnectionTestResult = z.infer<typeof sshConnectionTestResultSchema>;
export type HistoryEvent = z.infer<typeof historyEventSchema>;
export type AgentMonitor = z.infer<typeof agentMonitorSchema>;
export type RunningAgent = z.infer<typeof runningAgentSchema>;
export type QuietHours = z.infer<typeof quietHoursSchema>;
export type Preferences = z.infer<typeof preferencesSchema>;
export type BannerNotification = z.infer<typeof bannerNotificationSchema>;
export type AppView = z.infer<typeof appViewSchema>;

export type CompleteOnboardingRequest = {
  launchAtLogin: boolean;
};

export type UpdatePreferencesRequest = Preferences;
