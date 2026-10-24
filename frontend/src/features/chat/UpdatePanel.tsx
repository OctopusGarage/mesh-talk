import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  ArrowDownToLine,
  CheckCircle2,
  ExternalLink,
  LoaderCircle,
  RefreshCw,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/error";
import type { UpdateChannel } from "@/lib/types";
import { useUpdates } from "@/store/updates";

function readableSize(bytes: number): string {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export function UpdatePanel() {
  const { t } = useTranslation();
  const {
    phase,
    channel,
    currentVersion,
    setChannel,
    release,
    download,
    progress,
    error,
    canceling,
    check,
    startDownload,
    install,
    cancel,
    setError,
  } = useUpdates();

  const reveal = async () => {
    if (!download) return;
    try {
      await revealItemInDir(download.path);
    } catch (cause) {
      setError(errorMessage(cause));
    }
  };

  const openRelease = async (url = release?.release_url) => {
    if (!url) return;
    try {
      await openUrl(url);
    } catch (cause) {
      setError(errorMessage(cause));
    }
  };

  const installing = phase === "installing" || phase === "restarting";
  const busy = phase === "checking" || phase === "downloading" || installing;
  const percent = progress?.total
    ? Math.min(
        100,
        Math.max(0, Math.floor((progress.downloaded / progress.total) * 100)),
      )
    : null;
  const installKey = release?.package_name?.includes("_macos_")
    ? "mac"
    : release?.package_name?.includes("_windows_")
      ? "windows"
      : "linux";

  return (
    <section
      className="rounded-xl border border-border/70 bg-muted/35 p-3.5"
      aria-label={t("about.updates.title")}
      data-testid="update-panel"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sm font-semibold">{t("about.updates.title")}</h3>
        <Button
          size="sm"
          variant="outline"
          onClick={() => void check()}
          disabled={busy}
          data-testid="check-for-updates"
        >
          {phase === "checking" ? (
            <LoaderCircle
              className="h-4 w-4 animate-spin motion-reduce:animate-none"
              aria-hidden="true"
            />
          ) : (
            <RefreshCw className="h-4 w-4" />
          )}
          {phase === "checking"
            ? t("about.updates.checking")
            : t("about.updates.check")}
        </Button>
      </div>

      {!release && (
        <p className="mt-2 text-xs leading-relaxed text-muted-foreground">
          {t("about.updates.manual")}
        </p>
      )}
      <div className="mt-3 space-y-2">
        <div className="flex flex-wrap items-center gap-2">
          <label htmlFor="update-channel" className="text-xs font-medium">
            {t("about.updates.channel")}
          </label>
          <select
            id="update-channel"
            className="h-8 rounded-md border border-input bg-background px-2 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
            value={channel}
            disabled={busy}
            onChange={(event) =>
              setChannel(event.target.value as UpdateChannel)
            }
            aria-describedby="update-channel-help"
          >
            <option value="stable">{t("about.updates.stable")}</option>
            <option value="nightly">{t("about.updates.nightly")}</option>
          </select>
          {currentVersion && (
            <p className="ml-auto min-w-0 break-words text-xs text-muted-foreground">
              {t("about.updates.installed", { version: currentVersion })}
            </p>
          )}
        </div>
        <p
          id="update-channel-help"
          className={
            release && channel === "stable"
              ? "sr-only"
              : "text-xs leading-relaxed text-muted-foreground"
          }
        >
          {channel === "nightly"
            ? t("about.updates.nightlyWarning")
            : t("about.updates.stableHelp")}
        </p>
      </div>

      {release && (
        <div
          className="mt-3 space-y-2.5 border-t border-border/70 pt-3"
          role="status"
        >
          {!release.is_newer ? (
            <p className="flex items-center gap-2 text-sm">
              <CheckCircle2 className="h-4 w-4 text-emerald-600" />
              {t("about.updates.current", { version: release.current_version })}
            </p>
          ) : (
            <>
              <p className="text-sm font-medium">
                {t("about.updates.available", {
                  version: release.latest_version,
                })}
              </p>
              {release.notes && (
                <div className="max-h-28 overflow-y-auto whitespace-pre-wrap break-words text-xs leading-relaxed text-muted-foreground">
                  {release.notes}
                </div>
              )}
              {release.package_name ? (
                <>
                  <p className="break-words text-xs leading-relaxed text-muted-foreground">
                    {release.automatic_install
                      ? t(
                          installKey === "windows"
                            ? "about.updates.automaticWindowsHelp"
                            : "about.updates.automaticHelp",
                        )
                      : release.package_name}
                    {release.package_size
                      ? ` · ${readableSize(release.package_size)}`
                      : ""}
                  </p>
                  {phase === "downloading" && progress && (
                    <div role="status" aria-live="polite">
                      <div className="mb-1 flex justify-between text-xs text-muted-foreground">
                        <span>{t("about.updates.downloading")}</span>
                        <span>
                          {percent !== null
                            ? `${percent}%`
                            : readableSize(progress.downloaded)}
                        </span>
                      </div>
                      <progress
                        className="h-2 w-full accent-primary"
                        aria-label={t("about.updates.downloading")}
                        max={progress.total || 1}
                        value={
                          progress.total
                            ? Math.min(progress.downloaded, progress.total)
                            : undefined
                        }
                      />
                      <Button
                        size="sm"
                        variant="ghost"
                        className="mt-2"
                        onClick={() => void cancel()}
                        disabled={canceling}
                      >
                        {t("about.updates.cancel")}
                      </Button>
                    </div>
                  )}
                  {installing ? (
                    <p
                      className="flex items-center gap-2 text-sm"
                      role="status"
                      aria-live="polite"
                      data-testid="update-install-status"
                    >
                      <LoaderCircle
                        className="h-4 w-4 shrink-0 animate-spin motion-reduce:animate-none"
                        aria-hidden="true"
                      />
                      {t(
                        phase === "restarting"
                          ? "about.updates.restarting"
                          : "about.updates.installing",
                      )}
                    </p>
                  ) : download ? (
                    <div className="space-y-3" data-testid="update-downloaded">
                      <p className="flex items-center gap-2 text-sm font-medium">
                        <CheckCircle2 className="h-4 w-4 text-emerald-600" />
                        {t("about.updates.verified")}
                      </p>
                      <p className="text-xs leading-relaxed text-muted-foreground">
                        {download.installable
                          ? t("about.updates.readyToRestart")
                          : t(`about.updates.install.${installKey}`)}
                      </p>
                      <Button
                        size="sm"
                        onClick={() =>
                          void (download.installable ? install() : reveal())
                        }
                      >
                        {t(
                          download.installable
                            ? "about.updates.restartToUpdate"
                            : "about.updates.showDownload",
                        )}
                      </Button>
                    </div>
                  ) : phase !== "downloading" ? (
                    <Button
                      size="sm"
                      onClick={() => void startDownload()}
                      disabled={busy}
                      data-testid="download-update"
                    >
                      <ArrowDownToLine className="h-4 w-4" aria-hidden="true" />
                      {t(
                        release.automatic_install
                          ? "about.updates.updateAndRestart"
                          : "about.updates.download",
                      )}
                    </Button>
                  ) : null}
                  {release.automatic_install && !installing && (
                    <Button
                      size="sm"
                      variant="link"
                      className="ml-3 h-auto px-0 text-xs"
                      onClick={() => void openRelease()}
                    >
                      {t("about.updates.downloadManually")}
                    </Button>
                  )}
                </>
              ) : (
                <p className="text-xs leading-relaxed text-muted-foreground">
                  {t("about.updates.noPackage")}
                </p>
              )}
              {!release.automatic_install && (
                <Button
                  size="sm"
                  variant="link"
                  className="ml-2 h-auto px-0"
                  onClick={() => void openRelease()}
                >
                  {t("about.updates.releaseNotes")}
                  <ExternalLink className="h-3.5 w-3.5" />
                </Button>
              )}
            </>
          )}
        </div>
      )}
      {error && (
        <p
          className="mt-3 text-xs leading-relaxed text-destructive"
          role="alert"
          data-testid="update-error"
        >
          {t("about.updates.failed", { error })}
        </p>
      )}
      <div className="mt-3">
        <Button
          size="sm"
          variant="link"
          className="h-auto px-0 text-xs"
          onClick={() =>
            void openRelease(
              "https://github.com/OctopusGarage/mesh-talk/releases/latest",
            )
          }
        >
          {t("about.updates.stableDownload")}
          <ExternalLink className="h-3.5 w-3.5" />
        </Button>
      </div>
    </section>
  );
}
