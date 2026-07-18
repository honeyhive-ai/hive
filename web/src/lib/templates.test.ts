import { describe, it, expect, beforeEach } from "vitest";
import { loadTemplates, addTemplate, removeTemplate } from "./templates";

beforeEach(() => window.localStorage.clear());

describe("prompt templates", () => {
  it("starts empty", () => {
    expect(loadTemplates()).toEqual([]);
  });

  it("adds and persists a template", () => {
    addTemplate("Bug report", "Steps to reproduce:");
    const list = loadTemplates();
    expect(list).toHaveLength(1);
    expect(list[0].name).toBe("Bug report");
    expect(list[0].body).toBe("Steps to reproduce:");
    expect(list[0].id).toBeTruthy();
  });

  it("trims and rejects empty name or body", () => {
    expect(addTemplate("  ", "body")).toEqual([]);
    expect(addTemplate("name", "   ")).toEqual([]);
    const list = addTemplate("  Spaced  ", "  text  ");
    expect(list[0]).toMatchObject({ name: "Spaced", body: "text" });
  });

  it("removes by id", () => {
    addTemplate("a", "x");
    const after = addTemplate("b", "y");
    const id = after[0].id;
    const remaining = removeTemplate(id);
    expect(remaining.map((t) => t.name)).toEqual(["b"]);
  });

  it("tolerates corrupt storage", () => {
    window.localStorage.setItem("hive.promptTemplates", "{not json");
    expect(loadTemplates()).toEqual([]);
  });
});
