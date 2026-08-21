import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { BannerApp } from "./BannerApp";
import type { BannerClient } from "./lib/backend";
import type { BannerNotification } from "./lib/contracts";
import "./styles.css";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

const resizeCallbacks = new WeakMap<Element, ResizeObserverCallback>();

class TestResizeObserver {
  constructor(private readonly callback: ResizeObserverCallback) {}
  observe(target: Element) {
    resizeCallbacks.set(target, this.callback);
  }
  disconnect() { return undefined; }
  unobserve() { return undefined; }
}

function notifyResize(target: Element) {
  resizeCallbacks.get(target)?.([], {} as ResizeObserver);
}

Object.defineProperty(window, "ResizeObserver", {
  configurable: true,
  value: TestResizeObserver,
});
Object.defineProperty(window, "matchMedia", {
  configurable: true,
  value: vi.fn().mockReturnValue({ matches: false }),
});

const pointerCaptures = new WeakMap<Element, Set<number>>();
Object.defineProperties(HTMLElement.prototype, {
  hasPointerCapture: {
    configurable: true,
    value(this: HTMLElement, pointerId: number) {
      return pointerCaptures.get(this)?.has(pointerId) ?? false;
    },
  },
  releasePointerCapture: {
    configurable: true,
    value(this: HTMLElement, pointerId: number) {
      pointerCaptures.get(this)?.delete(pointerId);
    },
  },
  setPointerCapture: {
    configurable: true,
    value(this: HTMLElement, pointerId: number) {
      const captures = pointerCaptures.get(this) ?? new Set<number>();
      captures.add(pointerId);
      pointerCaptures.set(this, captures);
    },
  },
});

const notices: BannerNotification[] = [
  {
    id: 1,
    title: "Codex task completed",
    body: "This complete safe notification body\n\nremains visible until it is dismissed.",
    sound: "chime",
    delivery: "aizuBanner",
    language: "en",
    textSize: "standard",
    canActivateTerminal: true,
    approval: null,
  },
  {
    id: 2,
    title: "Claude Code needs input",
    body: "Choose an option before the task can continue.",
    sound: null,
    delivery: "aizuBanner",
    language: "en",
    textSize: "standard",
    canActivateTerminal: false,
    approval: null,
  },
];

function client(): BannerClient {
  let queued = [...notices];
  return {
    getBanners: vi.fn(() => Promise.resolve(queued)),
    dismiss: vi.fn((id: number) => {
      queued = queued.filter((notice) => notice.id !== id);
      return Promise.resolve();
    }),
    activate: vi.fn((id: number) => {
      queued = queued.filter((notice) => notice.id !== id);
      return Promise.resolve();
    }),
    acknowledgeApproval: vi.fn().mockResolvedValue(undefined),
    decideApproval: vi.fn((id: number) => {
      queued = queued.filter((notice) => notice.id !== id);
      return Promise.resolve();
    }),
    resize: vi.fn().mockResolvedValue(undefined),
    subscribe: vi.fn().mockResolvedValue(() => undefined),
  };
}

describe("Aizu Banner", () => {
  it("renders complete notification bodies without automatic dismissal", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);

    expect(await screen.findByText("Codex task completed")).toBeVisible();
    const formattedBody = container.querySelector(".aizu-banner__body");
    expect(formattedBody).not.toBeNull();
    expect(formattedBody?.querySelectorAll("p")).toHaveLength(2);
    expect(formattedBody).toHaveTextContent("This complete safe notification body");
    expect(formattedBody).toHaveTextContent("remains visible until it is dismissed.");
    if (formattedBody) {
      expect(window.getComputedStyle(formattedBody).userSelect).toBe("text");
    }
    expect(screen.getByText(notices[1].body)).toBeVisible();
    expect(container.querySelectorAll(".aizu-banner")).toHaveLength(2);
    expect(backend.dismiss).not.toHaveBeenCalled();
    await waitFor(() => expect(backend.resize).toHaveBeenCalled());
  });

  it("keeps overflow inside a scrollable viewport while measuring full content height", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);

    await screen.findByText("Codex task completed");
    const stack = container.querySelector<HTMLElement>(".banner-stack");
    const content = container.querySelector<HTMLElement>(".banner-stack__content");
    expect(stack).not.toBeNull();
    expect(content).not.toBeNull();
    if (!stack || !content) return;

    const stackStyle = window.getComputedStyle(stack);
    expect(stackStyle.overflowY).toBe("auto");
    expect(stackStyle.overflowX).toBe("hidden");
    Object.defineProperty(content, "scrollHeight", { configurable: true, value: 960 });
    notifyResize(content);

    await waitFor(() => expect(backend.resize).toHaveBeenLastCalledWith(980));
  });

  it("applies the persisted text size to each banner", async () => {
    const backend = client();
    const enlarged = { ...notices[0], textSize: "large" as const };
    backend.getBanners = vi.fn(() => Promise.resolve([enlarged]));
    const { container } = render(<BannerApp client={backend} />);

    await screen.findByText("Codex task completed");
    expect(container.querySelector(".aizu-banner")).toHaveAttribute("data-text-size", "large");
  });

  it("returns to the terminal from an actionable banner without opening Aizu", async () => {
    const user = userEvent.setup();
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);

    await user.click((await screen.findAllByText("Codex task completed"))[0]);
    await waitFor(() => expect(backend.activate).toHaveBeenCalledWith(1));
    expect(backend.dismiss).not.toHaveBeenCalled();
    expect(container.querySelector('[data-banner-id="1"]')).not.toBeInTheDocument();
  });

  it("supports keyboard terminal activation", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const action = container.querySelector<HTMLElement>(
      '[data-banner-id="1"] .aizu-banner__content',
    );
    expect(action).toHaveAttribute("role", "button");
    expect(action).toHaveAttribute("tabindex", "0");
    if (!action) return;

    fireEvent.keyDown(action, { key: "Enter" });

    await waitFor(() => expect(backend.activate).toHaveBeenCalledWith(1));
  });

  it("does not start dismissal while terminal activation is pending", async () => {
    const pending = deferred<undefined>();
    const backend = client();
    vi.mocked(backend.activate).mockReturnValueOnce(pending.promise);
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const action = container.querySelector<HTMLElement>(
      '[data-banner-id="1"] .aizu-banner__content',
    );
    const banner = container.querySelector<HTMLElement>('[data-banner-id="1"]');
    expect(action).not.toBeNull();
    expect(banner).not.toBeNull();
    if (!action || !banner) return;

    fireEvent.click(action);
    await waitFor(() => expect(backend.activate).toHaveBeenCalledWith(1));
    fireEvent.click((await screen.findAllByRole("button", { name: "Dismiss notification" }))[0]);
    fireEvent.pointerDown(banner, {
      button: 0,
      clientX: 140,
      clientY: 20,
      isPrimary: true,
      pointerId: 16,
    });
    fireEvent.pointerMove(banner, { clientX: 40, clientY: 20, pointerId: 16 });
    fireEvent.pointerUp(banner, { clientX: 40, clientY: 20, pointerId: 16 });

    expect(backend.dismiss).not.toHaveBeenCalled();
    expect(banner).not.toHaveClass("aizu-banner--dismiss-left");
    pending.resolve(undefined);
  });

  it("keeps unsupported banners passive and dismisses from the close button", async () => {
    const user = userEvent.setup();
    const backend = client();
    render(<BannerApp client={backend} />);

    await user.click(await screen.findByText("Claude Code needs input"));
    expect(backend.activate).not.toHaveBeenCalled();
    await user.click((await screen.findAllByRole("button", { name: "Dismiss notification" }))[0]);
    expect(backend.dismiss).toHaveBeenCalledWith(1);
  });

  it("does not activate while notification text is selected", async () => {
    const backend = client();
    const selection = vi.spyOn(window, "getSelection").mockReturnValue({
      isCollapsed: false,
    } as Selection);
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const body = container.querySelector<HTMLElement>(".aizu-banner__body");
    expect(body).not.toBeNull();
    if (!body) return;

    fireEvent.click(body);

    expect(backend.activate).not.toHaveBeenCalled();
    selection.mockRestore();
  });

  it("dismisses after a deliberate horizontal swipe", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const banner = container.querySelector<HTMLElement>('[data-banner-id="1"]');
    expect(banner).not.toBeNull();
    if (!banner) return;

    fireEvent.pointerDown(banner, { button: 0, clientX: 140, clientY: 20, isPrimary: true, pointerId: 7 });
    fireEvent.pointerMove(banner, { clientX: 42, clientY: 24, pointerId: 7 });
    expect(banner).toHaveClass("aizu-banner--dragging");
    expect(Number.parseInt(banner.style.getPropertyValue("--aizu-swipe-offset"), 10)).toBeLessThanOrEqual(-72);
    fireEvent.pointerUp(banner, { clientX: 42, clientY: 24, pointerId: 7 });

    expect(banner).toHaveClass("aizu-banner--dismiss-left");
    await waitFor(() => expect(backend.dismiss).toHaveBeenCalledWith(1));
    expect(backend.activate).not.toHaveBeenCalled();
    await waitFor(() => expect(container.querySelector('[data-banner-id="1"]')).not.toBeInTheDocument());
  });

  it("supports a mouse-only drag when WebKit does not emit pointer events", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const banner = container.querySelector<HTMLElement>('[data-banner-id="1"]');
    const handle = banner?.querySelector<HTMLElement>(".aizu-banner__mark");
    expect(banner).not.toBeNull();
    expect(handle).not.toBeNull();
    if (!banner || !handle) return;

    fireEvent.mouseDown(handle, { button: 0, clientX: 140, clientY: 20 });
    fireEvent.mouseMove(window, { button: 0, clientX: 40, clientY: 20 });
    await waitFor(() => expect(banner).toHaveClass("aizu-banner--dragging"));
    expect(Number.parseInt(banner.style.getPropertyValue("--aizu-swipe-offset"), 10)).toBeLessThanOrEqual(-72);
    fireEvent.mouseUp(window, { button: 0, clientX: 40, clientY: 20 });

    await waitFor(() => expect(banner).toHaveClass("aizu-banner--dismiss-left"));
    await waitFor(() => expect(backend.dismiss).toHaveBeenCalledWith(1));
  });

  it("supports a right swipe without sending dismiss twice", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const banner = container.querySelector<HTMLElement>('[data-banner-id="1"]');
    expect(banner).not.toBeNull();
    if (!banner) return;

    fireEvent.pointerDown(banner, { button: 0, clientX: 20, clientY: 20, isPrimary: true, pointerId: 12 });
    fireEvent.pointerMove(banner, { clientX: 120, clientY: 20, pointerId: 12 });
    fireEvent.pointerUp(banner, { clientX: 120, clientY: 20, pointerId: 12 });
    fireEvent.click((await screen.findAllByRole("button", { name: "Dismiss notification" }))[0]);

    expect(banner).toHaveClass("aizu-banner--dismiss-right");
    await waitFor(() => expect(backend.dismiss).toHaveBeenCalledTimes(1));
  });

  it("does not start a second dismiss gesture while close is pending", async () => {
    const pending = deferred<undefined>();
    const backend = client();
    vi.mocked(backend.dismiss).mockReturnValueOnce(pending.promise);
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const banner = container.querySelector<HTMLElement>('[data-banner-id="1"]');
    expect(banner).not.toBeNull();
    if (!banner) return;

    fireEvent.click((await screen.findAllByRole("button", { name: "Dismiss notification" }))[0]);
    fireEvent.pointerDown(banner, { button: 0, clientX: 140, clientY: 20, isPrimary: true, pointerId: 13 });
    fireEvent.pointerMove(banner, { clientX: 40, clientY: 20, pointerId: 13 });
    fireEvent.pointerUp(banner, { clientX: 40, clientY: 20, pointerId: 13 });

    expect(backend.dismiss).toHaveBeenCalledTimes(1);
    expect(banner).not.toHaveClass("aizu-banner--dismiss-left");
    pending.resolve(undefined);
  });

  it("restores short swipes and ignores vertical drags", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const banner = container.querySelector<HTMLElement>('[data-banner-id="1"]');
    expect(banner).not.toBeNull();
    if (!banner) return;

    fireEvent.pointerDown(banner, { button: 0, clientX: 100, clientY: 20, isPrimary: true, pointerId: 8 });
    fireEvent.pointerMove(banner, { clientX: 60, clientY: 22, pointerId: 8 });
    fireEvent.pointerUp(banner, { clientX: 60, clientY: 22, pointerId: 8 });
    expect(banner.style.getPropertyValue("--aizu-swipe-offset")).toBe("0px");

    fireEvent.pointerDown(banner, { button: 0, clientX: 100, clientY: 20, isPrimary: true, pointerId: 9 });
    fireEvent.pointerMove(banner, { clientX: 90, clientY: 110, pointerId: 9 });
    fireEvent.pointerUp(banner, { clientX: 90, clientY: 110, pointerId: 9 });
    expect(backend.dismiss).not.toHaveBeenCalled();
  });

  it("suppresses only the synthetic click after a short drag", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const banner = container.querySelector<HTMLElement>('[data-banner-id="1"]');
    const action = banner?.querySelector<HTMLElement>(".aizu-banner__content");
    expect(banner).not.toBeNull();
    expect(action).not.toBeNull();
    if (!banner || !action) return;

    fireEvent.pointerDown(banner, {
      button: 0,
      clientX: 100,
      clientY: 20,
      isPrimary: true,
      pointerId: 15,
    });
    fireEvent.pointerMove(banner, { clientX: 60, clientY: 20, pointerId: 15 });
    fireEvent.pointerUp(banner, { clientX: 60, clientY: 20, pointerId: 15 });
    fireEvent.click(action);
    expect(backend.activate).not.toHaveBeenCalled();

    await new Promise((resolve) => window.setTimeout(resolve, 0));
    fireEvent.click(action);
    await waitFor(() => expect(backend.activate).toHaveBeenCalledWith(1));
  });

  it("starts a horizontal swipe from selectable body text", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const body = container.querySelector<HTMLElement>(".aizu-banner__body");
    const banner = container.querySelector<HTMLElement>('[data-banner-id="1"]');
    expect(body).not.toBeNull();
    expect(banner).not.toBeNull();
    if (!body || !banner) return;

    fireEvent.pointerDown(body, { button: 0, clientX: 120, clientY: 40, isPrimary: true, pointerId: 10 });
    fireEvent.pointerMove(banner, { clientX: 20, clientY: 40, pointerId: 10 });
    fireEvent.pointerUp(banner, { clientX: 20, clientY: 40, pointerId: 10 });
    await waitFor(() => expect(backend.dismiss).toHaveBeenCalledWith(1));
  });

  it("keeps short body drags available for text selection", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const body = container.querySelector<HTMLElement>(".aizu-banner__body");
    const banner = container.querySelector<HTMLElement>('[data-banner-id="1"]');
    expect(body).not.toBeNull();
    expect(banner).not.toBeNull();
    if (!body || !banner) return;

    fireEvent.pointerDown(body, { button: 0, clientX: 120, clientY: 40, isPrimary: true, pointerId: 12 });
    fireEvent.pointerMove(banner, { clientX: 116, clientY: 40, pointerId: 12 });
    fireEvent.pointerUp(banner, { clientX: 116, clientY: 40, pointerId: 12 });

    expect(backend.dismiss).not.toHaveBeenCalled();
    expect(banner.style.getPropertyValue("--aizu-swipe-offset")).toBe("0px");
  });

  it("restores a swiped banner when persistence fails", async () => {
    const backend = client();
    vi.mocked(backend.dismiss).mockRejectedValueOnce(new Error("unavailable"));
    const { container } = render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    const banner = container.querySelector<HTMLElement>('[data-banner-id="1"]');
    expect(banner).not.toBeNull();
    if (!banner) return;

    fireEvent.pointerDown(banner, { button: 0, clientX: 140, clientY: 20, isPrimary: true, pointerId: 11 });
    fireEvent.pointerMove(banner, { clientX: 40, clientY: 20, pointerId: 11 });
    fireEvent.pointerUp(banner, { clientX: 40, clientY: 20, pointerId: 11 });

    await waitFor(() => expect(backend.dismiss).toHaveBeenCalledWith(1));
    await waitFor(() => expect(banner).not.toHaveClass("aizu-banner--dismiss-left"));
    expect(banner.style.getPropertyValue("--aizu-swipe-offset")).toBe("0px");
  });

  it("serializes overlapping refreshes and applies the queued follow-up", async () => {
    const older = deferred<BannerNotification[]>();
    const newer = deferred<BannerNotification[]>();
    const backend = client();
    vi.mocked(backend.getBanners)
      .mockImplementationOnce(() => older.promise)
      .mockImplementationOnce(() => newer.promise);

    render(<BannerApp client={backend} />);
    await waitFor(() => expect(backend.getBanners).toHaveBeenCalledTimes(1));

    older.resolve([notices[0]]);
    await waitFor(() => expect(backend.getBanners).toHaveBeenCalledTimes(2));
    newer.resolve([notices[1]]);

    expect(await screen.findByText(notices[1].body)).toBeVisible();
    expect(screen.queryByText(notices[0].body)).not.toBeInTheDocument();
  });

  it("re-reads backend state when the native window sends a payload-free wake-up", async () => {
    const backend = client();
    let queued: BannerNotification[] = [];
    backend.getBanners = vi.fn(() => Promise.resolve(queued));
    render(<BannerApp client={backend} />);
    await waitFor(() => expect(backend.getBanners).toHaveBeenCalled());
    const callsBeforeWake = vi.mocked(backend.getBanners).mock.calls.length;

    queued = [notices[0]];
    window.dispatchEvent(new Event("aizu-banner-refresh"));

    await waitFor(() => {
      expect(backend.getBanners).toHaveBeenCalledTimes(callsBeforeWake + 1);
    });
    expect(await screen.findByText(notices[0].title)).toBeVisible();
  });

  it("does not let an in-flight snapshot replace a pushed banner state", async () => {
    const firstStale = deferred<BannerNotification[]>();
    const secondStale = deferred<BannerNotification[]>();
    const backend = client();
    vi.mocked(backend.getBanners)
      .mockImplementationOnce(() => firstStale.promise)
      .mockImplementationOnce(() => secondStale.promise);
    render(<BannerApp client={backend} />);
    await waitFor(() => expect(backend.getBanners).toHaveBeenCalledTimes(1));
    const [[subscription]] = vi.mocked(backend.subscribe).mock.calls;
    subscription([notices[1]]);
    expect(await screen.findByText(notices[1].body)).toBeVisible();

    firstStale.resolve(notices);
    await waitFor(() => expect(backend.getBanners).toHaveBeenCalledTimes(2));
    secondStale.resolve([notices[1]]);

    expect(screen.queryByText("Codex task completed")).not.toBeInTheDocument();
    expect(await screen.findByText(notices[1].body)).toBeVisible();
  });

  it("refreshes authoritatively after dismiss so a concurrent new banner remains visible", async () => {
    const dismissPending = deferred<undefined>();
    const authoritativeRefresh = deferred<BannerNotification[]>();
    const backend = client();
    const newNotice = { ...notices[1], id: 3, title: "New remote question" };
    render(<BannerApp client={backend} />);
    await screen.findByText("Codex task completed");
    vi.mocked(backend.dismiss).mockReturnValueOnce(dismissPending.promise);
    vi.mocked(backend.getBanners).mockImplementationOnce(() => authoritativeRefresh.promise);

    fireEvent.click((await screen.findAllByRole("button", { name: "Dismiss notification" }))[0]);
    const [[subscription]] = vi.mocked(backend.subscribe).mock.calls;
    subscription([notices[1], newNotice]);
    expect(await screen.findByText("New remote question")).toBeVisible();
    dismissPending.resolve(undefined);
    await waitFor(() => expect(backend.getBanners).toHaveBeenCalledTimes(3));
    authoritativeRefresh.resolve([notices[1], newNotice]);

    expect(await screen.findByText("New remote question")).toBeVisible();
    expect(screen.queryByText("Codex task completed")).not.toBeInTheDocument();
  });

  it("shows the exact command and returns a one-shot approval decision", async () => {
    const backend = client();
    const pendingAcknowledgement = deferred<undefined>();
    const pendingDecision = deferred<undefined>();
    const approval: BannerNotification = {
      ...notices[1],
      id: -1,
      title: "Codex requests permission",
      body: "Review the exact command before choosing.",
      approval: {
        agent: "codex",
        toolName: "Bash",
        target: {
          kind: "shellCommand",
          command: "printf 'first line'\nprintf 'second line'",
        },
      },
    };
    let approvalQueue = [notices[0], approval];
    backend.getBanners = vi.fn(() => Promise.resolve(approvalQueue));
    backend.acknowledgeApproval = vi.fn(() => pendingAcknowledgement.promise);
    backend.decideApproval = vi.fn(() => pendingDecision.promise.then(() => {
      approvalQueue = approvalQueue.filter((banner) => banner.id !== -1);
    }));
    const { container } = render(<BannerApp client={backend} />);

    expect(await screen.findByText("Codex requests permission")).toBeVisible();
    expect(screen.getByRole("alertdialog")).toHaveAttribute("aria-modal", "true");
    expect(container.querySelector(".banner-stack")).toHaveAttribute(
      "data-presentation",
      "approval",
    );
    expect(container.querySelector(".aizu-banner")).toHaveClass("aizu-banner--approval");
    expect(screen.queryByText("Codex task completed")).not.toBeInTheDocument();
    expect(container.querySelector(".aizu-banner__approval-target")?.textContent).toBe(
      approval.approval?.target.kind === "shellCommand"
        ? approval.approval.target.command
        : undefined,
    );
    const allow = screen.getByRole("button", { name: "Allow once" });
    const deny = screen.getByRole("button", { name: "Deny" });
    expect(allow).toBeDisabled();
    expect(deny).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Dismiss notification" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Choose in terminal" })).not.toBeInTheDocument();
    await waitFor(() => expect(backend.acknowledgeApproval).toHaveBeenCalledWith(-1));
    pendingAcknowledgement.resolve(undefined);
    await waitFor(() => expect(allow).toBeEnabled());
    await userEvent.click(allow);

    expect(allow).toBeDisabled();
    expect(deny).toBeDisabled();
    await userEvent.click(deny);
    expect(backend.decideApproval).toHaveBeenCalledTimes(1);
    expect(backend.decideApproval).toHaveBeenCalledWith(-1, "allowOnce");
    pendingDecision.resolve(undefined);
    await waitFor(() => {
      expect(screen.queryByText("Codex requests permission")).not.toBeInTheDocument();
    });
    expect(await screen.findByText("Codex task completed")).toBeVisible();
    expect(container.querySelector(".banner-stack")).toHaveAttribute(
      "data-presentation",
      "passive",
    );
  });

  it("shows a Claude Code WebFetch URL without turning it into a link", async () => {
    const backend = client();
    const approval: BannerNotification = {
      ...notices[1],
      id: -3,
      title: "Claude Code requests permission",
      body: "Review the URL before choosing.",
      approval: {
        agent: "claudeCode",
        toolName: "WebFetch",
        target: {
          kind: "webFetch",
          url: "https://docs.example.com/guide?topic=hooks#approval",
        },
      },
    };
    backend.getBanners = vi.fn(() => Promise.resolve([approval]));
    const { container } = render(<BannerApp client={backend} />);

    expect(await screen.findByText("Claude Code requests permission")).toBeVisible();
    const target = screen.getByLabelText("URL to fetch");
    expect(target).toHaveTextContent("https://docs.example.com/guide?topic=hooks#approval");
    expect(target).toHaveClass("aizu-banner__approval-target");
    expect(container.querySelector("a")).not.toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("button", { name: "Allow once" })).toBeEnabled());
  });

  it("keeps approval visible until an explicit decision", async () => {
    const backend = client();
    const approval: BannerNotification = {
      ...notices[1],
      id: -2,
      title: "Codex が実行許可を求めています",
      body: "コマンドを確認して選択してください。",
      language: "ja",
      approval: {
        agent: "codex",
        toolName: "Bash",
        target: {
          kind: "shellCommand",
          command: "printf 'terminal fallback'",
        },
      },
    };
    backend.getBanners = vi.fn(() => Promise.resolve([approval]));
    const { container } = render(<BannerApp client={backend} />);

    await screen.findByText("Codex が実行許可を求めています");
    const dialog = screen.getByRole("alertdialog");
    await waitFor(() => expect(dialog).toHaveFocus());
    expect(screen.queryByRole("button", { name: "Terminalで選ぶ" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "通知を閉じる" })).not.toBeInTheDocument();

    fireEvent.pointerDown(dialog, {
      button: 0,
      clientX: 180,
      clientY: 40,
      isPrimary: true,
      pointerId: 21,
    });
    fireEvent.pointerMove(dialog, { clientX: 40, clientY: 40, pointerId: 21 });
    fireEvent.pointerUp(dialog, { clientX: 40, clientY: 40, pointerId: 21 });
    fireEvent.keyDown(dialog, { key: "Escape" });

    expect(dialog).not.toHaveClass("aizu-banner--dragging");
    expect(dialog.style.getPropertyValue("--aizu-swipe-offset")).toBe("0px");
    expect(container.querySelector('[data-banner-id="-2"]')).toBeInTheDocument();
    expect(backend.dismiss).not.toHaveBeenCalled();
    expect(backend.decideApproval).not.toHaveBeenCalled();
  });
});
