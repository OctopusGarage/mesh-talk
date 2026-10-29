import { Image, Palette, Languages } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Switch } from "@/components/ui/switch";
import { useTheme } from "@/lib/theme";
import { ThemePicker } from "../ThemePicker";
import {
  LANGUAGE_LABELS,
  resolveLanguage,
  setLanguage,
  SUPPORTED_LANGUAGES,
  type Language,
} from "@/lib/i18n";
import { SettingsRow, SELECT_CLASS } from "./primitives";
export function AppearanceSettings() {
  const { t, i18n } = useTranslation();
  const wallpaper = useTheme((s) => s.wallpaperEnabled);
  const setWallpaper = useTheme((s) => s.setWallpaperEnabled);
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
      <div className="pb-4">
        <h3 className="flex items-center gap-2 text-sm font-medium">
          <Palette className="h-4 w-4 text-muted-foreground" />
          {t("settings.theme")}
        </h3>
        <p className="mb-3 mt-1 text-xs text-muted-foreground">
          {t("settings.themeDesc")}
        </p>
        <ThemePicker />
      </div>
      <SettingsRow
        id="setting-wallpaper"
        icon={<Image className="h-4 w-4" />}
        title={t("redesign.wallpaper")}
        desc={t("redesign.wallpaperDesc")}
        control={
          <Switch
            id="setting-wallpaper"
            data-testid="settings-wallpaper"
            checked={wallpaper}
            onCheckedChange={setWallpaper}
          />
        }
      />
      <p className="mt-3 text-xs leading-relaxed text-muted-foreground">
        {t("settings.appearanceSyncHint")}
      </p>
    </>
  );
}
