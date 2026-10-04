import { describe, expect, it } from "vitest";
import en from "@/locales/en.json";

const locales = import.meta.glob<{ privacy?: Record<string, string> }>(
  "../locales/*.json",
  { eager: true, import: "default" },
);

describe("network privacy translations", () => {
  for (const [path, locale] of Object.entries(locales)) {
    it(`provides every privacy label and limitation in ${path}`, () => {
      const translated = locale.privacy;
      expect(Object.keys(translated ?? {}).sort()).toEqual(
        Object.keys(en.privacy).sort(),
      );
      expect(
        Object.values(translated ?? {}).every(
          (value) => value.trim().length > 0,
        ),
      ).toBe(true);
    });
  }
});
