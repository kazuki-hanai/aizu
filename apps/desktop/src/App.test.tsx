import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { App } from "./App";
import type { AppView, SshConnectionTestResult } from "./lib/contracts";
import { makeBackend, makeView } from "./test/fakeBackend";
import "./styles.css";

describe("Aizu desktop shell", () => {
  it("does not show redundant global status in the sidebar", async () => {
    render(<App backend={makeBackend(makeView({ onboardingComplete: true }))} />);

    expect(await screen.findByRole("button", { name: "Agents" })).toBeVisible();
    expect(screen.queryByText("Normal")).not.toBeInTheDocument();
    expect(screen.queryByText(/sources connected/u)).not.toBeInTheDocument();
  });

  it("requests permission from an explicit onboarding action", async () => {
    const user = userEvent.setup();
    render(<App backend={makeBackend(makeView())} />);

    expect(await screen.findByRole("heading", { name: "Keep agent events within reach." })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Allow" }));

    expect(await screen.findByText("Native notifications are available.")).toBeVisible();
    expect(screen.getByRole("button", { name: /Open Aizu/u })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Set up" }));
    expect(await screen.findByText("Hooks are installed; approve the commands in Codex.")).toBeVisible();
    expect(screen.getByRole("button", { name: /Open Aizu/u })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Confirm approval" }));
    expect(await screen.findByText("Required lifecycle hooks are configured.")).toBeVisible();
    expect(screen.getByRole("button", { name: /Open Aizu/u })).toBeEnabled();
  });

  it("shows actionable copy when notification permission is denied", async () => {
    render(
      <App
        backend={makeBackend(makeView({ notificationPermission: "denied" }))}
      />,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Open System Settings",
    );
    expect(screen.getByRole("button", { name: /Open Aizu/u })).toBeDisabled();
  });

  it("lets the user dismiss an action error", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(makeView());
    backend.configureAgents = () => Promise.reject(new Error("Setup failed"));
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "Set up" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Setup failed");
    await user.click(screen.getByRole("button", { name: "Dismiss error" }));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("lists every running Codex and Claude Code process", async () => {
    const { container } = render(
      <App
        backend={makeBackend(
          makeView({
            onboardingComplete: true,
            notificationPermission: "granted",
            runningAgents: [
              { agent: "codex", label: "Codex 1", sourceId: "local", sourceName: "This Mac", sourceKind: "local" },
              { agent: "codex", label: "Codex 2", sourceId: "local", sourceName: "This Mac", sourceKind: "local" },
              { agent: "claudeCode", label: "Claude Code 1", sourceId: "local", sourceName: "This Mac", sourceKind: "local" },
              { agent: "claudeCode", label: "Claude Code 1", sourceId: "ssh:mini-pc", sourceName: "mini-pc", sourceKind: "remoteSsh" },
            ],
          }),
        )}
      />,
    );

    expect(await screen.findByRole("heading", { name: "Running agents" })).toBeVisible();
    expect(screen.getByText("Codex 1")).toBeVisible();
    expect(screen.getByText("Codex 2")).toBeVisible();
    expect(screen.getAllByText("Claude Code 1")).toHaveLength(2);
    expect(screen.getByText("mini-pc")).toBeVisible();
    expect(screen.getByText("Running via SSH")).toBeVisible();
    expect(screen.getByText("4 agent processes across 2 sources.")).toBeVisible();
    expect(screen.getAllByText("Running")).toHaveLength(4);
    expect(screen.queryByText("Aizu is monitoring")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Current status")).not.toBeInTheDocument();
    expect(container.querySelectorAll(".agent-product-icon--codex img")).toHaveLength(2);
    expect(container.querySelectorAll(".agent-product-icon--claudeCode img")).toHaveLength(2);
    expect(container.querySelector(".agent-product-icon--codex img")).toHaveAttribute(
      "src",
      expect.stringContaining("OAI_OpenAI-Blossom_White.svg"),
    );
    expect(container.querySelector(".agent-product-icon--claudeCode img")).toHaveAttribute(
      "src",
      expect.stringContaining("ClaudeIcon-Rounded.svg"),
    );
  });

  it("shows agents with a compact empty activity state", async () => {
    render(
      <App
        backend={makeBackend(
          makeView({ onboardingComplete: true, notificationPermission: "granted" }),
        )}
      />,
    );

    expect(await screen.findByRole("button", { name: "Agents", current: "page" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Agents", current: "page" })).toHaveStyle({ boxShadow: "" });
    expect(screen.getByRole("heading", { name: "No recent activity" })).toBeVisible();
  });

  it("repairs a missing CLI after onboarding", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(makeView({
      onboardingComplete: true,
      cliStatus: "missing",
      cliVersion: null,
      sources: [{
        id: "local",
        name: "This Mac",
        kind: "local",
        status: "disabled",
        detail: "Aizu CLI is unavailable",
        lastEventAt: null,
        actionRequired: null,
      }],
    }));
    const installCli = vi.spyOn(backend, "installCli");
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "Sources" }));
    expect(screen.getByText("Aizu CLI is not installed")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Install CLI" }));

    await waitFor(() => expect(installCli).toHaveBeenCalledOnce());
    expect(screen.queryByText("Aizu CLI is not installed")).not.toBeInTheDocument();
  });

  it("offers a CLI update when versions differ", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(makeView({
      onboardingComplete: true,
      cliStatus: "versionMismatch",
      cliVersion: "0.0.9",
      appVersion: "0.1.0",
    }));
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "Sources" }));
    expect(screen.getByText("Installed 0.0.9; this app requires 0.1.0.")).toBeVisible();
    expect(screen.getByRole("button", { name: "Update CLI" })).toBeEnabled();
  });

  it("uses the same subtle row separators as the agents list", async () => {
    const user = userEvent.setup();
    const { container } = render(
      <App
        backend={makeBackend(
          makeView({
            onboardingComplete: true,
            sources: [
              {
                id: "local",
                name: "This Mac",
                kind: "local",
                status: "connected",
                detail: "Connected",
                lastEventAt: null,
                actionRequired: null,
              },
              {
                id: "ssh:mini-pc",
                name: "Mini PC",
                kind: "remoteSsh",
                status: "connected",
                detail: "Connected",
                lastEventAt: null,
                actionRequired: null,
              },
            ],
          }),
        )}
      />,
    );

    await user.click(await screen.findByRole("button", { name: "Sources" }));
    const rows = container.querySelectorAll<HTMLElement>(".source-row");
    expect(rows).toHaveLength(2);
    expect(getComputedStyle(rows[0]).borderBottomStyle).toBe("solid");
    expect(getComputedStyle(rows[0]).borderBottomWidth).toBe("1px");
    expect(getComputedStyle(rows[1]).borderBottomStyle).toBe("none");
  });

  it("requests notification permission before sending a test notification", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(
      makeView({ onboardingComplete: true, notificationPermission: "notDetermined" }),
    );
    const requestPermission = vi.spyOn(backend, "requestNotificationPermission");
    const sendTest = vi.spyOn(backend, "sendTestNotification");
    render(<App backend={backend} />);

    expect(screen.queryByRole("button", { name: "Send test notification" })).not.toBeInTheDocument();
    await user.click(await screen.findByRole("button", { name: "Settings" }));
    await user.click(await screen.findByRole("button", { name: "Send test notification" }));

    await waitFor(() => expect(sendTest).toHaveBeenCalledOnce());
    expect(requestPermission).toHaveBeenCalledOnce();
    expect(requestPermission.mock.invocationCallOrder[0]).toBeLessThan(
      sendTest.mock.invocationCallOrder[0] ?? Number.MAX_SAFE_INTEGER,
    );
  });

  it("explains how to enable notifications when a test is denied", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(
      makeView({ onboardingComplete: true, notificationPermission: "denied" }),
    );
    backend.requestNotificationPermission = () => Promise.resolve(
      makeView({ onboardingComplete: true, notificationPermission: "denied" }),
    );
    const sendTest = vi.spyOn(backend, "sendTestNotification");
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "Settings" }));
    await user.click(await screen.findByRole("button", { name: "Send test notification" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "System Settings > Notifications > Aizu",
    );
    expect(sendTest).not.toHaveBeenCalled();
  });

  it("explains how to enable banners when sound is allowed without alerts", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(
      makeView({ onboardingComplete: true, notificationPermission: "alertsDisabled" }),
    );
    backend.requestNotificationPermission = () => Promise.resolve(
      makeView({ onboardingComplete: true, notificationPermission: "alertsDisabled" }),
    );
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "Settings" }));
    await user.click(await screen.findByRole("button", { name: "Send test notification" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("choose Banners or Alerts");
  });

  it("keeps notification controls in Settings with a clear mute label", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(
      makeView({
        onboardingComplete: true,
        notificationPermission: "granted",
        lastEventAt: "2026-08-13T03:18:00Z",
      }),
    );
    let resolvePause: ((view: AppView) => void) | undefined;
    backend.setNotificationsPaused = vi.fn(() => new Promise<AppView>((resolve) => {
      resolvePause = resolve;
    }));
    render(<App backend={backend} />);

    expect(await screen.findByRole("heading", { name: "Agents" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "Pause" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.getByRole("heading", { name: "Settings" })).toBeVisible();
    expect(screen.queryByText(/Aug 13, 2026/u)).not.toBeInTheDocument();
    const mute = screen.getByRole("switch", { name: "Mute notifications" });
    await user.click(mute);

    expect(backend.setNotificationsPaused).toHaveBeenCalledOnce();
    expect(backend.setNotificationsPaused).toHaveBeenCalledWith(true);
    expect(mute).toBeDisabled();
    await user.click(mute);
    expect(backend.setNotificationsPaused).toHaveBeenCalledOnce();
    resolvePause?.(makeView({ onboardingComplete: true, notificationPermission: "granted", paused: true }));
    await waitFor(() => expect(screen.getByRole("switch", { name: "Mute notifications" })).toBeEnabled());
    expect(screen.getByRole("button", { name: "Send test notification" })).toBeVisible();
  });

  it("clears terminal history after explicit confirmation", async () => {
    const user = userEvent.setup();
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const backend = makeBackend(makeView({
      onboardingComplete: true,
      notificationPermission: "granted",
      history: [{
        id: "event-1",
        kind: "taskCompleted",
        title: "Build completed",
        summary: null,
        sourceName: "This Mac",
        occurredAt: "2026-08-12T12:00:00Z",
        deliveryStatus: "delivered",
        outcome: "succeeded",
      }],
    }));
    render(<App backend={backend} />);

    expect(await screen.findByText("Build completed")).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Clear recent activity" }));

    expect(await screen.findByRole("heading", { name: "No recent activity" })).toBeVisible();
  });

  it("shows trusted agent summaries only when agent details are enabled", async () => {
    const event = {
      id: "event-1",
      kind: "taskCompleted" as const,
      title: "Codex task completed",
      summary: "Aizu notification content verified",
      sourceName: "This Mac",
      occurredAt: "2026-08-12T12:00:00Z",
      deliveryStatus: "delivered" as const,
      outcome: "succeeded" as const,
    };

    const { unmount } = render(<App backend={makeBackend(makeView({
      onboardingComplete: true,
      notificationPermission: "granted",
      history: [event],
      preferences: {
        ...makeView().preferences,
        agentDetailsEnabled: false,
      },
    }))} />);
    expect(await screen.findByText("Codex task completed")).toBeVisible();
    expect(screen.queryByText("Aizu notification content verified")).not.toBeInTheDocument();
    unmount();

    render(<App backend={makeBackend(makeView({
      onboardingComplete: true,
      notificationPermission: "granted",
      history: [event],
      preferences: {
        ...makeView().preferences,
        agentDetailsEnabled: true,
      },
    }))} />);
    expect(await screen.findByText("Aizu notification content verified")).toBeVisible();
  });

  it("groups source and time as compact activity metadata", async () => {
    const { container } = render(<App backend={makeBackend(makeView({
      onboardingComplete: true,
      notificationPermission: "granted",
      history: [{
        id: "event-1",
        kind: "taskCompleted",
        title: "Codex task completed",
        summary: "A long but safe agent summary",
        sourceName: "Ubuntu-vm1",
        occurredAt: "2026-08-12T12:00:00Z",
        deliveryStatus: "delivered",
        outcome: "succeeded",
      }],
      preferences: { ...makeView().preferences, agentDetailsEnabled: true },
    }))} />);

    expect(await screen.findByText("Codex task completed")).toBeVisible();
    const row = container.querySelector<HTMLElement>(".history-row");
    const meta = row?.querySelector<HTMLElement>(".history-row__meta");
    expect(meta).toContainElement(screen.getByText("Ubuntu-vm1"));
    expect(meta?.querySelector("time")).toHaveAttribute("datetime", "2026-08-12T12:00:00Z");
    expect(row?.querySelector(".history-row__summary")).toHaveAttribute("title", "A long but safe agent summary");
    expect(row?.querySelector(".history-row__status")).not.toBeInTheDocument();
    expect(row?.querySelector(".sr-only")).toHaveTextContent("Delivered");
  });

  it("persists notification settings through the typed backend", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(
      makeView({ onboardingComplete: true, notificationPermission: "granted" }),
    );
    const updatePreferences = vi.spyOn(backend, "updatePreferences");
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "Settings" }));
    expect(screen.queryByRole("button", { name: "Save settings" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("switch", { name: "Task completion" }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Notification sound" }), "ping");
    await user.click(screen.getByText("Advanced"));
    await user.click(screen.getByRole("switch", { name: "Show agent details" }));

    await waitFor(async () => {
      expect(updatePreferences).toHaveBeenCalledTimes(3);
      await expect(backend.getView()).resolves.toMatchObject({
        preferences: {
          completionEnabled: false,
          agentDetailsEnabled: true,
          notificationSound: "ping",
          quietHours: { questionsBypass: false },
        },
      });
    });
  });

  it("persists the language and applies it immediately", async () => {
    const user = userEvent.setup();
    const initial = makeView({
      onboardingComplete: true,
      notificationPermission: "granted",
      preferences: { ...makeView().preferences, language: "en" },
    });
    const backend = makeBackend(initial);
    const updatePreferences = vi.spyOn(backend, "updatePreferences");
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "Settings" }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Language" }), "ja");

    expect(await screen.findByRole("heading", { name: "設定" })).toBeVisible();
    expect(screen.getByRole("button", { name: "エージェント" })).toBeVisible();
    expect(screen.getByRole("combobox", { name: "言語" })).toHaveValue("ja");
    expect(document.documentElement).toHaveAttribute("lang", "ja");
    expect(updatePreferences).toHaveBeenLastCalledWith({
      ...initial.preferences,
      language: "ja",
    });
    await expect(backend.getView()).resolves.toMatchObject({
      preferences: { language: "ja" },
    });

    await user.click(screen.getByRole("button", { name: "接続元" }));
    expect(screen.getByText("このMac")).toBeVisible();
    expect(screen.getByText("ローカル")).toBeVisible();
  });

  it("uses the selected language for app-owned notification guidance", async () => {
    const user = userEvent.setup();
    const deniedView = makeView({
      onboardingComplete: true,
      notificationPermission: "alertsDisabled",
      preferences: { ...makeView().preferences, language: "ja" },
    });
    const backend = makeBackend(deniedView);
    backend.requestNotificationPermission = vi.fn(() => Promise.resolve(deniedView));
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "設定" }));
    await user.click(screen.getByRole("button", { name: "テスト通知を送信" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "システム設定の「通知」からAizuを選び",
    );
  });

  it("allows choosing a language during onboarding", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(makeView({
      preferences: { ...makeView().preferences, language: "en" },
    }));
    render(<App backend={backend} />);

    await user.selectOptions(await screen.findByRole("combobox", { name: "Language" }), "ja");

    expect(await screen.findByRole("heading", { name: "エージェントの完了を、すぐ手元に。" })).toBeVisible();
    expect(screen.getByRole("button", { name: "許可" })).toBeVisible();
    await expect(backend.getView()).resolves.toMatchObject({ preferences: { language: "ja" } });
  });

  it("tests an SSH connection without adding the source", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(
      makeView({ onboardingComplete: true, notificationPermission: "granted" }),
    );
    let resolveTest: ((result: SshConnectionTestResult) => void) | undefined;
    backend.testSshConnection = vi.fn((): Promise<SshConnectionTestResult> => new Promise((resolve) => {
      resolveTest = resolve;
    }));
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "Sources" }));
    expect(screen.queryByRole("textbox", { name: "SSH config alias" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Add SSH source" }));
    expect(screen.getByRole("dialog", { name: "Add SSH source" })).toBeVisible();
    await user.type(screen.getByRole("textbox", { name: "SSH config alias" }), "build-host");
    await user.type(screen.getByRole("textbox", { name: "Local label" }), "Build host");
    await user.click(screen.getByRole("button", { name: "Test connection" }));

    expect(screen.getByRole("button", { name: "Testing" })).toBeDisabled();
    expect(backend.testSshConnection).toHaveBeenCalledWith("build-host");
    resolveTest?.({
      status: "compatible",
      message: "SSH connected and the remote Aizu CLI is compatible.",
      configResolved: true,
      reachable: true,
      protocolCompatible: true,
      remoteVersion: "0.1.0",
    });
    expect(await screen.findByRole("status")).toHaveTextContent(
      "SSH connected and the remote Aizu CLI is compatible.",
    );
    expect(screen.getByRole("status")).toHaveTextContent("Remote Aizu 0.1.0");
    await expect(backend.getView()).resolves.toMatchObject({ sources: [{ id: "local" }] });
  });

  it("shows a privacy-safe SSH connection failure inline", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(
      makeView({ onboardingComplete: true, notificationPermission: "granted" }),
    );
    backend.testSshConnection = vi.fn((): Promise<SshConnectionTestResult> => Promise.resolve({
      status: "authenticationRequired",
      message: "SSH authentication is required or was rejected.",
      configResolved: true,
      reachable: false,
      protocolCompatible: false,
      remoteVersion: null,
    }));
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "Sources" }));
    await user.click(screen.getByRole("button", { name: "Add SSH source" }));
    await user.type(screen.getByRole("textbox", { name: "SSH config alias" }), "private-host");
    await user.click(screen.getByRole("button", { name: "Test connection" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "SSH authentication is required or was rejected.",
    );
    expect(screen.getByRole("alert")).not.toHaveTextContent("private-host");
  });

  it("keeps failed SSH source input and provides modal keyboard behavior", async () => {
    const user = userEvent.setup();
    const backend = makeBackend(
      makeView({ onboardingComplete: true, notificationPermission: "granted" }),
    );
    backend.addRemoteSource = vi.fn(() => Promise.reject(new Error("Source already exists")));
    render(<App backend={backend} />);

    await user.click(await screen.findByRole("button", { name: "Sources" }));
    const trigger = screen.getByRole("button", { name: "Add SSH source" });
    await user.click(trigger);
    await user.type(screen.getByRole("textbox", { name: "SSH config alias" }), "mini-pc");
    await user.type(screen.getByRole("textbox", { name: "Local label" }), "Mini PC");
    await user.click(screen.getByRole("button", { name: "Add source" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Source already exists");
    expect(screen.getByRole("dialog", { name: "Add SSH source" })).toBeVisible();
    expect(screen.getByRole("textbox", { name: "SSH config alias" })).toHaveValue("mini-pc");

    const close = screen.getByRole("button", { name: "Close" });
    close.focus();
    await user.tab({ shift: true });
    expect(screen.getByRole("button", { name: "Add source" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog", { name: "Add SSH source" })).not.toBeInTheDocument();
    await waitFor(() => expect(trigger).toHaveFocus());
  });
});
