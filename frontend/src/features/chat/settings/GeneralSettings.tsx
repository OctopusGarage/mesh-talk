import { LocalAgentSettings } from "./LocalAgentSettings";
import { ScreenshotShortcutSettings } from "./ScreenshotShortcutSettings";
import { Languages, MinusSquare, Rocket } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Switch } from "@/components/ui/switch";
import {
  LANGUAGE_LABELS,
  resolveLanguage,
  setLanguage,
  SUPPORTED_LANGUAGES,
  type Language,
} from "@/lib/i18n";
import { SettingsRow, SELECT_CLASS } from "./primitives";
import type { SettingsController } from "./useSettingsController";
export function GeneralSettings({
  controller: c,
}: {
  controller: SettingsController;
}) {
  const { t, i18n } = useTranslation();
  const s = c.settings;
  return (
    <>
      <SettingsRow
        id="setting-language"
        icon={<Languages className="h-4 w-4" />}
        title={t("settings.language")}
        desc={t("settings.languageDesc")}
        control={
          <select
            id="setting-language"
            data-testid="settings-language-select"
            value={resolveLanguage(i18n.language) ?? "en"}
            onChange={(e) => setLanguage(e.target.value as Language)}
            className={SELECT_CLASS}
          >
            {SUPPORTED_LANGUAGES.map((language) => (
              <option key={language} value={language} lang={language}>
                {LANGUAGE_LABELS[language]}
              </option>
            ))}
          </select>
        }
      />
      {s && (
        <>
          <SettingsRow
            id="setting-launch-at-login"
            icon={<Rocket className="h-4 w-4" />}
            title={t("settings.launchAtLogin")}
            desc={t("settings.launchAtLoginDesc")}
            control={
              <Switch
                id="setting-launch-at-login"
                checked={c.autostart.on}
                disabled={c.saving || !c.autostart.loaded}
                onCheckedChange={(on) => void c.setAutostartEnabled(on)}
              />
            }
          />
          <SettingsRow
            id="setting-close-to-tray"
            icon={<MinusSquare className="h-4 w-4" />}
            title={t("settings.closeToTray")}
            desc={t("settings.closeToTrayDesc")}
            control={
              <Switch
                id="setting-close-to-tray"
                checked={s.minimize_to_tray}
                disabled={c.saving}
                onCheckedChange={(on) =>
                  void c.update({ minimize_to_tray: on }, "general")
                }
              />
            }
          />
          <LocalAgentSettings controller={c} />
          <ScreenshotShortcutSettings
            key={c.identity ?? "loading"}
            controller={c}
          />
        </>
      )}
    </>
  );
}
