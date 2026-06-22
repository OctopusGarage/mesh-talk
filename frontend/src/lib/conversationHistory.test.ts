import { describe, it, expect } from "vitest";
import { categorize, hasLink, matchesQuery } from "./conversationHistory";
import type { ChatMessage } from "@/store/chat";

function msg(over: Partial<ChatMessage>): ChatMessage {
  return {
    id: "1",
    fromMe: false,
    who: "bob",
    text: "",
    wallClock: 0,
    replyTo: null,
    file: null,
    ...over,
  };
}

function file(name: string): NonNullable<ChatMessage["file"]> {
  return { name, size: 1, mime: "", fileConv: "abc" };
}

describe("hasLink", () => {
  it("detects an http(s) URL", () => {
    expect(hasLink("see https://example.com now")).toBe(true);
    expect(hasLink("http://a.io")).toBe(true);
  });
  it("is false for plain text", () => {
    expect(hasLink("no links here")).toBe(false);
    expect(hasLink("ftp://nope.com")).toBe(false);
  });
});

describe("categorize", () => {
  it("classifies an image file as media", () => {
    expect(categorize(msg({ file: file("cat.PNG") }))).toBe("media");
    expect(categorize(msg({ file: file("clip.mp4") }))).toBe("media");
  });
  it("classifies a non-media file as files", () => {
    expect(categorize(msg({ file: file("report.pdf") }))).toBe("files");
    expect(categorize(msg({ file: file("data.zip") }))).toBe("files");
  });
  it("classifies a text message with a URL as links", () => {
    expect(categorize(msg({ text: "look https://x.io" }))).toBe("links");
  });
  it("classifies plain text as other", () => {
    expect(categorize(msg({ text: "hello there" }))).toBe("other");
  });
  it("a file message is never a link even if it had link-ish text", () => {
    expect(categorize(msg({ text: "https://x.io", file: file("a.pdf") }))).toBe(
      "files",
    );
  });
});

describe("matchesQuery", () => {
  it("empty query matches everything", () => {
    expect(matchesQuery(msg({ text: "anything" }), "  ")).toBe(true);
  });
  it("matches text case-insensitively", () => {
    expect(matchesQuery(msg({ text: "Hello World" }), "world")).toBe(true);
    expect(matchesQuery(msg({ text: "Hello" }), "bye")).toBe(false);
  });
  it("matches a file name", () => {
    expect(matchesQuery(msg({ file: file("Quarterly.pdf") }), "quarter")).toBe(
      true,
    );
  });
});
