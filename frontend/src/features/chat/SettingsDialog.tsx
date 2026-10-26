import { useLayoutEffect, useRef, useState } from "react";
import { Settings } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { SETTINGS_CATEGORIES, type SettingsCategory } from "./settings/model";
import { useSettingsController } from "./settings/useSettingsController";
import { SettingsError } from "./settings/primitives";
import { GeneralSettings } from "./settings/GeneralSettings";
import { AppearanceSettings } from "./settings/AppearanceSettings";
import { NotificationSettings } from "./settings/NotificationSettings";
import { NetworkSettings } from "./settings/NetworkSettings";
import { SecuritySettings } from "./settings/SecuritySettings";
import { FileSettings } from "./settings/FileSettings";
import { HistorySettings } from "./settings/HistorySettings";

/** Composition only: category navigation never derives selection from scroll position. */
export function SettingsDialog({
  open: controlledOpen,
  onOpenChange,
  onCloseAutoFocus,
  initialTarget,
}: {
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  onCloseAutoFocus?: (event: Event) => void;
  initialTarget?: { category: SettingsCategory; controlId?: string };
} = {}) {
  const { t } = useTranslation();
  const [internalOpen, setInternalOpen] = useState(false);
  const open = controlledOpen ?? internalOpen;
  const setOpen = onOpenChange ?? setInternalOpen;
  const [category, setCategory] = useState<SettingsCategory>("general");
  const [presetDraft, setPresetDraft] = useState(false);
  const panel = useRef<HTMLDivElement>(null);
  const focusTarget = useRef<string | null>(null);
  const controller = useSettingsController(open);
  const initialCategory = initialTarget?.category ?? "general";
  const initialControlId = initialTarget?.controlId;
  useLayoutEffect(() => {
    if (!open) return;
    focusTarget.current = initialControlId ? `#${initialControlId}` : null;
    setCategory(initialCategory);
  }, [open, controller.identity, initialCategory, initialControlId]);
  useLayoutEffect(() => {
    if (category !== "privacy") setPresetDraft(false);
    if (panel.current) panel.current.scrollTop = 0;
  }, [category]);
  useLayoutEffect(() => {
    if (!open || !focusTarget.current) return;
    const editor = panel.current?.querySelector<HTMLElement>(
      focusTarget.current,
    );
    if (!editor) return; // A category's settings may still be loading.
    editor.scrollIntoView({ block: "nearest" });
    if (editor.matches(":disabled")) panel.current?.focus();
    else editor.focus();
    focusTarget.current = null;
  }, [open, category, controller.settings, controller.identity]);
  const page = {
    general: <GeneralSettings controller={controller} />,
    appearance: <AppearanceSettings />,
    notifications: <NotificationSettings controller={controller} />,
    connection: <NetworkSettings controller={controller} />,
    privacy: (
      <SecuritySettings
        controller={controller}
        onDraftChange={setPresetDraft}
        onNotifications={() => {
          focusTarget.current = "#setting-notification-preview";
          setCategory("notifications");
        }}
      />
    ),
    files: <FileSettings controller={controller} />,
    history: <HistorySettings controller={controller} />,
  }[category];
  return (
    <Dialog open={open} onOpenChange={setOpen}>
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
        onEscapeKeyDown={(event) => {
          if (
            event.target instanceof Element &&
            event.target.closest("[data-shortcut-recording]")
          )
            event.preventDefault();
        }}
        onCloseAutoFocus={onCloseAutoFocus}
        className="grid h-[min(44rem,calc(100vh-2rem))] supports-[height:100dvh]:h-[min(44rem,calc(100dvh-2rem))] max-w-[min(56rem,calc(100vw-2rem))] grid-rows-[auto_minmax(0,1fr)_auto] gap-4 overflow-hidden"
        data-testid="settings-dialog"
      >
        <DialogHeader>
          <DialogTitle>{t("settings.title")}</DialogTitle>
          <DialogDescription>{t("settings.description")}</DialogDescription>
        </DialogHeader>
        <div className="flex min-h-0 min-w-0 gap-5 max-[700px]:flex-col max-[700px]:gap-3">
          <nav
            aria-label={t("redesign.settingsNav")}
            className="flex w-40 shrink-0 flex-col gap-1 overflow-y-auto border-r pr-3 max-[700px]:w-auto max-[700px]:flex-row max-[700px]:overflow-x-auto max-[700px]:border-b max-[700px]:border-r-0 max-[700px]:pb-2 max-[700px]:pr-0"
          >
            {SETTINGS_CATEGORIES.map((id) => (
              <button
                key={id}
                type="button"
                id={`settings-nav-${id}`}
                data-testid={`settings-nav-${id}`}
                aria-current={category === id ? "page" : undefined}
                aria-controls="settings-category-panel"
                onClick={() => setCategory(id)}
                className={`shrink-0 rounded-md px-2.5 py-2 text-left text-[13px] font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring ${category === id ? "bg-accent text-foreground" : "text-muted-foreground hover:bg-accent hover:text-foreground"}`}
              >
                {t(`settings.categories.${id}`)}
              </button>
            ))}
          </nav>
          <div
            id="settings-category-panel"
            ref={panel}
            role="region"
            aria-labelledby={`settings-nav-${category}`}
            tabIndex={0}
            className="min-h-0 min-w-0 flex-1 overflow-y-auto overscroll-contain pr-2 outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
          >
            <div
              key={`${controller.identity}:${category}`}
              data-testid={`settings-page-${category}`}
              className="min-w-0 space-y-3 pb-2"
            >
              <div>
                <h2 className="text-[15px] font-semibold">
                  {t(`settings.categories.${category}`)}
                </h2>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
                  {t(
                    category === "general" || category === "appearance"
                      ? "settings.scopeComputer"
                      : "settings.scopeAccount",
                  )}
                </p>
              </div>
              <SettingsError
                error={controller.loadError}
                onRetry={controller.reload}
              />
              {!controller.settings && !controller.loadError && (
                <p role="status" className="text-xs text-muted-foreground">
                  {t("common.loading")}
                </p>
              )}
              {!controller.loadError && (
                <SettingsError
                  error={controller.errors[category] ?? null}
                  onRetry={controller.reload}
                  onDismiss={() => controller.clearError(category)}
                />
              )}
              {page}
            </div>
          </div>
        </div>
        <div
          role="status"
          aria-live="polite"
          className="min-h-5 text-xs text-muted-foreground"
        >
          {controller.saving
            ? t("settings.saving")
            : category === "privacy" && presetDraft
              ? t("settings.presetNotApplied")
              : controller.savedCategory === category
                ? t("settings.saved")
                : t("settings.saveImmediately")}
        </div>
      </DialogContent>
    </Dialog>
  );
}
