import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";

import { browser, expect } from "@wdio/globals";
import "@wdio/tauri-service";

const executeFile = promisify(execFile);
const repositoryRoot = process.cwd();
const desktopBinary = path.join(repositoryRoot, "target/debug/aizu-desktop");
const cliBinary = path.join(repositoryRoot, "target/debug/aizu");

type View = {
  onboardingComplete: boolean;
  notificationPermission: string;
  paused: boolean;
  trayState: string;
  sources: Array<{ id: string; status: string }>;
  history: Array<{ title: string; deliveryStatus: string }>;
  preferences: {
    language: string;
    completionEnabled: boolean;
    questionEnabled: boolean;
    agentDetailsEnabled: boolean;
    soundEnabled: boolean;
    notificationDelivery: string;
    notificationSound: string;
    privacyMode: string;
    launchAtLogin: boolean;
    quietHours: {
      enabled: boolean;
      start: string;
      end: string;
      questionsBypass: boolean;
    };
  };
};

type CapturedNotification = {
  title: string;
  body: string;
};

const invokeView = async (command: string, args?: Record<string, unknown>): Promise<View> => {
  const result = await browser.tauri.execute(({ core }, payload) => core.invoke(payload.command, payload.args), {
    command,
    args,
  });
  return result as View;
};

describe("Aizu desktop MVP", () => {
  it("runs the isolated backend pipeline, settings, tray, SSH validation, and single instance", async () => {
    await expect($("h1=Keep agent events within reach.")).toBeDisplayed();
    const initial = await invokeView("get_app_view");
    await invokeView("update_preferences", {
      request: { ...initial.preferences, notificationDelivery: "system" },
    });

    const stateRoot = process.env.AIZU_STATE_DIR;
    expect(stateRoot).toBeTruthy();
    for (let index = 1; index <= 5; index += 1) {
      await executeFile(cliBinary, [
        "emit",
        "task.completed",
        "--state-dir",
        stateRoot as string,
        "--title",
        `backlog-${String(index)}`,
        "--outcome",
        "succeeded",
        "--agent",
        "codex",
      ]);
    }
    await browser.waitUntil(async () => {
      try {
        return (await invokeView("get_app_view")).history.length === 5;
      } catch {
        return false;
      }
    }, {
      timeout: 15_000,
      timeoutMsg: "pre-permission backlog was not durably ingested",
    });

    const permission = await invokeView("request_notification_permission");
    expect(permission.notificationPermission).toBe("granted");
    await expect($("button=Allow")).not.toExist();
    await browser.waitUntil(async () => {
      const notifications = await browser.tauri.execute(({ core }) =>
        core.invoke("get_e2e_notifications")) as CapturedNotification[];
      return notifications.length === 1
        && notifications[0]?.title === "Aizu backlog"
        && notifications[0]?.body === "5 agent events arrived while disconnected";
    }, { timeout: 5_000, timeoutMsg: "backlog did not collapse into one accurate summary" });
    const backlogNotifications = await browser.tauri.execute(({ core }) =>
      core.invoke("get_e2e_notifications")) as CapturedNotification[];
    expect(backlogNotifications).toHaveLength(1);
    expect(backlogNotifications[0]).toMatchObject({
      title: "Aizu backlog",
      body: "5 agent events arrived while disconnected",
    });

    const opened = await invokeView("complete_onboarding", {
      request: { launchAtLogin: false },
    });
    expect(opened.onboardingComplete).toBe(true);
    await expect($("h1=Agents")).toBeDisplayed();

    const systemPreferences = (await invokeView("get_app_view")).preferences;
    await invokeView("update_preferences", {
      request: { ...systemPreferences, notificationDelivery: "aizuBanner" },
    });
    await invokeView("send_test_notification");
    const banners = await browser.tauri.execute(({ core }) =>
      core.invoke("get_banners")) as CapturedNotification[];
    expect(banners).toContainEqual(expect.objectContaining({
      title: "Aizu test notification",
      body: "Aizu Banner is ready.",
    }));
    await browser.switchToWindow("banner");
    await expect($("strong=Aizu test notification")).toBeDisplayed();
    await expect($("span=Aizu Banner is ready.")).toBeDisplayed();
    expect(await browser.execute(() => {
      const banner = document.querySelector(".aizu-banner");
      return banner instanceof HTMLElement
        ? window.getComputedStyle(banner).animationName
        : null;
    })).toBe("aizu-banner-enter");
    const banner = $(".aizu-banner");
    const swipeHandle = $(".aizu-banner__mark");
    await browser.action("pointer", { parameters: { pointerType: "mouse" } })
      .move({ duration: 0, origin: swipeHandle, x: 0, y: 0 })
      .down({ button: 0 })
      .pause(50)
      .move({ duration: 240, origin: "pointer", x: -120, y: 0 })
      .up({ button: 0 })
      .perform();
    await browser.waitUntil(async () => {
      const state = await banner.getAttribute("class").catch(() => "");
      return (state?.includes("aizu-banner--dismiss-left") ?? false) || !(await banner.isExisting());
    }, { timeout: 1_000, timeoutMsg: "mouse actions did not start banner dismissal" });
    await expect(banner).not.toBeExisting();
    await browser.waitUntil(async () => {
      const remaining = await browser.tauri.execute(({ core }) =>
        core.invoke("get_banners")) as CapturedNotification[];
      return remaining.length === 0;
    }, { timeout: 2_000, timeoutMsg: "swiped banner remained in the backend queue" });
    await browser.switchToWindow("main");
    await expect($("h1=Agents")).toBeDisplayed();
    const paused = await invokeView("set_notifications_paused", { paused: true });
    expect(paused.trayState).toBe("paused");
    const resumed = await invokeView("set_notifications_paused", { paused: false });
    expect(resumed.trayState).toBe("normal");

    const preferences = {
      ...resumed.preferences,
      language: "ja",
      agentDetailsEnabled: true,
      notificationSound: "hero",
      quietHours: {
        ...resumed.preferences.quietHours,
        enabled: true,
        start: "23:00",
        end: "07:00",
      },
    };
    const updated = await invokeView("update_preferences", { request: preferences });
    expect(updated.preferences.notificationSound).toBe("hero");
    expect(updated.preferences.language).toBe("ja");
    expect(updated.preferences.agentDetailsEnabled).toBe(true);
    await expect($("h1=エージェント")).toBeDisplayed();
    expect(await browser.execute(() => document.documentElement.lang)).toBe("ja");
    expect((await invokeView("get_app_view")).preferences.quietHours.enabled).toBe(true);
    const persisted = JSON.parse(await readFile(path.join(stateRoot as string, "settings.json"), "utf8")) as {
      preferences: { agentDetailsEnabled: boolean; language: string; notificationSound: string; quietHours: { enabled: boolean } };
    };
    expect(persisted.preferences.agentDetailsEnabled).toBe(true);
    expect(persisted.preferences.notificationSound).toBe("hero");
    expect(persisted.preferences.language).toBe("ja");
    expect(persisted.preferences.quietHours.enabled).toBe(true);
    await invokeView("update_preferences", {
      request: {
        ...preferences,
        quietHours: { ...preferences.quietHours, enabled: false },
      },
    });

    const invalidSsh = await browser.tauri.execute(({ core }) =>
      core.invoke("test_ssh_connection", { hostAlias: "-unsafe alias" })) as {
        status: string;
        reachable: boolean;
      };
    expect(invalidSsh).toMatchObject({ status: "invalidAlias", reachable: false });

    const remote = await invokeView("add_remote_source", {
      request: { hostAlias: "127.0.0.1", localLabel: "E2E SSH" },
    });
    expect(remote.sources).toContainEqual(expect.objectContaining({
      id: "ssh:127.0.0.1",
      status: "reconnecting",
    }));
    const connected = await invokeView("set_e2e_remote_status", {
      hostAlias: "127.0.0.1",
      status: "connected",
    });
    expect(connected.sources).toContainEqual(expect.objectContaining({
      id: "ssh:127.0.0.1",
      status: "connected",
    }));
    const failed = await invokeView("set_e2e_remote_status", {
      hostAlias: "127.0.0.1",
      status: "error",
    });
    expect(failed.sources).toContainEqual(expect.objectContaining({
      id: "ssh:127.0.0.1",
      status: "error",
    }));

    const title = `desktop-e2e-${String(Date.now())}`;
    await executeFile(cliBinary, [
      "emit",
      "task.completed",
      "--state-dir",
      stateRoot as string,
      "--title",
      title,
      "--outcome",
      "succeeded",
      "--agent",
      "codex",
    ]);
    await browser.waitUntil(async () => {
      const view = await invokeView("get_app_view");
      return view.history.some((event) => event.title === title && event.deliveryStatus === "delivered");
    }, { timeout: 5_000, timeoutMsg: "local CLI event was not delivered after quiet hours ended" });
    await expect($(`strong=${title}`)).toBeDisplayed();

    const second = await executeFile(desktopBinary, [], {
      env: process.env,
      timeout: 5_000,
    });
    expect(second.stderr).toBe("");
    expect((await invokeView("get_app_view")).sources).toContainEqual(
      expect.objectContaining({ id: "local", status: "connected" }),
    );
  });
});
