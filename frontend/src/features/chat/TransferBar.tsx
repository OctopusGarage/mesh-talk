import { AnimatePresence, motion } from "framer-motion";
import { ease, useMotionOK } from "@/lib/motion";
import { useTranslation } from "react-i18next";
import { useTransfer } from "@/store/transfers";

/** A refined slim progress treatment for an in-flight file transfer keyed by
 *  `transferKey` (the per-file conv id for a save, or the optimistic message key for a
 *  send). Renders nothing when there is no active transfer for the key. Reads ONLY the
 *  transfers store, so its ~10/s updates don't re-render the surrounding message list. */
export function TransferBar({
  transferKey,
  waitingDirection,
  variant = "default",
  idleLabel,
}: {
  transferKey: string | undefined;
  waitingDirection?: "prepare" | "send";
  variant?: "default" | "compact" | "media";
  idleLabel?: string;
}) {
  const { t } = useTranslation();
  const tr = useTransfer(transferKey);
  const motionOK = useMotionOK();
  const active = !tr?.delivered && (!!tr || !!waitingDirection);
  const direction = tr?.direction ?? waitingDirection;
  const done = tr?.done ?? 0;
  const total = tr?.total ?? 0;
  const hasProgress =
    Number.isFinite(done) && Number.isFinite(total) && done >= 0 && total > 0;
  const pct = hasProgress
    ? Math.min(100, Math.floor((done / total) * 100))
    : undefined;
  const verb =
    direction === "prepare"
      ? t("transfer.preparing")
      : direction === "send"
        ? tr?.consent === "declined"
          ? t("transfer.recipientDeclined")
          : tr?.consent === "pending"
            ? t("transfer.awaitingApproval")
            : hasProgress && done >= total
              ? t("transfer.confirming")
              : tr?.done === 0
                ? t("transfer.awaitingRecipient")
                : t("transfer.sending")
        : t("transfer.saving");
  const eta =
    direction === "send" &&
    hasProgress &&
    done > 0 &&
    done < total &&
    tr?.etaSeconds != null &&
    Number.isFinite(tr.etaSeconds) &&
    tr.etaSeconds >= 0
      ? tr.etaSeconds < 60
        ? t("transfer.remainingSeconds", {
            seconds: Math.max(1, tr.etaSeconds),
          })
        : t("transfer.remainingMinutes", {
            minutes: Math.ceil(tr.etaSeconds / 60),
          })
      : null;
  if (!active && idleLabel)
    return (
      <span className="font-mono text-[11px] tabular-nums opacity-70">
        {idleLabel}
      </span>
    );
  const compact = variant !== "default";
  return (
    <AnimatePresence initial={false}>
      {active && (
        <motion.div
          key={transferKey}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: motionOK ? 0.12 : 0.08, ease }}
          className={
            variant === "media"
              ? "rounded-full border border-white/20 bg-background/90 px-3 py-2 text-foreground shadow-sm backdrop-blur-md"
              : compact
                ? "min-w-0 py-0.5"
                : "px-3 py-1.5"
          }
        >
          <div className="mb-1 flex flex-wrap items-center justify-between gap-x-3 gap-y-0.5 text-[11px]">
            <span
              title={verb}
              className={
                variant === "media"
                  ? "min-w-0 flex-1 truncate text-muted-foreground"
                  : "text-muted-foreground"
              }
            >
              {verb}
            </span>
            {compact && eta && (
              <span className="text-muted-foreground">{eta}</span>
            )}
            {pct !== undefined && (
              <span className="font-mono text-signal tabular-nums">{pct}%</span>
            )}
          </div>
          {eta && !compact && (
            <div className="mb-1 text-[11px] text-muted-foreground">{eta}</div>
          )}
          <span className="sr-only" role="status" aria-live="polite">
            {verb}
            {pct !== undefined ? ` ${Math.floor(pct / 10) * 10}%` : ""}
          </span>
          <div
            role="progressbar"
            aria-label={verb}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={pct}
            className={
              compact
                ? "h-0.5 w-full overflow-hidden rounded-full bg-muted"
                : "h-1 w-full overflow-hidden rounded-full bg-muted"
            }
          >
            <div
              className="transfer-progress h-full w-full origin-left rounded-full bg-signal"
              style={{
                transform: `scaleX(${hasProgress ? Math.min(1, done / total) : 0})`,
              }}
            />
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
