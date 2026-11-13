import { afterEach, beforeEach, expect, it, vi } from "vitest";

let format: typeof import("./format");
let i18n: (typeof import("./i18n"))["default"];
beforeEach(async () => {
  vi.resetModules();
  i18n = (await import("./i18n")).default;
  await i18n.changeLanguage("en");
  format = await import("./format");
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});

it("reuses native display strings for unchanged local minute and historical day", () => {
  const date = new Date(2020, 2, 12, 14, 23);
  const expectedTime = date.toLocaleTimeString(i18n.resolvedLanguage, {
    hour: "2-digit",
    minute: "2-digit",
  });
  const expectedDay = date.toLocaleDateString(i18n.resolvedLanguage, {
    month: "short",
    day: "numeric",
  });
  const time = vi.spyOn(Date.prototype, "toLocaleTimeString");
  const day = vi.spyOn(Date.prototype, "toLocaleDateString");
  for (let index = 0; index < 12; index++) {
    expect(format.formatTime(date.getTime() + index * 1000)).toBe(expectedTime);
    expect(format.formatDay(date.getTime() + index * 1000)).toBe(expectedDay);
  }
  expect(time).toHaveBeenCalledTimes(1);
  expect(day).toHaveBeenCalledTimes(1);
});

it("shares one 256-entry LRU budget between time and historical day strings", () => {
  const base = new Date(2020, 0, 1, 10, 17);
  const time = vi.spyOn(Date.prototype, "toLocaleTimeString");
  const day = vi.spyOn(Date.prototype, "toLocaleDateString");
  const initial = format.formatTime(base.getTime());
  // One time plus 255 dates fills the shared budget.
  for (let index = 0; index < 255; index++)
    format.formatDay(new Date(2020, 0, index + 1).getTime());
  expect(format.formatTime(base.getTime())).toBe(initial); // Refresh LRU position.
  expect(time).toHaveBeenCalledTimes(1);
  format.formatDay(new Date(2020, 0, 256).getTime());
  format.formatDay(base.getTime()); // Oldest date was evicted across domains.
  expect(day).toHaveBeenCalledTimes(257);
  expect(format.formatTime(base.getTime())).toBe(initial);
  expect(time).toHaveBeenCalledTimes(1);
});

it("uses current language and preserves second/millisecond equivalence", async () => {
  const date = new Date(2020, 2, 12, 14, 23);
  for (const language of ["en", "ko", "pt-BR", "en"]) {
    await i18n.changeLanguage(language);
    expect(format.formatTime(date.getTime())).toBe(
      date.toLocaleTimeString(i18n.resolvedLanguage, {
        hour: "2-digit",
        minute: "2-digit",
      }),
    );
    expect(format.formatTime(date.getTime() / 1000)).toBe(
      format.formatTime(date.getTime()),
    );
    expect(format.formatDay(date.getTime())).toBe(
      date.toLocaleDateString(i18n.resolvedLanguage, {
        month: "short",
        day: "numeric",
      }),
    );
  }
});

it("does not retain invalid dates, unresolved locale, or failed formatting", () => {
  const date = new Date(2020, 2, 12, 14, 23);
  const native = Date.prototype.toLocaleTimeString;
  const time = vi.spyOn(Date.prototype, "toLocaleTimeString");
  format.formatTime(NaN);
  format.formatTime(NaN);
  expect(time).toHaveBeenCalledTimes(2);
  const language = i18n.resolvedLanguage;
  i18n.resolvedLanguage = undefined;
  format.formatTime(date.getTime());
  format.formatTime(date.getTime());
  expect(time).toHaveBeenCalledTimes(4);
  i18n.resolvedLanguage = language;
  time.mockImplementationOnce(() => {
    throw new Error("native formatting failed");
  });
  expect(() => format.formatTime(date.getTime())).toThrow(
    "native formatting failed",
  );
  expect(format.formatTime(date.getTime())).toBe(
    native.call(date, language, { hour: "2-digit", minute: "2-digit" }),
  );
  expect(time).toHaveBeenCalledTimes(6);
});

it("reclassifies today and yesterday before consulting historical display strings", async () => {
  vi.useFakeTimers();
  const date = new Date(2020, 2, 12, 14, 23);
  vi.setSystemTime(new Date(2020, 2, 14, 12));
  expect(format.formatDay(date.getTime())).toBe(
    date.toLocaleDateString(i18n.resolvedLanguage, {
      month: "short",
      day: "numeric",
    }),
  );
  vi.setSystemTime(new Date(2020, 2, 13, 12));
  expect(format.formatDay(date.getTime())).toBe("Yesterday");
  await i18n.changeLanguage("ko");
  expect(format.formatDay(date.getTime())).toBe("어제");
  vi.setSystemTime(new Date(2020, 2, 12, 23));
  expect(format.formatDay(date.getTime())).toBe("오늘");
});
