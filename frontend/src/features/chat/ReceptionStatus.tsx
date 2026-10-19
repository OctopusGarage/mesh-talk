import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

/** One quiet local summary; the virtualized log itself is deliberately not live. */
export function ReceptionStatus({
  conversation,
  unread,
  earlier,
  onJump,
  recovery,
  unavailable = false,
  onMarkRead,
  overflow = 0,
}: {
  conversation: string;
  unread: string[];
  earlier: string[];
  onJump: (id: string) => void;
  unavailable?: boolean;
  onMarkRead?: () => void;
  overflow?: number;
  recovery?: {
    phase: "catching_up" | "waiting" | "idle";
    received: number;
    checkedAt?: number;
  };
}) {
  const { t } = useTranslation();
  const [announced, setAnnounced] = useState({
    conversation: "",
    count: 0,
    sequence: 0,
  });
  const batch = useRef({
    conversation,
    seen: new Set<string>(),
    count: unread.length,
    timer: 0,
  });
  useEffect(() => {
    if (batch.current.conversation !== conversation) {
      window.clearTimeout(batch.current.timer);
      batch.current = { conversation, seen: new Set(), count: 0, timer: 0 };
    }
    const current = batch.current;
    current.count = unread.length;
    const additions = unread.filter((id) => !current.seen.has(id));
    additions.forEach((id) => current.seen.add(id));
    // Bound this transient announcement cache independently of the persisted ledger.
    while (current.seen.size > 4096)
      current.seen.delete(current.seen.values().next().value!);
    if (additions.length && !current.timer) {
      current.timer = window.setTimeout(() => {
        current.timer = 0;
        setAnnounced((previous) => ({
          conversation,
          count: current.count,
          sequence: previous.sequence + 1,
        }));
      }, 600);
    }
  }, [conversation, unread]);
  useEffect(
    () => () => {
      window.clearTimeout(batch.current.timer);
      batch.current.timer = 0;
      batch.current.seen.clear();
    },
    [],
  );
  const firstEarlier = earlier.find((id) => unread.includes(id));
  const firstNew = unread.find((id) => !earlier.includes(id));
  return (
    <>
      <div
        role="status"
        aria-live="polite"
        aria-atomic="true"
        className="sr-only"
        data-testid="reception-announcement"
      >
        {announced.conversation === conversation && announced.count > 0 && (
          <span key={announced.sequence}>
            {t("conversation.unreadSummary", { count: announced.count })}
          </span>
        )}
      </div>
      {unavailable && (
        <div
          role="status"
          className="flex flex-wrap items-center justify-center gap-2 px-5 py-2 text-xs text-muted-foreground"
          data-testid="unread-unavailable"
        >
          <span>{t("conversation.unreadUnavailable")}</span>
          <button
            type="button"
            className="rounded-sm text-signal underline-offset-4 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring"
            onClick={onMarkRead}
          >
            {t("conversation.markConversationRead")}
          </button>
        </div>
      )}
      {!unavailable && overflow > 0 && (
        <div
          className="flex flex-wrap items-center justify-center gap-2 px-5 py-2 text-xs text-muted-foreground"
          data-testid="unread-overflow"
        >
          <span>{t("conversation.moreUnread")}</span>
          <button
            type="button"
            className="rounded-sm text-signal underline-offset-4 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring"
            onClick={onMarkRead}
          >
            {t("conversation.markConversationRead")}
          </button>
        </div>
      )}
      {(firstEarlier ||
        firstNew ||
        recovery?.checkedAt !== undefined ||
        (recovery && recovery.phase !== "idle")) && (
        <div
          className="flex flex-wrap items-center justify-center gap-x-4 gap-y-1 border-b px-5 py-1.5 text-xs text-muted-foreground"
          data-testid="reception-status"
        >
          {recovery && recovery.phase !== "idle" && (
            <span role="status" aria-live="polite">
              {t(
                recovery.phase === "catching_up"
                  ? "conversation.catchingUp"
                  : "conversation.waitingForPeer",
              )}
            </span>
          )}
          {recovery?.phase === "idle" && recovery.checkedAt !== undefined && (
            <span
              data-testid="peer-check-observation"
              title={t("conversation.peerCheckedDetail", {
                time: new Date(recovery.checkedAt).toLocaleString(),
              })}
            >
              {t("conversation.peerChecked")}
            </span>
          )}
          {firstEarlier && (
            <button
              type="button"
              className="rounded-sm text-signal underline-offset-4 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring"
              data-testid="earlier-messages"
              onClick={() => onJump(firstEarlier)}
            >
              {t("conversation.earlierMessages")}
            </button>
          )}
          {firstNew && (
            <button
              type="button"
              className="rounded-sm text-signal underline-offset-4 hover:underline focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring"
              data-testid="new-messages"
              onClick={() => onJump(firstNew)}
            >
              {t("conversation.newMessages")}
            </button>
          )}
        </div>
      )}
    </>
  );
}
