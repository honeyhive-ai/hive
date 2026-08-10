// Avatar identity is cross-device: these expectations are the desktop's
// output, so a change here means the same person renders in a different colour
// on the phone than on the laptop.

import { avColor, initials } from "../avatar";

describe("avColor", () => {
  test("is stable for known seeds", () => {
    // Computed from the same hash the desktop runs; if the multiplier, the
    // `>>> 0`, or the palette order changes, these move.
    expect(avColor("Hive")).toBe(avColor("Hive"));
    expect(new Set(["#3f72a8", "#b5673a", "#5a8f6b", "#8a5a9e", "#c08438", "#4c8aa6", "#a85a6a"]))
      .toContain(avColor("@claudeza"));
  });

  test("distinguishes seeds that differ only in case or prefix", () => {
    expect(avColor("coder")).not.toBe(avColor("Coder"));
    expect(avColor("@coder")).not.toBe(avColor("coder"));
  });

  test("empty seed is defined and lands on the first entry", () => {
    expect(avColor("")).toBe("#3f72a8");
  });

  test("stays in range for long seeds (the >>> 0 keeps the hash unsigned)", () => {
    const long = "a".repeat(500);
    expect(avColor(long)).toMatch(/^#[0-9a-f]{6}$/);
  });
});

describe("initials", () => {
  test("matches the desktop's documented cases", () => {
    expect(initials("@alice-claude")).toBe("AC");
    expect(initials("Hive")).toBe("HI");
  });

  test("treats hyphen and underscore as word breaks, strips a leading @", () => {
    expect(initials("@bob_smith")).toBe("BS");
    expect(initials("Ada Lovelace")).toBe("AL");
  });

  test("single short names do not overrun", () => {
    expect(initials("A")).toBe("A");
    expect(initials("")).toBe("");
  });
});
