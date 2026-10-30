import { useMemo, useState } from "react";
import { Check, CheckCheck, Inbox, Loader2, RefreshCw } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { captureChatOwnership, useChat } from "@/store/chat";
import { useAuth } from "@/store/auth";
import { useContactPolicy } from "@/store/contactPolicy";
import { useUnreadBadge } from "@/store/unreadBadge";
import { unreadSources } from "./unreadSources";

/** One review path for every app badge count, including sources outside the sidebar. */
export function UnreadMessages({
  compact = false,
}: { compact?: boolean } = {}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [limit, setLimit] = useState(50);
  const [refreshing, setRefreshing] = useState(false);
  const unread = useChat((s) => s.unread);
  const pendingUnknownUnread = useChat((s) => s.pendingUnknownUnread);
  const accounts = useChat((s) => s.accounts);
  const channels = useChat((s) => s.channels);
  const favorites = useChat((s) => s.favorites);
  const ready = useChat((s) => s.ready);
  const ownAccount = useChat((s) => s.myAccountId);
  const unreadOwner = useChat((s) => s.unreadOwner);
  const owner = useAuth((s) => s.user?.id);
  const canClear = Boolean(ownAccount && owner && owner === unreadOwner);
  const hiddenContacts = useContactPolicy((s) => s.contacts);
  const badgeError = useUnreadBadge((s) => s.error);
  const retryBadge = useUnreadBadge((s) => s.retry);
  const sources = useMemo(
    () =>
      unreadSources(
        {
          unread,
          pendingUnknownUnread,
          accounts,
          channels,
          favorites,
          myAccountId: ownAccount,
        },
        hiddenContacts,
      ),
    [
      unread,
      pendingUnknownUnread,
      accounts,
      channels,
      favorites,
      hiddenContacts,
      ownAccount,
    ],
  );
  const total = sources.reduce((sum, source) => sum + source.count, 0);
  const filtered = sources.filter((source) =>
    `${source.name} ${source.id}`
      .toLocaleLowerCase()
      .includes(query.trim().toLocaleLowerCase()),
  );
  const refresh = async () => {
    if (refreshing) return;
    const lease = captureChatOwnership();
    if (!lease.current()) return;
    setRefreshing(true);
    try {
      await useChat.getState().refreshRoster();
      if (lease.current()) retryBadge();
    } finally {
      if (lease.current()) setRefreshing(false);
    }
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(value) => {
        setOpen(value);
        if (value) {
          setQuery("");
          setLimit(50);
        }
      }}
    >
      <DialogTrigger asChild>
        <button
          type="button"
          data-testid="unread-review-trigger"
          title={t("unread.title")}
          className="sidebar-tool flex min-h-11 min-w-0 w-full items-center gap-2 rounded-md px-2 text-left text-[13px] text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <Inbox aria-hidden="true" className="h-4 w-4 shrink-0" />
          <span
            className="min-w-0 flex-1 truncate"
            aria-hidden={compact || undefined}
          >
            {t(compact ? "unread.shortLabel" : "unread.title")}
          </span>
          {compact && <span className="sr-only">{t("unread.title")}</span>}
          {total > 0 && (
            <Badge
              className="shrink-0 tabular-nums"
              aria-label={t("conversation.unreadSummary", { count: total })}
            >
              {total > 999 ? "999+" : total.toLocaleString()}
            </Badge>
          )}
          {badgeError && (
            <span
              className="h-1.5 w-1.5 shrink-0 rounded-full bg-attention"
              aria-label={t("unread.badgeFailed")}
            />
          )}
        </button>
      </DialogTrigger>
      <DialogContent data-testid="unread-review" className="max-w-lg gap-0 p-0">
        <DialogHeader className="px-5 pb-4 pt-5 pr-12">
          <DialogTitle>{t("unread.title")}</DialogTitle>
          <DialogDescription>{t("unread.description")}</DialogDescription>
        </DialogHeader>
        {badgeError && (
          <div
            data-testid="unread-badge-error"
            role="status"
            className="mx-5 mb-4 rounded-md bg-muted px-3 py-2 text-xs text-muted-foreground"
          >
            <p>{t("unread.badgeFailed")}</p>
            <details className="mt-1">
              <summary className="cursor-pointer">
                {t("unread.details")}
              </summary>
              <p className="mt-1 max-h-20 overflow-auto break-words">
                {badgeError}
              </p>
            </details>
            <Button
              variant="link"
              size="sm"
              className="h-auto px-0 pt-2"
              onClick={retryBadge}
            >
              {t("unread.retryBadge")}
            </Button>
          </div>
        )}
        {sources.length > 0 && (
          <div className="px-5 pb-3">
            <Input
              value={query}
              placeholder={t("unread.search")}
              aria-label={t("unread.search")}
              onChange={(event) => {
                setQuery(event.target.value);
                setLimit(50);
              }}
            />
          </div>
        )}
        <div
          className="max-h-[min(50vh,24rem)] overflow-y-auto border-y px-3"
          tabIndex={0}
          aria-label={t("unread.title")}
        >
          {sources.length === 0 ? (
            <div role="status" className="px-2 py-10 text-center">
              <CheckCheck
                aria-hidden="true"
                className="mx-auto mb-3 h-5 w-5 text-signal"
              />
              <p className="text-sm font-medium">{t("unread.empty")}</p>
              <p className="mt-1 text-xs text-muted-foreground">
                {t("unread.emptyDetail")}
              </p>
            </div>
          ) : filtered.length === 0 ? (
            <p
              role="status"
              className="px-2 py-8 text-center text-sm text-muted-foreground"
            >
              {t("unread.noResults")}
            </p>
          ) : (
            <ul className="divide-y">
              {filtered.slice(0, limit).map((source) => {
                const name =
                  source.id === ownAccount
                    ? t("personal.title")
                    : source.name ||
                      `${t(source.sender ? "unread.unknownSender" : "unread.unavailable")} · ${source.id.slice(0, 12)}`;
                return (
                  <li key={source.key} className="flex items-center gap-2 py-2">
                    <div className="min-w-0 flex-1 px-2 py-1">
                      {source.conversation ? (
                        <button
                          type="button"
                          disabled={!ready}
                          className="block max-w-full rounded-sm text-left text-sm font-medium text-foreground hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50"
                          onClick={() => {
                            setOpen(false);
                            void useChat
                              .getState()
                              .open({ ...source.conversation!, name });
                          }}
                        >
                          <span className="block truncate" title={name}>
                            {name}
                          </span>
                        </button>
                      ) : (
                        <p
                          className="truncate text-sm font-medium"
                          title={name}
                        >
                          {name}
                        </p>
                      )}
                      <p
                        className="mt-0.5 truncate text-xs text-muted-foreground"
                        title={source.id}
                      >
                        {source.sender
                          ? t("unread.pendingDetail")
                          : source.hidden
                            ? t("unread.hidden")
                            : source.conversation
                              ? t(
                                  source.conversation.kind === "channel"
                                    ? "unread.channel"
                                    : "unread.direct",
                                )
                              : t("unread.unavailableDetail")}
                      </p>
                    </div>
                    <span className="shrink-0 text-xs tabular-nums text-muted-foreground">
                      {source.count.toLocaleString()}
                    </span>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="h-11 w-11 shrink-0"
                      disabled={!canClear}
                      data-testid={`clear-unread-${source.key}`}
                      title={t("unread.clearSource", { name })}
                      aria-label={t("unread.clearSource", { name })}
                      onClick={() => {
                        if (source.sender)
                          useChat
                            .getState()
                            .dismissPendingUnread(source.sender);
                        else
                          useChat.getState().markConversationRead(source.key);
                      }}
                    >
                      <Check aria-hidden="true" className="h-4 w-4" />
                    </Button>
                  </li>
                );
              })}
            </ul>
          )}
          {filtered.length > limit && (
            <Button
              variant="ghost"
              className="mb-2 w-full"
              onClick={() => setLimit((value) => value + 50)}
            >
              {t("unread.showMore")}
            </Button>
          )}
        </div>
        <div className="flex flex-wrap items-center justify-between gap-2 px-5 py-3">
          <Button
            variant="ghost"
            size="sm"
            className="gap-2"
            disabled={!ready || refreshing}
            onClick={() => void refresh()}
          >
            {refreshing ? (
              <Loader2
                aria-hidden="true"
                className="h-4 w-4 animate-spin motion-reduce:animate-none"
              />
            ) : (
              <RefreshCw aria-hidden="true" className="h-4 w-4" />
            )}
            {t(refreshing ? "unread.refreshing" : "unread.refresh")}
          </Button>
          <Button
            variant="secondary"
            size="sm"
            disabled={!canClear || total === 0}
            onClick={() => useChat.getState().markAllRead()}
          >
            {t("unread.markAllRead")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
