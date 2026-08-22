import { describe, expect, it } from "vitest";
import { parseUnifiedDiff } from "./diff";

describe("parseUnifiedDiff", () => {
  it("splits a multi-file diff and counts adds/removes per file", () => {
    const diff = [
      "diff --git a/src/a.ts b/src/a.ts",
      "index 111..222 100644",
      "--- a/src/a.ts",
      "+++ b/src/a.ts",
      "@@ -1,2 +1,2 @@",
      "-old",
      "+new",
      " ctx",
      "diff --git a/README.md b/README.md",
      "--- a/README.md",
      "+++ b/README.md",
      "@@ -0,0 +1 @@",
      "+hello",
    ].join("\n");
    const files = parseUnifiedDiff(diff);
    expect(files).toHaveLength(2);
    expect(files[0].path).toBe("src/a.ts");
    expect(files[0].added).toBe(1);
    expect(files[0].removed).toBe(1);
    expect(files[1].path).toBe("README.md");
    expect(files[1].added).toBe(1);
    expect(files[1].removed).toBe(0);
  });

  it("handles a headerless diff as one section", () => {
    const files = parseUnifiedDiff("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b");
    expect(files).toHaveLength(1);
    expect(files[0].path).toBe("x");
  });
});
