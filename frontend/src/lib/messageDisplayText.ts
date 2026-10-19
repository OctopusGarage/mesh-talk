import type { TFunction } from "i18next";
/** The core projects hidden content to this marker; original bodies never reach this formatter. */
export function messageDisplayText(text: string, t: TFunction): string {
  return text === "[Message hidden by your language setting]"
    ? t("dmLanguage.hidden")
    : text;
}
