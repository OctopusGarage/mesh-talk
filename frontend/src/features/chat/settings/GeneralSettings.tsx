import { MinusSquare, Rocket } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Switch } from "@/components/ui/switch";
import { SettingsRow } from "./primitives";
import type { SettingsController } from "./useSettingsController";
export function GeneralSettings({
  controller: c,
}: {
  controller: SettingsController;
}) {
  const { t } = useTranslation();
  const s = c.settings;
  return (
    <>
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
        </>
      )}
    </>
  );
}
