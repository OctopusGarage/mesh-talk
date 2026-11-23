import i18n, { type BackendModule } from "i18next";
import { initReactI18next } from "react-i18next";
import en from "@/locales/en.json";

export const SUPPORTED_LANGUAGES = [
  "en",
  "es",
  "pt-BR",
  "ja",
  "ko",
  "zh-Hans",
  "zh-Hant",
  "yue",
] as const;
export type Language = (typeof SUPPORTED_LANGUAGES)[number];

export const LANGUAGE_LABELS: Record<Language, string> = {
  en: "English",
  es: "Español",
  "pt-BR": "Português",
  ja: "日本語",
  ko: "한국어",
  "zh-Hans": "简体中文",
  "zh-Hant": "繁體中文",
  yue: "粵語",
};

const KEY = "mesh-talk-lang";
const localLanguageListeners = new Set<(language: Language) => void>();

function isLanguage(value: string): value is Language {
  return (SUPPORTED_LANGUAGES as readonly string[]).includes(value);
}

/** Resolve saved/browser tags without accepting unrelated language prefixes. */
export function resolveLanguage(raw: string): Language | null {
  const lower = raw.trim().toLowerCase();
  const exact = SUPPORTED_LANGUAGES.find(
    (code) => code.toLowerCase() === lower,
  );
  if (exact) return exact;
  if (/^zh-(?:tw|hk|mo|hant)(?:-|$)/.test(lower)) return "zh-Hant";
  // Preserve the legacy saved "zh" choice as Simplified Chinese.
  if (/^zh(?:-|$)/.test(lower)) return "zh-Hans";
  const base = lower.split("-")[0];
  if (base === "pt") return "pt-BR";
  return SUPPORTED_LANGUAGES.find((code) => code === base) ?? null;
}

/** Explicit persisted choice only; browser defaults are not account preferences. */
export function savedLanguage(): Language | null {
  try {
    if (typeof localStorage !== "undefined") {
      const stored = localStorage.getItem(KEY);
      if (stored) return resolveLanguage(stored);
    }
  } catch {
    // Embedded browsers can deny storage; account preferences still work in session.
  }
  return null;
}

/** Persisted choice, else the browser's preferred language, else English. */
function initialLanguage(): Language {
  const saved = savedLanguage();
  if (saved) return saved;
  if (typeof navigator !== "undefined") {
    for (const tag of navigator.languages ?? [navigator.language]) {
      const resolved = resolveLanguage(tag);
      if (resolved) return resolved;
    }
  }
  return "en";
}

i18n.on("languageChanged", (language) => {
  if (typeof document !== "undefined")
    document.documentElement.lang = resolveLanguage(language) ?? "en";
});

const catalogs = {
  es: () => import("@/locales/es.json"),
  ja: () => import("@/locales/ja.json"),
  "zh-Hans": () => import("@/locales/zh-Hans.json"),
  "zh-Hant": () => import("@/locales/zh-Hant.json"),
  yue: () => import("@/locales/yue.json"),
  "pt-BR": () => import("@/locales/pt-BR.json"),
  ko: () => import("@/locales/ko.json"),
} satisfies Record<Exclude<Language, "en">, () => Promise<{ default: object }>>;

const bundledCatalogs: BackendModule = {
  type: "backend",
  init() {},
  read(language, _namespace, callback) {
    if (!isLanguage(language) || language === "en") {
      callback(null, {});
      return;
    }
    void catalogs[language]().then(
      (catalog) => callback(null, catalog.default),
      (error: Error) => callback(error, false),
    );
  },
};

export const i18nReady = i18n
  .use(bundledCatalogs)
  .use(initReactI18next)
  .init({
    resources: { en: { translation: en } },
    lng: initialLanguage(),
    fallbackLng: "en",
    supportedLngs: [...SUPPORTED_LANGUAGES],
    load: "currentOnly",
    partialBundledLanguages: true,
    interpolation: { escapeValue: false },
  });

/** Switch and persist the active language. */
export function setLanguage(lng: Language) {
  if (!isLanguage(lng)) return;
  applyAccountLanguage(lng);
  for (const listener of localLanguageListeners) listener(lng);
}

/** Load a linked catalog before its owner rechecks authority and commits the preference. */
export async function prepareAccountLanguage(
  value: string | null,
): Promise<boolean> {
  if (value !== null && !isLanguage(value)) return false;
  const language = value ?? "en";
  await i18nReady;
  await i18n.loadLanguages(language);
  return i18n.hasResourceBundle(language, "translation");
}

/** Account imports never emit local edits, including asynchronous i18next events. */
export function applyAccountLanguage(value: string | null): boolean {
  if (value !== null && !isLanguage(value)) return false;
  const lng = value ?? "en";
  try {
    if (typeof localStorage !== "undefined") localStorage.setItem(KEY, lng);
  } catch {
    // A denied preference write must not prevent an in-session switch.
  }
  void i18n.changeLanguage(lng);
  return true;
}

/** Subscribe only to explicit changes made through the local language control. */
export function subscribeLanguageChanges(
  listener: (language: Language) => void,
): () => void {
  localLanguageListeners.add(listener);
  return () => {
    localLanguageListeners.delete(listener);
  };
}

export default i18n;
