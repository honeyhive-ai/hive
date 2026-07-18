import { describe, it, expect } from "vitest";
import { attachmentMarker, splitAttachments, fileBaseName, isImagePath } from "./attachments";

describe("attachments", () => {
  it("round-trips markers", () => {
    const body = `Look at this ${attachmentMarker("/tmp/a.png")}`;
    const { text, paths } = splitAttachments(body);
    expect(paths).toEqual(["/tmp/a.png"]);
    expect(text).toBe("Look at this");
  });

  it("extracts multiple paths and collapses blank lines", () => {
    const body = `Review\n\n${attachmentMarker("/x/a.png")}\n${attachmentMarker("/x/b.txt")}`;
    const { text, paths } = splitAttachments(body);
    expect(paths).toEqual(["/x/a.png", "/x/b.txt"]);
    expect(text).toBe("Review");
  });

  it("returns body unchanged when no markers", () => {
    expect(splitAttachments("plain text")).toEqual({ text: "plain text", paths: [] });
  });

  it("derives base name across separators", () => {
    expect(fileBaseName("/a/b/c.png")).toBe("c.png");
    expect(fileBaseName("C:\\docs\\d.txt")).toBe("d.txt");
    expect(fileBaseName("solo.md")).toBe("solo.md");
  });

  it("detects image extensions", () => {
    expect(isImagePath("/x/a.PNG")).toBe(true);
    expect(isImagePath("/x/a.jpeg")).toBe(true);
    expect(isImagePath("/x/a.txt")).toBe(false);
  });
});
