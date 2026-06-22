import { describe, it, expect } from "vitest";
import { messageSegments, mentionsName } from "./mentions";

describe("messageSegments", () => {
  it("splits a mention out of surrounding text", () => {
    expect(messageSegments("hi @alice there")).toEqual([
      { text: "hi ", kind: "plain" },
      { text: "@alice", kind: "mention" },
      { text: " there", kind: "plain" },
    ]);
  });
  it("plain text is one plain segment", () => {
    expect(messageSegments("just text")).toEqual([
      { text: "just text", kind: "plain" },
    ]);
  });
  it("handles adjacent mentions without empty segments", () => {
    expect(messageSegments("@a @b")).toEqual([
      { text: "@a", kind: "mention" },
      { text: " ", kind: "plain" },
      { text: "@b", kind: "mention" },
    ]);
  });
  it("detects a URL as a link segment", () => {
    expect(messageSegments("see https://example.com/x now")).toEqual([
      { text: "see ", kind: "plain" },
      { text: "https://example.com/x", kind: "link" },
      { text: " now", kind: "plain" },
    ]);
  });
  it("handles a mention and a link together", () => {
    expect(messageSegments("@bob http://a.io")).toEqual([
      { text: "@bob", kind: "mention" },
      { text: " ", kind: "plain" },
      { text: "http://a.io", kind: "link" },
    ]);
  });
});

describe("mentionsName", () => {
  it("matches a whole-word mention", () => {
    expect(mentionsName("hey @alice!", "alice")).toBe(true);
  });
  it("is case-insensitive", () => {
    expect(mentionsName("@Alice", "alice")).toBe(true);
  });
  it("respects a word boundary (no partial match)", () => {
    expect(mentionsName("@aliced", "alice")).toBe(false);
  });
  it("empty name never matches", () => {
    expect(mentionsName("@x", "")).toBe(false);
  });
  it("escapes regex metacharacters in the name", () => {
    expect(mentionsName("@a.b", "a.b")).toBe(true);
    expect(mentionsName("@axb", "a.b")).toBe(false);
  });
});
