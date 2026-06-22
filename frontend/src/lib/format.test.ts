import { describe, it, expect } from "vitest";
import { shortId, formatTime, humanSize, formatHostPort } from "./format";

describe("shortId", () => {
  it("truncates to the default 8 chars", () => {
    expect(shortId("0123456789abcdef")).toBe("01234567");
  });
  it("returns short ids unchanged", () => {
    expect(shortId("abc")).toBe("abc");
  });
  it("honours a custom length", () => {
    expect(shortId("0123456789", 4)).toBe("0123");
  });
});

describe("humanSize", () => {
  it("bytes", () => expect(humanSize(512)).toBe("512 B"));
  it("kilobytes (ceil)", () => expect(humanSize(2048)).toBe("2 KB"));
  it("megabytes (1 dp)", () => expect(humanSize(1_500_000)).toBe("1.4 MB"));
});

describe("formatHostPort", () => {
  it("leaves IPv4 unbracketed", () =>
    expect(formatHostPort("127.0.0.1", 80)).toBe("127.0.0.1:80"));
  it("brackets a bare IPv6 literal", () =>
    expect(formatHostPort("::1", 80)).toBe("[::1]:80"));
  it("leaves an already-bracketed host alone", () =>
    expect(formatHostPort("[::1]", 80)).toBe("[::1]:80"));
  it("leaves a hostname unaffected", () =>
    expect(formatHostPort("example.com", 443)).toBe("example.com:443"));
});

describe("formatTime", () => {
  it("treats a seconds timestamp and the equivalent millis identically", () => {
    // toMillis() must scale a < 1e12 value (seconds) up; same instant => same HH:MM.
    const secs = 1_700_000_000;
    const millis = 1_700_000_000_000;
    expect(formatTime(secs)).toBe(formatTime(millis));
  });
});
