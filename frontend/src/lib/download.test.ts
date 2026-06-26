import { describe, it, expect } from "vitest";
import { joinPath } from "./download";

describe("joinPath", () => {
  it("joins a dir and name with the dir's native separator", () => {
    expect(joinPath("/Users/x/Downloads", "a.png")).toBe(
      "/Users/x/Downloads/a.png",
    );
    // A trailing separator isn't doubled.
    expect(joinPath("/Users/x/Downloads/", "a.png")).toBe(
      "/Users/x/Downloads/a.png",
    );
    // Windows path → backslash separator.
    expect(joinPath("C:\\Users\\x\\Downloads", "a.png")).toBe(
      "C:\\Users\\x\\Downloads\\a.png",
    );
    // No dir → bare name (caller lets the OS dialog pick the location).
    expect(joinPath("", "a.png")).toBe("a.png");
  });
});
