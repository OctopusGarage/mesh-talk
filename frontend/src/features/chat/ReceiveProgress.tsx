import { useTranslation } from "react-i18next";
import type { FileStatus } from "@/lib/types";

export function ReceiveProgress({
  status,
}: {
  status: FileStatus | undefined;
}) {
  const { t } = useTranslation();
  if (status?.ready) return null;
  if (status?.unavailable)
    return (
      <div className="mt-2 text-[11px] opacity-75" role="status">
        {t("files.unavailable")}
      </div>
    );
  if (status?.consent === "pending" || status?.consent === "declined")
    return (
      <div className="mt-2 text-[11px] opacity-75" role="status">
        {t(
          status.consent === "pending"
            ? "files.approvalRequired"
            : "files.declined",
        )}
      </div>
    );
  const total = status?.total ?? 0;
  const done = status?.done ?? 0;
  const hasProgress =
    Number.isFinite(done) && Number.isFinite(total) && done >= 0 && total > 0;
  const ratio = hasProgress ? Math.min(1, done / total) : 0;
  const percent = hasProgress ? Math.floor(ratio * 100) : undefined;
  return (
    <div
      className="mt-2"
      role="status"
      aria-label={
        hasProgress
          ? t("transfer.receivingProgress", { percent })
          : t("transfer.waiting")
      }
    >
      <div className="mb-1 flex items-center justify-between gap-3 text-[11px] opacity-75">
        <span>
          {hasProgress ? t("transfer.receiving") : t("transfer.waiting")}
        </span>
        {hasProgress && (
          <span className="font-mono tabular-nums">{percent}%</span>
        )}
      </div>
      <div
        role="progressbar"
        aria-label={t("transfer.receiving")}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
        className="h-1 w-full overflow-hidden rounded-full bg-current/15"
      >
        <div
          className="h-full origin-left bg-current transition-transform duration-300"
          style={{ transform: `scaleX(${ratio})` }}
        />
      </div>
    </div>
  );
}
