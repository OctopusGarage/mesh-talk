import { useEffect, useMemo, useRef, useState } from "react";
import { Virtuoso } from "react-virtuoso";
import { AnimatePresence, motion } from "framer-motion";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  ChevronDown,
  CircleAlert,
  Loader2,
  MessagesSquare,
  Upload,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  IdentityCrest,
  PresenceDot,
  AvatarEditMenu,
} from "@/components/identity";
import { GroupAvatar } from "@/components/GroupAvatar";
import { needsCustomWindowControls } from "@/lib/platform";
import { Composer } from "./Composer";
import {
  messageRowKey,
  useConversationViewport,
} from "./useConversationViewport";
import {
  IMAGE_EXTENSIONS,
  VIDEO_EXTENSIONS,
  isImage,
  isVideo,
} from "./mediaFile";
import { MessageBubble } from "./MessageBubble";
import { MembersDialog } from "./MembersDialog";
import { VerifyContactDialog } from "./VerifyContactDialog";
import { CallButtons } from "./CallDialog";
import { ConversationHistoryDialog } from "./ConversationHistoryDialog";
import { TransferBar } from "./TransferBar";
import { DiagnosticsDialog } from "./DiagnosticsDialog";
import { OfflineConnectDialog } from "./OfflineConnectDialog";
import { HiddenContactsDialog } from "./HiddenContactsDialog";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { chat as chatApi, obs } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { mentionsName } from "@/lib/mentions";
import { formatDay } from "@/lib/format";
import { cn } from "@/lib/utils";
import { ease, useMotionOK } from "@/lib/motion";
import { useAuth } from "@/store/auth";
import { useContactPolicy } from "@/store/contactPolicy";
import {
  convKey,
  displayName,
  useChat,
  captureChatOwnership,
  type ChatMessage,
} from "@/store/chat";
import {
  presenceLabel,
  presenceStatus,
  usePresenceFor,
} from "@/store/presence";
import type { ReactionInfo } from "@/lib/types";

function captureComposer() {
  const lease = captureChatOwnership();
  const active = useChat.getState().active;
  const key = active ? convKey(active) : null;
  return () =>
    lease.current() &&
    key !== null &&
    useChat.getState().active !== null &&
    convKey(useChat.getState().active!) === key;
}

function EmptyState() {
  const { t } = useTranslation();
  const accounts = useChat((s) => s.accounts);
  const channels = useChat((s) => s.channels);
  const hidden = useContactPolicy((s) => s.contacts);
  const policyLoaded = useContactPolicy((s) => s.loaded);
  const policyError = useContactPolicy((s) => s.error);
  const owner = useAuth((s) => s.user?.id);
  const visibleCount =
    channels.length +
    (policyLoaded
      ? accounts.filter((account) => !hidden[account.account_id]).length
      : 0);
  const hasConversations = visibleCount > 0;
  const hiddenOnly = policyLoaded && !hasConversations && accounts.length > 0;
  const policyUnavailable = !policyLoaded && !hasConversations;
  const [guideOpen, setGuideOpen] = useState(false);
  const [connectOpen, setConnectOpen] = useState(false);
  return (
    <div className="conversation-welcome flex flex-1 flex-col items-center justify-center gap-3 px-6 text-center">
      <div className="flex h-11 w-11 items-center justify-center rounded-lg bg-muted text-signal">
        <MessagesSquare className="h-5 w-5" />
      </div>
      <div>
        <p className="font-display text-base font-semibold tracking-tight">
          {t(
            hasConversations
              ? "conversation.noneSelectedTitle"
              : policyUnavailable
                ? policyError
                  ? "contactVisibility.loadError"
                  : "redesign.loadingContacts"
                : hiddenOnly
                  ? "contactVisibility.allHidden"
                  : "redesign.noPeersTitle",
          )}
        </p>
        {hasConversations && (
          <p className="mt-1 text-[13px] leading-5 text-muted-foreground">
            {t("conversation.noneSelectedDesc")}
          </p>
        )}
        {!hasConversations && !hiddenOnly && !policyUnavailable && (
          <p className="mt-1 max-w-sm text-[13px] leading-5 text-muted-foreground">
            {t("redesign.noPeersBody")}
          </p>
        )}
      </div>
      {policyUnavailable && policyError && owner && (
        <button
          type="button"
          onClick={() => void useContactPolicy.getState().load(owner)}
          className="rounded-md px-3 py-2 text-sm font-medium text-signal hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {t("contactVisibility.retry")}
        </button>
      )}
      {hiddenOnly && <HiddenContactsDialog />}
      {!hasConversations && !hiddenOnly && !policyUnavailable && (
        <>
          <Button
            type="button"
            data-testid="empty-connect"
            onClick={() => setConnectOpen(true)}
          >
            {t("redesign.connectToSomeone")}
          </Button>
          <button
            type="button"
            onClick={() => setGuideOpen(true)}
            className="rounded-md px-2 py-1 text-sm font-medium text-signal underline-offset-4 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {t("redesign.connectionHelp")}
          </button>
          <DiagnosticsDialog
            open={guideOpen}
            onOpenChange={setGuideOpen}
            initialTab="help"
          />
          <OfflineConnectDialog
            open={connectOpen}
            onOpenChange={setConnectOpen}
            includeSharedNetwork
          />
        </>
      )}
    </div>
  );
}

// Shown on first boot while the node's encrypted stores are still being unlocked in the
// background (the heavy argon2 KDF + store opens). The login invoke already returned, so
// this is a non-blocking "two-phase startup" state — not a frozen window.
function UnlockingState() {
  const { t } = useTranslation();
  const [slow, setSlow] = useState(false);
  const [guideOpen, setGuideOpen] = useState(false);
  useEffect(() => {
    const id = window.setTimeout(() => setSlow(true), 15_000);
    return () => window.clearTimeout(id);
  }, []);
  return (
    <div
      role="status"
      className="flex flex-1 flex-col items-center justify-center gap-4 px-6 text-center"
    >
      <div className="relative flex h-16 w-16 items-center justify-center">
        <Loader2 className="h-7 w-7 animate-spin text-signal" />
      </div>
      <div>
        <p className="font-display text-lg font-semibold tracking-tight">
          {t("conversation.unlockingTitle")}
        </p>
        <p className="text-sm text-muted-foreground">
          {t("conversation.unlockingDesc")}
        </p>
        {slow && (
          <p className="mx-auto mt-3 max-w-sm text-sm leading-relaxed text-muted-foreground">
            {t("redesign.slowStartup")}
          </p>
        )}
      </div>
      {slow && (
        <>
          <button
            type="button"
            onClick={() => setGuideOpen(true)}
            className="rounded-md px-3 py-2 text-sm font-medium text-signal hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {t("redesign.openConnection")}
          </button>
          <DiagnosticsDialog
            open={guideOpen}
            onOpenChange={setGuideOpen}
            initialTab="help"
          />
        </>
      )}
    </div>
  );
}

function StartupFailureState() {
  const { t } = useTranslation();
  const setError = useChat((s) => s.setError);
  const [guideOpen, setGuideOpen] = useState(false);
  const revealLogs = async () => {
    try {
      await revealItemInDir(await obs.logFile());
    } catch (e) {
      setError(t("diagnostics.actionFailed", { error: errorMessage(e) }));
    }
  };

  return (
    <div
      role="alert"
      className="flex flex-1 flex-col items-center justify-center gap-4 px-6 text-center"
    >
      <div className="flex h-12 w-12 items-center justify-center rounded-lg border border-destructive/30 bg-destructive/5 text-destructive">
        <CircleAlert className="h-6 w-6" aria-hidden="true" />
      </div>
      <div className="max-w-md">
        <p className="font-display text-lg font-semibold tracking-tight">
          {t("conversation.startFailedTitle")}
        </p>
        <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
          {t("conversation.startFailedDesc")}
        </p>
      </div>
      <div className="flex flex-wrap items-center justify-center gap-2">
        <button
          type="button"
          onClick={() => setGuideOpen(true)}
          className="rounded-md bg-signal px-3 py-2 text-sm font-medium text-primary-foreground hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {t("redesign.openConnection")}
        </button>
        <button
          type="button"
          onClick={() => void revealLogs()}
          className="rounded-md px-3 py-2 text-sm font-medium text-signal hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {t("diagnostics.revealLogs")}
        </button>
      </div>
      <DiagnosticsDialog
        open={guideOpen}
        onOpenChange={setGuideOpen}
        initialTab="help"
      />
    </div>
  );
}

/** A quiet centered date separator between days in the message log. */
function DaySeparator({ label }: { label: string }) {
  return (
    <div className="mx-auto flex max-w-[820px] items-center justify-center px-5 py-4">
      <span className="font-mono text-[10px] uppercase tracking-wide text-muted-foreground">
        {label}
      </span>
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
        hideId
        status={status}
        variant="compact"
      />
      <span className="pl-12 text-[11px] text-muted-foreground">
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
        <AvatarEditMenu
          id={id}
          ariaLabel={t("avatar.editGroup")}
          category="group"
        >
          <GroupAvatar channelId={id} size={36} title={name} />
        </AvatarEditMenu>
        <PresenceDot
          status={status}
          size="md"
          className="absolute -bottom-0.5 -right-0.5"
        />
      </div>
      <div className="min-w-0">
        <div
          className="truncate font-display font-semibold tracking-tight"
          title={name}
        >
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
  const admitText = useChat((s) => s.admitText);
  const retry = useChat((s) => s.retry);
  const sendFile = useChat((s) => s.sendFile);
  const setError = useChat((s) => s.setError);
  const toggleReaction = useChat((s) => s.toggleReaction);
  const deleteMessage = useChat((s) => s.deleteMessage);
  const [pendingDelete, setPendingDelete] = useState<{
    key: string;
    id: string;
  } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const recallMessage = useChat((s) => s.recallMessage);
  const sendSticker = useChat((s) => s.sendSticker);
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
  // Resolve a channel author's device user_id → display name, so bubbles show names, not raw
  // ids. Channel members win over the general roster (channel-specific naming).
  const nameByDevice = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of peers) if (p.name) map.set(p.user_id, p.name);
    for (const m of members) if (m.name) map.set(m.user_id, m.name);
    return map;
  }, [peers, members]);
  const key = active ? convKey(active) : "";
  useEffect(() => {
    setPendingDelete(null);
  }, [key]);
  const drafts = useRef(new Map<string, string>());
  const messages = useChat((s) =>
    active ? (s.messages[key] ?? NO_MESSAGES) : NO_MESSAGES,
  );
  const reactions = useChat((s) =>
    active ? (s.reactions[key] ?? NO_REACTIONS) : NO_REACTIONS,
  );
  const loading = useChat((s) => s.loading);
  const historyError = useChat((s) => s.historyError === key);
  const searchTarget = useChat((s) => s.searchTarget);
  const ready = useChat((s) => s.ready);
  const bootFailed = useChat((s) => s.bootFailed);
  const myName = useAuth((s) => s.user?.display_name || s.user?.username || "");

  const viewport = useConversationViewport(
    key,
    messages,
    loading,
    historyError,
    searchTarget,
  );

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
        const lease = captureChatOwnership();
        const key = convKey(activeRef.current);
        const current = () =>
          lease.current() &&
          useChat.getState().active != null &&
          convKey(useChat.getState().active!) === key;
        void (async () => {
          for (const path of p.paths) {
            if (!current()) return;
            try {
              // An image/video dropped here is sent as MEDIA (inline preview) — same as the
              // image button; any other file is a generic attachment. (`isImage`/`isVideo`
              // match the path's extension.)
              const media = isImage(path) || isVideo(path);
              await sendFileRef.current(path, media);
            } catch (e) {
              if (current())
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

  const [reply, setReply] = useState<{
    key: string;
    message: ChatMessage;
  } | null>(null);
  const replyTo = reply?.key === key ? reply.message : null;
  // "Re-edit" a recalled message: a (text, bump-counter) pushed into the composer.
  const [prefill, setPrefill] = useState<{
    key: string;
    text: string;
    n: number;
  } | null>(null);
  // `showJump` reveals the jump-to-bottom button whenever the user has scrolled up.
  // Virtuoso's `followOutput` only sticks to the newest message while already at the
  // bottom, so reading history is never interrupted by an inbound message.
  useEffect(() => {
    setReply(null);
    setPrefill(null);
  }, [key]);

  // Message-enter motion: animate ONLY messages that arrive after a conversation's
  // initial load (newly sent/received), never the initial backlog and never on scroll
  // (virtuoso unmounts/remounts rows as they scroll out of the window). `seenKeys` is the
  // baseline of keys already shown; it's READ in render (pure, StrictMode-safe) and only
  // MUTATED in a post-commit effect. `primed` excludes the very first backlog load.
  const seenKeys = useRef<Set<string>>(new Set());
  const primed = useRef(false);
  useEffect(() => {
    seenKeys.current = new Set();
    primed.current = false;
  }, [key]);
  useEffect(() => {
    // Record every currently-present key as "seen" after commit, and prime after the
    // first non-empty load so subsequent genuinely-new messages animate exactly once.
    messages.forEach((m, i) => seenKeys.current.add(messageRowKey(m, i)));
    if (!primed.current && messages.length > 0) primed.current = true;
  }, [messages, key]);
  const isFresh = (m: ChatMessage, i: number) =>
    primed.current && !seenKeys.current.has(messageRowKey(m, i));

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

  // Route picked/pasted media bytes through the normal file-send pipeline: write to a temp
  // file, then send the path. `name` (the picker has one) is kept so the real filename +
  // extension survive to the chat list and the inline preview.
  const sendImageBytes = async (
    bytes: Uint8Array,
    ext: string,
    name?: string,
  ) => {
    const lease = captureComposer();
    if (!lease()) return;
    try {
      const path = await chatApi.writeTempFile(Array.from(bytes), ext, name);
      if (!lease()) return;
      // Image button / paste / screenshot → media intent (inline preview).
      await sendFile(path, true);
    } catch (e) {
      if (lease())
        setError(t("composer.couldntOpenFile", { error: errorMessage(e) }));
    }
  };

  if (!active) {
    return (
      <main
        className={cn(
          "flex min-w-0 flex-1 flex-col",
          ready && "conversation-canvas",
        )}
      >
        {needsCustomWindowControls() && (
          <div
            aria-hidden
            data-testid="empty-chat-drag-region"
            data-tauri-drag-region
            className="h-10 shrink-0"
          />
        )}
        {ready ? (
          <EmptyState />
        ) : bootFailed ? (
          <StartupFailureState />
        ) : (
          <UnlockingState />
        )}
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
    <main className="conversation-canvas relative flex min-w-0 flex-1 flex-col">
      <AnimatePresence>
        {dragOver && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0, pointerEvents: "none" }}
            transition={{ duration: motionOK ? 0.12 : 0.08, ease }}
            className="pointer-events-none absolute inset-0 z-30 flex flex-col items-center justify-center gap-2 bg-background/80"
          >
            <motion.div
              initial={{
                opacity: 0,
                transform: motionOK ? "translateY(4px)" : "none",
              }}
              animate={{
                opacity: 1,
                transform: motionOK ? "translateY(0px)" : "none",
              }}
              exit={{
                opacity: 0,
                transform: motionOK ? "translateY(4px)" : "none",
                pointerEvents: "none",
              }}
              transition={{ duration: motionOK ? 0.12 : 0.08, ease }}
              className="flex flex-col items-center gap-2 rounded-lg border border-dashed border-signal bg-card px-8 py-6 shadow-elevation"
            >
              <Upload className="h-8 w-8 text-signal" />
              <p className="text-sm font-medium">
                {t("conversation.dropToSend", { name: headerName })}
              </p>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
      <header
        data-testid="conversation-header"
        data-tauri-drag-region
        data-titlebar-inset
        className="flex min-h-[68px] items-center gap-3 border-b bg-card px-5 py-2.5"
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
            <>
              <CallButtons accountId={active.id} name={headerName} />
              <VerifyContactDialog accountId={active.id} name={headerName} />
            </>
          )
        )}
      </header>

      <div className="conversation-log-surface relative flex-1 overflow-hidden">
        {!loading && historyError && (
          <div
            role="alert"
            className={cn(
              "z-10 flex items-center justify-center gap-2 px-4 text-[13px]",
              messages.length > 0
                ? "absolute inset-x-4 top-3 mx-auto max-w-[38rem] rounded-md border bg-card py-2 shadow-elevation"
                : "h-full flex-col text-center",
            )}
          >
            <CircleAlert
              className="h-4 w-4 shrink-0 text-destructive"
              aria-hidden="true"
            />
            <span>{t("redesign.historyFailed")}</span>
            <button
              type="button"
              onClick={() => void useChat.getState().reload()}
              className="rounded-md px-2 py-1 font-medium text-signal hover:bg-accent"
            >
              {t("common.retry")}
            </button>
          </div>
        )}
        {!loading && !historyError && viewport.searchMiss && (
          <div
            role="status"
            className="absolute inset-x-4 top-3 z-10 mx-auto max-w-[38rem] rounded-md border bg-card px-4 py-2 text-center text-[13px] shadow-elevation"
          >
            {t("search.outsideRecent")}
          </div>
        )}
        {loading && messages.length === 0 && (
          <div
            role="status"
            aria-busy="true"
            className="flex h-full flex-col items-center justify-center gap-3 text-sm text-muted-foreground"
          >
            <Loader2
              className="h-5 w-5 animate-spin text-signal motion-reduce:animate-none"
              aria-hidden="true"
            />
            {t("redesign.loadingHistory")}
          </div>
        )}
        {!loading && !historyError && messages.length === 0 && (
          <div
            data-testid="conversation-empty"
            className="conversation-welcome flex h-full flex-col items-center justify-center gap-3 px-6 text-center text-[13px] text-muted-foreground"
          >
            <MessagesSquare
              aria-hidden="true"
              className="h-6 w-6 text-signal"
            />
            <p>{t("redesign.emptyConversation")}</p>
          </div>
        )}
        {messages.length > 0 && (
          <Virtuoso
            ref={viewport.virtuosoRef}
            scrollerRef={viewport.scrollerRef}
            // `key` resets all virtualization state (scroll pos, measured heights) when
            // switching conversations — equivalent to the old `[messages.length, key]` reset.
            key={key}
            data={messages}
            // Announce newly-arriving messages to assistive tech. `polite` so it waits for
            // a pause rather than interrupting; the per-bubble aria-label carries the text.
            role="log"
            aria-live="polite"
            aria-label={t("conversation.messageLog", { name: headerName })}
            className="h-full py-3"
            // Keep a short conversation anchored to the bottom (just above the composer)
            // instead of floating at the top with a large empty gap below it.
            alignToBottom
            // Start pinned to the newest message (chat opens at the bottom).
            initialTopMostItemIndex={viewport.initialIndex}
            // Stick to the bottom on new messages only while the user is already there
            // (preserves scroll position when reading history / prepending older items).
            followOutput={(isAtBottom) => (isAtBottom ? "auto" : false)}
            atBottomStateChange={viewport.atBottomStateChange}
            rangeChanged={({ startIndex }) => viewport.rangeChanged(startIndex)}
            onScroll={viewport.onScroll}
            // A little tolerance so "at bottom" isn't lost to sub-pixel rounding.
            atBottomThreshold={48}
            increaseViewportBy={400}
            itemContent={(i, m) => {
              const prev = messages[i - 1];
              const next = messages[i + 1];
              const joins = (neighbor?: ChatMessage) =>
                !!neighbor &&
                neighbor.who === m.who &&
                neighbor.fromMe === m.fromMe &&
                formatDay(neighbor.wallClock) === formatDay(m.wallClock) &&
                Math.abs(neighbor.wallClock - m.wallClock) < 5 * 60_000;
              const grouped = joins(prev) && !m.recalled && !prev?.recalled;
              const showTime =
                !joins(next) || !!next?.recalled || !!m.failed || !!m.pending;
              // Author name + avatar only earn their place in channels; in a 1:1 DM the
              // conversation header already says who you're talking to, so the per-message
              // author is redundant clutter (and would leak the raw device id).
              const showAuthor = isChannel && !grouped;
              // A date separator opens each new calendar day (and the very first item).
              const showDay =
                !prev || formatDay(prev.wallClock) !== formatDay(m.wallClock);
              const parent = m.replyTo ? (byId.get(m.replyTo) ?? null) : null;
              const mentioned =
                isChannel && !m.fromMe && mentionsName(m.text, myName);
              return (
                <div
                  className={cn(
                    "mx-auto max-w-[820px] px-0 pb-1 transition-colors duration-500 motion-reduce:transition-none",
                    viewport.highlightedKey === messageRowKey(m, i) &&
                      "rounded-md bg-signal/10",
                  )}
                >
                  {showDay && <DaySeparator label={formatDay(m.wallClock)} />}
                  <MessageBubble
                    m={m}
                    parent={parent}
                    showAuthor={showAuthor}
                    grouped={grouped}
                    showTime={showTime}
                    isChannel={isChannel}
                    authorAvatarId={accountByDevice.get(m.who) ?? m.who}
                    authorName={nameByDevice.get(m.who)}
                    fresh={motionOK && isFresh(m, i)}
                    reactions={m.id ? (reactionsByTarget.get(m.id) ?? []) : []}
                    selfReactionId={selfReactionId}
                    myName={myName}
                    mentioned={mentioned}
                    onReply={(message) => setReply({ key, message })}
                    onReact={toggleReaction}
                    onRetry={retry}
                    onDelete={(msg) =>
                      msg.id && setPendingDelete({ key, id: msg.id })
                    }
                    onRecall={(msg) => msg.id && void recallMessage(msg.id)}
                    onReEdit={(text) =>
                      setPrefill((p) => ({
                        key,
                        text,
                        n: (p?.n ?? 0) + 1,
                      }))
                    }
                  />
                </div>
              );
            }}
            // Stable per-row identity so reactions/edits don't remount unrelated rows.
            computeItemKey={(i, m) => messageRowKey(m, i)}
          />
        )}
        <AnimatePresence>
          {viewport.showJump && (
            <motion.button
              type="button"
              initial={{
                opacity: 0,
                transform: motionOK ? "translateY(4px)" : "none",
              }}
              animate={{
                opacity: 1,
                transform: motionOK ? "translateY(0px)" : "none",
              }}
              exit={{
                opacity: 0,
                transform: motionOK ? "translateY(4px)" : "none",
              }}
              transition={{ duration: motionOK ? 0.12 : 0.08, ease }}
              aria-label={t("conversation.jumpToLatest")}
              onClick={viewport.jumpToLatest}
              className="absolute bottom-3 right-4 z-10 rounded-md border bg-card p-2 shadow-elevation hover:bg-accent"
            >
              <ChevronDown className="h-4 w-4" />
            </motion.button>
          )}
        </AnimatePresence>
      </div>

      <TransferBar transferKey={active.id} />

      <Dialog
        open={pendingDelete?.key === key}
        onOpenChange={(open) => {
          if (!open && !deleting) setPendingDelete(null);
        }}
      >
        <DialogContent className="max-w-sm" data-testid="delete-message-dialog">
          <DialogHeader>
            <DialogTitle>{t("message.deleteLocalTitle")}</DialogTitle>
            <DialogDescription>
              {t("message.deleteLocalDescription")}
            </DialogDescription>
          </DialogHeader>
          <div className="flex justify-end gap-2">
            <Button
              variant="ghost"
              disabled={deleting}
              onClick={() => setPendingDelete(null)}
            >
              {t("common.cancel")}
            </Button>
            <Button
              variant="destructive"
              data-testid="delete-message-confirm"
              disabled={deleting}
              onClick={() => {
                if (!pendingDelete || pendingDelete.key !== key) return;
                setDeleting(true);
                void deleteMessage(pendingDelete.id).finally(() => {
                  setDeleting(false);
                  setPendingDelete(null);
                });
              }}
            >
              {deleting ? t("common.loading") : t("message.deleteLocalAction")}
            </Button>
          </div>
        </DialogContent>
      </Dialog>

      <Composer
        key={key}
        initialDraft={drafts.current.get(key) ?? ""}
        onDraftChange={(text) => {
          if (text) drafts.current.set(key, text);
          else drafts.current.delete(key);
        }}
        placeholder={
          isChannel
            ? t("conversation.messageChannel", { name: headerName })
            : t("conversation.messageContact", { name: headerName })
        }
        mentionNames={mentionNames}
        myName={myName}
        replyTo={replyTo}
        prefill={prefill?.key === key ? prefill : null}
        onSendSticker={(id, fallback) => void sendSticker(id, fallback)}
        onCancelReply={() => setReply(null)}
        onAttach={async () => {
          const current = captureComposer();
          if (!current()) return;
          try {
            const path = await openFileDialog({ multiple: false });
            // Attach button → generic attachment (lands in the received-files tray).
            if (current() && typeof path === "string")
              await sendFile(path, false);
          } catch (e) {
            if (current())
              setError(
                t("composer.couldntOpenFile", { error: errorMessage(e) }),
              );
          }
        }}
        onImage={async () => {
          const current = captureComposer();
          if (!current()) return;
          // Image button → the NATIVE file dialog (a JS `<input type=file>` is flaky in
          // WKWebView and silently no-ops), filtered to media, sent with `media: true` so it
          // previews inline. Path-based, so no multi-MB bytes round-trip over IPC.
          try {
            const path = await openFileDialog({
              multiple: false,
              filters: [
                {
                  name: "Images & Video",
                  extensions: [...IMAGE_EXTENSIONS, ...VIDEO_EXTENSIONS],
                },
              ],
            });
            if (current() && typeof path === "string")
              await sendFile(path, true);
          } catch (e) {
            if (current())
              setError(
                t("composer.couldntOpenFile", { error: errorMessage(e) }),
              );
          }
        }}
        onSend={(t) => {
          const accepted = admitText(t, replyTo?.id ?? null);
          if (accepted) {
            setReply(null);
            setPrefill(null);
          }
          return accepted;
        }}
        onPasteFiles={async (files) => {
          const current = captureComposer();
          for (const file of files) {
            if (!current()) return;
            try {
              // The temp-file IPC accepts at most 64 MiB. Reject before materializing a
              // large File as a JSON byte array in the webview.
              if (file.size > 64 * 1024 * 1024)
                throw new Error(
                  "Copied file exceeds 64 MB; use Attach instead",
                );
              const ext =
                file.name.split(".").pop() || file.type.split("/")[1] || "bin";
              const path = await chatApi.writeTempFile(
                Array.from(new Uint8Array(await file.arrayBuffer())),
                ext,
                file.name || undefined,
              );
              if (!current()) return;
              await sendFile(
                path,
                file.type.startsWith("image/") ||
                  isImage(file.name) ||
                  isVideo(file.name),
              );
            } catch (e) {
              if (current())
                setError(
                  t("composer.couldntOpenFile", { error: errorMessage(e) }),
                );
              throw e;
            }
          }
        }}
        onScreenshot={async (hideWindow) => {
          const current = captureComposer();
          if (!current()) return;
          try {
            const bytes = await chatApi.captureScreen(hideWindow);
            // Empty bytes = the user cancelled the capture: send nothing.
            if (current() && bytes.length > 0)
              await sendImageBytes(bytes, "png");
          } catch (e) {
            if (!current()) return;
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
