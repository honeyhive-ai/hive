import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { Markdown, nodeText } from "./Markdown";

describe("Markdown", () => {
  it("keeps single newlines as line breaks", () => {
    const { container } = render(<Markdown content={"line one\nline two\nline three"} />);
    expect(container.querySelectorAll("br")).toHaveLength(2);
    // One paragraph, not three — the breaks are inside it.
    expect(container.querySelectorAll("p")).toHaveLength(1);
  });

  it("still splits blank-line-separated text into paragraphs", () => {
    const { container } = render(<Markdown content={"first para\n\nsecond para"} />);
    expect(container.querySelectorAll("p")).toHaveLength(2);
  });

  it("leaves fenced code untouched", () => {
    const { container } = render(<Markdown content={"```\na\nb\n```"} />);
    expect(container.querySelectorAll("pre code")).toHaveLength(1);
    expect(container.querySelector("pre")?.textContent).toBe("a\nb\n");
    expect(container.querySelectorAll("br")).toHaveLength(0);
  });

  it("renders gfm and links without navigating the webview", () => {
    render(<Markdown content={"- [x] done\n- [ ] todo\n\n[site](https://example.com)"} />);
    const link = screen.getByRole("link", { name: "site" });
    expect(link).toHaveAttribute("href", "https://example.com");
    expect(screen.getAllByRole("checkbox")).toHaveLength(2);
  });

  it("still renders gfm tables", () => {
    const { container } = render(
      <Markdown content={"| a | b |\n| - | - |\n| 1 | 2 |"} />,
    );
    expect(container.querySelectorAll("table")).toHaveLength(1);
    expect(container.querySelectorAll("thead th")).toHaveLength(2);
    expect(container.querySelectorAll("tbody tr")).toHaveLength(1);
    // Table rows are built from the newlines — they must not become <br>.
    expect(container.querySelectorAll("br")).toHaveLength(0);
  });

  it("hard-breaks a wrapped list item", () => {
    const { container } = render(<Markdown content={"- first\n  continued\n- second"} />);
    expect(container.querySelectorAll("li")).toHaveLength(2);
    // Accepted trade-off, do not "fix": a lazy continuation line inside a list
    // item now breaks instead of reflowing. Hard breaks apply to every author,
    // and preserving runtime line structure is worth this.
    expect(container.querySelectorAll("li")[0].querySelectorAll("br")).toHaveLength(1);
  });

  it("nodeText collects the source text of a node tree (#97)", () => {
    expect(nodeText("abc")).toBe("abc");
    expect(nodeText(["a", "b", 1])).toBe("ab1");
    expect(nodeText(null)).toBe("");
  });

  it("Copy writes the code block's SOURCE to the clipboard (#97)", async () => {
    // The copy path used to read `pre.innerText`, which jsdom returns as undefined —
    // so any clipboard assertion passed vacuously. It now copies the source text.
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });
    render(<Markdown content={"```ts\nconst a = 1;\nconst b = 2;\n```"} />);
    fireEvent.click(screen.getByRole("button", { name: "Copy code" }));
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText.mock.calls[0][0]).toContain("const a = 1;");
    expect(writeText.mock.calls[0][0]).toContain("const b = 2;");
  });
});
