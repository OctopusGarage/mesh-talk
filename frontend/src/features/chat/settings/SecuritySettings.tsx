import { useEffect, useState } from "react";
import { EyeOff, KeyRound, ShieldCheck } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Switch } from "@/components/ui/switch";
import { Button } from "@/components/ui/button";
import type { AppSettings } from "@/lib/types";
import { HiddenContactsDialog } from "../HiddenContactsDialog";
import { SettingsRow, SettingsSection, SELECT_CLASS } from "./primitives";
import type { SettingsController } from "./useSettingsController";

export function SecuritySettings({
  controller: c,
  onNotifications,
  onDraftChange,
}: {
  controller: SettingsController;
  onNotifications: () => void;
  onDraftChange: (draft: boolean) => void;
}) {
  const { t } = useTranslation();
  const s = c.settings;
  const [pendingPreset, setPreset] = useState<
    AppSettings["privacy_level"] | null
  >(null);
  const preset = pendingPreset ?? s?.privacy_level ?? "balanced";
  const draft = !!s && preset !== "custom" && preset !== s.privacy_level;
  useEffect(() => {
    setPreset(null);
  }, [s?.privacy_level]);
  useEffect(() => {
    onDraftChange(draft);
  }, [draft, onDraftChange]);
  if (!s) return null;
  const icon = <ShieldCheck className="h-4 w-4" />;
  return (
    <>
      <SettingsRow
        id="setting-privacy-level"
        icon={icon}
        title={t("settings.privacyLevel")}
        desc={t("settings.privacyLevelDesc")}
        control={
          <select
            id="setting-privacy-level"
            value={preset}
            disabled={c.saving}
            onChange={(e) =>
              setPreset(e.target.value as AppSettings["privacy_level"])
            }
            className={SELECT_CLASS}
          >
            {s.privacy_level === "custom" && (
              <option value="custom" disabled>
                {t("settings.devicePresetCustom")}
              </option>
            )}
            {(["balanced", "private", "strict"] as const).map((level) => (
              <option key={level} value={level}>
                {t(`settings.level_${level}`)}
              </option>
            ))}
          </select>
        }
      />
      <div className="space-y-2 pb-5 pt-2">
        <p className="text-xs leading-relaxed text-muted-foreground">
          {t("settings.presetEffects")}
        </p>
        <p className="text-xs text-muted-foreground">
          {t("settings.presetPending")}
        </p>
        <Button
          size="sm"
          variant="secondary"
          disabled={
            c.saving || preset === "custom" || preset === s.privacy_level
          }
          onClick={() => void c.update({ privacy_level: preset }, "privacy")}
        >
          {t("settings.applyPreset")}
        </Button>
      </div>
      <SettingsSection>
        <SettingsRow
          title={t("settings.notificationPreview")}
          desc={t("settings.notificationPreviewDesc")}
          control={
            <div className="flex flex-col items-start gap-1">
              <span className="text-xs font-medium">
                {t(
                  s.notification_preview
                    ? "settings.protectionOn"
                    : "settings.protectionOff",
                )}
              </span>
              <button
                type="button"
                onClick={onNotifications}
                className="max-w-48 text-left text-xs text-signal underline underline-offset-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              >
                {t("settings.previewSettingsLink")}
              </button>
            </div>
          }
        />
        <SettingsRow
          id="setting-auto-lock"
          icon={<KeyRound className="h-4 w-4" />}
          title={t("settings.autoLock")}
          desc={t("settings.autoLockDesc")}
          control={
            <select
              id="setting-auto-lock"
              value={s.auto_lock_seconds}
              disabled={c.saving}
              className={SELECT_CLASS}
              onChange={(e) =>
                void c.update(
                  {
                    privacy_level: "custom",
                    auto_lock_seconds: Number(e.target.value),
                  },
                  "privacy",
                )
              }
            >
              <option value={0}>{t("settings.lockOff")}</option>
              <option value={60}>{t("settings.lockOneMinute")}</option>
              <option value={600}>{t("settings.lockTenMinutes")}</option>
            </select>
          }
        />
        <SettingsRow
          id="setting-lock-when-hidden"
          icon={<KeyRound className="h-4 w-4" />}
          title={t("settings.lockWhenHidden")}
          desc={t("settings.lockWhenHiddenDesc")}
          control={
            <Switch
              id="setting-lock-when-hidden"
              checked={s.lock_when_hidden}
              disabled={c.saving}
              onCheckedChange={(on) =>
                void c.update(
                  { privacy_level: "custom", lock_when_hidden: on },
                  "privacy",
                )
              }
            />
          }
        />
        <SettingsRow
          id="setting-confirm-copy"
          icon={icon}
          title={t("settings.confirmCopy")}
          desc={t("settings.confirmCopyDesc")}
          control={
            <Switch
              id="setting-confirm-copy"
              checked={s.confirm_sensitive_copy}
              disabled={c.saving}
              onCheckedChange={(on) =>
                void c.update(
                  { privacy_level: "custom", confirm_sensitive_copy: on },
                  "privacy",
                )
              }
            />
          }
        />
        <SettingsRow
          id="setting-clipboard-clear"
          icon={icon}
          title={t("settings.clipboardClear")}
          desc={t("settings.clipboardClearDesc")}
          control={
            <select
              id="setting-clipboard-clear"
              value={s.clipboard_clear_seconds}
              disabled={c.saving}
              className={SELECT_CLASS}
              onChange={(e) =>
                void c.update(
                  {
                    privacy_level: "custom",
                    clipboard_clear_seconds: Number(e.target.value),
                  },
                  "privacy",
                )
              }
            >
              <option value={0}>{t("settings.clearOff")}</option>
              <option value={30}>{t("settings.clearThirtySeconds")}</option>
              <option value={60}>{t("settings.clearOneMinute")}</option>
            </select>
          }
        />
        <SettingsRow
          id="setting-stay-signed-in"
          icon={<KeyRound className="h-4 w-4" />}
          title={t("settings.staySignedIn")}
          desc={
            <>
              {t("settings.staySignedInDesc")}
              <p className="mt-1">{t("settings.scopeComputer")}</p>
            </>
          }
          control={
            <Switch
              id="setting-stay-signed-in"
              checked={s.stay_signed_in}
              disabled={c.saving}
              onCheckedChange={(on) =>
                void c.update({ stay_signed_in: on }, "privacy")
              }
            />
          }
        />
      </SettingsSection>
      <div className="pt-5">
        <SettingsSection title={t("contactVisibility.section")}>
          <SettingsRow
            icon={<EyeOff className="h-4 w-4" />}
            title={t("contactVisibility.title")}
            desc={t("contactVisibility.settingsDesc")}
            control={<HiddenContactsDialog />}
          />
        </SettingsSection>
      </div>
      <div className="space-y-2 pt-4 text-xs leading-relaxed text-muted-foreground">
        <p>{t("settings.managedDeviceLimit")}</p>
        <p>{t("settings.exportLimit")}</p>
      </div>
    </>
  );
}
