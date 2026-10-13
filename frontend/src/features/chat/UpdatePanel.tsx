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
import { useUpdates } from "@/store/updates";

function readableSize(bytes: number): string {
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export function UpdatePanel() {
  const { t } = useTranslation();
  const {
    phase,
    release,
    download,
    progress,
    error,
    canceling,
    check,
    startDownload,
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

  const openRelease = async () => {
    if (!release) return;
    try {
      await openUrl(release.release_url);
    } catch (cause) {
      setError(errorMessage(cause));
    }
  };

  const busy = phase === "checking" || phase === "downloading";
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
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="text-sm font-semibold">{t("about.updates.title")}</h3>
          <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
            {t("about.updates.manual")}
          </p>
        </div>
        <Button
          size="sm"
          variant="outline"
          onClick={() => void check()}
          disabled={busy}
          data-testid="check-for-updates"
        >
          {phase === "checking" ? (
            <LoaderCircle className="h-4 w-4 animate-spin" />
          ) : (
            <RefreshCw className="h-4 w-4" />
          )}
          {phase === "checking"
            ? t("about.updates.checking")
            : t("about.updates.check")}
        </Button>
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
                  <p className="break-all text-xs text-muted-foreground">
                    {release.package_name}
                    {release.package_size
                      ? ` · ${readableSize(release.package_size)}`
                      : ""}
                  </p>
                  {phase === "downloading" && progress && (
                    <div role="status" aria-live="polite">
                      <div className="mb-1 flex justify-between text-xs text-muted-foreground">
                        <span>{t("about.updates.downloading")}</span>
                        <span>
                          {progress.total
                            ? Math.floor(
                                (progress.downloaded / progress.total) * 100,
                              )
                            : 0}
                          %
                        </span>
                      </div>
                      <progress
                        className="h-2 w-full accent-primary"
                        max={progress.total || 1}
                        value={progress.downloaded}
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
                  {download ? (
                    <div className="space-y-3" data-testid="update-downloaded">
                      <p className="flex items-center gap-2 text-sm font-medium">
                        <CheckCircle2 className="h-4 w-4 text-emerald-600" />
                        {t("about.updates.verified")}
                      </p>
                      <p className="text-xs leading-relaxed text-muted-foreground">
                        {t(`about.updates.install.${installKey}`)}
                      </p>
                      <Button size="sm" onClick={() => void reveal()}>
                        {t("about.updates.showDownload")}
                      </Button>
                    </div>
                  ) : (
                    <Button
                      size="sm"
                      onClick={() => void startDownload()}
                      disabled={busy}
                      data-testid="download-update"
                    >
                      {phase === "downloading" ? (
                        <LoaderCircle className="h-4 w-4 animate-spin" />
                      ) : (
                        <ArrowDownToLine className="h-4 w-4" />
                      )}
                      {t("about.updates.download")}
                    </Button>
                  )}
                </>
              ) : (
                <p className="text-xs leading-relaxed text-muted-foreground">
                  {t("about.updates.noPackage")}
                </p>
              )}
              <Button
                size="sm"
                variant="link"
                className="ml-2 h-auto px-0"
                onClick={() => void openRelease()}
              >
                {t("about.updates.releaseNotes")}
                <ExternalLink className="h-3.5 w-3.5" />
              </Button>
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
    </section>
  );
}
