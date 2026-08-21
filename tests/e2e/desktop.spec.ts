import { execFile, spawn } from "node:child_process";
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
    textSize: string;
    completionEnabled: boolean;
    questionEnabled: boolean;
    agentDetailsEnabled: boolean;
    commandApprovalsEnabled: boolean;
    centerApprovalDialogs: boolean;
    notificationDisplay: string;
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
  id: number;
  title: string;
  body: string;
  textSize: string;
  canActivateTerminal: boolean;
};

const invokeView = async (command: string, args?: Record<string, unknown>): Promise<View> => {
  const result = await browser.tauri.execute(({ core }, payload) => core.invoke(payload.command, payload.args), {
    command,
    args,
  });
  return result as View;
};

const invokeCurrentWindow = async <T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> => browser.execute(async (payload) => {
  const core = (window as typeof window & {
    __TAURI__?: {
      core?: {
        invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
      };
    };
  }).__TAURI__?.core;
  if (!core) throw new Error("Tauri core API is unavailable in the current window");
  return core.invoke(payload.command, payload.args);
}, { command, args }) as Promise<T>;

const runPermissionHook = (stateRoot: string) => {
  const child = spawn(cliBinary, [
    "--state-dir",
    stateRoot,
    "hook",
    "--agent",
    "codex",
    "--event",
    "PermissionRequest",
    "--strict",
  ], { stdio: ["pipe", "pipe", "pipe"] });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk: string) => { stdout += chunk; });
  child.stderr.on("data", (chunk: string) => { stderr += chunk; });
  child.stdin.end(JSON.stringify({
    hook_event_name: "PermissionRequest",
    tool_name: "Bash",
    tool_input: {
      command: "printf 'Aizu approval E2E'",
      description: "Run the approval E2E command?",
    },
  }));
  return new Promise<{ code: number | null; stderr: string; stdout: string }>((resolve, reject) => {
    const timeout = setTimeout(() => {
      child.kill("SIGTERM");
      reject(new Error("local permission hook did not finish"));
    }, 10_000);
    child.once("error", (error) => {
      clearTimeout(timeout);
      reject(error);
    });
    child.once("close", (code) => {
      clearTimeout(timeout);
      resolve({ code, stderr, stdout });
    });
  });
};

describe("Aizu desktop MVP", () => {
  it("runs the isolated backend pipeline, settings, tray, SSH validation, and single instance", async () => {
    await expect($("h1=Keep agent events within reach.")).toBeDisplayed();
    const initial = await invokeView("get_app_view");
    expect(initial.preferences.commandApprovalsEnabled).toBe(false);
    expect(initial.preferences.centerApprovalDialogs).toBe(true);
    expect(initial.preferences.notificationDisplay).toBe("primary");
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
    const standardHeadingSize = await browser.execute(() => {
      const heading = document.querySelector(".topbar h1");
      return heading instanceof HTMLElement ? Number.parseFloat(getComputedStyle(heading).fontSize) : 0;
    });

    const systemPreferences = (await invokeView("get_app_view")).preferences;
    await invokeView("update_preferences", {
      request: { ...systemPreferences, notificationDelivery: "aizuBanner" },
    });
    await invokeView("send_test_notification");
    await browser.switchToWindow("banner");
    const banners = await invokeCurrentWindow<CapturedNotification[]>("get_banners");
    expect(banners).toContainEqual(expect.objectContaining({
      title: "Aizu test notification",
      body: "Aizu Banner is ready.",
    }));
    await expect($("strong=Aizu test notification")).toBeDisplayed();
    const testBody = $(".aizu-banner__body");
    await expect(testBody).toBeDisplayed();
    await expect(testBody).toHaveText("Aizu Banner is ready.");
    const banner = $(".aizu-banner");
    expect(await banner.getAttribute("data-text-size")).toBe("standard");
    await browser.switchToWindow("main");
    const bannerPreferences = (await invokeView("get_app_view")).preferences;
    await invokeView("update_preferences", {
      request: { ...bannerPreferences, textSize: "large" },
    });
    await browser.waitUntil(async () => {
      const presentation = await browser.execute(() => {
        const heading = document.querySelector(".topbar h1");
        return {
          textSize: document.documentElement.dataset.textSize,
          headingSize: heading instanceof HTMLElement
            ? Number.parseFloat(getComputedStyle(heading).fontSize)
            : 0,
        };
      });
      return presentation.textSize === "large"
        && presentation.headingSize > standardHeadingSize;
    }, {
      timeout: 2_000,
      timeoutMsg: "large text size was not rendered in the main window",
    });
    const largeHeadingSize = await browser.execute(() => {
      const heading = document.querySelector(".topbar h1");
      return heading instanceof HTMLElement ? Number.parseFloat(getComputedStyle(heading).fontSize) : 0;
    });
    expect(largeHeadingSize).toBeGreaterThan(standardHeadingSize);
    await browser.switchToWindow("banner");
    await browser.waitUntil(async () => {
      const queued = await invokeCurrentWindow<CapturedNotification[]>("get_banners");
      return queued[0]?.textSize === "large";
    }, {
      timeout: 2_000,
      timeoutMsg: "queued Aizu Banner did not adopt the updated text size",
    });
    await browser.waitUntil(async () =>
      (await $(".aizu-banner").getAttribute("data-text-size")) === "large",
    {
      timeout: 2_000,
      timeoutMsg: "visible Aizu Banner did not adopt the updated text size",
    });
    const entranceMotion = await browser.execute(() => {
      const banner = document.querySelector(".aizu-banner");
      if (!(banner instanceof HTMLElement)) return null;
      return {
        animationName: window.getComputedStyle(banner).animationName,
        reducedMotion: window.matchMedia("(prefers-reduced-motion: reduce)").matches,
      };
    });
    expect(entranceMotion).not.toBeNull();
    expect(entranceMotion?.animationName).toBe(
      entranceMotion?.reducedMotion ? "none" : "aizu-banner-enter",
    );
    const swipeHandle = $(".aizu-banner__body");
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
        core.invoke("get_e2e_banners")) as CapturedNotification[];
      return remaining.length === 0;
    }, { timeout: 2_000, timeoutMsg: "swiped banner remained in the backend queue" });

    await browser.tauri.execute(({ core }) => core.invoke("show_e2e_scrollable_banners"));
    await browser.waitUntil(async () => {
      const queued = await invokeCurrentWindow<CapturedNotification[]>("get_banners");
      return queued.length === 3;
    }, { timeout: 2_000, timeoutMsg: "scrollable banner fixture was not presented" });
    await browser.waitUntil(async () => {
      const metrics = await browser.execute(() => {
        const stack = document.querySelector(".banner-stack");
        return stack instanceof HTMLElement
          ? { clientHeight: stack.clientHeight, scrollHeight: stack.scrollHeight }
          : null;
      });
      return metrics !== null && metrics.scrollHeight > metrics.clientHeight;
    }, { timeout: 2_000, timeoutMsg: "overflowing banners did not create a scroll viewport" });
    await browser.execute(() => {
      const stack = document.querySelector(".banner-stack");
      if (stack instanceof HTMLElement) stack.scrollTop = stack.scrollHeight;
    });
    await browser.waitUntil(async () => await browser.execute(() => {
      const stack = document.querySelector(".banner-stack");
      const last = document.querySelector('[data-banner-id="8675403"]');
      if (!(stack instanceof HTMLElement) || !(last instanceof HTMLElement)) return false;
      const viewport = stack.getBoundingClientRect();
      const bannerBounds = last.getBoundingClientRect();
      return stack.scrollTop > 0
        && bannerBounds.top < viewport.bottom
        && bannerBounds.bottom <= viewport.bottom + 1;
    }), { timeout: 2_000, timeoutMsg: "last notification was not reachable by scrolling" });
    const [, , overflowWindowHeight] = await invokeCurrentWindow<[boolean, boolean, number]>(
      "get_e2e_banner_window_state",
    );
    const scrollable = await invokeCurrentWindow<CapturedNotification[]>("get_banners");
    const remainingScrollable = scrollable.at(-1);
    if (!remainingScrollable) throw new Error("scroll fixture queue was unexpectedly empty");
    for (const queued of scrollable.slice(0, -1)) {
      await invokeCurrentWindow("dismiss_banner", { id: queued.id });
    }
    await browser.waitUntil(async () => {
      const queued = await invokeCurrentWindow<CapturedNotification[]>("get_banners");
      if (queued.length !== 1) return false;
      const [, , currentHeight] = await invokeCurrentWindow<[boolean, boolean, number]>(
        "get_e2e_banner_window_state",
      );
      return currentHeight < overflowWindowHeight;
    }, { timeout: 2_000, timeoutMsg: "banner window did not shrink with its remaining content" });
    await invokeCurrentWindow("dismiss_banner", { id: remainingScrollable.id });
    await browser.waitUntil(async () =>
      (await invokeCurrentWindow<CapturedNotification[]>("get_banners")).length === 0,
    { timeout: 2_000, timeoutMsg: "scroll fixture banners were not dismissed" });

    await browser.switchToWindow("main");
    await expect($("h1=Agents")).toBeDisplayed();
    const hookResult = await runPermissionHook(stateRoot as string);
    expect(hookResult.code).toBe(0);
    expect(hookResult.stderr).toBe("");
    expect(hookResult.stdout).toBe("");
    const blockedBannerRead = await browser.execute(async () => {
      const core = (window as typeof window & {
        __TAURI__?: { core?: { invoke: (command: string) => Promise<unknown> } };
      }).__TAURI__?.core;
      if (!core) return "core API unavailable";
      try {
        await core.invoke("get_banners");
        return "allowed";
      } catch (error) {
        return String(error);
      }
    }) as string;
    expect(blockedBannerRead).toContain("only from the banner window");
    const forgedEvent = await browser.execute(async () => {
      const emitTo = (window as typeof window & {
        __aizu_e2e_emit_to__?: (
          target: string,
          event: string,
          payload: unknown,
        ) => Promise<void>;
      }).__aizu_e2e_emit_to__;
      if (!emitTo) return "event API unavailable";
      try {
        await emitTo("banner", "aizu://banners-changed", []);
        return "allowed";
      } catch (error) {
        return String(error);
      }
    });
    expect(forgedEvent).not.toBe("event API unavailable");
    expect(forgedEvent).not.toBe("allowed");
    await browser.switchToWindow("banner");
    await expect($("strong=Codex is waiting for permission")).toBeDisplayed();
    await expect($("button=Allow once")).not.toBeExisting();
    await expect($("button=Deny")).not.toBeExisting();
    await expect($("button=Choose in terminal")).not.toBeExisting();
    await expect($("pre=printf 'Aizu approval E2E'")).not.toBeExisting();
    const permissionBanners = await invokeCurrentWindow<CapturedNotification[]>("get_banners");
    const permissionBanner = permissionBanners.find((candidate) =>
      candidate.title === "Codex is waiting for permission");
    expect(JSON.stringify(permissionBanner)).not.toContain("printf 'Aizu approval E2E'");
    const permissionId = permissionBanner?.id ?? 0;
    await browser.switchToWindow("main");
    const blockedDismiss = await browser.execute(async (id) => {
      const core = (window as typeof window & {
        __TAURI__?: {
          core?: {
            invoke: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
          };
        };
      }).__TAURI__?.core;
      if (!core) return "core API unavailable";
      try {
        await core.invoke("dismiss_banner", { id });
        return "allowed";
      } catch (error) {
        return String(error);
      }
    }, permissionId) as string;
    expect(blockedDismiss).toContain("only from the banner window");
    await browser.switchToWindow("banner");
    await invokeCurrentWindow("dismiss_banner", { id: permissionId });
    await expect($("strong=Codex is waiting for permission")).not.toBeExisting();
    await browser.waitUntil(async () => {
      const view = await invokeView("get_app_view");
      return view.history.some((event) =>
        event.title === "Codex is waiting for permission" && event.deliveryStatus === "delivered");
    }, { timeout: 2_000, timeoutMsg: "permission notification was not retained in history" });
    await browser.waitUntil(async () => {
      const remaining = await browser.tauri.execute(({ core }) =>
        core.invoke("get_e2e_banners")) as CapturedNotification[];
      return remaining.length === 0;
    }, { timeout: 2_000, timeoutMsg: "permission banner remained queued" });
    await browser.switchToWindow("main");
    const terminalDefaultPreferences = (await invokeView("get_app_view")).preferences;
    const approvalPreferences = await invokeView("update_preferences", {
      request: { ...terminalDefaultPreferences, commandApprovalsEnabled: true },
    });
    expect(approvalPreferences.preferences.commandApprovalsEnabled).toBe(true);
    const approvalHook = runPermissionHook(stateRoot as string);
    await browser.waitUntil(async () => {
      const queued = await browser.tauri.execute(({ core }) =>
        core.invoke("get_e2e_banners")) as CapturedNotification[];
      return queued.some((banner) => banner.title === "Codex requests permission");
    }, { timeout: 4_000, timeoutMsg: "approval request never reached the backend banner queue" });
    await browser.switchToWindow("banner");
    await expect($("strong=Codex requests permission")).toBeDisplayed();
    expect(await browser.execute(() => {
      const dialog = document.querySelector('[role="alertdialog"]');
      return dialog instanceof HTMLElement ? getComputedStyle(dialog).opacity : "0";
    })).toBe("1");
    const [approvalWindowVisible] = await invokeCurrentWindow<[boolean, boolean, number]>(
      "get_e2e_banner_window_state",
    );
    expect(approvalWindowVisible).toBe(true);
    await expect($("pre=printf 'Aizu approval E2E'")).toBeDisplayed();
    await expect($('[data-presentation="approval"]')).toBeDisplayed();
    const approvalDialog = $('[role="alertdialog"]');
    await expect(approvalDialog).toBeDisplayed();
    expect(await approvalDialog.getAttribute("aria-modal")).toBe("true");
    await expect($("button=Dismiss notification")).not.toBeExisting();
    await expect($("button=Choose in terminal")).not.toBeExisting();
    await expect($("button=Deny")).toBeDisplayed();
    await browser.action("pointer", { parameters: { pointerType: "mouse" } })
      .move({ duration: 0, origin: approvalDialog, x: 180, y: 80 })
      .down({ button: 0 })
      .move({ duration: 120, origin: "pointer", x: -160, y: 0 })
      .up({ button: 0 })
      .perform();
    await expect(approvalDialog).toBeDisplayed();
    const allowOnce = $("button=Allow once");
    await expect(allowOnce).toBeEnabled();
    await allowOnce.click();
    const approved = await approvalHook;
    expect(approved.code).toBe(0);
    expect(approved.stderr).toBe("");
    expect(JSON.parse(approved.stdout)).toEqual({
      hookSpecificOutput: {
        hookEventName: "PermissionRequest",
        decision: { behavior: "allow" },
      },
    });
    await expect($("strong=Codex requests permission")).not.toBeExisting();
    await browser.switchToWindow("main");
    const paused = await invokeView("set_notifications_paused", { paused: true });
    expect(paused.trayState).toBe("paused");
    const resumed = await invokeView("set_notifications_paused", { paused: false });
    expect(resumed.trayState).toBe("attention");

    const preferences = {
      ...resumed.preferences,
      language: "ja",
      textSize: "large",
      agentDetailsEnabled: true,
      notificationDisplay: "pointer",
      notificationSound: "bloom",
      quietHours: {
        ...resumed.preferences.quietHours,
        enabled: true,
        start: "23:00",
        end: "07:00",
      },
    };
    const updated = await invokeView("update_preferences", { request: preferences });
    expect(updated.preferences.notificationSound).toBe("bloom");
    expect(updated.preferences.language).toBe("ja");
    expect(updated.preferences.textSize).toBe("large");
    expect(updated.preferences.agentDetailsEnabled).toBe(true);
    expect(updated.preferences.notificationDisplay).toBe("pointer");
    await expect($("h1=エージェント")).toBeDisplayed();
    expect(await browser.execute(() => document.documentElement.lang)).toBe("ja");
    expect(await browser.execute(() => document.documentElement.dataset.textSize)).toBe("large");
    expect((await invokeView("get_app_view")).preferences.quietHours.enabled).toBe(true);
    const persisted = JSON.parse(await readFile(path.join(stateRoot as string, "settings.json"), "utf8")) as {
      preferences: { agentDetailsEnabled: boolean; commandApprovalsEnabled: boolean; centerApprovalDialogs: boolean; language: string; notificationDisplay: string; notificationSound: string; textSize: string; quietHours: { enabled: boolean } };
    };
    expect(persisted.preferences.agentDetailsEnabled).toBe(true);
    expect(persisted.preferences.commandApprovalsEnabled).toBe(true);
    expect(persisted.preferences.centerApprovalDialogs).toBe(true);
    expect(persisted.preferences.notificationDisplay).toBe("pointer");
    expect(persisted.preferences.notificationSound).toBe("bloom");
    expect(persisted.preferences.language).toBe("ja");
    expect(persisted.preferences.textSize).toBe("large");
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

    await browser.tauri.execute(({ core }) => core.invoke("hide_e2e_main_window"));
    const second = await executeFile(desktopBinary, [], {
      env: process.env,
      timeout: 5_000,
    });
    expect(second.stderr).toBe("");
    await browser.waitUntil(async () =>
      await browser.tauri.execute(({ core }) =>
        core.invoke("is_e2e_main_window_visible")) as boolean,
    { timeout: 2_000, timeoutMsg: "second instance did not reveal the existing main window" });
    expect((await invokeView("get_app_view")).sources).toContainEqual(
      expect.objectContaining({ id: "local", status: "connected" }),
    );

    await browser.tauri.execute(({ core }) => core.invoke("hide_e2e_main_window"));
    await browser.tauri.execute(({ core }) => core.invoke("show_e2e_terminal_banner"));
    await browser.switchToWindow("banner");
    const actionable = $('[data-banner-id="8675309"]');
    await expect(actionable).toHaveAttribute("data-terminal-activation", "available");
    await expect(actionable.$("h2=Completed")).toBeDisplayed();
    await expect(actionable.$("li=All tests passed")).toBeDisplayed();
    const commandBlock = actionable.$("pre code.language-sh");
    await expect(commandBlock).toBeDisplayed();
    await expect(commandBlock).toHaveText("mise run check");
    await actionable.$(".aizu-banner__content").click();
    await browser.waitUntil(async () => {
      const count = await browser.tauri.execute(({ core }) =>
        core.invoke("get_e2e_terminal_activation_count")) as number;
      return count === 1;
    }, { timeout: 2_000, timeoutMsg: "banner click did not activate its terminal target" });
    await expect(actionable).not.toBeExisting();
    const mainVisible = await browser.tauri.execute(({ core }) =>
      core.invoke("is_e2e_main_window_visible")) as boolean;
    expect(mainVisible).toBe(false);
  });
});
