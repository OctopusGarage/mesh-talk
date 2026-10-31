import { useEffect, useState } from "react";
import { HardDrive } from "lucide-react";
import { useTranslation } from "react-i18next";
import { settings } from "@/lib/api";
import type { HistoryStorageUsage } from "@/lib/api/settings";
import { readWithDeadline } from "@/lib/readDeadline";
import { Button } from "@/components/ui/button";
import { SettingsRow } from "./primitives";

const REVIEW_BYTES = 1024 ** 3;
function formatBytes(bytes: number, language: string) {
  const index = Math.min(
    4,
    Math.max(0, Math.floor(Math.log2(Math.max(1, bytes)) / 10)),
  );
  return `${new Intl.NumberFormat(language, { maximumFractionDigits: 1 }).format(bytes / 1024 ** index)} ${["B", "KiB", "MiB", "GiB", "TiB"][index]}`;
}
export function HistoryStorageSummary({
  identity,
}: {
  identity: string | null;
}) {
  const { t, i18n } = useTranslation();
  const [revision, setRevision] = useState(0);
  const [result, setResult] = useState<{
    identity: string;
    revision: number;
    usage?: HistoryStorageUsage;
    failed?: boolean;
  } | null>(null);
  useEffect(() => {
    if (identity === null) return;
    let current = true;
    void readWithDeadline(() => settings.historyUsage()).then(
      (usage) => {
        if (current) setResult({ identity, revision, usage });
      },
      () => {
        if (current) setResult({ identity, revision, failed: true });
      },
    );
    return () => {
      current = false;
    };
  }, [identity, revision]);
  if (identity === null) return null;
  const view =
    result?.identity === identity && result.revision === revision
      ? result
      : null;
  const usage = view?.usage;
  const size = usage ? formatBytes(usage.bytes, i18n.language) : null;
  return (
    <SettingsRow
      icon={<HardDrive className="h-4 w-4" />}
      title={t("settings.storageUsage")}
      desc={
        <>
          <span>{t("settings.storageUsageDesc")}</span>
          {usage && usage.bytes >= REVIEW_BYTES && (
            <p className="mt-1 font-medium text-foreground">
              {t("settings.storageReview")}
            </p>
          )}
        </>
      }
      control={
        <div
          className="flex max-w-full flex-wrap items-center gap-2"
          aria-live="polite"
        >
          <span className="text-[13px] tabular-nums">
            {usage
              ? usage.complete
                ? size
                : t("settings.storageAtLeast", { size })
              : view?.failed
                ? t("settings.storageUnavailable")
                : t("common.loading")}
          </span>
          {view && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setRevision((value) => value + 1)}
            >
              {t(
                view.failed
                  ? "contactVisibility.retry"
                  : "settings.storageRefresh",
              )}
            </Button>
          )}
        </div>
      }
    />
  );
}
