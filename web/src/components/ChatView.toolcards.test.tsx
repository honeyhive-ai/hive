import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { ToolCallCards } from "./ChatView";

describe("ToolCallCards", () => {
  it("renders a card per call with name, args, and matched result", () => {
    render(
      <ToolCallCards
        calls={[
          { id: "c1", name: "read_file", inputJson: '{"path":"a.rs"}', serverId: "fs" },
          { id: "c2", name: "search", inputJson: "{}", serverId: null },
        ]}
        results={[
          { callId: "c1", content: "fn main() {}", isError: false },
          { callId: "c2", content: "boom", isError: true },
        ]}
      />,
    );
    expect(screen.getByText("read_file")).toBeTruthy();
    expect(screen.getByText("search")).toBeTruthy();
    // status reflects the matched result
    expect(screen.getByText("done")).toBeTruthy();
    expect(screen.getByText("error")).toBeTruthy();
    // args are pretty-printed
    expect(screen.getByText(/"path": "a\.rs"/)).toBeTruthy();
  });

  it("renders a call with no result (no status badge)", () => {
    render(
      <ToolCallCards
        calls={[{ id: "x", name: "pending_tool", inputJson: "{}", serverId: null }]}
        results={[]}
      />,
    );
    expect(screen.getByText("pending_tool")).toBeTruthy();
    expect(screen.queryByText("done")).toBeNull();
    expect(screen.queryByText("error")).toBeNull();
  });
});
