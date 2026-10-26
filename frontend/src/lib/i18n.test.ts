import { afterEach, describe, expect, it, vi } from "vitest";
import i18n, {
  applyAccountLanguage,
  savedLanguage,
  subscribeLanguageChanges,
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

it("notifies local language subscribers once and never echoes account imports", async () => {
  const changed = vi.fn();
  const unsubscribe = subscribeLanguageChanges(changed);
  try {
    setLanguage("pt-BR");
    await Promise.resolve();
    expect(changed).toHaveBeenCalledExactlyOnceWith("pt-BR");
    expect(applyAccountLanguage("ko")).toBe(true);
    await Promise.resolve();
    expect(i18n.resolvedLanguage).toBe("ko");
    expect(changed).toHaveBeenCalledTimes(1);
    expect(applyAccountLanguage("ko-KR")).toBe(false);
    expect(changed).toHaveBeenCalledTimes(1);
  } finally {
    unsubscribe();
  }
  setLanguage("en");
  expect(changed).toHaveBeenCalledTimes(1);
});

it("does not treat a browser default as an explicit saved language", () => {
  const entries = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => entries.get(key) ?? null,
    setItem: (key: string, value: string) => entries.set(key, value),
  });
  vi.stubGlobal("navigator", { languages: ["ko-KR"], language: "ko-KR" });
  expect(savedLanguage()).toBeNull();
  setLanguage("pt-BR");
  expect(savedLanguage()).toBe("pt-BR");
});

it("reads denied storage safely and applies account languages without local echoes", () => {
  vi.stubGlobal("localStorage", {
    getItem() {
      throw new Error("Storage denied");
    },
    setItem() {
      throw new Error("Storage denied");
    },
  });
  const changed = vi.fn();
  const unsubscribe = subscribeLanguageChanges(changed);
  try {
    expect(savedLanguage()).toBeNull();
    for (const language of SUPPORTED_LANGUAGES) {
      expect(applyAccountLanguage(language)).toBe(true);
      expect(i18n.resolvedLanguage).toBe(language);
    }
    expect(changed).not.toHaveBeenCalled();
  } finally {
    unsubscribe();
  }
});

it.each([
  [null, ["unsupported", "ko-KR", "pt-BR"], "ko"],
  ["PT-br", ["ko-KR"], "pt-BR"],
  ["unsupported", ["pt-PT", "ko-KR"], "pt-BR"],
] as const)(
  "initializes from saved choice %s and ordered browser languages %j",
  async (stored, languages, expected) => {
    vi.stubGlobal("localStorage", { getItem: () => stored });
    vi.stubGlobal("navigator", { languages, language: "en" });
    vi.resetModules();
    const { default: initialized, savedLanguage: saved } =
      await import("./i18n");
    expect(initialized.resolvedLanguage).toBe(expected);
    expect(saved()).toBe(stored === "PT-br" ? "pt-BR" : null);
  },
);
