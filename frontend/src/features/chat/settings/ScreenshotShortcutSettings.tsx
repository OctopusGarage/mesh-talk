import { useEffect, useState, type KeyboardEvent } from "react";
import { Camera, Globe, RotateCcw, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { useSettings } from "@/store/settings";
import {
  DEFAULT_SCREENSHOT_SHORTCUTS,
  bindingFromKeyboard,
  formatShortcut,
  screenshotShortcutProblem,
  type ScreenshotShortcuts,
} from "@/lib/screenshotShortcuts";
import { SettingsRow } from "./primitives";
import type { SettingsController } from "./useSettingsController";
import { screenshotShortcutsApi } from "@/lib/api/screenshotShortcuts";
import { captureOwner } from "@/store/ownership";

type CaptureAction = "capture" | "capture_hidden";

/** Recording is a focused, cancellable edit; only host-acknowledged bindings are displayed. */
export function ScreenshotShortcutSettings({
  controller: c,
}: {
  controller: SettingsController;
}) {
  const { t } = useTranslation();
  const [recording, setRecording] = useState<CaptureAction | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [recordingReady, setRecordingReady] = useState(false);
  const registration = useSettings((s) => s.screenshotRegistration);
  const pending = useSettings((s) => s.screenshotRegistrationPending);
  const sync = useSettings((s) => s.syncScreenshotShortcuts);
  const value =
    c.settings?.screenshot_shortcuts ?? DEFAULT_SCREENSHOT_SHORTCUTS;

  useEffect(() => {
    if (!recording) return;
    let current = true;
    const lease = captureOwner();
    const owner = useSettings.getState().owner;
    const reconcileAbandonedPause = () => {
      const store = useSettings.getState();
      if (
        !current &&
        lease.current() &&
        store.owner === owner &&
        !store.screenshotRecording
      )
        void store.syncScreenshotShortcuts();
    };
    useSettings.setState({ screenshotRecording: true });
    // Release OS ownership first, so recording an existing global chord reaches the field.
    void (owner ? screenshotShortcutsApi.pause(owner) : Promise.resolve()).then(
      () => {
        if (current) setRecordingReady(true);
        else reconcileAbandonedPause();
      },
      () => {
        if (current) {
          setProblem("registrationFailed");
          setRecording(null);
        } else reconcileAbandonedPause();
      },
    );
    return () => {
      current = false;
      if (useSettings.getState().owner === owner)
        useSettings.setState({ screenshotRecording: false });
    };
  }, [recording]);

  const save = (candidate: ScreenshotShortcuts) => {
    const issue = screenshotShortcutProblem(candidate);
    if (issue) {
      setProblem(issue);
      return;
    }
    setProblem(null);
    setRecording(null);
    void c.update({ screenshot_shortcuts: candidate }, "shortcuts");
  };
  const record = (
    event: KeyboardEvent<HTMLButtonElement>,
    action: CaptureAction,
  ) => {
    if (recording !== action || event.key === "Tab") return;
    event.preventDefault();
    event.stopPropagation();
    if (event.key === "Escape") {
      setRecording(null);
      setProblem(null);
      return;
    }
    if (!recordingReady) return;
    if (
      event.nativeEvent.isComposing ||
      event.repeat ||
      /^(Control|Shift|Alt|Meta)/.test(event.code)
    )
      return;
    const binding = bindingFromKeyboard(event.nativeEvent);
    if (!binding) {
      setProblem("invalid");
      return;
    }
    save({ ...value, [action]: binding });
  };

  return (
    <>
      {(["capture", "capture_hidden"] as const).map((action) => {
        const id =
          action === "capture"
            ? "setting-screenshot-capture"
            : "setting-screenshot-hidden";
        const active = recording === action;
        return (
          <SettingsRow
            key={action}
            id={id}
            icon={<Camera className="h-4 w-4" />}
            title={t(
              action === "capture" ? "screenshot.now" : "screenshot.hidden",
            )}
            desc={t(
              active
                ? "screenshot.shortcuts.recordingHint"
                : "screenshot.shortcuts.bindingHint",
            )}
            control={
              <div className="flex max-w-full items-center gap-1">
                <Button
                  id={id}
                  aria-describedby={`${id}-help`}
                  variant="outline"
                  size="sm"
                  disabled={c.saving || !c.settings}
                  data-shortcut-recording={active ? "" : undefined}
                  aria-pressed={active}
                  className="min-w-32 border-input text-[12px] tabular-nums"
                  aria-busy={active && !recordingReady}
                  onClick={() => {
                    setRecordingReady(false);
                    setRecording(active ? null : action);
                    setProblem(null);
                  }}
                  onKeyDown={(event) => record(event, action)}
                  onBlur={() => {
                    setRecording(null);
                    setProblem(null);
                  }}
                >
                  {active
                    ? t(
                        recordingReady
                          ? "screenshot.shortcuts.recording"
                          : "screenshot.shortcuts.checking",
                      )
                    : formatShortcut(value[action]) ||
                      t("screenshot.shortcuts.unassigned")}
                </Button>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8 shrink-0 text-muted-foreground"
                  data-testid={`clear-screenshot-${action === "capture" ? "capture" : "hidden"}`}
                  disabled={c.saving || !value[action] || !c.settings}
                  title={t("screenshot.shortcuts.clear")}
                  aria-label={t("screenshot.shortcuts.clearAction", {
                    action: t(
                      action === "capture"
                        ? "screenshot.now"
                        : "screenshot.hidden",
                    ),
                  })}
                  onClick={() => save({ ...value, [action]: null })}
                >
                  <X className="h-3.5 w-3.5" />
                </Button>
              </div>
            }
          />
        );
      })}
      {problem && (
        <p role="alert" className="py-2 text-[12px] text-destructive">
          {t(`screenshot.shortcuts.${problem}`)}
        </p>
      )}
      <SettingsRow
        id="setting-screenshot-global"
        icon={<Globe className="h-4 w-4" />}
        title={t("screenshot.shortcuts.globalTitle")}
        desc={t("screenshot.shortcuts.globalDescription")}
        control={
          <Switch
            id="setting-screenshot-global"
            checked={value.global}
            disabled={
              c.saving ||
              !c.settings ||
              (!value.capture && !value.capture_hidden)
            }
            onCheckedChange={(global) => save({ ...value, global })}
          />
        }
      />
      <div className="flex flex-wrap items-center justify-between gap-2 py-2">
        <p aria-live="polite" className="text-[12px] text-muted-foreground">
          {t(
            value.global
              ? pending
                ? "screenshot.shortcuts.checking"
                : registration.global_active
                  ? "screenshot.shortcuts.globalActive"
                  : "screenshot.shortcuts.globalInactive"
              : "screenshot.shortcuts.appOnly",
          )}
        </p>
        <Button
          variant="ghost"
          size="sm"
          className="gap-1.5 text-[12px]"
          data-testid="reset-screenshot-shortcuts"
          disabled={c.saving || !c.settings}
          onClick={() =>
            save({ ...DEFAULT_SCREENSHOT_SHORTCUTS, global: value.global })
          }
        >
          <RotateCcw className="h-3.5 w-3.5" />
          {t("screenshot.shortcuts.restore")}
        </Button>
      </div>
      {registration.error && value.global && (
        <div role="alert" className="space-y-1 pb-2 text-[12px]">
          <p className="text-destructive">
            {t("screenshot.shortcuts.registrationFailed")}
          </p>
          <p className="max-h-20 overflow-auto break-words text-muted-foreground">
            {registration.error}
          </p>
          <Button
            variant="outline"
            size="sm"
            disabled={pending}
            onClick={() => void sync()}
          >
            {t("common.retry")}
          </Button>
        </div>
      )}
    </>
  );
}
