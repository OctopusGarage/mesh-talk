import { afterEach, describe, it, expect, vi } from "vitest";
import {
  shortId,
  formatTime,
  formatDay,
  formatAgo,
  humanSize,
  formatHostPort,
} from "./format";
import i18n from "./i18n";

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

describe("localized conversation dates", () => {
  afterEach(async () => {
    vi.useRealTimers();
    await i18n.changeLanguage("en");
  });

  it.each([
    ["pt-BR", "Hoje", "Ontem"],
    ["ko", "오늘", "어제"],
    ["en", "Today", "Yesterday"],
  ])(
    "uses %s for day separators and message times",
    async (language, today, yesterday) => {
      vi.useFakeTimers();
      vi.setSystemTime(new Date(2026, 9, 24, 12));
      await i18n.changeLanguage(language);
      expect(formatDay(new Date(2026, 9, 24, 9).getTime())).toBe(today);
      expect(formatDay(new Date(2026, 9, 23, 9).getTime())).toBe(yesterday);
      const earlier = new Date(2026, 8, 12, 9);
      expect(formatDay(earlier.getTime())).toBe(
        earlier.toLocaleDateString(language, {
          month: "short",
          day: "numeric",
        }),
      );
      expect(formatTime(earlier.getTime())).toBe(
        earlier.toLocaleTimeString(language, {
          hour: "2-digit",
          minute: "2-digit",
        }),
      );
    },
  );

  it.each(["pt-BR", "ko"])(
    "uses %s for observed elapsed time",
    async (language) => {
      await i18n.changeLanguage(language);
      const relative = new Intl.RelativeTimeFormat(language, {
        numeric: "auto",
        style: "narrow",
      });
      expect(formatAgo(0)).toBe(relative.format(0, "second"));
      expect(formatAgo(8)).toBe(relative.format(-8, "second"));
      expect(formatAgo(125)).toBe(relative.format(-2, "minute"));
      expect(formatAgo(7300)).toBe(relative.format(-2, "hour"));
    },
  );
});
