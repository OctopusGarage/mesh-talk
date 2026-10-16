import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Code2, ExternalLink, Info, Scale } from "lucide-react";
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
import { Logo } from "@/components/Logo";
import { obs } from "@/lib/api";
import { UpdatePanel } from "./UpdatePanel";

const REPO_URL = "https://github.com/OctopusGarage/mesh-talk";
const LICENSE_URL =
  "https://github.com/OctopusGarage/mesh-talk/blob/main/LICENSE";

/** A minimal About surface: app name + version, one-line description, source/license
 * links, and a shortcut into the Diagnostics dialog. Version comes from `env_info`. */
export function AboutDialog({
  menuItem = false,
  open: controlledOpen,
  onOpenChange,
  hideTrigger = false,
}: {
  menuItem?: boolean;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  hideTrigger?: boolean;
}) {
  const { t } = useTranslation();
  const [localOpen, setLocalOpen] = useState(false);
  const open = controlledOpen ?? localOpen;
  const setOpen = onOpenChange ?? setLocalOpen;
  const [version, setVersion] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    void obs.envInfo().then(
      (e) => {
        setVersion(e.app_version);
      },
      () => {
        setVersion(null);
      },
    );
  }, [open]);

  const open_ = (url: string) => {
    void openUrl(url).catch(() => {});
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      {!hideTrigger && (
        <DialogTrigger asChild>
          <Button
            variant="ghost"
            size={menuItem ? "sm" : "icon"}
            className={
              menuItem ? "w-full justify-start gap-2.5 px-2" : undefined
            }
            data-testid="sidebar-action-about"
            title={t("about.title")}
          >
            <Info className="h-4 w-4" />
            {menuItem && <span>{t("about.title")}</span>}
          </Button>
        </DialogTrigger>
      )}
      <DialogContent className="max-w-lg" data-testid="about-dialog">
        <DialogHeader>
          <DialogTitle>{t("about.title")}</DialogTitle>
          <DialogDescription>{t("about.description")}</DialogDescription>
        </DialogHeader>

        <div className="grid gap-4">
          <div className="flex items-center gap-4">
            <Logo size={48} className="shrink-0" />
            <div className="min-w-0">
              <div className="font-display text-xl font-semibold tracking-tight">
                Mesh-Talk
              </div>
              <div className="font-mono text-xs text-muted-foreground">
                {version
                  ? t("about.version", { version })
                  : t("diagnostics.nodeNotReady")}
              </div>
            </div>
          </div>

          <p className="text-sm leading-relaxed text-muted-foreground">
            {t("about.tagline")}
          </p>

          <UpdatePanel />

          <div className="grid gap-2">
            <Button
              variant="secondary"
              size="sm"
              className="justify-start"
              onClick={() => open_(REPO_URL)}
            >
              <Code2 className="h-4 w-4" /> {t("about.sourceRepo")}
              <ExternalLink className="ml-auto h-3.5 w-3.5 opacity-60" />
            </Button>
            <Button
              variant="secondary"
              size="sm"
              className="justify-start"
              onClick={() => open_(LICENSE_URL)}
            >
              <Scale className="h-4 w-4" /> {t("about.license")}
              <ExternalLink className="ml-auto h-3.5 w-3.5 opacity-60" />
            </Button>
          </div>

          {/* Attribution for bundled third-party assets (Noto Animated Emoji stickers). */}
          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("about.credits")}
          </p>
        </div>
      </DialogContent>
    </Dialog>
  );
}
