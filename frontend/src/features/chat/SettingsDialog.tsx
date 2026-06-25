import { useEffect, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  Bell,
  FolderOpen,
  History,
  KeyRound,
  Languages,
  MinusSquare,
  Palette,
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
import { settings as settingsApi } from "@/lib/api";
import {
  resolveLanguage,
  setLanguage,
  SUPPORTED_LANGUAGES,
  type Language,
} from "@/lib/i18n";
import { ThemePicker } from "./ThemePicker";

/** A section group: a small display-font label over a stack of rows. */
function Section({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-2">
      <h3 className="font-mono text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
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
    <div className="flex items-center justify-between gap-4 rounded-lg border p-3">
      <div className="flex min-w-0 items-start gap-3">
        <div className="mt-0.5 text-muted-foreground">{icon}</div>
        <div className="min-w-0">
          <label htmlFor={id} className="block text-sm font-medium">
            {title}
          </label>
          <p className="text-xs leading-relaxed text-muted-foreground">
            {desc}
          </p>
        </div>
      </div>
      <div className="shrink-0">{control}</div>
    </div>
  );
}

const SELECT_CLASS =
  "h-9 rounded-md border border-input bg-background px-2 text-sm transition-colors hover:border-ring focus-visible:border-ring focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background";

const LANGUAGE_LABELS: Record<Language, string> = {
  en: "English",
  es: "Español",
  ja: "日本語",
  "zh-Hans": "简体中文",
  "zh-Hant": "繁體中文",
  yue: "粵語",
};

export function SettingsDialog() {
  const { t, i18n } = useTranslation();
  const [open, setOpen] = useState(false);
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

  // Load current state whenever the dialog opens.
  useEffect(() => {
    if (!open) return;
    void settingsApi.get().then(
      (s) => {
        setMinimizeToTray(s.minimize_to_tray);
        setNotifications(s.notifications);
        setDownloadDir(s.download_dir);
        setStaySignedIn(s.stay_signed_in);
        setRetentionDays(s.retention_days);
      },
      () => {},
    );
    void settingsApi.autostartEnabled().then(
      (on) => setLaunchAtLogin(on),
      () => {},
    );
  }, [open]);

  // Persist the toggles. Re-read the current settings first so we don't clobber fields the
  // dialog doesn't manage (e.g. download_dir, set in the Files tray).
  const persist = (next: {
    minimize_to_tray: boolean;
    notifications: boolean;
  }) => {
    void settingsApi
      .get()
      .then((cur) => settingsApi.set({ ...cur, ...next }))
      .catch(() => {});
  };

  const onMinimize = (v: boolean) => {
    setMinimizeToTray(v);
    persist({ minimize_to_tray: v, notifications });
  };
  const onNotifications = (v: boolean) => {
    setNotifications(v);
    persist({ minimize_to_tray: minimizeToTray, notifications: v });
  };
  // Toggling "stay signed in" off makes the backend immediately forget the saved
  // keychain secret (see set_app_settings), so the next launch shows the login screen.
  const onStaySignedIn = (v: boolean) => {
    setStaySignedIn(v);
    void settingsApi
      .get()
      .then((cur) => settingsApi.set({ ...cur, stay_signed_in: v }))
      .catch(() => {});
  };
  // Persisting retention triggers an immediate backend prune of older messages (see
  // set_app_settings), so a tightened window takes effect at once.
  const onRetention = (days: number) => {
    setRetentionDays(days);
    void settingsApi
      .get()
      .then((cur) => settingsApi.set({ ...cur, retention_days: days }))
      .catch(() => {});
  };
  const onLaunch = async (v: boolean) => {
    setAutostartBusy(true);
    try {
      await settingsApi.setAutostart(v);
      // Reflect the actual OS state rather than the optimistic toggle.
      setLaunchAtLogin(await settingsApi.autostartEnabled());
    } catch {
      // Revert on failure.
      setLaunchAtLogin(await settingsApi.autostartEnabled().catch(() => !v));
    } finally {
      setAutostartBusy(false);
    }
  };

  const chooseDir = async () => {
    try {
      const dir = await openDialog({
        directory: true,
        defaultPath: downloadDir || undefined,
      });
      if (typeof dir === "string") {
        const cur = await settingsApi.get();
        await settingsApi.set({ ...cur, download_dir: dir });
        setDownloadDir(dir);
      }
    } catch {
      // Cancelled / unavailable — leave the current folder untouched.
    }
  };

  // The active language, mapped onto a supported code (i18n.language may be a
  // region tag like "en-US"; resolveLanguage also handles the zh-* variants).
  const currentLang = resolveLanguage(i18n.language) ?? "en";

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          data-testid="sidebar-action-settings"
          title={t("settings.title")}
        >
          <Settings className="h-4 w-4" />
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-md" data-testid="settings-dialog">
        <DialogHeader>
          <DialogTitle>{t("settings.title")}</DialogTitle>
          <DialogDescription>{t("settings.description")}</DialogDescription>
        </DialogHeader>

        <div className="grid max-h-[70vh] gap-5 overflow-y-auto pr-0.5">
          <Section title={t("settings.sectionAppearance")}>
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

          <Section title={t("settings.sectionBackground")}>
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
                  disabled={autostartBusy}
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
                  aria-label={t("settings.staySignedIn")}
                />
              }
            />
          </Section>

          <Section title={t("settings.sectionFiles")}>
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
                  >
                    {downloadDir
                      ? t("settings.changeFolder")
                      : t("settings.chooseFolder")}
                  </Button>
                </div>
              }
            />
          </Section>

          <Section title={t("settings.sectionChat")}>
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
        </div>
      </DialogContent>
    </Dialog>
  );
}
