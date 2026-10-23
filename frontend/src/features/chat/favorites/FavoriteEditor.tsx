import { FavoriteMessageText } from "./FavoriteMessageText";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type {
  FavoriteMetadata,
  FavoriteSource,
  MessageFavorite,
} from "@/lib/api/messageFavorites";
import type { ChatMessage } from "@/store/conversationModel";
import { useMessageFavorites } from "@/store/messageFavorites";
import { captureRuntimeOwner } from "@/store/ownership";
import { errorMessage } from "@/lib/error";
import { formatDay, formatTime } from "@/lib/format";

export const noteClass =
  "min-h-20 w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50";
export interface FavoriteDraft {
  source: FavoriteSource;
  messages: Pick<
    ChatMessage,
    "id" | "who" | "text" | "wallClock" | "file" | "expiresAtMs" | "sticker"
  >[];
  existing?: MessageFavorite;
}
export function FavoriteEditor({
  draft,
  onClose,
}: {
  draft: FavoriteDraft;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const lease = useRef(captureRuntimeOwner()).current;
  const mounted = useRef(false);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const [clock, setClock] = useState(Date.now());
  useEffect(() => {
    const timer = setInterval(() => setClock(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  const [existing, setExisting] = useState(draft.existing);
  const [metadata, setMetadata] = useState<FavoriteMetadata>(
    draft.existing?.metadata ?? { title: "", description: "", notes: {} },
  );
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [alreadySaved, setAlreadySaved] = useState(false);
  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (saving || !lease.current()) return;
    setSaving(true);
    setError(null);
    try {
      const result = existing
        ? await useMessageFavorites.getState().edit(existing, metadata)
        : await useMessageFavorites.getState().save(draft.source, metadata);
      if (!lease.current() || !mounted.current) return;
      if (
        !existing &&
        JSON.stringify(result.metadata) !== JSON.stringify(metadata)
      ) {
        setExisting(result);
        // Preserve newly typed remarks on a concurrent duplicate. The next
        // explicit Save edits that item's existing revision with this draft.
        setAlreadySaved(true);
      } else onClose();
    } catch (e) {
      if (lease.current() && mounted.current) setError(errorMessage(e));
    } finally {
      if (lease.current() && mounted.current) setSaving(false);
    }
  };
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !saving) onClose();
      }}
    >
      <DialogContent className="max-w-2xl" data-testid="favorite-editor">
        <DialogHeader>
          <DialogTitle>
            {t(existing ? "messageFavorites.edit" : "messageFavorites.add")}
          </DialogTitle>
          <DialogDescription>
            {t("messageFavorites.editorHint", { count: draft.messages.length })}
          </DialogDescription>
        </DialogHeader>
        <form
          onSubmit={(event) => void submit(event)}
          className="flex min-h-0 flex-col gap-4"
        >
          <div className="max-h-[60dvh] space-y-4 overflow-y-auto px-0.5">
            <label className="block space-y-1.5 text-sm font-medium">
              <span>{t("messageFavorites.itemTitle")}</span>
              <Input
                data-testid="favorite-title"
                value={metadata.title}
                maxLength={200}
                disabled={saving}
                onChange={(event) =>
                  setMetadata({ ...metadata, title: event.target.value })
                }
                placeholder={t("messageFavorites.titlePlaceholder")}
              />
            </label>
            <label className="block space-y-1.5 text-sm font-medium">
              <span>{t("messageFavorites.description")}</span>
              <textarea
                data-testid="favorite-description"
                className={noteClass}
                value={metadata.description}
                maxLength={4000}
                disabled={saving}
                onChange={(event) =>
                  setMetadata({ ...metadata, description: event.target.value })
                }
                placeholder={t("messageFavorites.descriptionPlaceholder")}
              />
            </label>
            <ol className="divide-y rounded-md border px-3">
              {draft.messages.map((message, index) => (
                <li key={message.id ?? index} className="space-y-2 py-3">
                  <div className="flex flex-wrap gap-x-2 text-xs text-muted-foreground">
                    <span className="break-all">{message.who}</span>
                    <time>
                      {formatDay(message.wallClock)}{" "}
                      {formatTime(message.wallClock)}
                    </time>
                  </div>
                  {message.expiresAtMs != null &&
                  message.expiresAtMs <= clock ? (
                    <p className="text-sm text-muted-foreground">
                      {t("messageFavorites.contentUnavailable")}
                    </p>
                  ) : (
                    <FavoriteMessageText
                      text={message.text}
                      sticker={message.sticker}
                      fallback={
                        message.file?.name ||
                        t("messageFavorites.contentUnavailable")
                      }
                    />
                  )}
                  <details
                    open={metadata.notes[message.id!] ? true : undefined}
                  >
                    <summary className="cursor-pointer rounded py-1 text-xs font-medium text-signal focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
                      {t("messageFavorites.messageNote")}
                    </summary>
                    <label className="mt-1 block">
                      <span className="sr-only">
                        {t("messageFavorites.noteFor", { index: index + 1 })}
                      </span>
                      <textarea
                        data-testid={`favorite-note-${index}`}
                        className={noteClass}
                        value={metadata.notes[message.id!] ?? ""}
                        maxLength={1000}
                        disabled={saving}
                        onChange={(event) =>
                          setMetadata({
                            ...metadata,
                            notes: {
                              ...metadata.notes,
                              [message.id!]: event.target.value,
                            },
                          })
                        }
                      />
                    </label>
                  </details>
                </li>
              ))}
            </ol>
          </div>
          {alreadySaved && (
            <p role="status" className="text-sm text-muted-foreground">
              {t("messageFavorites.alreadySaved")}
            </p>
          )}
          {error && (
            <p role="alert" className="break-words text-sm text-destructive">
              {t("messageFavorites.saveFailed", { error })}
            </p>
          )}
          <div className="flex justify-end gap-2 border-t pt-3">
            <Button
              type="button"
              variant="ghost"
              disabled={saving}
              onClick={onClose}
            >
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={saving} data-testid="favorite-save">
              {t(saving ? "messageFavorites.saving" : "common.save")}
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
