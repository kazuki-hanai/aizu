import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SafeMarkdown } from "./SafeMarkdown";

describe("SafeMarkdown", () => {
  it("renders compact Markdown structure and preserves command formatting", () => {
    const { container } = render(
      <SafeMarkdown>{[
        "## Result",
        "First line",
        "Second line with `mise run check`.",
        "",
        "- **Build** passed",
        "- Review _complete_",
        "",
        "```sh",
        "printf 'first line'",
        "printf 'second line'",
        "```",
      ].join("\n")}</SafeMarkdown>,
    );

    expect(screen.getByRole("heading", { level: 2, name: "Result" })).toBeVisible();
    expect(screen.getByText("mise run check")).toBeInstanceOf(HTMLElement);
    expect(screen.getByText("mise run check").tagName).toBe("CODE");
    expect(container.querySelectorAll("br")).toHaveLength(1);
    const list = screen.getByRole("list");
    expect(within(list).getAllByRole("listitem")).toHaveLength(2);
    expect(within(list).getByText("Build").tagName).toBe("STRONG");
    const block = container.querySelector("pre code.language-sh");
    expect(block?.textContent).toBe("printf 'first line'\nprintf 'second line'\n");
  });

  it("renders GFM tables without creating interactive content", () => {
    const { container } = render(
      <SafeMarkdown>{[
        "| State | Result |",
        "| --- | --- |",
        "| Check | Done |",
        "",
        "[Open docs](https://example.com/docs)",
        "![Build diagram](https://example.com/image.png)",
      ].join("\n")}</SafeMarkdown>,
    );

    expect(screen.getByRole("table")).toBeVisible();
    expect(screen.getByText("Open docs")).toHaveClass("aizu-markdown__link");
    expect(screen.getByText("Build diagram")).toHaveClass("aizu-markdown__image-alt");
    expect(container.querySelector("a")).not.toBeInTheDocument();
    expect(container.querySelector("img")).not.toBeInTheDocument();
  });

  it("does not insert raw HTML from notification content", () => {
    const { container } = render(
      <SafeMarkdown>{"Before <img src=x onerror=alert(1)> <script>alert(2)</script> after"}</SafeMarkdown>,
    );

    expect(container.querySelector("img")).not.toBeInTheDocument();
    expect(container.querySelector("script")).not.toBeInTheDocument();
    expect(container.innerHTML).not.toContain("onerror");
  });
});
