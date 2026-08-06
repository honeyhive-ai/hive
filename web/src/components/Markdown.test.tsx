import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Markdown, codeBlockText } from "./Markdown";

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
});

describe("codeBlockText", () => {
  const pre = (text: string) => {
    const el = document.createElement("pre");
    el.textContent = text;
    return el;
  };

  it("drops the trailing newline a fenced block always carries", () => {
    // Matches innerText, which strips trailing LFs per spec. Pasting into a
    // terminal must leave the command on the prompt, not run it.
    expect(codeBlockText(pre("npm run build\n"))).toBe("npm run build");
    expect(codeBlockText(pre("a\nb\n\n\n"))).toBe("a\nb");
  });

  it("keeps interior blank lines and leading indentation", () => {
    expect(codeBlockText(pre("def f():\n\n    return 1\n"))).toBe("def f():\n\n    return 1");
  });

  it("keeps trailing whitespace that is not a newline", () => {
    expect(codeBlockText(pre("a\n  \n"))).toBe("a\n  ");
  });

  it("is empty for a missing ref", () => {
    expect(codeBlockText(null)).toBe("");
  });
});

describe("Copy button", () => {
  it("puts the rendered code on the clipboard", async () => {
    const writeText = vi.fn(async () => {});
    Object.assign(navigator, { clipboard: { writeText } });

    render(<Markdown content={"```sh\nnpm run build\ncargo test\n```"} />);
    await userEvent.click(screen.getByRole("button", { name: "Copy code" }));

    expect(writeText).toHaveBeenCalledWith("npm run build\ncargo test");
    expect(await screen.findByText("Copied ✓")).toBeInTheDocument();
  });

  it("copies an indented code block with its indentation intact", async () => {
    const writeText = vi.fn(async () => {});
    Object.assign(navigator, { clipboard: { writeText } });

    // Four-space indent is the block marker; the inner two spaces are content.
    render(<Markdown content={"    if x:\n      pass"} />);
    await userEvent.click(screen.getByRole("button", { name: "Copy code" }));

    expect(writeText).toHaveBeenCalledWith("if x:\n  pass");
  });

  it("documents why this path reads textContent", () => {
    // The premise of the change: jsdom has no layout, so innerText is
    // undefined and every clipboard assertion above would pass vacuously if
    // the handler still read it. If jsdom ever implements innerText, this
    // fails — read the comment on codeBlockText and re-decide, don't just
    // delete the test.
    const { container } = render(<Markdown content={"```\na\n```"} />);
    expect((container.querySelector("pre") as HTMLElement).innerText).toBeUndefined();
  });
});
