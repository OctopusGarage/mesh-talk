import { useEffect, useMemo, useRef, useState } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { ChevronDown, Loader2, MessagesSquare, Upload } from "lucide-react";
import { useTranslation } from "react-i18next";
import { IdentityCrest, PresenceDot } from "@/components/identity";
import { Composer } from "./Composer";
import { MessageBubble } from "./MessageBubble";
import { MembersDialog } from "./MembersDialog";
import { VerifyContactDialog } from "./VerifyContactDialog";
import { ConversationHistoryDialog } from "./ConversationHistoryDialog";
import { TransferBar } from "./TransferBar";
import { chat as chatApi } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { formatDay } from "@/lib/format";
import { useMotionOK } from "@/lib/motion";
import { useAuth } from "@/store/auth";
import { convKey, displayName, useChat, type ChatMessage } from "@/store/chat";
import {
  presenceLabel,
  presenceStatus,
  usePresenceFor,
} from "@/store/presence";
import type { ReactionInfo } from "@/lib/types";

function EmptyState() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-4 text-center">
      <div className="relative">
        <div
          aria-hidden
          className="absolute inset-0 -z-10 rounded-2xl bg-signal/15 blur-2xl"
        />
        <div className="flex h-16 w-16 items-center justify-center rounded-2xl border bg-card text-signal shadow-elevation">
          <MessagesSquare className="h-8 w-8" />
        </div>
      </div>
      <div>
        <p className="font-display text-lg font-semibold tracking-tight">
          {t("conversation.noneSelectedTitle")}
        </p>
        <p className="text-sm text-muted-foreground">
          {t("conversation.noneSelectedDesc")}
        </p>
      </div>
    </div>
  );
}

// Shown on first boot while the node's encrypted stores are still being unlocked in the
// background (the heavy argon2 KDF + store opens). The login invoke already returned, so
// this is a non-blocking "two-phase startup" state — not a frozen window.
function UnlockingState() {
  const { t } = useTranslation();
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-4 text-center">
      <div className="relative flex h-16 w-16 items-center justify-center">
        <span
          aria-hidden
          className="absolute inset-0 animate-ping rounded-full bg-signal/20"
        />
        <Loader2 className="h-7 w-7 animate-spin text-signal" />
      </div>
      <div>
        <p className="font-display text-lg font-semibold tracking-tight">
          {t("conversation.unlockingTitle")}
        </p>
        <p className="text-sm text-muted-foreground">
          {t("conversation.unlockingDesc")}
        </p>
      </div>
    </div>
  );
}

/** A quiet centered date separator between days in the message log. */
function DaySeparator({ label }: { label: string }) {
  return (
    <div className="flex items-center gap-3 px-4 py-3">
      <span className="h-px flex-1 bg-border" />
      <span className="font-mono text-[11px] uppercase tracking-wider text-muted-foreground">
        {label}
      </span>
      <span className="h-px flex-1 bg-border" />
    </div>
  );
}

/** DM header — the large IdentityCrest (glyph + name + presence + verified), with the
 *  safety number reachable via the verify dialog rendered alongside in the header. */
function DmHeader({ id, name }: { id: string; name: string }) {
  const { t } = useTranslation();
  const peers = useChat((s) => s.peers);
  const presence = usePresenceFor(id);
  const status = presenceStatus(presence);

  // Verified state for the crest: a single trust read for the active account (not per-row,
  // so no fan-out), refreshed when the conversation or its presenting device changes.
  const fingerprint = peers.find((p) => p.account_id === id)?.user_id ?? "";
  const [verified, setVerified] = useState(false);
  useEffect(() => {
    if (!fingerprint) {
      setVerified(false);
      return;
    }
    let live = true;
    void chatApi
      .getTrust(id, fingerprint)
      .then((tr) => live && setVerified(tr.verified && !tr.fingerprint_changed))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [id, fingerprint]);

  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <IdentityCrest
        id={id}
        name={name}
        verified={verified}
        status={status}
        variant="compact"
      />
      <span className="pl-12 text-xs text-muted-foreground">
        {presenceLabel(presence, t)}
      </span>
    </div>
  );
}

/** Channel header — a mesh/group glyph + name + member count. */
function ChannelHeader({
  id,
  name,
  memberCount,
}: {
  id: string;
  name: string;
  memberCount: number;
}) {
  const { t } = useTranslation();
  const presence = usePresenceFor(id);
  const status = presenceStatus(presence);
  return (
    <div className="flex min-w-0 items-center gap-3">
      <div className="relative">
        <div className="flex h-9 w-9 items-center justify-center rounded-[28%] border border-border bg-secondary text-muted-foreground">
          <MessagesSquare className="h-4 w-4" />
        </div>
        <PresenceDot
          status={status}
          size="md"
          className="absolute -bottom-0.5 -right-0.5"
        />
      </div>
      <div className="min-w-0">
        <div className="truncate font-display font-semibold tracking-tight">
          {name}
        </div>
        <div className="truncate text-xs text-muted-foreground">
          {memberCount
            ? t("conversation.members", { count: memberCount })
            : t("conversation.channel")}
        </div>
      </div>
    </div>
  );
}

// Stable empty references. Returning a fresh `[]` from a zustand selector makes
// useSyncExternalStore read a new snapshot on every render → an infinite render loop
// ("Maximum update depth exceeded") that tears the whole app down to a blank screen.
const NO_MESSAGES: ChatMessage[] = [];
const NO_REACTIONS: ReactionInfo[] = [];

export function ConversationView() {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const active = useChat((s) => s.active);
  const favorites = useChat((s) => s.favorites);
  const send = useChat((s) => s.send);
  const retry = useChat((s) => s.retry);
  const sendFile = useChat((s) => s.sendFile);
  const setError = useChat((s) => s.setError);
  const toggleReaction = useChat((s) => s.toggleReaction);
  const myId = useChat((s) => s.myId);
  const myAccountId = useChat((s) => s.myAccountId);
  const members = useChat((s) => s.members);
  const peers = useChat((s) => s.peers);
  // Map a channel author's device user_id → account id, so the per-message author glyph
  // resolves the account-keyed (propagated/custom) avatar instead of a device id that never matches.
  const accountByDevice = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of peers) if (p.account_id) map.set(p.user_id, p.account_id);
    return map;
  }, [peers]);
  const key = active ? convKey(active) : "";
  const messages = useChat((s) =>
    active ? (s.messages[key] ?? NO_MESSAGES) : NO_MESSAGES,
  );
  const reactions = useChat((s) =>
    active ? (s.reactions[key] ?? NO_REACTIONS) : NO_REACTIONS,
  );
  const loading = useChat((s) => s.loading);
  const ready = useChat((s) => s.ready);
  const myName = useAuth((s) => s.user?.username ?? "");

  const virtuosoRef = useRef<VirtuosoHandle>(null);

  // Native (Tauri) drag-and-drop: dropping file(s) onto the conversation sends them there.
  // The webview drag-drop event is global, so we subscribe once and read the latest
  // active conversation / sendFile through refs (avoids re-subscribing on every change).
  const [dragOver, setDragOver] = useState(false);
  const activeRef = useRef(active);
  const sendFileRef = useRef(sendFile);
  const setErrorRef = useRef(setError);
  activeRef.current = active;
  sendFileRef.current = sendFile;
  setErrorRef.current = setError;
  useEffect(() => {
    const unlisten = getCurrentWebview().onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "over") {
        setDragOver(activeRef.current != null);
      } else if (p.type === "leave") {
        setDragOver(false);
      } else if (p.type === "drop") {
        setDragOver(false);
        if (!activeRef.current) return;
        void (async () => {
          for (const path of p.paths) {
            try {
              await sendFileRef.current(path);
            } catch (e) {
              setErrorRef.current(
                t("composer.couldntOpenFile", { error: errorMessage(e) }),
              );
            }
          }
        })();
      }
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [t]);

  const [replyTo, setReplyTo] = useState<ChatMessage | null>(null);
  // `showJump` reveals the jump-to-bottom button whenever the user has scrolled up.
  // Virtuoso's `followOutput` only sticks to the newest message while already at the
  // bottom, so reading history is never interrupted by an inbound message.
  const [showJump, setShowJump] = useState(false);

  useEffect(() => {
    setReplyTo(null);
    setShowJump(false);
  }, [key]);

  // Message-enter motion: animate ONLY messages that arrive after a conversation's
  // initial load (newly sent/received), never the initial backlog and never on scroll
  // (virtuoso unmounts/remounts rows as they scroll out of the window). `seenKeys` is the
  // baseline of keys already shown; it's READ in render (pure, StrictMode-safe) and only
  // MUTATED in a post-commit effect. `primed` excludes the very first backlog load.
  const rowKey = (m: ChatMessage, i: number) =>
    m.id ?? m.clientId ?? `pending-${i}`;
  const seenKeys = useRef<Set<string>>(new Set());
  const primed = useRef(false);
  useEffect(() => {
    seenKeys.current = new Set();
    primed.current = false;
  }, [key]);
  useEffect(() => {
    // Record every currently-present key as "seen" after commit, and prime after the
    // first non-empty load so subsequent genuinely-new messages animate exactly once.
    messages.forEach((m, i) => seenKeys.current.add(rowKey(m, i)));
    if (!primed.current && messages.length > 0) primed.current = true;
  }, [messages, key]);
  const isFresh = (m: ChatMessage, i: number) =>
    primed.current && !seenKeys.current.has(rowKey(m, i));

  const byId = useMemo(() => {
    const m = new Map<string, ChatMessage>();
    for (const msg of messages) if (msg.id) m.set(msg.id, msg);
    return m;
  }, [messages]);

  const reactionsByTarget = useMemo(() => {
    const m = new Map<string, ReactionInfo[]>();
    for (const r of reactions) {
      const arr = m.get(r.target) ?? [];
      arr.push(r);
      m.set(r.target, arr);
    }
    return m;
  }, [reactions]);

  const mentionNames = useMemo(
    () =>
      active?.kind === "channel"
        ? members.map((m) => m.name).filter(Boolean)
        : active
          ? [active.name]
          : [],
    [active, members],
  );

  // Route image bytes (clipboard paste or screenshot) through the normal file-send
  // pipeline: write to a temp file, then send the path.
  const sendImageBytes = async (bytes: Uint8Array, ext: string) => {
    try {
      const path = await chatApi.writeTempFile(Array.from(bytes), ext);
      await sendFile(path);
    } catch (e) {
      setError(t("composer.couldntOpenFile", { error: errorMessage(e) }));
    }
  };

  if (!active) {
    return (
      <main className="flex min-w-0 flex-1 flex-col">
        {ready ? <EmptyState /> : <UnlockingState />}
      </main>
    );
  }

  const isChannel = active.kind === "channel";
  // Reaction `who` is keyed by account id for account conversations, device user-id for channels.
  const selfReactionId = isChannel ? myId : myAccountId;
  // Alias (if set) overrides the conversation's announced name in the header + composer.
  const headerName = displayName(favorites, active.id, active.name);

  return (
    // min-w-0 lets this flex child shrink below its content's intrinsic width, so a wide
    // message/image wraps within the pane instead of pushing it past the window edge (and
    // getting clipped under the shell's overflow-hidden / behind the sidebar).
    <main className="relative flex min-w-0 flex-1 flex-col">
      {dragOver && (
        <div className="pointer-events-none absolute inset-0 z-30 flex flex-col items-center justify-center gap-2 bg-signal/10 backdrop-blur-sm">
          <div className="flex flex-col items-center gap-2 rounded-2xl border-2 border-dashed border-signal bg-background/90 px-8 py-6 shadow-elevation-lg">
            <Upload className="h-8 w-8 text-signal" />
            <p className="text-sm font-medium">
              {t("conversation.dropToSend", { name: headerName })}
            </p>
          </div>
        </div>
      )}
      <header
        data-testid="conversation-header"
        data-tauri-drag-region
        data-titlebar-inset
        className="flex items-center gap-3 border-b px-5 py-3"
      >
        {isChannel ? (
          <ChannelHeader
            id={active.id}
            name={headerName}
            memberCount={members.length}
          />
        ) : (
          <DmHeader id={active.id} name={headerName} />
        )}
        <div className="flex-1" />
        <ConversationHistoryDialog
          conversation={{ ...active, name: headerName }}
        />
        {isChannel ? (
          <MembersDialog />
        ) : (
          active.kind === "account" && (
            <VerifyContactDialog accountId={active.id} name={headerName} />
          )
        )}
      </header>

      <div className="relative flex-1 overflow-hidden">
        {loading && messages.length === 0 && (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("common.loading")}
          </p>
        )}
        {!loading && messages.length === 0 && (
          <p
            data-testid="conversation-empty"
            className="py-8 text-center text-sm text-muted-foreground"
          >
            {t("conversation.noMessages")}
          </p>
        )}
        {messages.length > 0 && (
          <Virtuoso
            ref={virtuosoRef}
            // `key` resets all virtualization state (scroll pos, measured heights) when
            // switching conversations — equivalent to the old `[messages.length, key]` reset.
            key={key}
            data={messages}
            // Announce newly-arriving messages to assistive tech. `polite` so it waits for
            // a pause rather than interrupting; the per-bubble aria-label carries the text.
            role="log"
            aria-live="polite"
            aria-label={t("conversation.messageLog", { name: headerName })}
            className="h-full py-4"
            // Keep a short conversation anchored to the bottom (just above the composer)
            // instead of floating at the top with a large empty gap below it.
            alignToBottom
            // Start pinned to the newest message (chat opens at the bottom).
            initialTopMostItemIndex={messages.length - 1}
            // Stick to the bottom on new messages only while the user is already there
            // (preserves scroll position when reading history / prepending older items).
            followOutput={(isAtBottom) => (isAtBottom ? "auto" : false)}
            atBottomStateChange={(b) => setShowJump(!b)}
            // A little tolerance so "at bottom" isn't lost to sub-pixel rounding.
            atBottomThreshold={48}
            increaseViewportBy={400}
            itemContent={(i, m) => {
              const prev = messages[i - 1];
              // Author name + avatar only earn their place in channels; in a 1:1 DM the
              // conversation header already says who you're talking to, so the per-message
              // author is redundant clutter (and would leak the raw device id).
              const showAuthor =
                isChannel &&
                (!prev || prev.who !== m.who || prev.fromMe !== m.fromMe);
              // A date separator opens each new calendar day (and the very first item).
              const showDay =
                !prev || formatDay(prev.wallClock) !== formatDay(m.wallClock);
              const parent = m.replyTo ? (byId.get(m.replyTo) ?? null) : null;
              return (
                <div className="px-0 pb-0.5">
                  {showDay && <DaySeparator label={formatDay(m.wallClock)} />}
                  <MessageBubble
                    m={m}
                    parent={parent}
                    showAuthor={showAuthor}
                    isChannel={isChannel}
                    authorAvatarId={accountByDevice.get(m.who) ?? m.who}
                    fresh={motionOK && isFresh(m, i)}
                    reactions={m.id ? (reactionsByTarget.get(m.id) ?? []) : []}
                    selfReactionId={selfReactionId}
                    myName={myName}
                    onReply={setReplyTo}
                    onReact={toggleReaction}
                    onRetry={retry}
                  />
                </div>
              );
            }}
            // Stable per-row identity so reactions/edits don't remount unrelated rows.
            computeItemKey={(i, m) => rowKey(m, i)}
          />
        )}
        {showJump && (
          <button
            type="button"
            aria-label={t("conversation.jumpToLatest")}
            onClick={() =>
              virtuosoRef.current?.scrollToIndex({
                index: messages.length - 1,
                behavior: "smooth",
              })
            }
            className="absolute bottom-3 right-4 z-10 rounded-full border bg-background/90 p-2 shadow-md backdrop-blur hover:bg-accent"
          >
            <ChevronDown className="h-4 w-4" />
          </button>
        )}
      </div>

      <TransferBar transferKey={active.id} />

      <Composer
        placeholder={
          isChannel
            ? t("conversation.messageChannel", { name: headerName })
            : t("conversation.messageContact", { name: headerName })
        }
        mentionNames={mentionNames}
        replyTo={replyTo}
        onCancelReply={() => setReplyTo(null)}
        onAttach={async () => {
          try {
            const path = await openFileDialog({ multiple: false });
            if (typeof path === "string") await sendFile(path);
          } catch (e) {
            setError(t("composer.couldntOpenFile", { error: errorMessage(e) }));
          }
        }}
        onSend={(t) => {
          send(t, replyTo?.id ?? null);
          setReplyTo(null);
        }}
        onPasteImage={sendImageBytes}
        onScreenshot={async (hideWindow) => {
          try {
            const bytes = await chatApi.captureScreen(hideWindow);
            // Empty bytes = the user cancelled the capture: send nothing.
            if (bytes.length > 0) await sendImageBytes(bytes, "png");
          } catch (e) {
            const msg = errorMessage(e);
            // The backend returns this sentinel when macOS Screen Recording isn't granted
            // (otherwise screencapture silently yields only the desktop wallpaper).
            setError(
              msg.includes("screen-recording-permission")
                ? t("screenshot.permission")
                : t("screenshot.failed", { error: msg }),
            );
          }
        }}
      />
    </main>
  );
}
