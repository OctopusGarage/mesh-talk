import { useTranslation } from "react-i18next";
import type { FileStatus } from "@/lib/types";

export function ReceiveProgress({
  status,
}: {
  status: FileStatus | undefined;
}) {
  const { t } = useTranslation();
  if (status?.ready) return null;
  const total = status?.total ?? 0;
  const done = status?.done ?? 0;
  const percent =
    total > 0 ? Math.min(100, Math.floor((done / total) * 100)) : 0;
  return (
    <div
      className="mt-2"
      role="status"
      aria-label={
        total > 0
          ? t("transfer.receivingProgress", { percent })
          : t("transfer.waiting")
      }
    >
      <div className="mb-1 flex items-center justify-between gap-3 text-[11px] opacity-75">
        <span>
          {total > 0 ? t("transfer.receiving") : t("transfer.waiting")}
        </span>
        {total > 0 && (
          <span className="font-mono tabular-nums">{percent}%</span>
        )}
      </div>
      <div
        role="progressbar"
        aria-label={t("transfer.receiving")}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={total > 0 ? percent : undefined}
        className="h-1 w-full overflow-hidden rounded-full bg-current/15"
      >
        <div
          className="h-full origin-left bg-current transition-transform duration-300"
          style={{ transform: `scaleX(${total > 0 ? done / total : 0})` }}
        />
      </div>
    </div>
  );
}
