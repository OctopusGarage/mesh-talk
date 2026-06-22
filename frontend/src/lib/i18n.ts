import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "@/locales/en.json";
import es from "@/locales/es.json";
import ja from "@/locales/ja.json";
import zhHans from "@/locales/zh-Hans.json";
import zhHant from "@/locales/zh-Hant.json";
import yue from "@/locales/yue.json";

export const SUPPORTED_LANGUAGES = [
  "en",
  "es",
  "ja",
  "zh-Hans",
  "zh-Hant",
  "yue",
] as const;
export type Language = (typeof SUPPORTED_LANGUAGES)[number];

const KEY = "mesh-talk-lang";

function isLanguage(value: string): value is Language {
  return (SUPPORTED_LANGUAGES as readonly string[]).includes(value);
}

/** Map a stored or browser language tag to a supported code (or null). */
export function resolveLanguage(raw: string): Language | null {
  if (isLanguage(raw)) return raw;
  const lower = raw.toLowerCase();
  // Legacy persisted "zh" (was Simplified) → zh-Hans, so existing users don't break.
  if (lower === "zh" || lower === "zh-cn" || lower === "zh-hans")
    return "zh-Hans";
  if (lower === "zh-tw" || lower === "zh-hk" || lower === "zh-hant")
    return "zh-Hant";
  if (lower.startsWith("yue")) return "yue";
  if (lower.startsWith("zh")) return "zh-Hans";
  if (lower.startsWith("es")) return "es";
  if (lower.startsWith("ja")) return "ja";
  if (lower.startsWith("en")) return "en";
  return null;
}

/** Persisted choice, else the browser's preferred language, else English. */
function initialLanguage(): Language {
  if (typeof localStorage !== "undefined") {
    const stored = localStorage.getItem(KEY);
    if (stored) {
      const resolved = resolveLanguage(stored);
      if (resolved) return resolved;
    }
  }
  const nav = typeof navigator !== "undefined" ? navigator.language : "";
  return resolveLanguage(nav) ?? "en";
}

void i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    es: { translation: es },
    ja: { translation: ja },
    "zh-Hans": { translation: zhHans },
    "zh-Hant": { translation: zhHant },
    yue: { translation: yue },
  },
  lng: initialLanguage(),
  fallbackLng: "en",
  interpolation: { escapeValue: false }, // React already escapes
});

/** Switch and persist the active language. */
export function setLanguage(lng: Language) {
  if (typeof localStorage !== "undefined") localStorage.setItem(KEY, lng);
  void i18n.changeLanguage(lng);
}

export default i18n;
