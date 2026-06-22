import { useTranslation } from "react-i18next";
import { useTransfer } from "@/store/transfers";

/** A refined slim progress treatment for an in-flight file transfer keyed by
 *  `transferKey` (the per-file conv id for a save, or the conversation target for a
 *  send). Renders nothing when there is no active transfer for the key. Reads ONLY the
 *  transfers store, so its ~10/s updates don't re-render the surrounding message list. */
export function TransferBar({
  transferKey,
}: {
  transferKey: string | undefined;
}) {
  const { t } = useTranslation();
  const tr = useTransfer(transferKey);
  if (!tr || tr.total === 0) return null;
  const pct = Math.min(100, Math.round((tr.done / tr.total) * 100));
  const verb =
    tr.direction === "send" ? t("transfer.sending") : t("transfer.saving");
  return (
    <div className="px-3 py-1.5">
      <div className="mb-1 flex justify-between text-xs">
        <span className="text-muted-foreground">{verb}</span>
        <span className="font-mono text-signal">{pct}%</span>
      </div>
      <div className="h-1 w-full overflow-hidden rounded-full bg-muted">
        <div
          className="h-full rounded-full bg-signal transition-[width] duration-150"
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}
