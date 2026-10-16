import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  Bell,
  BellRing,
  ChevronDown,
  FlaskConical,
  FolderOpen,
  History,
  Image,
  EyeOff,
  KeyRound,
  Languages,
  MinusSquare,
  Palette,
  Play,
  Rocket,
  Settings,
  ShieldCheck,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Badge } from "@/components/ui/badge";
import { settings as settingsApi } from "@/lib/api";
import { privacyFromSettings } from "@/lib/privacySettings";
import { errorMessage } from "@/lib/error";
import {
  selectablePostOffices,
  savedIndirectPostOffice,
} from "@/lib/postOfficeOptions";
import { balancedPrivacy, useSettings } from "@/store/settings";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import type { AppSettings } from "@/lib/types";
import { useTheme } from "@/lib/theme";
import {
  DEFAULT_RINGTONE,
  RINGTONE_IDS,
  asRingtoneId,
  previewRingtone,
  type RingtoneId,
} from "@/lib/ringtones";
import {
  resolveLanguage,
  setLanguage,
  SUPPORTED_LANGUAGES,
  type Language,
} from "@/lib/i18n";
import { ThemePicker } from "./ThemePicker";
import { HiddenContactsDialog } from "./HiddenContactsDialog";
import { DataRecoverySettings } from "./DataRecoverySettings";
import { PrivacySettings } from "./PrivacySettings";
import { AttachmentReceiveSettings } from "./AttachmentReceiveSettings";
import {
  scrollToSettingsSection,
  settingsCategory,
  settingsScrollBehavior,
} from "./settingsScroll";
import { createSettingsDialogSession } from "./settingsDialogSession";

/** A section group: a small display-font label over a stack of rows. */
function Section({
  id,
  title,
  children,
}: {
  id?: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section id={id} className="scroll-mt-2 space-y-2">
      <h3 className="text-[12px] font-semibold text-muted-foreground">
        {title}
      </h3>
      <div className="grid gap-2">{children}</div>
    </section>
  );
}

/** A row scaffold: icon + label/help on the left, a control on the right. */
function Row({
  id,
  icon,
  title,
  desc,
  control,
}: {
  id?: string;
  icon: React.ReactNode;
  title: string;
  desc: string;
  control: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 border-b px-0.5 py-2.5 last:border-b-0">
      <div className="flex min-w-0 items-start gap-3">
        <div className="mt-0.5 text-muted-foreground">{icon}</div>
        <div className="min-w-0">
          <label htmlFor={id} className="block text-[13px] font-medium">
            {title}
          </label>
          <p className="text-[12px] leading-[1.45] text-muted-foreground">
            {desc}
          </p>
        </div>
      </div>
      <div className="shrink-0">{control}</div>
    </div>
  );
}

const SELECT_CLASS =
  "h-9 rounded-md border border-input bg-background px-2 text-[13px] transition-[border-color,box-shadow] duration-150 hover:border-ring/40 focus-visible:border-ring focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring/25 [@media(hover:none)]:min-h-11";

const LANGUAGE_LABELS: Record<Language, string> = {
  en: "English",
  es: "Español",
  ja: "日本語",
  "zh-Hans": "简体中文",
  "zh-Hant": "繁體中文",
  yue: "粵語",
};

export function SettingsDialog({
  open: controlledOpen,
  onOpenChange,
}: {
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
} = {}) {
  const { t, i18n } = useTranslation();
  const translate = useRef(t);
  useEffect(() => {
    translate.current = t;
  }, [t]);
  const wallpaperEnabled = useTheme((s) => s.wallpaperEnabled);
  const setWallpaperEnabled = useTheme((s) => s.setWallpaperEnabled);
  const owner = useAuth((s) => s.user?.id ?? null);
  const peers = useChat((s) => s.peers);
  const [internalOpen, setInternalOpen] = useState(false);
  const open = controlledOpen ?? internalOpen;
  const setOpen = onOpenChange ?? setInternalOpen;
  // Selectors return primitives only (stable refs) — a fresh object once black-screened the app.
  // Primitive state only (memory: zustand selectors returning fresh objects each
  // render once black-screened the app — kept as local primitives here regardless).
  const [minimizeToTray, setMinimizeToTray] = useState(true);
  const [notifications, setNotifications] = useState(true);
  const [launchAtLogin, setLaunchAtLogin] = useState(false);
  const [autostartBusy, setAutostartBusy] = useState(false);
  const [downloadDir, setDownloadDir] = useState("");
  const [staySignedIn, setStaySignedIn] = useState(true);
  const [retentionDays, setRetentionDays] = useState(0);
  const [callsEnabled, setCallsEnabled] = useState(false);
  const [postOfficeEnabled, setPostOfficeEnabled] = useState(true);
  const [postOfficeSendEnabled, setPostOfficeSendEnabled] = useState(true);
  const [postOfficeReceiveEnabled, setPostOfficeReceiveEnabled] =
    useState(true);
  const [physicalLanOnly, setPhysicalLanOnly] = useState(false);
  const [bridgeDiscoveryEnabled, setBridgeDiscoveryEnabled] = useState(false);
  const [bridgeHubId, setBridgeHubId] = useState("");
  const selectableRelays = selectablePostOffices(peers);
  const savedIndirectRelay = savedIndirectPostOffice(peers, bridgeHubId);
  const [ringtone, setRingtone] = useState<RingtoneId>(DEFAULT_RINGTONE);
  const [privacy, setPrivacy] = useState(balancedPrivacy);
  const settingsScroll = useRef<HTMLDivElement>(null);
  const [moreBelow, setMoreBelow] = useState(true);
  const [activeSection, setActiveSection] = useState("privacy");
  useLayoutEffect(() => {
    if (open) {
      setMoreBelow(true);
      setActiveSection("privacy");
    }
  }, [open]);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [loadedOwner, setLoadedOwner] = useState<string | null>(null);
  const [autostartLoaded, setAutostartLoaded] = useState(false);
  const [saving, setSaving] = useState(false);
  const session = useRef<ReturnType<typeof createSettingsDialogSession> | null>(
    null,
  );
  if (!session.current)
    session.current = createSettingsDialogSession(setSaving);
  useLayoutEffect(() => {
    session.current!.setOwner(owner);
  }, [owner]);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [settingsErrorKind, setSettingsErrorKind] = useState<"load" | "save">(
    "load",
  );

  const loadCurrentSettings = useCallback(() => {
    const request = session.current!.beginLoad();
    setSettingsLoaded(false);
    setAutostartLoaded(false);
    setSettingsError(null);
    void session.current!.load(
      request,
      () => settingsApi.get(),
      (s) => {
        setMinimizeToTray(s.minimize_to_tray);
        setNotifications(s.notifications);
        setDownloadDir(s.download_dir);
        setStaySignedIn(s.stay_signed_in);
        setRetentionDays(s.retention_days);
        setCallsEnabled(s.calls_enabled);
        setPostOfficeEnabled(s.post_office_enabled);
        setPostOfficeSendEnabled(s.post_office_send_enabled ?? true);
        setPostOfficeReceiveEnabled(s.post_office_receive_enabled ?? true);
        setPhysicalLanOnly(s.physical_lan_only ?? false);
        setBridgeDiscoveryEnabled(s.bridge_discovery_enabled ?? false);
        setBridgeHubId(s.bridge_hub_id ?? "");
        setRingtone(asRingtoneId(s.ringtone));
        const policy = privacyFromSettings(s);
        setPrivacy(policy);
        useSettings.getState().setPrivacy(policy);
        setLoadedOwner(owner);
        setSettingsLoaded(true);
      },
      (e) => {
        setSettingsErrorKind("load");
        setSettingsError(
          translate.current("settings.loadFailed", {
            error: errorMessage(e),
          }),
        );
      },
    );
    void session.current!.load(
      request,
      () => settingsApi.autostartEnabled(),
      (on) => {
        setLaunchAtLogin(on);
        setAutostartLoaded(true);
      },
      (e) => {
        setSettingsErrorKind("load");
        setSettingsError(
          translate.current("settings.loadFailed", {
            error: errorMessage(e),
          }),
        );
      },
    );
  }, [owner]);

  // Load current state whenever the dialog opens.
  useEffect(() => {
    if (open) loadCurrentSettings();
    const current = session.current!;
    return () => {
      current.invalidateView();
    };
  }, [open, owner, loadCurrentSettings]);

  const saveSetting = <T,>(
    write: () => Promise<T>,
    apply: (value: T) => void,
  ) => {
    if (!settingsLoaded || loadedOwner !== owner) return Promise.resolve();
    setSettingsError(null);
    return session.current!.save(write, apply, (error) => {
      setSettingsErrorKind("save");
      setSettingsError(
        t("settings.saveFailed", { error: errorMessage(error) }),
      );
    });
  };

  const onMinimize = (v: boolean) => {
    void saveSetting(
      () => settingsApi.update({ minimize_to_tray: v }),
      () => setMinimizeToTray(v),
    );
  };
  const onNotifications = (v: boolean) => {
    void saveSetting(
      () => settingsApi.update({ notifications: v }),
      () => setNotifications(v),
    );
  };
  const onPostOffice = (enabled: boolean) => {
    void saveSetting(
      () => settingsApi.update({ post_office_enabled: enabled }),
      () => setPostOfficeEnabled(enabled),
    );
  };
  const onBridgeDiscovery = (enabled: boolean) => {
    void saveSetting(
      () => settingsApi.update({ bridge_discovery_enabled: enabled }),
      () => setBridgeDiscoveryEnabled(enabled),
    );
  };
  const saveBridgeHub = (id: string) => {
    void saveSetting(
      () => settingsApi.update({ bridge_hub_id: id }),
      () => setBridgeHubId(id),
    );
  };
  const savePrivacy = (patch: Partial<AppSettings>) => {
    void saveSetting(
      async () => {
        await settingsApi.update(patch);
        return privacyFromSettings(await settingsApi.get());
      },
      (saved) => {
        setPrivacy(saved);
        useSettings.getState().setPrivacy(saved);
      },
    );
  };
  // Toggling "stay signed in" off makes the backend immediately forget the saved
  // keychain secret (see set_app_settings), so the next launch shows the login screen.
  const onStaySignedIn = (v: boolean) => {
    void saveSetting(
      () => settingsApi.update({ stay_signed_in: v }),
      () => setStaySignedIn(v),
    );
  };
  // Persisting retention triggers an immediate backend prune of older messages (see
  // set_app_settings), so a tightened window takes effect at once.
  const onRetention = (days: number) => {
    void saveSetting(
      () => settingsApi.update({ retention_days: days }),
      () => setRetentionDays(days),
    );
  };
  // Reflect the saved value in the reactive store only after the write succeeds.
  const onCalls = (v: boolean) => {
    void saveSetting(
      () => settingsApi.update({ calls_enabled: v }),
      () => {
        setCallsEnabled(v);
        useSettings.getState().setCallsEnabled(v);
      },
    );
  };
  // The preview follows a successful save so UI and playback agree on the selected choice.
  const onRingtone = (id: RingtoneId) => {
    void saveSetting(
      () => settingsApi.update({ ringtone: id }),
      () => {
        setRingtone(id);
        useSettings.getState().setRingtone(id);
        previewRingtone(id);
      },
    );
  };
  const onLaunch = (v: boolean) => {
    if (!autostartLoaded) return;
    setAutostartBusy(true);
    void saveSetting(
      async () => {
        await settingsApi.setAutostart(v);
        // The OS launch agent is the source of truth.
        return settingsApi.autostartEnabled();
      },
      (enabled) => setLaunchAtLogin(enabled),
    ).finally(() => setAutostartBusy(false));
  };

  const chooseDir = async () => {
    const request = session.current!.request();
    try {
      const dir = await openDialog({
        directory: true,
        defaultPath: downloadDir || undefined,
      });
      if (!session.current!.isCurrent(request)) return;
      if (typeof dir === "string") {
        await saveSetting(
          () => settingsApi.update({ download_dir: dir }),
          () => setDownloadDir(dir),
        );
      }
    } catch (e) {
      if (session.current!.isCurrent(request)) {
        setSettingsErrorKind("save");
        setSettingsError(t("settings.saveFailed", { error: errorMessage(e) }));
      }
    }
  };

  // The active language, mapped onto a supported code (i18n.language may be a
  // region tag like "en-US"; resolveLanguage also handles the zh-* variants).
  const currentLang = resolveLanguage(i18n.language) ?? "en";

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
      }}
    >
      {controlledOpen === undefined && (
        <DialogTrigger asChild>
          <Button
            variant="ghost"
            data-testid="sidebar-nav-settings"
            title={t("settings.title")}
            aria-label={t("settings.title")}
            className="h-10 w-full justify-start gap-3 px-3 text-muted-foreground hover:text-foreground"
          >
            <Settings className="h-4 w-4" />
            <span>{t("settings.title")}</span>
          </Button>
        </DialogTrigger>
      )}
      <DialogContent
        className="max-w-[min(52rem,calc(100vw-2rem))] overflow-hidden"
        data-testid="settings-dialog"
      >
        <DialogHeader>
          <DialogTitle>{t("settings.title")}</DialogTitle>
          <DialogDescription>{t("settings.description")}</DialogDescription>
        </DialogHeader>
        {!settingsLoaded && !settingsError && (
          <p role="status" className="text-xs text-muted-foreground">
            {t("common.loading")}
          </p>
        )}
        {settingsError && (
          <div
            role="alert"
            data-testid="settings-error"
            className="flex items-start justify-between gap-3 rounded-md border border-destructive/35 bg-destructive/5 px-3 py-2 text-sm text-destructive"
          >
            <span>{settingsError}</span>
            <button
              type="button"
              onClick={
                settingsErrorKind === "load"
                  ? loadCurrentSettings
                  : () => setSettingsError(null)
              }
              className="shrink-0 font-medium underline underline-offset-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              {t(
                settingsErrorKind === "load"
                  ? "contactVisibility.retry"
                  : "common.dismiss",
              )}
            </button>
          </div>
        )}

        <div className="flex min-h-0 gap-4">
          <nav
            aria-label={t("redesign.settingsNav")}
            className="flex w-36 shrink-0 flex-col gap-0.5 border-r pr-3 max-[700px]:w-28"
          >
            {[
              ["privacy", t("privacy.section")],
              ["appearance", t("settings.sectionAppearance")],
              ["background", t("redesign.appNotifications")],
              ["files", t("redesign.messagesFiles")],
              ["recovery", t("recovery.section")],
              ["experimental", t("redesign.advanced")],
            ].map(([id, label]) => (
              <button
                key={id}
                type="button"
                aria-current={activeSection === id ? "location" : undefined}
                onClick={() => {
                  setActiveSection(id);
                  scrollToSettingsSection(settingsScroll.current, id);
                }}
                className={`rounded-md px-2.5 py-2 text-left text-[13px] font-medium hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${activeSection === id ? "bg-accent text-foreground" : "text-muted-foreground"}`}
              >
                {label}
              </button>
            ))}
          </nav>

          <div
            ref={settingsScroll}
            onScroll={() => {
              const element = settingsScroll.current;
              if (element) {
                setMoreBelow(
                  element.scrollTop + element.clientHeight <
                    element.scrollHeight - 4,
                );
                const top = element.getBoundingClientRect().top + 24;
                const sections = element.querySelectorAll<HTMLElement>(
                  ":scope > [id^='settings-']",
                );
                for (const section of sections) {
                  if (section.getBoundingClientRect().top <= top)
                    setActiveSection(
                      settingsCategory(section.id.replace("settings-", "")),
                    );
                }
              }
            }}
            className="grid h-[min(66vh,calc(100vh-11rem))] min-h-0 min-w-0 flex-1 content-start gap-7 overflow-y-auto pr-2"
          >
            <div id="settings-privacy" className="scroll-mt-2">
              <PrivacySettings />
            </div>
            <Section
              id="settings-security"
              title={t("settings.securitySection")}
            >
              <Row
                id="setting-post-office"
                icon={<ShieldCheck className="h-4 w-4" />}
                title={t("settings.postOffice")}
                desc={t("settings.postOfficeDesc")}
                control={
                  <Switch
                    id="setting-post-office"
                    checked={postOfficeEnabled}
                    onCheckedChange={onPostOffice}
                    disabled={!settingsLoaded || saving}
                  />
                }
              />
              {(
                [
                  {
                    id: "post-office-send",
                    key: "postOfficeSend",
                    value: postOfficeSendEnabled,
                    field: "post_office_send_enabled",
                    update: setPostOfficeSendEnabled,
                    requiresRelay: true,
                  },
                  {
                    id: "post-office-receive",
                    key: "postOfficeReceive",
                    value: postOfficeReceiveEnabled,
                    field: "post_office_receive_enabled",
                    update: setPostOfficeReceiveEnabled,
                    requiresRelay: true,
                  },
                  {
                    id: "physical-lan-only",
                    key: "physicalLanOnly",
                    value: physicalLanOnly,
                    field: "physical_lan_only",
                    update: setPhysicalLanOnly,
                    requiresRelay: false,
                  },
                ] as const
              ).map((option) => (
                <Row
                  key={option.id}
                  id={`setting-${option.id}`}
                  icon={<ShieldCheck className="h-4 w-4" />}
                  title={t(`settings.${option.key}`)}
                  desc={t(`settings.${option.key}Desc`)}
                  control={
                    <Switch
                      id={`setting-${option.id}`}
                      checked={option.value}
                      disabled={
                        !settingsLoaded ||
                        saving ||
                        (option.requiresRelay && !postOfficeEnabled)
                      }
                      onCheckedChange={(enabled) => {
                        void saveSetting(
                          () => settingsApi.update({ [option.field]: enabled }),
                          () => option.update(enabled),
                        );
                      }}
                    />
                  }
                />
              ))}
              <Row
                id="setting-bridge-discovery"
                icon={<ShieldCheck className="h-4 w-4" />}
                title={t("settings.bridgeDiscovery")}
                desc={t("settings.bridgeDiscoveryDesc")}
                control={
                  <Switch
                    id="setting-bridge-discovery"
                    checked={bridgeDiscoveryEnabled}
                    onCheckedChange={onBridgeDiscovery}
                    disabled={
                      !settingsLoaded ||
                      saving ||
                      !postOfficeEnabled ||
                      physicalLanOnly
                    }
                  />
                }
              />
              <Row
                id="setting-bridge-hub"
                icon={<ShieldCheck className="h-4 w-4" />}
                title={t("settings.bridgeHub")}
                desc={t("settings.bridgeHubDesc")}
                control={
                  <select
                    id="setting-bridge-hub"
                    value={bridgeHubId}
                    onChange={(event) => saveBridgeHub(event.target.value)}
                    aria-label={t("settings.bridgeHub")}
                    disabled={!settingsLoaded || saving || !postOfficeEnabled}
                    className={`${SELECT_CLASS} w-56 max-w-full`}
                  >
                    <option value="">{t("settings.bridgeHubAutomatic")}</option>
                    {bridgeHubId &&
                      !selectableRelays.some(
                        (peer) => peer.user_id === bridgeHubId,
                      ) && (
                        <option value={bridgeHubId} disabled>
                          {savedIndirectRelay
                            ? t("settings.bridgeHubRemote", {
                                name: savedIndirectRelay.name,
                              })
                            : t("settings.bridgeHubUnavailable", {
                                id: bridgeHubId.slice(0, 8),
                              })}
                        </option>
                      )}
                    {selectableRelays.map((peer) => (
                      <option key={peer.user_id} value={peer.user_id}>
                        {peer.name} · {peer.user_id.slice(0, 8)}
                      </option>
                    ))}
                  </select>
                }
              />
              <Row
                id="setting-privacy-level"
                icon={<ShieldCheck className="h-4 w-4" />}
                title={t("settings.privacyLevel")}
                desc={t("settings.privacyLevelDesc")}
                control={
                  <select
                    id="setting-privacy-level"
                    value={privacy.privacy_level}
                    onChange={(event) =>
                      savePrivacy({
                        privacy_level: event.target
                          .value as AppSettings["privacy_level"],
                      })
                    }
                    disabled={!settingsLoaded || saving}
                    className={SELECT_CLASS}
                  >
                    {(["balanced", "private", "strict", "custom"] as const).map(
                      (level) => (
                        <option key={level} value={level}>
                          {t(`settings.level_${level}`)}
                        </option>
                      ),
                    )}
                  </select>
                }
              />
              <Row
                id="setting-notification-preview"
                icon={<Bell className="h-4 w-4" />}
                title={t("settings.notificationPreview")}
                desc={t("settings.notificationPreviewDesc")}
                control={
                  <Switch
                    id="setting-notification-preview"
                    checked={privacy.notification_preview}
                    onCheckedChange={(notification_preview) =>
                      savePrivacy({
                        privacy_level: "custom",
                        notification_preview,
                      })
                    }
                    disabled={!settingsLoaded || saving}
                  />
                }
              />
              <Row
                id="setting-auto-lock"
                icon={<KeyRound className="h-4 w-4" />}
                title={t("settings.autoLock")}
                desc={t("settings.autoLockDesc")}
                control={
                  <select
                    id="setting-auto-lock"
                    value={privacy.auto_lock_seconds}
                    onChange={(event) =>
                      savePrivacy({
                        privacy_level: "custom",
                        auto_lock_seconds: Number(event.target.value),
                      })
                    }
                    disabled={!settingsLoaded || saving}
                    className={SELECT_CLASS}
                  >
                    <option value={0}>{t("settings.lockOff")}</option>
                    <option value={60}>{t("settings.lockOneMinute")}</option>
                    <option value={600}>{t("settings.lockTenMinutes")}</option>
                  </select>
                }
              />
              <Row
                id="setting-lock-when-hidden"
                icon={<KeyRound className="h-4 w-4" />}
                title={t("settings.lockWhenHidden")}
                desc={t("settings.lockWhenHiddenDesc")}
                control={
                  <Switch
                    id="setting-lock-when-hidden"
                    checked={privacy.lock_when_hidden}
                    onCheckedChange={(lock_when_hidden) =>
                      savePrivacy({ privacy_level: "custom", lock_when_hidden })
                    }
                    disabled={!settingsLoaded || saving}
                  />
                }
              />
              <Row
                id="setting-confirm-copy"
                icon={<ShieldCheck className="h-4 w-4" />}
                title={t("settings.confirmCopy")}
                desc={t("settings.confirmCopyDesc")}
                control={
                  <Switch
                    id="setting-confirm-copy"
                    checked={privacy.confirm_sensitive_copy}
                    onCheckedChange={(confirm_sensitive_copy) =>
                      savePrivacy({
                        privacy_level: "custom",
                        confirm_sensitive_copy,
                      })
                    }
                    disabled={!settingsLoaded || saving}
                  />
                }
              />
              <Row
                id="setting-clipboard-clear"
                icon={<ShieldCheck className="h-4 w-4" />}
                title={t("settings.clipboardClear")}
                desc={t("settings.clipboardClearDesc")}
                control={
                  <select
                    id="setting-clipboard-clear"
                    value={privacy.clipboard_clear_seconds}
                    onChange={(event) =>
                      savePrivacy({
                        privacy_level: "custom",
                        clipboard_clear_seconds: Number(event.target.value),
                      })
                    }
                    disabled={!settingsLoaded || saving}
                    className={SELECT_CLASS}
                  >
                    <option value={0}>{t("settings.clearOff")}</option>
                    <option value={30}>
                      {t("settings.clearThirtySeconds")}
                    </option>
                    <option value={60}>{t("settings.clearOneMinute")}</option>
                  </select>
                }
              />
              <p className="text-xs leading-relaxed text-muted-foreground">
                {t("settings.managedDeviceLimit")}
              </p>
              <p className="text-xs leading-relaxed text-muted-foreground">
                {t("settings.exportLimit")}
              </p>
            </Section>
            <Section
              id="settings-contacts"
              title={t("contactVisibility.section")}
            >
              <Row
                icon={<EyeOff className="h-4 w-4" />}
                title={t("contactVisibility.title")}
                desc={t("contactVisibility.settingsDesc")}
                control={<HiddenContactsDialog />}
              />
            </Section>
            <Section
              id="settings-appearance"
              title={t("settings.sectionAppearance")}
            >
              <div>
                <div className="flex items-center gap-2 text-sm font-medium">
                  <Palette className="h-4 w-4 text-muted-foreground" />
                  {t("settings.theme")}
                </div>
                <p className="mb-3 mt-0.5 text-xs text-muted-foreground">
                  {t("settings.themeDesc")}
                </p>
                <ThemePicker />
              </div>
              <Row
                id="setting-wallpaper"
                icon={<Image className="h-4 w-4" />}
                title={t("redesign.wallpaper")}
                desc={t("redesign.wallpaperDesc")}
                control={
                  <Switch
                    id="setting-wallpaper"
                    data-testid="settings-wallpaper"
                    checked={wallpaperEnabled}
                    onCheckedChange={setWallpaperEnabled}
                    aria-label={t("redesign.wallpaper")}
                  />
                }
              />
              <Row
                id="setting-language"
                icon={<Languages className="h-4 w-4" />}
                title={t("settings.language")}
                desc={t("settings.languageDesc")}
                control={
                  <select
                    id="setting-language"
                    data-testid="settings-language-select"
                    value={currentLang}
                    onChange={(e) => setLanguage(e.target.value as Language)}
                    className={SELECT_CLASS}
                    aria-label={t("settings.language")}
                  >
                    {SUPPORTED_LANGUAGES.map((lng) => (
                      <option key={lng} value={lng}>
                        {LANGUAGE_LABELS[lng]}
                      </option>
                    ))}
                  </select>
                }
              />
            </Section>

            <Section
              id="settings-background"
              title={t("settings.sectionBackground")}
            >
              <Row
                id="setting-launch-at-login"
                icon={<Rocket className="h-4 w-4" />}
                title={t("settings.launchAtLogin")}
                desc={t("settings.launchAtLoginDesc")}
                control={
                  <Switch
                    id="setting-launch-at-login"
                    checked={launchAtLogin}
                    onCheckedChange={(v) => void onLaunch(v)}
                    disabled={
                      !settingsLoaded ||
                      !autostartLoaded ||
                      saving ||
                      autostartBusy
                    }
                    aria-label={t("settings.launchAtLogin")}
                  />
                }
              />
              <Row
                id="setting-close-to-tray"
                icon={<MinusSquare className="h-4 w-4" />}
                title={t("settings.closeToTray")}
                desc={t("settings.closeToTrayDesc")}
                control={
                  <Switch
                    id="setting-close-to-tray"
                    checked={minimizeToTray}
                    onCheckedChange={onMinimize}
                    disabled={!settingsLoaded || saving}
                    aria-label={t("settings.closeToTray")}
                  />
                }
              />
              <Row
                id="setting-notifications"
                icon={<Bell className="h-4 w-4" />}
                title={t("settings.notifications")}
                desc={t("settings.notificationsDesc")}
                control={
                  <Switch
                    id="setting-notifications"
                    checked={notifications}
                    onCheckedChange={onNotifications}
                    disabled={!settingsLoaded || saving}
                    aria-label={t("settings.notifications")}
                  />
                }
              />
              <Row
                id="setting-stay-signed-in"
                icon={<KeyRound className="h-4 w-4" />}
                title={t("settings.staySignedIn")}
                desc={t("settings.staySignedInDesc")}
                control={
                  <Switch
                    id="setting-stay-signed-in"
                    checked={staySignedIn}
                    onCheckedChange={onStaySignedIn}
                    disabled={!settingsLoaded || saving}
                    aria-label={t("settings.staySignedIn")}
                  />
                }
              />
            </Section>

            <Section id="settings-files" title={t("redesign.messagesFiles")}>
              <AttachmentReceiveSettings enabled={open} />
              <Row
                icon={<FolderOpen className="h-4 w-4" />}
                title={t("settings.downloadFolder")}
                desc={
                  downloadDir
                    ? t("settings.downloadFolderSet")
                    : t("settings.downloadFolderDesc")
                }
                control={
                  <div className="flex max-w-[11rem] flex-col items-end gap-1">
                    {downloadDir && (
                      <span
                        className="max-w-full truncate font-mono text-[11px] text-muted-foreground"
                        title={downloadDir}
                      >
                        {downloadDir}
                      </span>
                    )}
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => void chooseDir()}
                      disabled={!settingsLoaded || saving}
                    >
                      {downloadDir
                        ? t("settings.changeFolder")
                        : t("settings.chooseFolder")}
                    </Button>
                  </div>
                }
              />
              <Row
                id="setting-retention"
                icon={<History className="h-4 w-4" />}
                title={t("settings.retention")}
                desc={t("settings.retentionDesc")}
                control={
                  <select
                    id="setting-retention"
                    data-testid="settings-retention-select"
                    value={retentionDays}
                    onChange={(e) => onRetention(Number(e.target.value))}
                    disabled={!settingsLoaded || saving}
                    className={SELECT_CLASS}
                    aria-label={t("settings.retention")}
                  >
                    <option value={0}>{t("settings.retentionForever")}</option>
                    <option value={7}>
                      {t("settings.retentionDays", { count: 7 })}
                    </option>
                    <option value={30}>
                      {t("settings.retentionDays", { count: 30 })}
                    </option>
                    <option value={90}>
                      {t("settings.retentionDays", { count: 90 })}
                    </option>
                  </select>
                }
              />
            </Section>

            <Section id="settings-recovery" title={t("recovery.section")}>
              <Row
                icon={<KeyRound className="h-4 w-4" />}
                title={t("recovery.createBackup")}
                desc={t("recovery.exportIntro")}
                control={<DataRecoverySettings />}
              />
            </Section>

            <Section
              id="settings-experimental"
              title={t("settings.sectionExperimental")}
            >
              <Row
                id="setting-calls"
                icon={<FlaskConical className="h-4 w-4 text-amber-500" />}
                title={t("settings.calls")}
                desc={t("settings.callsDesc")}
                control={
                  <div className="flex flex-col items-end gap-1.5">
                    <Badge className="bg-amber-500/15 text-amber-600 dark:text-amber-400">
                      {t("call.experimental")}
                    </Badge>
                    <Switch
                      id="setting-calls"
                      checked={callsEnabled}
                      onCheckedChange={onCalls}
                      disabled={!settingsLoaded || saving}
                      aria-label={t("settings.calls")}
                    />
                  </div>
                }
              />
              {callsEnabled && (
                <Row
                  id="setting-ringtone"
                  icon={<BellRing className="h-4 w-4" />}
                  title={t("settings.ringtone")}
                  desc={t("settings.ringtoneDesc")}
                  control={
                    <div className="flex items-center gap-1.5">
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => previewRingtone(ringtone)}
                        title={t("settings.ringtonePreview")}
                        aria-label={t("settings.ringtonePreview")}
                      >
                        <Play className="h-3.5 w-3.5" />
                      </Button>
                      <select
                        id="setting-ringtone"
                        data-testid="settings-ringtone-select"
                        value={ringtone}
                        onChange={(e) =>
                          onRingtone(e.target.value as RingtoneId)
                        }
                        disabled={!settingsLoaded || saving}
                        className={SELECT_CLASS}
                        aria-label={t("settings.ringtone")}
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
            </Section>
          </div>
        </div>
        {moreBelow && (
          <button
            type="button"
            data-testid="settings-scroll-more"
            onClick={() =>
              settingsScroll.current?.scrollBy({
                top: settingsScroll.current.clientHeight * 0.8,
                behavior: settingsScrollBehavior(),
              })
            }
            className="flex min-h-8 w-full items-center justify-center gap-1 text-xs text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {t("settings.scrollMore")}
            <ChevronDown className="h-3.5 w-3.5" />
          </button>
        )}
      </DialogContent>
    </Dialog>
  );
}
