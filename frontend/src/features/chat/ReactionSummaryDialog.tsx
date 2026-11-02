import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import type { ReactionSummaryPage } from "@/lib/types";
import { errorMessage } from "@/lib/error";
import { useChat, type Conversation } from "@/store/chat";
import { captureChatOwner } from "@/store/ownership";
import { readReactionPage } from "@/store/conversationReactions";

/** Replaces each page; inspecting an unusually busy message never accumulates
 * its entire reaction list or author identities in the renderer. */
export function ReactionSummaryDialog({
  conversation,
  target,
  readOnly,
  close,
}: {
  conversation: Conversation;
  target: string;
  readOnly: boolean;
  close: () => void;
}) {
  const { t } = useTranslation();
  const [request, setRequest] = useState<{
    cursor: string | null;
    revision: number;
  }>({ cursor: null, revision: 0 });
  const [page, setPage] = useState<ReactionSummaryPage | null>(null);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    const lease = captureChatOwner(useChat.getState);
    let cancelled = false;
    const current = () => !cancelled && lease.current();
    setBusy(true);
    setError(null);
    setPage(null);
    void (async () => {
      try {
        let cursor = request.cursor;
        while (current()) {
          const next = await readReactionPage(
            lease.owner!,
            conversation,
            [target],
            cursor,
            true,
          );
          if (!current()) return;
          if (next.pending) {
            cursor = next.next;
            await new Promise<void>((resolve) => setTimeout(resolve, 0));
          } else {
            setPage(next);
            return;
          }
        }
      } catch (failure) {
        if (current()) setError(errorMessage(failure));
      } finally {
        if (current()) setBusy(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [conversation, target, request]);
  const restart = () =>
    setRequest((old) => ({ cursor: null, revision: old.revision + 1 }));
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) close();
      }}
    >
      <DialogContent className="max-w-sm" data-testid="reaction-summary-dialog">
        <DialogHeader>
          <DialogTitle>{t("message.reactionsTitle")}</DialogTitle>
          <DialogDescription>
            {t("message.reactionsDescription")}
          </DialogDescription>
        </DialogHeader>
        {busy && <p role="status">{t("common.loading")}</p>}
        {error && (
          <p role="alert" className="break-words text-destructive">
            {error}
          </p>
        )}
        {!busy && !error && page?.items.length === 0 && (
          <p>{t("message.reactionsEmpty")}</p>
        )}
        <div className="flex max-h-72 flex-wrap gap-2 overflow-y-auto">
          {page?.items.map((row) => (
            <button
              key={row.emoji}
              type="button"
              disabled={readOnly || busy}
              aria-label={t(
                row.from_me ? "message.reactionRemove" : "message.reactionAdd",
                { emoji: row.emoji },
              )}
              className="max-w-full break-all rounded-full border border-border px-3 py-1 text-sm"
              onClick={async () => {
                setBusy(true);
                await useChat
                  .getState()
                  .toggleReaction(target, row.emoji, row.from_me);
                restart();
              }}
            >
              {row.emoji}{" "}
              <span className="font-mono tabular-nums">{row.count}</span>
            </button>
          ))}
        </div>
        <div className="flex justify-end gap-2">
          {(error || request.cursor) && (
            <Button variant="outline" disabled={busy} onClick={restart}>
              {t(error ? "message.retry" : "message.reactionsStart")}
            </Button>
          )}
          {page?.next && (
            <Button
              disabled={busy}
              onClick={() =>
                setRequest((old) => ({
                  cursor: page.next,
                  revision: old.revision + 1,
                }))
              }
            >
              {t("message.reactionsNext")}
            </Button>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
