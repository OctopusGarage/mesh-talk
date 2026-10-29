import { useEffect, useState } from "react";
import {
  LogOut,
  BookOpen,
  Info,
  LockKeyhole,
  MoreHorizontal,
  Moon,
  Radar,
  Settings,
  Sun,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { useTheme } from "@/lib/theme";
import { useAuth } from "@/store/auth";
import { useAppLock } from "@/store/appLock";
import { LinkDeviceDialog } from "../LinkDeviceDialog";
import { DiagnosticsDialog } from "../DiagnosticsDialog";
import { SettingsDialog } from "../SettingsDialog";
import { AboutDialog } from "../AboutDialog";
import { HelpDialog } from "@/features/help/HelpDialog";
import { resolveHelpTarget } from "@/features/help/targets";
import type { SettingsCategory } from "../settings/model";

/** Secondary actions share one labeled utility menu beside connection status. */
export function SidebarUtilities() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [connectionOpen, setConnectionOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [settingsTarget, setSettingsTarget] = useState<{
    category: SettingsCategory;
    controlId?: string;
  }>();
  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    void listen<string>("tray-navigate", ({ payload }) => {
      if (
        !active ||
        !["diagnostics", "settings", "about", "help"].includes(payload)
      )
        return;
      setOpen(false);
      setConnectionOpen(payload === "diagnostics");
      setSettingsOpen(payload === "settings");
      setAboutOpen(payload === "about");
      setHelpOpen(payload === "help");
      setSettingsTarget(undefined);
    })
      .then((stop) => {
        if (active) unlisten = stop;
        else stop();
      })
      .catch(() => {});
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);
  const theme = useTheme((s) => s.theme);
  const toggleTheme = useTheme((s) => s.toggle);
  const logout = useAuth((s) => s.logout);
  const lockApp = useAppLock((s) => s.lock);

  return (
    <>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            data-testid="sidebar-overflow"
            title={t("sidebar.moreActions")}
            aria-label={t("sidebar.moreActions")}
          >
            <MoreHorizontal className="h-4 w-4" />
          </Button>
        </PopoverTrigger>
        <PopoverContent
          align="start"
          side="top"
          className="w-56 p-1.5"
          data-testid="sidebar-overflow-menu"
        >
          <div className="grid gap-0.5">
            <LinkDeviceDialog menuItem />
            <Button
              variant="ghost"
              size="sm"
              className="w-full justify-start gap-2.5 px-2"
              data-testid="sidebar-nav-connection"
              onClick={() => {
                setOpen(false);
                setConnectionOpen(true);
              }}
            >
              <Radar className="h-4 w-4" />
              {t("redesign.connection")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className="w-full justify-start gap-2.5 px-2"
              data-testid="sidebar-nav-settings"
              onClick={() => {
                setOpen(false);
                setSettingsTarget(undefined);
                setSettingsOpen(true);
              }}
            >
              <Settings className="h-4 w-4" />
              {t("settings.title")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className="w-full justify-start gap-2.5 px-2"
              data-testid="sidebar-nav-help"
              onClick={() => {
                setOpen(false);
                setHelpOpen(true);
              }}
            >
              <BookOpen className="h-4 w-4" />
              {t("help.title")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              className="w-full justify-start gap-2.5 px-2"
              data-testid="sidebar-action-about"
              title={t("about.title")}
              onClick={() => {
                setOpen(false);
                setAboutOpen(true);
              }}
            >
              <Info className="h-4 w-4" />
              {t("about.title")}
            </Button>
            <div className="my-1 h-px bg-border" />
            <button
              type="button"
              data-testid="sidebar-lock"
              onClick={() => {
                setOpen(false);
                void lockApp();
              }}
              className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-accent"
            >
              <LockKeyhole className="h-4 w-4" />
              <span>{t("settings.lockNow")}</span>
            </button>
            <button
              type="button"
              data-testid="sidebar-theme-toggle"
              onClick={() => toggleTheme()}
              className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-sm transition-colors hover:bg-accent"
            >
              {theme === "light" ? (
                <Moon className="h-4 w-4 text-muted-foreground" />
              ) : (
                <Sun className="h-4 w-4 text-muted-foreground" />
              )}
              <span>
                {theme === "light"
                  ? t("sidebar.darkMode")
                  : t("sidebar.lightMode")}
              </span>
            </button>
            <button
              type="button"
              data-testid="sidebar-sign-out"
              onClick={() => logout()}
              className="flex w-full items-center gap-2.5 rounded-md px-2 py-1.5 text-left text-sm text-destructive transition-colors hover:bg-destructive/10"
            >
              <LogOut className="h-4 w-4" />
              <span>{t("sidebar.signOut")}</span>
            </button>
          </div>
        </PopoverContent>
      </Popover>
      <DiagnosticsDialog
        open={connectionOpen}
        onOpenChange={setConnectionOpen}
      />
      <SettingsDialog
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        initialTarget={settingsTarget}
      />
      <HelpDialog
        open={helpOpen}
        onOpenChange={setHelpOpen}
        onNavigate={(id) => {
          const target = resolveHelpTarget(id);
          if (!target) return;
          setHelpOpen(false);
          if (target.kind === "settings") {
            setSettingsTarget(target);
            setSettingsOpen(true);
          } else setConnectionOpen(true);
        }}
      />
      <AboutDialog open={aboutOpen} onOpenChange={setAboutOpen} hideTrigger />
    </>
  );
}
