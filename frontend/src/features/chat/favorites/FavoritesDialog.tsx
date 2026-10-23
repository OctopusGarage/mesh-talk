import { FavoriteMessageText } from "./FavoriteMessageText";
import { useEffect, useRef, useState } from "react";
import { Bookmark, ChevronLeft, Pencil, Search, Trash2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  messageFavorites,
  type MessageFavorite,
  type FavoriteMessage,
} from "@/lib/api/messageFavorites";
import { chat } from "@/lib/api";
import { formatDay, formatTime } from "@/lib/format";
import {
  fromHistoryItem,
  sameSearchReference,
} from "@/store/conversationState";
import { captureRuntimeOwner } from "@/store/ownership";
import { useMessageFavorites } from "@/store/messageFavorites";
import { useChat } from "@/store/chat";
import { subscribeSyncInvalidation } from "@/lib/syncInvalidation";
import { errorMessage } from "@/lib/error";
import { FavoriteEditor, type FavoriteDraft } from "./FavoriteEditor";
import { FavoriteAttachment } from "./FavoriteAttachment";

function expired(message: FavoriteMessage) {
  return message.expires_at_ms != null && message.expires_at_ms <= Date.now();
}
function itemDraft(item: MessageFavorite): FavoriteDraft {
  return {
    existing: item,
    source: item.source,
    messages: item.messages.map((message) => ({
      id: message.id,
      who: message.sender_name || message.who,
      text: expired(message) ? "" : message.text,
      wallClock: message.wall_clock,
      expiresAtMs: message.expires_at_ms,
      sticker: message.sticker,
      file:
        expired(message) || !message.file
          ? null
          : { ...message.file, fileConv: message.file.file_conv },
    })),
  };
}
export function FavoritesDialog() {
  const { t } = useTranslation();
  const ready = useChat((s) => s.ready);
  const [open, setOpen] = useState(false);
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          data-testid="open-message-favorites"
          variant="ghost"
          disabled={!ready}
          className="mt-2 w-full justify-start gap-2 text-[13px]"
        >
          <Bookmark className="h-4 w-4" aria-hidden="true" />
          {t("messageFavorites.title")}
        </Button>
      </DialogTrigger>
      {open && <FavoritesContents onClose={() => setOpen(false)} />}
    </Dialog>
  );
}
function FavoritesContents({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const items = useMessageFavorites((s) => s.items);
  const loading = useMessageFavorites((s) => s.loading);
  const error = useMessageFavorites((s) => s.error);
  const hasMore = useMessageFavorites((s) => s.hasMore);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<MessageFavorite | null>(null);
  const [editing, setEditing] = useState<MessageFavorite | null>(null);
  const [removing, setRemoving] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [opening, setOpening] = useState(false);
  const [clock, setClock] = useState(Date.now());
  const lease = useRef(captureRuntimeOwner()).current;
  const actions = useRef(0);
  useEffect(
    () => () => {
      actions.current++;
    },
    [],
  );
  const queryRef = useRef(query);
  queryRef.current = query;
  const refresh = () => void useMessageFavorites.getState().refresh();
  useEffect(() => {
    const timer = setTimeout(() => {
      if (lease.current()) void useMessageFavorites.getState().load(query);
    }, 200);
    return () => clearTimeout(timer);
  }, [query, lease]);
  useEffect(() => {
    const clockTimer = setInterval(() => setClock(Date.now()), 1000);
    return () => clearInterval(clockTimer);
  }, []);
  useEffect(() => {
    const timer = setInterval(() => {
      setClock(Date.now());
      if (lease.current() && !useMessageFavorites.getState().loading)
        void useMessageFavorites.getState().refresh();
    }, 15_000);
    const stop = subscribeSyncInvalidation((change) => {
      if (
        lease.current() &&
        change.owner === lease.owner &&
        change.kind === "state" &&
        !useMessageFavorites.getState().loading
      )
        void useMessageFavorites.getState().refresh();
    });
    return () => {
      clearInterval(timer);
      stop();
    };
  }, [lease]);
  const retireDetailActions = () => {
    actions.current++;
    setOpening(false);
    setRemoving(false);
    return actions.current;
  };
  const remove = async () => {
    if (!selected || removing || !lease.current()) return;
    const request = retireDetailActions();
    const current = () => lease.current() && request === actions.current;
    setRemoving(true);
    setActionError(null);
    try {
      await useMessageFavorites.getState().remove(selected);
      if (current()) {
        setSelected(null);
        setConfirmRemove(false);
      }
    } catch (e) {
      if (current()) setActionError(errorMessage(e));
    } finally {
      if (current()) setRemoving(false);
    }
  };
  const original = async (item: MessageFavorite, message: FavoriteMessage) => {
    if (opening || !lease.current()) return;
    const request = retireDetailActions();
    const current = () => lease.current() && request === actions.current;
    setOpening(true);
    setActionError(null);
    try {
      const reference = {
        kind: item.source.kind,
        target: item.source.target,
        message_id: message.id,
      };
      const result = await chat.searchContext(lease.owner!, reference);
      if (!current()) return;
      if (
        result.status !== "found" ||
        !sameSearchReference(result.reference, reference) ||
        !result.messages.some(
          (row) =>
            row.id === message.id &&
            !row.recalled &&
            (row.expires_at_ms == null || row.expires_at_ms > Date.now()),
        )
      ) {
        setActionError(t("search.contextUnavailable"));
        return;
      }
      await useChat.getState().open(
        {
          kind: item.source.kind,
          id: item.source.target,
          name: item.source_label || item.source.target,
        },
        { reference, context: result.messages.map(fromHistoryItem) },
      );
      if (current()) onClose();
    } catch {
      if (current()) setActionError(t("search.contextUnavailable"));
    } finally {
      if (current()) setOpening(false);
    }
  };
  return (
    <>
      <DialogContent
        className="flex max-h-[85dvh] max-w-3xl flex-col overflow-hidden"
        data-testid="message-favorites"
      >
        <DialogHeader>
          <DialogTitle>{t("messageFavorites.title")}</DialogTitle>
          <DialogDescription>{t("messageFavorites.hint")}</DialogDescription>
        </DialogHeader>
        {selected ? (
          <>
            <div className="flex flex-wrap items-center gap-2 border-b pb-3">
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  retireDetailActions();
                  setSelected(null);
                  setActionError(null);
                  setConfirmRemove(false);
                }}
              >
                <ChevronLeft className="mr-1 h-4 w-4" aria-hidden="true" />
                {t("messageFavorites.back")}
              </Button>
              <div className="flex-1" />
              <Button
                size="sm"
                variant="outline"
                onClick={async () => {
                  // Refresh the exact saved item before editing. A removed remote item
                  // must never be silently restored by an editor holding an old copy.
                  if (!lease.current()) return;
                  const request = retireDetailActions();
                  try {
                    const result = await messageFavorites.inspect(
                      lease.owner!,
                      selected.source,
                    );
                    if (!lease.current() || request !== actions.current) return;
                    if (!result.existing) {
                      setActionError(t("messageFavorites.contentUnavailable"));
                      return;
                    }
                    setEditing(result.existing);
                  } catch (e) {
                    if (lease.current() && request === actions.current)
                      setActionError(errorMessage(e));
                  }
                }}
              >
                <Pencil className="mr-1 h-4 w-4" aria-hidden="true" />
                {t("messageFavorites.edit")}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => setConfirmRemove(true)}
              >
                <Trash2 className="mr-1 h-4 w-4" aria-hidden="true" />
                {t("messageFavorites.remove")}
              </Button>
            </div>
            <div
              className="min-h-0 space-y-4 overflow-y-auto px-1"
              data-expiry-clock={clock}
            >
              {selected.metadata.title && (
                <h3 className="break-words font-display text-lg font-semibold">
                  {selected.metadata.title}
                </h3>
              )}
              {selected.metadata.description && (
                <p
                  className="whitespace-pre-wrap break-words rounded-md bg-accent/50 p-3 text-sm"
                  data-testid="favorite-detail-description"
                >
                  {selected.metadata.description}
                </p>
              )}
              <p className="break-words text-xs text-muted-foreground">
                {selected.source_label || selected.source.target} ·{" "}
                {formatDay(selected.saved_at_ms)}
              </p>
              <ol className="divide-y">
                {selected.messages.map((message, index) => (
                  <li key={message.id} className="space-y-2 py-4">
                    <div className="flex flex-wrap gap-x-2 text-xs text-muted-foreground">
                      <span className="break-all font-medium text-foreground">
                        {message.sender_name || message.who}
                      </span>
                      <time>
                        {formatDay(message.wall_clock)}{" "}
                        {formatTime(message.wall_clock)}
                      </time>
                    </div>
                    {expired(message) ? (
                      <p className="text-sm text-muted-foreground">
                        {t("messageFavorites.contentUnavailable")}
                      </p>
                    ) : (
                      <>
                        {(message.text || message.sticker) && (
                          <FavoriteMessageText
                            text={message.text}
                            sticker={message.sticker}
                            fallback=""
                          />
                        )}
                        {message.file && (
                          <FavoriteAttachment
                            key={message.file.file_conv}
                            file={message.file}
                          />
                        )}
                      </>
                    )}
                    {selected.metadata.notes[message.id] && (
                      <div className="rounded-md bg-accent/50 p-3">
                        <p className="mb-1 text-xs font-medium text-muted-foreground">
                          {t("messageFavorites.noteFor", { index: index + 1 })}
                        </p>
                        <p className="whitespace-pre-wrap break-words text-sm">
                          {selected.metadata.notes[message.id]}
                        </p>
                      </div>
                    )}
                    <Button
                      size="sm"
                      variant="link"
                      className="h-auto px-0 text-xs"
                      disabled={opening || expired(message)}
                      onClick={() => void original(selected, message)}
                    >
                      {t("messageFavorites.viewOriginal")}
                    </Button>
                  </li>
                ))}
              </ol>
            </div>
            {confirmRemove && (
              <div className="space-y-2 border-t pt-3">
                <p className="text-sm">{t("messageFavorites.removeConfirm")}</p>
                <div className="flex gap-2">
                  <Button
                    size="sm"
                    variant="destructive"
                    disabled={removing}
                    onClick={() => void remove()}
                  >
                    {t("messageFavorites.remove")}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={removing}
                    onClick={() => setConfirmRemove(false)}
                  >
                    {t("common.cancel")}
                  </Button>
                </div>
              </div>
            )}
          </>
        ) : (
          <>
            <label className="relative block">
              <Search
                className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-muted-foreground"
                aria-hidden="true"
              />
              <span className="sr-only">{t("messageFavorites.search")}</span>
              <Input
                data-testid="favorites-search"
                className="pl-9"
                value={query}
                maxLength={200}
                onChange={(event) => setQuery(event.target.value)}
                placeholder={t("messageFavorites.search")}
              />
            </label>
            <div className="min-h-0 overflow-y-auto" aria-busy={loading}>
              {error && (
                <div role="alert" className="space-y-2 py-5 text-sm">
                  <p className="break-words text-destructive">{error}</p>
                  <Button variant="outline" onClick={refresh}>
                    {t("common.retry")}
                  </Button>
                </div>
              )}
              {!loading && !error && items.length === 0 && (
                <div className="px-4 py-12 text-center">
                  <Bookmark
                    className="mx-auto mb-3 h-6 w-6 text-muted-foreground"
                    aria-hidden="true"
                  />
                  <p className="font-medium">
                    {t(
                      query.trim()
                        ? "messageFavorites.noResults"
                        : "messageFavorites.empty",
                    )}
                  </p>
                  <p className="mx-auto mt-2 max-w-sm text-sm text-muted-foreground">
                    {t(
                      query.trim()
                        ? "messageFavorites.searchHint"
                        : "messageFavorites.emptyHint",
                    )}
                  </p>
                </div>
              )}
              <ul className="divide-y">
                {items.map((item) => (
                  <li key={item.id}>
                    <button
                      type="button"
                      data-testid="favorite-item"
                      className="block w-full space-y-1.5 rounded-md px-3 py-4 text-left hover:bg-accent/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                      onClick={() => {
                        retireDetailActions();
                        setSelected(item);
                        setActionError(null);
                      }}
                    >
                      <span className="block break-words text-sm font-semibold">
                        {item.metadata.title ||
                          item.source_label ||
                          t("messageFavorites.untitled")}
                      </span>
                      {item.metadata.description && (
                        <span className="line-clamp-2 block whitespace-pre-wrap break-words text-sm">
                          {item.metadata.description}
                        </span>
                      )}
                      <span className="line-clamp-2 block break-words text-xs text-muted-foreground">
                        {item.messages
                          .map((message) =>
                            expired(message)
                              ? t("messageFavorites.contentUnavailable")
                              : message.text || message.file?.name || "",
                          )
                          .filter(Boolean)
                          .join(" · ")}
                      </span>
                      {query.trim() &&
                        Object.entries(item.metadata.notes)
                          .filter(([, note]) =>
                            note
                              .toLocaleLowerCase()
                              .includes(query.trim().toLocaleLowerCase()),
                          )
                          .map(([id, note]) => (
                            <span
                              key={id}
                              className="line-clamp-2 block break-words text-xs text-signal"
                            >
                              {t("messageFavorites.messageNote")}: {note}
                            </span>
                          ))}
                      <span className="block text-xs text-muted-foreground">
                        {t("messageFavorites.messageCount", {
                          count: item.messages.length,
                        })}{" "}
                        · {formatDay(item.saved_at_ms)}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
              {loading && (
                <p
                  role="status"
                  className="py-4 text-center text-sm text-muted-foreground"
                >
                  {t("common.loading")}
                </p>
              )}
              {hasMore && (
                <Button
                  variant="ghost"
                  className="mt-3 w-full"
                  disabled={loading}
                  onClick={() =>
                    void useMessageFavorites.getState().load(query, true)
                  }
                >
                  {t("messageFavorites.loadMore")}
                </Button>
              )}
            </div>
          </>
        )}
        {actionError && (
          <p role="alert" className="break-words text-sm text-destructive">
            {actionError}
          </p>
        )}
      </DialogContent>
      {editing && (
        <FavoriteEditor
          key={`${editing.id}:${editing.revision}`}
          draft={itemDraft(editing)}
          onClose={() => {
            retireDetailActions();
            setEditing(null);
            setSelected(null);
            refresh();
          }}
        />
      )}
    </>
  );
}
