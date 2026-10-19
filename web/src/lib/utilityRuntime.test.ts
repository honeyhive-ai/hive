import { describe, it, expect } from "vitest";
import { quickTaskOptions } from "./utilityRuntime";

const runtimes = [
  { id: "gemma", label: "Gateway · gemma4:12b" },
  { id: "lfm2", label: "" },
];

describe("quickTaskOptions", () => {
  it("leads with Same as chat, then every runtime", () => {
    expect(quickTaskOptions(runtimes, null)).toEqual([
      { value: "", label: "Same as chat" },
      { value: "gemma", label: "Gateway · gemma4:12b" },
      { value: "lfm2", label: "lfm2" },
    ]);
  });

  it("keeps a removed runtime visible as missing", () => {
    const opts = quickTaskOptions(runtimes, "gone");
    expect(opts.at(-1)).toEqual({ value: "gone", label: "gone (missing)" });
  });

  it("does not duplicate a listed selection", () => {
    expect(quickTaskOptions(runtimes, "lfm2")).toHaveLength(3);
  });
});
