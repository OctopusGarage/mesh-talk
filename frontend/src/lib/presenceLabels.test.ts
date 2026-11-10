import { expect, it } from "vitest";
import i18n, { SUPPORTED_LANGUAGES } from "./i18n";

const templates = {
  en: "People observed online by this client: {{count}}",
  es: "Personas observadas en línea por este cliente: {{count}}",
  ja: "このクライアントでオンライン確認：{{count}} 人",
  "zh-Hans": "当前客户端已知在线 {{count}} 人",
  "zh-Hant": "目前用戶端已知在線 {{count}} 人",
  yue: "呢個客戶端已知在線 {{count}} 人",
  "pt-BR": "Pessoas observadas online por este cliente: {{count}}",
  ko: "이 클라이언트에서 온라인으로 확인한 사용자: {{count}}명",
};

it.each(SUPPORTED_LANGUAGES)(
  "%s presence counts remain client-relative for every plural form",
  (language) => {
    const t = i18n.getFixedT(language);
    for (const count of [0, 1, 2]) {
      expect(t("sidebar.peopleOnLan", { count })).toBe(
        templates[language].replace("{{count}}", String(count)),
      );
    }
  },
);
