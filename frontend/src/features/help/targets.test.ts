import { describe, expect, it } from "vitest";
import { resolveHelpTarget } from "./targets";

describe("controlled help navigation", () => {
  it("only resolves declared topics, never a URL or selector supplied by a caller", () => {
    expect(resolveHelpTarget("download-location")).toEqual({
      kind: "settings",
      category: "files",
      controlId: "setting-download-folder",
    });
    expect(resolveHelpTarget("diagnostics")).toEqual({ kind: "diagnostics" });
    expect(resolveHelpTarget("first-run")).toBeNull();
    for (const value of [
      "__proto__",
      "constructor",
      "javascript:alert(1)",
      "#setting-retention",
      "../backup",
    ])
      expect(resolveHelpTarget(value)).toBeNull();
  });
});
