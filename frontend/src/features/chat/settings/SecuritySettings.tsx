import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { EyeOff, KeyRound, Plug, ShieldCheck } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Switch } from "@/components/ui/switch";
import { Button } from "@/components/ui/button";
import type { AppSettings } from "@/lib/types";
import {
  localAgentAccess,
  type LocalAgentAccess,
} from "@/lib/api/localAgentAccess";
import { errorMessage } from "@/lib/error";
import { HiddenContactsDialog } from "../HiddenContactsDialog";
import { SettingsRow, SettingsSection, SELECT_CLASS } from "./primitives";
import type { SettingsController } from "./useSettingsController";

export function SecuritySettings({
  controller: c,
  onNotifications,
  onDraftChange,
  onAgentSettings,
}: {
  controller: SettingsController;
  onNotifications: () => void;
  onDraftChange: (draft: boolean) => void;
  onAgentSettings: () => void;
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
      <p className="text-xs leading-relaxed text-muted-foreground">
        {t("settings.privacyEffectiveState")}
      </p>
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
              <p className="mt-1">
                {t(
                  s.stay_signed_in
                    ? "settings.savedLoginEnabled"
                    : "settings.savedLoginDisabled",
                )}
              </p>
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
      <AgentPrivacyStatus controller={c} onManage={onAgentSettings} />
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

/** Read the integration owner's actual state; retry never changes its grants. */
function AgentPrivacyStatus({
  controller: c,
  onManage,
}: {
  controller: SettingsController;
  onManage: () => void;
}) {
  const { t } = useTranslation();
  const [attempt, setAttempt] = useState(0);
  const [result, setResult] = useState<{
    identity: string | null;
    owner: string;
    access: LocalAgentAccess | null;
    error: string | null;
  } | null>(null);
  const lifetime = useRef({ active: false });
  useLayoutEffect(() => {
    const current = { active: true };
    lifetime.current = current;
    return () => {
      current.active = false;
    };
  }, [c.identity, c.owner, attempt]);
  useEffect(() => {
    const current = lifetime.current;
    const owner = c.owner;
    const identity = c.identity;
    setResult(null);
    if (!owner) return;
    void localAgentAccess.get(owner).then(
      (access) => {
        if (current.active) setResult({ identity, owner, access, error: null });
      },
      (cause) => {
        if (current.active)
          setResult({
            identity,
            owner,
            access: null,
            error: errorMessage(cause),
          });
      },
    );
  }, [c.identity, c.owner, attempt]);
  const current =
    result?.identity === c.identity && result.owner === c.owner ? result : null;
  const access = current?.access;
  const [clock, setClock] = useState(Date.now);
  const expired =
    !!access?.enabled &&
    (access.expired ||
      (access.expires_at_ms !== null && access.expires_at_ms <= clock));
  useEffect(() => {
    if (!access?.enabled || access.expired || access.expires_at_ms === null)
      return;
    const remaining = access.expires_at_ms - Date.now();
    if (remaining <= 0) {
      setClock(Date.now());
      return;
    }
    const timer = window.setTimeout(
      () => {
        setClock(Date.now());
        setAttempt((value) => value + 1);
      },
      Math.min(remaining, 2_147_483_647),
    );
    return () => window.clearTimeout(timer);
  }, [
    access?.enabled,
    access?.expired,
    access?.expires_at_ms,
    c.identity,
    c.owner,
  ]);
  const error = current?.error ?? access?.error;
  const loading = !!c.owner && !current;
  return (
    <section data-testid="privacy-agent-status" className="border-t pt-2">
      <SettingsRow
        icon={<Plug className="h-4 w-4" />}
        title={t("agentAccess.title")}
        desc={t("agentAccess.privacyStatusDescription")}
        control={
          <Button
            size="sm"
            variant="outline"
            disabled={!c.owner}
            onClick={onManage}
            data-testid="manage-privacy-agent-access"
          >
            {t("agentAccess.manageAccess")}
          </Button>
        }
      />
      <div className="space-y-2 pb-3 text-xs text-muted-foreground">
        <p role="status">
          {loading
            ? t("common.loading")
            : access
              ? t(
                  expired
                    ? "agentAccess.expired"
                    : access.running
                      ? "agentAccess.running"
                      : access.enabled
                        ? "agentAccess.unavailable"
                        : "agentAccess.off",
                )
              : t("agentAccess.statusFailed")}
        </p>
        {access && (
          <p>
            {t("agentAccess.grantedConversations", {
              count: access.destinations.length,
            })}
          </p>
        )}
        {error && (
          <p role="alert" className="break-words text-destructive">
            {error}
          </p>
        )}
        {(error || (access?.enabled && !access.running && !expired)) && (
          <Button
            size="sm"
            variant="outline"
            disabled={loading}
            data-testid="retry-privacy-agent-status"
            onClick={() => setAttempt((value) => value + 1)}
          >
            {t("contactVisibility.retry")}
          </Button>
        )}
      </div>
    </section>
  );
}
