import { MediaDeviceCheck } from "./MediaDeviceCheck";
import { useEffect, useLayoutEffect, useRef } from "react";
import { useNotificationPermission } from "@/lib/notify";
import { NotificationPermissionHelp } from "../NotificationPermissionHelp";
import { Bell, BellRing, FlaskConical, Play } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Switch } from "@/components/ui/switch";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { asRingtoneId, previewRingtone, RINGTONE_IDS } from "@/lib/ringtones";
import { SettingsRow, SettingsSection, SELECT_CLASS } from "./primitives";
import type { SettingsController } from "./useSettingsController";
export function NotificationSettings({
  controller: c,
}: {
  controller: SettingsController;
}) {
  const { t } = useTranslation();
  const s = c.settings;
  const view = useRef({ active: false });
  useLayoutEffect(() => {
    const lifetime = { active: true };
    view.current = lifetime;
    return () => {
      lifetime.active = false;
    };
  }, [c.identity]);
  const requesting = useNotificationPermission((state) => state.requesting);
  useEffect(() => {
    if (s?.notifications) void useNotificationPermission.getState().refresh();
  }, [s?.notifications]);
  if (!s) return null;
  return (
    <>
      <SettingsRow
        id="setting-notifications"
        controlAlignment="start"
        icon={<Bell className="h-4 w-4" />}
        title={t("settings.notifications")}
        desc={
          <div className="space-y-3">
            <p>{t("settings.notificationsDesc")}</p>
            {s.notifications && <NotificationPermissionHelp />}
          </div>
        }
        control={
          <Switch
            id="setting-notifications"
            checked={s.notifications}
            disabled={c.saving || requesting}
            onCheckedChange={async (on) => {
              const lifetime = view.current;
              const current = () => lifetime.active;
              if (await c.update({ notifications: on }, "notifications")) {
                if (on && current())
                  void useNotificationPermission.getState().enable(current);
              }
            }}
          />
        }
      />
      <SettingsRow
        id="setting-notification-preview"
        icon={<Bell className="h-4 w-4" />}
        title={t("settings.notificationPreview")}
        desc={t("settings.notificationPreviewDesc")}
        reason={
          !s.notifications ? t("settings.previewDisabledReason") : undefined
        }
        control={
          <Switch
            id="setting-notification-preview"
            checked={s.notification_preview}
            disabled={c.saving || !s.notifications}
            onCheckedChange={(on) =>
              void c.update(
                { privacy_level: "custom", notification_preview: on },
                "notifications",
              )
            }
          />
        }
      />
      <div className="pt-6">
        <SettingsSection title={t("settings.sectionExperimental")}>
          <SettingsRow
            id="setting-calls"
            icon={<FlaskConical className="h-4 w-4" />}
            title={t("settings.calls")}
            desc={t("settings.callsDesc")}
            control={
              <div className="flex items-center gap-2">
                <Badge className="bg-amber-500/15 text-amber-600 dark:text-amber-400">
                  {t("call.experimental")}
                </Badge>
                <Switch
                  id="setting-calls"
                  checked={s.calls_enabled}
                  disabled={c.saving}
                  onCheckedChange={(on) =>
                    void c.update({ calls_enabled: on }, "notifications")
                  }
                />
              </div>
            }
          />
          {s.calls_enabled && (
            <SettingsRow
              id="setting-ringtone"
              icon={<BellRing className="h-4 w-4" />}
              title={t("settings.ringtone")}
              desc={t("settings.ringtoneDesc")}
              control={
                <div className="flex min-w-0 items-center gap-1.5">
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={t("settings.ringtonePreview")}
                    onClick={() => previewRingtone(asRingtoneId(s.ringtone))}
                  >
                    <Play className="h-3.5 w-3.5" />
                  </Button>
                  <select
                    id="setting-ringtone"
                    data-testid="settings-ringtone-select"
                    aria-label={t("settings.ringtone")}
                    value={asRingtoneId(s.ringtone)}
                    disabled={c.saving}
                    onChange={async (event) => {
                      const id = asRingtoneId(event.target.value);
                      if (await c.update({ ringtone: id }, "notifications"))
                        previewRingtone(id);
                    }}
                    className={SELECT_CLASS}
                  >
                    {RINGTONE_IDS.map((id) => (
                      <option key={id} value={id}>
                        {t(`call.ringtone.${id}`)}
                      </option>
                    ))}
                  </select>
                </div>
              }
            />
          )}
          {s.calls_enabled && (
            <MediaDeviceCheck key={c.identity ?? "loading"} />
          )}
        </SettingsSection>
      </div>
    </>
  );
}
