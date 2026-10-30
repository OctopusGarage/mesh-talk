import { useTranslation } from "react-i18next";
import { Image, ImageOff } from "lucide-react";
import { humanSize } from "@/lib/format";
import type { FileStatus } from "@/lib/types";

export function ReceiveProgress({
  status,
  presentation = "file",
  size,
  preparing = false,
}: {
  status: FileStatus | undefined;
  presentation?: "file" | "image";
  size?: number;
  preparing?: boolean;
}) {
  const { t } = useTranslation();
  const total = status?.total ?? 0;
  const done = status?.done ?? 0;
  const hasProgress =
    Number.isFinite(done) && Number.isFinite(total) && done >= 0 && total > 0;
  const ratio = hasProgress ? Math.min(1, done / total) : 0;
  const percent = hasProgress ? Math.floor(ratio * 100) : undefined;
  const receiving = hasProgress && done > 0;

  if (presentation === "image") {
    const terminalKey = status?.unavailable
      ? "files.unavailable"
      : status?.consent === "pending"
        ? "files.approvalRequired"
        : status?.consent === "declined"
          ? "files.declined"
          : undefined;
    const imagePreparing = preparing || status?.ready || ratio === 1;
    const phase = t(
      terminalKey ??
        (imagePreparing
          ? "transfer.imagePreparing"
          : receiving
            ? "transfer.imageReceiving"
            : "transfer.imageWaiting"),
    );
    const transferring = !terminalKey && !imagePreparing;
    const showRing = transferring && receiving;
    const PhotoIcon =
      status?.unavailable || status?.consent === "declined" ? ImageOff : Image;
    return (
      <div className="flex min-w-0 max-w-full flex-col items-center gap-3 text-center text-card-foreground [overflow-wrap:anywhere]">
        <span className="sr-only" role="status" aria-live="polite">
          {phase}
        </span>
        <div
          role={transferring ? "progressbar" : undefined}
          aria-label={transferring ? phase : undefined}
          aria-valuemin={transferring ? 0 : undefined}
          aria-valuemax={transferring ? 100 : undefined}
          aria-valuenow={transferring ? percent : undefined}
          className="relative flex h-12 w-12 items-center justify-center"
        >
          {showRing && (
            <svg
              width="48"
              height="48"
              viewBox="0 0 48 48"
              fill="none"
              aria-hidden="true"
              className="absolute inset-0 -rotate-90"
            >
              <circle
                cx="24"
                cy="24"
                r="22.5"
                stroke="currentColor"
                strokeWidth="3"
                className="text-muted-foreground/25"
              />
              <circle
                cx="24"
                cy="24"
                r="22.5"
                pathLength="100"
                stroke="currentColor"
                strokeWidth="3"
                strokeLinecap="round"
                strokeDasharray="100"
                strokeDashoffset={100 - ratio * 100}
                className="text-signal transition-[stroke-dashoffset] duration-[160ms] ease-out motion-reduce:transition-none"
              />
            </svg>
          )}
          <PhotoIcon
            size={showRing ? 18 : 24}
            className="text-muted-foreground"
            aria-hidden="true"
          />
        </div>
        <div className="min-w-0 max-w-full space-y-1">
          <p className="text-xs font-medium tabular-nums">
            {showRing
              ? t("transfer.imageReceivingProgress", { percent })
              : phase}
          </p>
          <p className="text-[11px] text-muted-foreground">
            {size !== undefined && Number.isFinite(size) && size > 0
              ? t("transfer.imageDetails", { size: humanSize(size) })
              : t("transfer.image")}
          </p>
        </div>
      </div>
    );
  }

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
  return (
    <div
      className="mt-2"
      role="status"
      aria-label={
        receiving
          ? t("transfer.receivingProgress", { percent })
          : t("transfer.waiting")
      }
    >
      <div className="mb-1 flex items-center justify-between gap-3 text-[11px] opacity-75">
        <span>
          {receiving ? t("transfer.receiving") : t("transfer.waiting")}
        </span>
        {hasProgress && (
          <span className="font-mono tabular-nums">{percent}%</span>
        )}
      </div>
      <div
        role="progressbar"
        aria-label={t(receiving ? "transfer.receiving" : "transfer.waiting")}
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
