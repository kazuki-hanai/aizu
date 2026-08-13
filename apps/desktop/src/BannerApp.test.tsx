import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { BannerApp } from "./BannerApp";
import type { BannerClient } from "./lib/backend";
import type { BannerNotification } from "./lib/contracts";
import "./styles.css";

class TestResizeObserver {
  observe() { return undefined; }
  disconnect() { return undefined; }
  unobserve() { return undefined; }
}

Object.defineProperty(window, "ResizeObserver", {
  configurable: true,
  value: TestResizeObserver,
});

const notices: BannerNotification[] = [
  {
    id: 1,
    title: "Codex task completed",
    body: "This complete safe notification body remains visible until it is dismissed.",
    sound: "ping",
    delivery: "aizuBanner",
    language: "en",
  },
  {
    id: 2,
    title: "Claude Code needs input",
    body: "Choose an option before the task can continue.",
    sound: null,
    delivery: "aizuBanner",
    language: "en",
  },
];

function client(): BannerClient {
  return {
    getBanners: vi.fn().mockResolvedValue(notices),
    dismiss: vi.fn().mockResolvedValue(undefined),
    open: vi.fn().mockResolvedValue(undefined),
    resize: vi.fn().mockResolvedValue(undefined),
    subscribe: vi.fn().mockResolvedValue(() => undefined),
  };
}

describe("Aizu Banner", () => {
  it("renders complete notification bodies without automatic dismissal", async () => {
    const backend = client();
    const { container } = render(<BannerApp client={backend} />);

    expect(await screen.findByText(notices[0].body)).toBeVisible();
    expect(screen.getByText(notices[1].body)).toBeVisible();
    expect(container.querySelectorAll(".aizu-banner")).toHaveLength(2);
    expect(backend.dismiss).not.toHaveBeenCalled();
    await waitFor(() => expect(backend.resize).toHaveBeenCalled());
  });

  it("dismisses explicitly and opens Aizu from notification content", async () => {
    const user = userEvent.setup();
    const backend = client();
    render(<BannerApp client={backend} />);

    await user.click((await screen.findAllByRole("button", { name: "Dismiss notification" }))[0]);
    expect(backend.dismiss).toHaveBeenCalledWith(1);
    await user.click(screen.getAllByRole("button", { name: "Open Aizu" })[1]);
    expect(backend.open).toHaveBeenCalledWith(2);
  });
});
