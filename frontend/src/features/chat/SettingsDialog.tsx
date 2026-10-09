import { useCallback, useEffect, useRef, useState } from "react";
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
import { errorMessage } from "@/lib/error";
import { useSettings } from "@/store/settings";
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
import { PrivacySettings } from "./PrivacySettings";
import {
  scrollToSettingsSection,
  settingsCategory,
  settingsScrollBehavior,
} from "./settingsScroll";

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
  const wallpaperEnabled = useTheme((s) => s.wallpaperEnabled);
  const setWallpaperEnabled = useTheme((s) => s.setWallpaperEnabled);
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
  const [ringtone, setRingtone] = useState<RingtoneId>(DEFAULT_RINGTONE);
  const settingsScroll = useRef<HTMLDivElement>(null);
  const [moreBelow, setMoreBelow] = useState(true);
  const [activeSection, setActiveSection] = useState("privacy");
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [autostartLoaded, setAutostartLoaded] = useState(false);
  const [saving, setSaving] = useState(false);
  const savingRef = useRef(false);
  const loadGeneration = useRef(0);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [settingsErrorKind, setSettingsErrorKind] = useState<"load" | "save">(
    "load",
  );

  const loadCurrentSettings = useCallback(() => {
    const request = ++loadGeneration.current;
    setSettingsLoaded(false);
    setAutostartLoaded(false);
    setSettingsError(null);
    void settingsApi.get().then(
      (s) => {
        if (request !== loadGeneration.current) return;
        setMinimizeToTray(s.minimize_to_tray);
        setNotifications(s.notifications);
        setDownloadDir(s.download_dir);
        setStaySignedIn(s.stay_signed_in);
        setRetentionDays(s.retention_days);
        setCallsEnabled(s.calls_enabled);
        setRingtone(asRingtoneId(s.ringtone));
        setSettingsLoaded(true);
      },
      (e) => {
        if (request === loadGeneration.current) {
          setSettingsErrorKind("load");
          setSettingsError(
            t("settings.loadFailed", { error: errorMessage(e) }),
          );
        }
      },
    );
    void settingsApi.autostartEnabled().then(
      (on) => {
        if (request !== loadGeneration.current) return;
        setLaunchAtLogin(on);
        setAutostartLoaded(true);
      },
      (e) => {
        if (request === loadGeneration.current) {
          setSettingsErrorKind("load");
          setSettingsError(
            t("settings.loadFailed", { error: errorMessage(e) }),
          );
        }
      },
    );
  }, [t]);

  // Load current state whenever the dialog opens.
  useEffect(() => {
    if (open) loadCurrentSettings();
  }, [open, loadCurrentSettings]);

  const saveSetting = async (write: () => Promise<void>, apply: () => void) => {
    if (!settingsLoaded || savingRef.current) return;
    savingRef.current = true;
    setSaving(true);
    setSettingsError(null);
    try {
      await write();
      apply();
    } catch (e) {
      setSettingsErrorKind("save");
      setSettingsError(t("settings.saveFailed", { error: errorMessage(e) }));
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
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
        setLaunchAtLogin(await settingsApi.autostartEnabled());
      },
      () => {},
    ).finally(() => setAutostartBusy(false));
  };

  const chooseDir = async () => {
    try {
      const dir = await openDialog({
        directory: true,
        defaultPath: downloadDir || undefined,
      });
      if (typeof dir === "string") {
        await saveSetting(
          () => settingsApi.update({ download_dir: dir }),
          () => setDownloadDir(dir),
        );
      }
    } catch (e) {
      setSettingsErrorKind("save");
      setSettingsError(t("settings.saveFailed", { error: errorMessage(e) }));
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
        if (next) setMoreBelow(true);
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
