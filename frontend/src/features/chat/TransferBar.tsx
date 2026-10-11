import { useTranslation } from "react-i18next";
import { useTransfer } from "@/store/transfers";

/** A refined slim progress treatment for an in-flight file transfer keyed by
 *  `transferKey` (the per-file conv id for a save, or the optimistic message key for a
 *  send). Renders nothing when there is no active transfer for the key. Reads ONLY the
 *  transfers store, so its ~10/s updates don't re-render the surrounding message list. */
export function TransferBar({
  transferKey,
  waitingDirection,
}: {
  transferKey: string | undefined;
  waitingDirection?: "prepare" | "send";
}) {
  const { t } = useTranslation();
  const tr = useTransfer(transferKey);
  if (tr?.delivered || (!tr && !waitingDirection)) return null;
  const direction = tr?.direction ?? waitingDirection;
  const done = tr?.done ?? 0;
  const total = tr?.total ?? 0;
  const pct =
    total > 0 ? Math.min(99, Math.max(0, Math.floor((done / total) * 100))) : 0;
  const verb =
    direction === "prepare"
      ? t("transfer.preparing")
      : direction === "send"
        ? tr?.consent === "declined"
          ? t("transfer.recipientDeclined")
          : tr?.consent === "pending"
            ? t("transfer.awaitingApproval")
            : tr && tr.total > 0 && tr.done >= tr.total
              ? t("transfer.confirming")
              : tr?.done === 0
                ? t("transfer.awaitingRecipient")
                : t("transfer.sending")
        : t("transfer.saving");
  const eta =
    direction === "send" && tr?.etaSeconds != null
      ? tr.etaSeconds < 60
        ? t("transfer.remainingSeconds", {
            seconds: Math.max(1, tr.etaSeconds),
          })
        : t("transfer.remainingMinutes", {
            minutes: Math.ceil(tr.etaSeconds / 60),
          })
      : null;
  return (
    <div className="px-3 py-1.5">
      <div className="mb-1 flex justify-between text-xs">
        <span className="text-muted-foreground">{verb}</span>
        <span className="font-mono text-signal tabular-nums">{pct}%</span>
      </div>
      {eta && (
        <div className="mb-1 text-[11px] text-muted-foreground">{eta}</div>
      )}
      <span className="sr-only" role="status" aria-live="polite">
        {verb} {Math.floor(pct / 10) * 10}%
      </span>
      <div
        role="progressbar"
        aria-label={verb}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
        className="h-1 w-full overflow-hidden rounded-full bg-muted"
      >
        <div
          className="h-full w-full origin-left rounded-full bg-signal"
          style={{
            transform: `scaleX(${total > 0 ? Math.max(0, Math.min(1, done / total)) : 0})`,
          }}
        />
      </div>
    </div>
  );
}
