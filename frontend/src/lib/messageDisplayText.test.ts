import { afterEach, expect, it } from "vitest";
import i18n, { setLanguage, SUPPORTED_LANGUAGES } from "./i18n";
import { messageDisplayText } from "./messageDisplayText";
afterEach(() => setLanguage("en"));
it.each(SUPPORTED_LANGUAGES)(
  "localizes the masked message marker in %s without recovering message text",
  (language) => {
    setLanguage(language);
    expect(
      messageDisplayText(
        "[Message hidden by your language setting]",
        i18n.t.bind(i18n),
      ),
    ).toBe(i18n.t("dmLanguage.hidden"));
    expect(messageDisplayText("Ordinary message", i18n.t.bind(i18n))).toBe(
      "Ordinary message",
    );
  },
);
