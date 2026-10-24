import { afterEach, describe, expect, it, vi } from "vitest";
import i18n, {
  resolveLanguage,
  setLanguage,
  SUPPORTED_LANGUAGES,
} from "./i18n";

afterEach(async () => {
  await i18n.changeLanguage("en");
  vi.unstubAllGlobals();
});

describe("interface language resolution", () => {
  it.each([
    ["pt", "pt-BR"],
    ["pt-BR", "pt-BR"],
    ["pt-PT", "pt-BR"],
    ["PT-br", "pt-BR"],
    ["ko", "ko"],
    ["ko-KR", "ko"],
    ["KO-kr", "ko"],
    ["zh", "zh-Hans"],
    ["zh-TW", "zh-Hant"],
    ["zh-MO", "zh-Hant"],
    ["zh-Hant-HK", "zh-Hant"],
    ["zh-Hans-SG", "zh-Hans"],
    ["  KO-kr  ", "ko"],
    ["esoteric", null],
    ["jargon", null],
    ["yesterday", null],
    ["ja-JP", "ja"],
    ["unsupported", null],
  ])("resolves %s to %s", (input, expected) => {
    expect(resolveLanguage(input)).toBe(expected);
  });

  it.each(["pt-BR", "ko"] as const)(
    "ships %s resources and switches without falling back to English",
    async (language) => {
      expect(SUPPORTED_LANGUAGES).toContain(language);
      // Use the public setter as the settings menu does.
      setLanguage(language as Parameters<typeof setLanguage>[0]);
      await i18n.changeLanguage(language);
      expect(i18n.resolvedLanguage).toBe(language);
      for (const key of [
        "settings.language",
        "settings.categories.general",
        "help.englishFallback",
        "privacy.limits",
      ]) {
        expect(i18n.exists(key, { lng: language, fallbackLng: false })).toBe(
          true,
        );
        expect(i18n.t(key)).not.toBe(i18n.getFixedT("en")(key));
      }
    },
  );
});

it("switches the interface and document language when preference storage is denied", () => {
  vi.stubGlobal("localStorage", {
    setItem() {
      throw new Error("Storage denied");
    },
  });
  vi.stubGlobal("document", { documentElement: { lang: "en" } });
  setLanguage("ko");
  expect(i18n.resolvedLanguage).toBe("ko");
  expect(document.documentElement.lang).toBe("ko");
});
