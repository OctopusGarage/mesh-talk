import { usePacks } from "./packs";
import { stickerById } from "@/lib/stickerPacks";
import { snapshotSticker } from "@/lib/stickerMedia";
import { mentionsName } from "@/lib/mentions";
import { useAuth } from "./auth";
import { createRosterRefresh } from "./rosterRefresh";
import {
  notifySyncInvalidation,
  setPushStateSupport,
  supportsPushState,
} from "@/lib/syncInvalidation";
import { readWithDeadline } from "@/lib/readDeadline";
import { notifyIncomingFileDismissed } from "@/lib/incomingFileDismissal";
import { saveAttachment } from "@/lib/attachmentSave";
import { create } from "zustand";
import { createChannelActions, type ChannelActions } from "./channelActions";
import { createChannelRefresh } from "./channelRefresh";
import { chat } from "@/lib/api/chat";
import { contactPolicy, favorites as favoritesApi } from "@/lib/api/contacts";
import { errorMessage } from "@/lib/error";
import { subscribeNodeEvents } from "@/lib/events";
import {
  loadPendingUnread,
  loadUnread,
  queueReceptionPersistence,
  flushReceptionPersistence,
  loadReceptionLedger,
  loadUnreadOverflow,
  loadFullHistoryBaselines,
} from "@/lib/unreadStorage";
import {
  emptyReception,
  markConversationsRead,
  suppressIdentities,
  expireReception,
  applyAccountSeen,
  mergeReception,
  receiveIdentity,
  viewIdentities,
  receptionPatch,
  type ConversationReception,
} from "./reception";
import {
  applyChatCallSignal,
  applyChatFileProgress,
  applyChatProfile,
  resetChatFileAvailability,
  resetChatFiles,
  stopChatCalls,
  syncChatAvatarIdentity,
} from "./chatRuntimeEffects";
import {
  captureOwner,
  captureChatOwner,
  registerRuntimeSnapshot,
} from "./ownership";
import {
  type OutgoingIntentContext,
  sendIntent,
  beginSendIntent,
  dispatchIntent,
  refreshDeliveryStatuses,
} from "./outgoingIntent";
import {
  type ConversationHistoryState,
  emptyConversationHistory,
  reloadConversation,
  revealConversationIdentity,
  scanConversationHistory,
} from "./conversationHistory";
import {
  convKey,
  expireConversationMessages,
  openConversationState,
  deleteConversationMessage,
  clearConversationMessages,
} from "./conversationState";
export { convKey, fromHistoryItem } from "./conversationState";
import type {
  AccountInfo,
  ChannelInfo,
  ChannelMemberInfo,
  ChannelMessageEvent,
  DmReceivedEvent,
  FavoriteInfo,
  FileReceivedEvent,
  PeerInfo,
} from "@/lib/types";

import type {
  ChatMessage,
  Conversation,
  SearchTarget,
} from "./conversationModel";
export type {
  ConvKind,
  ChatMessage,
  Conversation,
  SearchTarget,
} from "./conversationModel";

/** Cap on the received-files tray list. */
const INCOMING_FILES_CAP = 300;

export interface IncomingFile {
  protection?: import("@/lib/types").MessageProtection | null;
  expiresAtMs?: number | null;
  fromName: string;
  name: string;
  size: number;
  mime: string;
  fileConv: string;
  /** Inline media (media button) vs generic attachment (attach button), by sender intent. */
  media: boolean;
}

/** Stable empty favorites map so a "no favorites yet" selector keeps a constant ref. */
export const NO_FAVORITES: Record<string, FavoriteInfo> = {};

/** The name to show for a contact: its user-set alias if any, else the announced name. */
export function displayName(
  favorites: Record<string, FavoriteInfo>,
  id: string,
  announced: string,
): string {
  return favorites[id]?.custom_alias || announced;
}

/** Total boot window, including time spent waiting for native reads. */
const BOOT_TIMEOUT_MS = 30_000;
const BOOT_POLL_MS = 500;
const BOOT_RECOVERY_INTERVAL_MS = 10_000;

/** Both startup and full runtime identities bind persisted reception exactly once. */
function applyVerifiedIdentity(
  set: Set,
  identity: {
    owner: string;
    account_id: string;
    device_id: string;
    history_revision?: number;
  },
  ready: boolean,
) {
  const owner = identity.owner;
  const acct = identity.account_id;
  const persistedUnread = loadUnread(owner, acct);
  const persistedPendingUnread = loadPendingUnread(owner, acct);
  const persistedReception = emptyReception(
    loadReceptionLedger(owner, acct),
    loadUnreadOverflow(owner, acct),
  );
  set((state) => {
    const sameScope = state.unreadOwner === owner && state.myAccountId === acct;
    const invalidateScope = state.unreadOwner !== "" && !sameScope;
    const replacedScope =
      invalidateScope ||
      (state.myAccountId !== "" && state.myAccountId !== acct);
    const hydrated = sameScope
      ? state
      : mergeReception(
          { ...persistedReception, unread: persistedUnread },
          replacedScope ? { ...emptyReception(), unread: {} } : state,
        );
    const expiryPatch = receptionPatch(
      hydrated,
      expireReception(hydrated, Date.now()),
    );
    const reception = { ...hydrated, ...expiryPatch };
    const unread = reception.unread;
    const pendingUnknownUnread = sameScope
      ? state.pendingUnknownUnread
      : { ...persistedPendingUnread };
    if (!sameScope && !replacedScope)
      for (const [sender, count] of Object.entries(state.pendingUnknownUnread))
        pendingUnknownUnread[sender] = Math.min(
          1_000_000,
          (pendingUnknownUnread[sender] ?? 0) + count,
        );
    return {
      ...(invalidateScope ? invalidateConversationIdentity(state) : {}),
      ...expiryPatch,
      ...(!sameScope
        ? {
            ...reception,
            accountSeen: reception.accountSeen,
            historyBaselines: Object.keys(persistedReception.ledger),
            historyFullBaselines: loadFullHistoryBaselines(owner, acct),
          }
        : {}),
      identityEpoch: invalidateScope
        ? state.identityEpoch + 1
        : state.identityEpoch,
      historyRevision: identity.history_revision ?? null,
      myId: identity.device_id,
      myAccountId: acct,
      unread,
      pendingUnknownUnread,
      unreadOwner: owner,
      ready,
      bootFailed: false,
    };
  });
}

/**
 * Poll my_id/account_id until the node finishes opening (post-login KDF unlock takes a
 * moment), then mark ready and refresh the roster. Returns true once ready, false if the
 * boot window is exhausted. `isCancelled` lets a caller (e.g. logout) abort the loop;
 * when cancelled we stop quietly without flipping any flags.
 */
async function pollUntilReady(
  set: Set,
  get: Get,
  isCancelled: () => boolean,
  singleAttempt = false,
): Promise<boolean> {
  const deadline = performance.now() + BOOT_TIMEOUT_MS;
  while (performance.now() < deadline && !isCancelled()) {
    const owner = captureOwner().owner;
    if (!owner) return false;
    const identityEpoch = get().identityEpoch;
    const superseded = () =>
      isCancelled() || get().identityEpoch !== identityEpoch;
    let observedStartup:
      Awaited<ReturnType<typeof chat.ownerStartupIdentity>> | undefined;
    try {
      const identity = await readWithDeadline(
        () => chat.ownerIdentity(owner),
        Math.min(10_000, deadline - performance.now()),
      );
      if (superseded() || performance.now() >= deadline) return false;
      if (identity.owner !== owner)
        throw new Error("Node identity is not ready for this owner");
      // Remember the native ticket even when the node was already ready at the
      // first poll, so a later transient outage cannot look like a new startup.
      const startup = await readWithDeadline(
        () => chat.ownerStartupIdentity(owner),
        Math.max(1, Math.min(2_000, deadline - performance.now())),
      ).catch(() => undefined);
      if (superseded() || performance.now() >= deadline) return false;
      if (startup === null && get().startupId !== null) {
        observedStartup = null;
        throw new Error("Runtime startup retired during identity observation");
      }
      if (startup?.owner === owner && startup.startup_id) {
        if (
          startup.phase !== "ready" ||
          startup.account_id !== identity.account_id ||
          startup.device_id !== identity.device_id
        ) {
          observedStartup = startup;
          throw new Error(
            "Runtime startup changed during identity observation",
          );
        }
        set({ startupId: startup.startup_id });
      }
      if (identity.display_name && useAuth.getState().user?.id === owner)
        useAuth.setState((state) => ({
          user: state.user
            ? { ...state.user, display_name: identity.display_name! }
            : null,
        }));
      applyVerifiedIdentity(set, identity, true);
      const acct = identity.account_id;
      setPushStateSupport(identity.supports_push_state ? owner : null);
      syncChatAvatarIdentity(acct, () => !isCancelled());
      // Identity completes boot. Roster/history hydration owns its separate
      // bounded lane and must not extend the startup deadline or hold bootBusy.
      void get().refreshRoster();
      return true;
    } catch {
      if (superseded()) return false;
      try {
        const startup =
          observedStartup !== undefined
            ? observedStartup
            : await readWithDeadline(
                () => chat.ownerStartupIdentity(owner),
                Math.max(1, Math.min(2_000, deadline - performance.now())),
              );
        if (superseded() || performance.now() >= deadline) return false;
        if (startup === null && get().startupId !== null) {
          resetChatFileAvailability();
          set((state) => ({
            ...invalidateConversationIdentity(state),
            identityEpoch: state.identityEpoch + 1,
            myId: "",
            myAccountId: "",
            startupId: null,
          }));
        }
        if (startup?.owner === owner && startup.startup_id) {
          const hasIdentity =
            (startup.phase === "identity" || startup.phase === "ready") &&
            Boolean(startup.account_id && startup.device_id);
          const changed = get().startupId !== startup.startup_id;
          if ((changed || !hasIdentity) && get().myAccountId) {
            resetChatFileAvailability();
            set((state) => ({
              ...invalidateConversationIdentity(state),
              identityEpoch: state.identityEpoch + 1,
              myId: "",
              myAccountId: "",
            }));
          }
          set({ startupId: startup.startup_id });
          if (hasIdentity)
            applyVerifiedIdentity(
              set,
              {
                owner,
                device_id: startup.device_id!,
                account_id: startup.account_id!,
              },
              false,
            );
        }
      } catch {
        // Older native hosts and transient observation failures retain the
        // existing full-runtime retry path; they never grant startup identity.
      }
      if (isCancelled() || singleAttempt) return false;
      const remaining = deadline - performance.now();
      if (remaining <= 0) return false;
      await new Promise((r) =>
        setTimeout(r, Math.min(BOOT_POLL_MS, remaining)),
      );
    }
  }
  return false;
}

// --- per-conversation API dispatch ----------------------------------------

function reactFor(
  c: Conversation,
  target: string,
  emoji: string,
  remove: boolean,
) {
  return c.kind === "account"
    ? chat.reactAccount(c.id, target, emoji, remove)
    : chat.reactChannel(c.id, target, emoji, remove);
}
/** Whether this conversation is a channel (drives the `is_channel` flag on the lifecycle
 * commands; 1:1 chats are account-addressed). */
const isChannelConv = (c: Conversation) => c.kind === "channel";

export interface ChatState
  extends ConversationHistoryState, OutgoingIntentContext, ChannelActions {
  bootBusy: boolean;
  bootRequest: number;
  /** Native startup ticket; distinct from the frontend run and account identity. */
  startupId: string | null;
  rosterRequest: number;
  favoritesRequest: number;
  ready: boolean;
  myAccountId: string;
  expiryNow: number;
  peers: PeerInfo[];
  accounts: AccountInfo[];
  channels: ChannelInfo[];
  /** Each channel's members (by channel_id), cached for the composite group avatar. */
  channelMembersById: Record<string, ChannelMemberInfo[]>;
  searchTarget: (SearchTarget & { key: string; request: number }) | null;
  unreadOwner: string;
  pendingUnknownUnread: Record<string, number>;
  members: ChannelMemberInfo[];
  // The active channel's owner (device user_id). Only the owner may change membership.
  channelOwner: string;
  incomingFiles: IncomingFile[];
  // Per-contact UI prefs (pin + custom alias), keyed by account_id/channel_id. Persisted
  // on the Rust side; mirrored here so the sidebar can sort/rename without a roundtrip.
  favorites: Record<string, FavoriteInfo>;
  error: string | null; // transient action error (file/reaction send), surfaced to the user
  bootFailed: boolean; // identity did not resolve within the latest boot window

  resetForLock: () => void;
  start: () => () => void;
  retryBoot: (background?: boolean) => void;
  loadFavorites: () => Promise<void>;
  togglePinned: (id: string, pinned: boolean) => Promise<void>;
  setChannelHidden: (id: string, hidden: boolean) => Promise<void>;
  setAlias: (id: string, alias: string | null) => Promise<void>;
  dismissFile: (fileConv: string) => void;
  clearError: () => void;
  refreshRoster: () => Promise<void>;
  refreshChannels: (channelId?: string | null) => Promise<void>;
  deleteContact: (account: string) => Promise<boolean>;
  open: (c: Conversation, target?: SearchTarget) => Promise<void>;
  markActiveRead: () => void;
  markConversationRead: (key: string) => void;
  markAllRead: () => void;
  dismissPendingUnread: (sender: string) => void;
  applyAccountReadReceipts: (
    owner: string,
    accountId: string,
    key: string,
    ids: string[],
  ) => void;
  markLegacyActiveRead: () => void;
  markMessagesViewed: (key: string, ids: string[]) => void;
  revealUnread: (key: string, id: string) => Promise<boolean>;
  send: (text: string, replyTo: string | null) => Promise<void>;
  admitTextAndWait: (
    text: string,
    replyTo: string | null,
    expiresAtMs?: number | null,
    protection?: import("@/lib/types").MessageProtection | null,
  ) => Promise<boolean>;
  admitText: (
    text: string,
    replyTo: string | null,
    expiresAtMs?: number | null,
    protection?: import("@/lib/types").MessageProtection | null,
  ) => boolean;
  retry: (clientId: string) => Promise<void>;
  sendFile: (
    path: string,
    media: boolean,
    expiresAtMs?: number | null,
  ) => Promise<void>;
  admitImage: (
    path: string,
    protection: import("@/lib/types").MessageProtection,
    expiresAtMs?: number | null,
    onAccepted?: () => void,
  ) => Promise<"refused" | "failed" | "accepted">;
  saveFile: (fileConv: string, dest: string) => Promise<void>;
  toggleReaction: (target: string, emoji: string) => Promise<void>;
  /** Delete one message from this device only (local). */
  deleteMessage: (target: string) => Promise<void>;
  /** Recall (unsend) one of my own messages within the 2-minute window. */
  recallMessage: (target: string) => Promise<void>;
  /** Clear all local history for the active conversation. */
  clearConversation: () => Promise<boolean>;
  /** Send an animated sticker (by id) as its own message; `fallback` is its emoji char. */
  sendSticker: (
    stickerId: string,
    fallback: string,
    expiresAtMs?: number | null,
  ) => Promise<void>;
}

function invalidateConversationIdentity(state: ChatState): Partial<ChatState> {
  return {
    accountSeen: undefined,
    ...emptyConversationHistory(),
    historyRevision: state.historyRevision,
    activeRequest: state.activeRequest,
    loadingRequest: state.loadingRequest,
    ready: false,
    unreadOwner: "",
    pendingUnknownUnread: {},
    searchTarget: null,
    members: [],
    channelOwner: "",
    statusBusy: false,
  };
}

/** One empty session snapshot for initial state, login, and app lock. */
function emptySessionData() {
  return {
    accountSeen: undefined,
    ...emptyConversationHistory(),
    expiryNow: Date.now(),
    identityEpoch: 0,
    bootBusy: false,
    bootRequest: 0,
    startupId: null,
    statusBusy: false,
    rosterRequest: 0,
    favoritesRequest: 0,
    ready: false,
    myId: "",
    myAccountId: "",
    peers: [],
    accounts: [],
    channels: [],
    channelMembersById: {},
    searchTarget: null,
    unreadOwner: "",
    pendingUnknownUnread: {},
    members: [],
    channelOwner: "",
    incomingFiles: [],
    favorites: NO_FAVORITES,
    error: null,
    bootFailed: false,
  };
}

// The coordinator captures owner/runtime authority when work is requested.
// Its callbacks are lazy because the store itself is initialized below.
const channelRefresh = createChannelRefresh({
  read: () => useChat.getState(),
  write: (patch) => {
    useChat.setState(patch);
    refreshChannelRecovery(useChat.setState, useChat.getState);
  },
  capture: () => captureChat(useChat.getState),
  commands: chat,
});

const rosterRefresh = createRosterRefresh({
  capture: () => captureChat(useChat.getState),
  refresh: (hasPending) =>
    refreshRosterSnapshot(useChat.setState, useChat.getState, hasPending),
});

async function refreshRosterSnapshot(
  rawSet: Set,
  get: Get,
  hasPending: () => boolean,
) {
  const lease = captureChat(get);
  const set = guardedSet(rawSet, lease.current);
  if (!lease.current()) return;
  if (!get().ready || get().historyScanBusy) return;
  const request = get().rosterRequest + 1;
  set({ rosterRequest: request });
  try {
    let identity;
    try {
      identity = await readWithDeadline(() => chat.ownerIdentity(lease.owner!));
    } catch {
      if (get().rosterRequest === request && !hasPending())
        set({ ready: false });
      return;
    }
    if (!lease.current() || get().rosterRequest !== request) return;
    if (
      identity.device_id !== get().myId ||
      identity.account_id !== get().myAccountId
    ) {
      // Runtime/account replacement can happen without a host logout. Invalidate
      // old in-flight conversation work; the run's owned ticker performs fresh boot.
      resetChatFileAvailability();
      rawSet((state) => ({
        ...invalidateConversationIdentity(state),
        identityEpoch: state.identityEpoch + 1,
        // The replacement identity is already resolved. Bind new arrivals to it
        // while retryBoot hydrates its persisted state, after dropping old work.
        myId: identity.device_id,
        myAccountId: identity.account_id,
      }));
      get().retryBoot();
      return;
    }
    if (identity.display_name && useAuth.getState().user?.id === lease.owner)
      useAuth.setState((state) => ({
        user: state.user
          ? { ...state.user, display_name: identity.display_name! }
          : null,
      }));
    const [peers, accounts] = await Promise.all([
      readWithDeadline(() => chat.listPeers()),
      readWithDeadline(() => chat.listAccounts()),
    ]);
    if (!lease.current() || get().rosterRequest !== request) return;
    setPushStateSupport(identity.supports_push_state ? lease.owner : null);
    const channelWork = channelRefresh.ifDue();
    // Polling returns fresh arrays even when discovery has not changed. Preserve
    // the previous references so sidebar subscribers do not redraw every tick.
    const previous = get();
    const rosterChanged =
      JSON.stringify([previous.peers, previous.accounts]) !==
      JSON.stringify([peers, accounts]);
    const resolved = peers.filter(
      (peer) => peer.account_id && previous.pendingUnknownUnread[peer.user_id],
    );
    if (rosterChanged || resolved.length) {
      const unread = { ...previous.unread };
      const pendingUnknownUnread = { ...previous.pendingUnknownUnread };
      for (const peer of resolved) {
        const key = `account:${peer.account_id}`;
        unread[key] = (unread[key] ?? 0) + pendingUnknownUnread[peer.user_id];
        delete pendingUnknownUnread[peer.user_id];
      }
      set({
        ...(rosterChanged ? { peers, accounts } : {}),
        ...(resolved.length ? { unread, pendingUnknownUnread } : {}),
      });
    }
    await channelWork;
    if (!lease.current() || get().rosterRequest !== request) return;
    const channels = get().channels;
    refreshChannelRecovery(rawSet, get);
    if (identity.history_revision !== undefined) {
      await scanConversationHistory(
        rawSet,
        get,
        [
          // Saved Messages belongs to the runtime even when the contact
          // directory has no row for our own account.
          ...Array.from(
            new Set([
              ...accounts.map((account) => account.account_id),
              identity.account_id,
            ]),
          ).map((accountId): Conversation => ({
            kind: "account",
            id: accountId,
            name: "",
          })),
          ...channels.map((channel): Conversation => ({
            kind: "channel",
            id: channel.channel_id,
            name: channel.name,
          })),
        ],
        identity.history_revision,
        () => lease.current() && get().rosterRequest === request,
      );
      if (!lease.current() || get().rosterRequest !== request) return;
    }
  } catch {
    // node may still be starting; ignore
  }
}

export const useChat = create<ChatState>((rawSet, get) => ({
  ...emptySessionData(),
  runEpoch: 0,

  clearError: () => rawSet({ error: null }),
  setError: (msg) => rawSet({ error: msg }),

  resetForLock: () => {
    setPushStateSupport(null);
    lastUnknownSenderRefresh = 0;
    rawSet((state) => ({
      ...emptySessionData(),
      runEpoch: state.runEpoch + 1,
    }));
    resetChatFiles();
  },

  loadFavorites: async () => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const request = get().favoritesRequest + 1;
    set({ favoritesRequest: request });
    try {
      const list = await favoritesApi.get(lease.owner!);
      if (!lease.current() || get().favoritesRequest !== request) return;
      const map: Record<string, FavoriteInfo> = {};
      for (const f of list) map[f.id] = f;
      set({ favorites: map });
    } catch {
      // favorites are local-only UI prefs; a load failure is non-fatal.
    }
  },

  // Optimistically update the local mirror, persist, then reconcile from disk so the
  // truth on disk (which prunes empty entries) is reflected.
  togglePinned: async (id, pinned) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    set((s) => {
      const prev = s.favorites[id];
      return {
        favorites: {
          ...s.favorites,
          [id]: {
            id,
            pinned,
            custom_alias: prev?.custom_alias ?? null,
            hidden: prev?.hidden ?? false,
          },
        },
      };
    });
    try {
      await favoritesApi.setFavorite(lease.owner!, id, pinned);
      if (lease.current())
        notifySyncInvalidation({ owner: lease.owner!, kind: "contacts" });
    } catch (e) {
      set({ error: `Couldn't update pin: ${errorMessage(e)}` });
    }
    if (lease.current()) await get().loadFavorites();
  },

  setAlias: async (id, alias) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const trimmed = alias?.trim() ? alias.trim() : null;
    set((s) => {
      const prev = s.favorites[id];
      return {
        favorites: {
          ...s.favorites,
          [id]: {
            id,
            pinned: prev?.pinned ?? false,
            custom_alias: trimmed,
            hidden: prev?.hidden ?? false,
          },
        },
      };
    });
    try {
      await favoritesApi.setAlias(lease.owner!, id, trimmed);
      if (lease.current())
        notifySyncInvalidation({ owner: lease.owner!, kind: "contacts" });
    } catch (e) {
      set({ error: `Couldn't rename contact: ${errorMessage(e)}` });
    }
    if (lease.current()) await get().loadFavorites();
  },

  setChannelHidden: async (id, hidden) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    set((s) => {
      const prev = s.favorites[id];
      return {
        favorites: {
          ...s.favorites,
          [id]: {
            id,
            pinned: prev?.pinned ?? false,
            custom_alias: prev?.custom_alias ?? null,
            hidden,
          },
        },
        active:
          hidden && s.active?.kind === "channel" && s.active.id === id
            ? null
            : s.active,
      };
    });
    try {
      await favoritesApi.setChannelHidden(lease.owner!, id, hidden);
      if (lease.current())
        notifySyncInvalidation({ owner: lease.owner!, kind: "contacts" });
    } catch (e) {
      set({
        error: `Couldn't update channel visibility: ${errorMessage(e)}`,
      });
    }
    if (lease.current()) await get().loadFavorites();
  },

  // Re-attempt the node-id poll after a boot failure (events + roster interval from the
  // original start() are still live, so we only need to re-resolve my_id/account_id).
  retryBoot: (background = false) => {
    const lease = captureChat(get, false);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    if (get().bootBusy) return;
    const request = get().bootRequest + 1;
    set({
      bootFailed: background && get().bootFailed,
      ready: false,
      bootBusy: true,
      bootRequest: request,
    });
    void (async () => {
      // Stop UI continuation when the captured owner/run or boot request is superseded.
      const ok = await pollUntilReady(
        set,
        get,
        () => !lease.current() || get().bootRequest !== request,
        background,
      );
      if (ok && lease.current() && get().bootRequest === request)
        void get().loadFavorites();
      if (get().bootRequest === request)
        set({ bootBusy: false, bootFailed: !ok });
    })();
  },

  saveFile: async (fileConv, dest) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    try {
      const path = await saveAttachment({ fileConv, name: "" }, lease, {
        mode: "as",
        destination: dest,
      });
      if (path && lease.current()) get().dismissFile(fileConv);
    } catch (e) {
      set({ error: `Couldn't save file: ${errorMessage(e)}` });
    }
  },

  dismissFile: (fileConv) => {
    notifyIncomingFileDismissed(fileConv);
    rawSet((s) => ({
      incomingFiles: s.incomingFiles.filter((f) => f.fileConv !== fileConv),
    }));
  },

  start: () => {
    rawSet((s) => ({ runEpoch: s.runEpoch + 1 }));
    const lease = captureChat(get, false);
    const set = guardedSet(rawSet, lease.current);
    // Fresh slate per login (the store survives logout/login of a different account).
    lastUnknownSenderRefresh = 0;
    resetChatFiles();
    stopChatCalls();
    set({
      ...emptySessionData(),
      bootBusy: true,
      bootRequest: 1,
    });
    // Load favorites only after the captured owner's identity boot succeeds.
    // Poll my_id until the node finishes opening (post-login KDF unlock takes a moment).
    let cancelled = false;
    const bootRequest = get().bootRequest;
    void (async () => {
      const ok = await pollUntilReady(
        set,
        get,
        () =>
          cancelled || !lease.current() || get().bootRequest !== bootRequest,
      );
      if (ok && lease.current()) void get().loadFavorites();
      if (lease.current() && get().bootRequest === bootRequest)
        set({ bootBusy: false });
      // Exhausted the boot window without the node coming up — surface it so the UI can
      // offer a retry instead of sitting on "starting…" forever. (Skip if cancelled by
      // teardown, so a logout mid-boot doesn't flash a spurious failure.)
      if (!ok && !cancelled && get().bootRequest === bootRequest)
        set({ bootFailed: true });
    })();

    const expiryWallStart = Date.now();
    const expiryMonotonicStart = performance.now();
    const expire = () => {
      if (!lease.current()) return;
      const now = Math.max(
        get().expiryNow,
        Date.now(),
        expiryWallStart + Math.floor(performance.now() - expiryMonotonicStart),
      );
      const expiredFiles = get().incomingFiles.filter(
        (file) => file.expiresAtMs != null && file.expiresAtMs <= now,
      );
      set((state) => ({
        ...expireConversationMessages(state, now),
        ...receptionPatch(state, expireReception(state, now)),
        incomingFiles: state.incomingFiles.filter(
          (file) => file.expiresAtMs == null || file.expiresAtMs > now,
        ),
        expiryNow: now,
      }));
      for (const file of expiredFiles)
        notifyIncomingFileDismissed(file.fileConv);
    };
    const expiry = setInterval(expire, 1000);
    const flushReceipts = () => flushReceptionPersistence();
    if (typeof window !== "undefined")
      window.addEventListener("pagehide", flushReceipts);
    if (typeof document !== "undefined")
      document.addEventListener?.("visibilitychange", flushReceipts);
    if (typeof window !== "undefined") window.addEventListener("focus", expire);
    if (typeof document !== "undefined")
      document.addEventListener?.("visibilitychange", expire);

    let rosterBusy = false;
    let rosterDirty = false;
    const refreshRoster = async (invalidate = false) => {
      if (rosterBusy) {
        rosterDirty ||= invalidate;
        return;
      }
      rosterBusy = true;
      try {
        do {
          rosterDirty = false;
          await get().refreshRoster();
        } while (rosterDirty && lease.current());
      } finally {
        rosterBusy = false;
      }
    };
    let nextRosterFallback = 0;
    let nextStatusFallback = 0;
    let nextBootRecovery = 0;
    let bootFailureObserved = false;
    const roster = setInterval(() => {
      if (lease.current() && get().ready) {
        bootFailureObserved = false;
        const now = performance.now();
        if (!supportsPushState(lease.owner) || now >= nextRosterFallback) {
          nextRosterFallback = now + 30000;
          void refreshRoster();
        }
        if (!supportsPushState(lease.owner) || now >= nextStatusFallback) {
          nextStatusFallback = now + 30000;
          void get().refreshStatuses();
        }
      } else if (lease.current() && !get().bootBusy) {
        const failed = get().bootFailed;
        const now = performance.now();
        if (failed && !bootFailureObserved) {
          bootFailureObserved = true;
          nextBootRecovery = now + BOOT_RECOVERY_INTERVAL_MS;
        }
        if (!failed || now >= nextBootRecovery) {
          nextBootRecovery = now + BOOT_RECOVERY_INTERVAL_MS;
          // A failed boot remains visible while one bounded background read checks
          // for recovery. Never let a transient identity timeout become terminal.
          get().retryBoot(failed);
        }
      }
    }, 2000);

    const unlisten = subscribeNodeEvents({
      onStateChanged: (e) => {
        const state = get();
        if (
          !lease.current() ||
          !state.ready ||
          e.owner !== lease.owner ||
          e.account_id !== state.myAccountId ||
          e.device_id !== state.myId
        )
          return;
        notifySyncInvalidation({ owner: e.owner, kind: e.kind });
        if (e.kind === "state" || e.kind === "roster") void refreshRoster(true);
        if (e.kind === "delivery" || e.kind === "files")
          void state.refreshStatuses(undefined, true);
      },
      onIntegrationMessageAccepted: (e) => {
        const state = get();
        if (
          !lease.current() ||
          !state.ready ||
          e.owner !== lease.owner ||
          e.account_id !== state.myAccountId ||
          !state.active ||
          e.destination.kind !== state.active.kind ||
          e.destination.id !== state.active.id
        )
          return;
        // Outgoing delegated sends do not count as inbound notifications. Fetch
        // their content through the same guarded history path as ordinary sends.
        void state.reload();
      },

      onDm: (e) => {
        if (lease.current()) get_handleDm(set, get, e);
      },
      onChannelChanged: (e) => {
        const state = get();
        if (
          !lease.current() ||
          !state.ready ||
          e.owner !== lease.owner ||
          e.account_id !== state.myAccountId ||
          e.device_id !== state.myId
        )
          return;
        void state.refreshChannels(e.channel_id);
      },
      onChannelMessage: (e) => {
        if (lease.current()) get_handleChannel(set, get, e);
      },
      onFile: (e) => {
        if (lease.current()) get_handleFile(set, get, e);
      },
      onFileProgress: (e) => {
        if (lease.current()) applyChatFileProgress(e);
      },
      onAccountProfile: (event) => {
        const state = get();
        if (
          lease.current() &&
          useAuth.getState().user?.id === event.user.id &&
          state.myAccountId === event.account_id &&
          state.myId === event.device_id
        )
          useAuth.setState({
            user: event.user,
            ...(event.saved_session != null
              ? { savedSession: event.saved_session }
              : {}),
          });
      },
      onProfile: (e) => {
        if (lease.current()) applyChatProfile(e);
      },
      onCallSignal: (e) => {
        if (lease.current()) applyChatCallSignal(e);
      },
    });

    return () => {
      cancelled = true;
      flushReceptionPersistence();
      if (typeof window !== "undefined")
        window.removeEventListener("pagehide", flushReceipts);
      if (typeof document !== "undefined")
        document.removeEventListener?.("visibilitychange", flushReceipts);
      clearInterval(roster);
      clearInterval(expiry);
      if (typeof window !== "undefined")
        window.removeEventListener("focus", expire);
      if (typeof document !== "undefined")
        document.removeEventListener?.("visibilitychange", expire);
      unlisten();
      if (get().runEpoch === lease.run) {
        stopChatCalls();
        rawSet((s) => ({ runEpoch: s.runEpoch + 1, ready: false }));
      }
    };
  },

  deleteContact: async (account) => {
    const lease = captureChat(get);
    if (
      !lease.current() ||
      !lease.owner ||
      !get().ready ||
      account === get().myAccountId
    )
      return false;
    try {
      await contactPolicy.delete(lease.owner, account);
      if (!lease.current()) return false;
      rawSet((state) => {
        const unread = { ...state.unread };
        delete unread[`account:${account}`];
        const closing =
          state.active?.kind === "account" && state.active.id === account;
        return {
          ...clearConversationMessages(state, `account:${account}`),
          reactions: { ...state.reactions, [`account:${account}`]: [] },
          accounts: state.accounts.filter((a) => a.account_id !== account),
          unread,
          rosterRequest: state.rosterRequest + 1,
          ...(closing
            ? {
                active: null,
                searchTarget: null,
                historyError: null,
                activeRequest: state.activeRequest + 1,
                loading: false,
              }
            : {}),
        };
      });
      return true;
    } catch {
      return false;
    }
  },

  refreshChannels: (channelId = null) => channelRefresh.request(channelId),

  refreshRoster: () => rosterRefresh(),

  open: async (c, target) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const request = get().activeRequest + 1;
    set((state) => openConversationState(state, c, target, request));
    await get().reload();
    if (!lease.current() || get().activeRequest !== request) return;
    if (
      (!target || !("reference" in target)) &&
      !get().loading &&
      get().historyError !== convKey(c) &&
      isConversationVisible()
    )
      set((state) => ({
        unread: {
          ...state.unread,
          [convKey(c)]:
            (state.unreadIds[convKey(c)]?.length ?? 0) +
            (state.overflowUnread[convKey(c)] ?? 0),
        },
      }));
    if (c.kind === "channel") {
      try {
        const baseline = get().members;
        const ownerBaseline = get().channelOwner;
        const info = await chat.channelMembers(c.id);
        if (!lease.current() || get().activeRequest !== request) return;
        // A newer active-view refresh wins; a background avatar-only update
        // from before navigation must not discard this navigation's details.
        if (get().members === baseline && get().channelOwner === ownerBaseline)
          set({ members: info.members, channelOwner: info.owner });
      } catch {
        /* ignore */
      }
    }
  },

  markLegacyActiveRead: () => {
    const state = get();
    if (
      !state.active ||
      state.loading ||
      state.historyError === convKey(state.active) ||
      !isConversationVisible()
    )
      return;
    const key = convKey(state.active);
    const tracked =
      (state.unreadIds[key]?.length ?? 0) + (state.overflowUnread[key] ?? 0);
    if (state.unread[key] !== tracked)
      rawSet({ unread: { ...state.unread, [key]: tracked } });
  },

  applyAccountReadReceipts: (owner, accountId, key, ids) => {
    const state = get();
    if (
      !state.ready ||
      state.unreadOwner !== owner ||
      state.myAccountId !== accountId
    )
      return;
    rawSet(receptionPatch(state, applyAccountSeen(state, key, ids)));
  },

  markActiveRead: () => {
    const { active, loading, historyError } = get();
    if (
      !active ||
      loading ||
      historyError === convKey(active) ||
      !isConversationVisible()
    )
      return;
    const key = convKey(active);
    rawSet((state) => markConversationsRead(state, [key]));
  },

  markConversationRead: (key) => {
    if (!canReviewUnread(get()) || !/^(account|channel):.+/u.test(key)) return;
    rawSet((state) => markConversationsRead(state, [key]));
  },

  markAllRead: () => {
    if (!canReviewUnread(get())) return;
    rawSet((state) => ({
      ...markConversationsRead(state, [
        ...new Set([
          ...Object.keys(state.unread),
          ...Object.keys(state.ledger),
          ...Object.keys(state.overflowUnread),
        ]),
      ]),
      pendingUnknownUnread: {},
    }));
  },

  dismissPendingUnread: (sender) => {
    if (!canReviewUnread(get())) return;
    rawSet((state) => ({
      pendingUnknownUnread: Object.fromEntries(
        Object.entries(state.pendingUnknownUnread).filter(
          ([id]) => id !== sender,
        ),
      ),
    }));
  },

  markMessagesViewed: (key, ids) => {
    const state = get();
    if (
      !state.ready ||
      !state.active ||
      convKey(state.active) !== key ||
      state.loading ||
      state.historyError === key ||
      !isConversationVisible()
    )
      return;
    const present = new Set(
      (state.messages[key] ?? []).flatMap((row) => (row.id ? [row.id] : [])),
    );
    const viewed = ids.filter(
      (id) =>
        present.has(id) &&
        state.ledger[key]?.[id]?.seen !== true &&
        !state.ledger[key]?.[id]?.suppressed,
    );
    if (!viewed.length) return;
    rawSet((state) =>
      receptionPatch(state, viewIdentities(state, key, viewed)),
    );
  },

  revealUnread: (key, id) => revealConversationIdentity(rawSet, get, key, id),

  reload: () => reloadConversation(rawSet, get),
  refreshStatuses: (conversation, invalidate) =>
    refreshDeliveryStatuses(rawSet, get, conversation, invalidate),

  send: async (text, replyTo) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const c = get().active;
    if (!get().ready || !c || !text.trim()) return;
    await sendIntent(set, get, c, { kind: "text", text, replyTo });
  },

  // The composer needs an immediate admission answer so it never clears a draft that
  // could not enter the local optimistic queue. Delivery still settles asynchronously.
  admitText: (text, replyTo, expiresAtMs, protection) => {
    const lease = captureChat(get);
    if (!lease.current()) return false;
    const c = get().active;
    if (!get().ready || !c || !text.trim()) return false;
    const dispatch = beginSendIntent(
      guardedSet(rawSet, lease.current),
      get,
      c,
      {
        kind: "text",
        text,
        replyTo,
        expiresAtMs,
        protection,
      },
    );
    if (!dispatch) return false;
    void dispatch;
    return true;
  },

  // Account composers retain the draft until the core has durably accepted it.
  admitTextAndWait: async (text, replyTo, expiresAtMs, protection) => {
    const lease = captureChat(get);
    if (!lease.current()) return false;
    const c = get().active;
    if (!get().ready || !c || !text.trim()) return false;
    return (
      beginSendIntent(
        guardedSet(rawSet, lease.current),
        get,
        c,
        {
          kind: "text",
          text,
          replyTo,
          expiresAtMs,
          protection,
        },
        { waitForHydration: false, retainFailure: !protection },
      ) ?? false
    );
  },

  // Re-send a previously-failed optimistic bubble (reusing its clientId/text/replyTo).
  // Clears the failed state, shows pending again, then runs the same dispatch as send().
  retry: async (clientId) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const c = get().active;
    if (!get().ready || !c) return;
    const key = convKey(c);
    const intent = get().intents[clientId];
    const orig = intent?.message;
    if (
      !intent ||
      convKey(intent.conversation) !== key ||
      !orig?.failed ||
      orig.id
    )
      return;
    const pending: ChatMessage = {
      ...orig,
      pending: true,
      failed: false,
      failReason: undefined,
      wallClock: Date.now(),
    };
    set((s) => ({
      intents: { ...s.intents, [clientId]: { ...intent, message: pending } },
      messages: {
        ...s.messages,
        [key]: (s.messages[key] ?? []).map((m) =>
          m.clientId === clientId ? pending : m,
        ),
      },
    }));
    await dispatchIntent(set, get, { ...intent, message: pending });
  },

  admitImage: async (path, protection, expiresAtMs, onAccepted) => {
    const lease = captureChat(get);
    const c = get().active;
    if (!lease.current() || !get().ready || !c) return "refused";
    const admission = beginSendIntent(
      guardedSet(rawSet, lease.current),
      get,
      c,
      { kind: "file", path, media: true, expiresAtMs, protection },
      { waitForHydration: false, onAccepted },
    );
    if (!admission) return "refused";
    // Local transfer gives the bubble sole retry ownership, including native failure.
    return (await admission) ? "accepted" : "failed";
  },
  sendFile: async (path, media, expiresAtMs) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const c = get().active;
    if (!get().ready || !c) return;
    await sendIntent(set, get, c, { kind: "file", path, media, expiresAtMs });
  },

  toggleReaction: async (target, emoji) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const c = get().active;
    if (!c) return;
    const key = convKey(c);
    // Reaction `who` is keyed by account id for account conversations, device user-id for
    // channels — so "did I already react?" must compare against the matching id, or an
    // account-conversation reaction can never be detected as ours (never toggles off).
    const selfId = c.kind === "account" ? get().myAccountId : get().myId;
    const mine = (get().reactions[key] ?? []).find(
      (r) => r.target === target && r.emoji === emoji && r.who.includes(selfId),
    );
    try {
      await reactFor(c, target, emoji, Boolean(mine));
      // Only reload if still on this conversation (matches send/sendFile).
      if (lease.current() && get().active && convKey(get().active!) === key)
        await get().reload();
    } catch (e) {
      set({ error: `Couldn't update reaction: ${errorMessage(e)}` });
    }
  },

  deleteMessage: async (target) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const c = get().active;
    if (!c) return;
    const key = convKey(c);
    try {
      await chat.deleteMessage(c.id, target, isChannelConv(c));
      if (!lease.current()) return;
      set((state) => ({
        ...deleteConversationMessage(state, key, target),
        ...receptionPatch(state, suppressIdentities(state, key, [target])),
      }));
      if (get().active && convKey(get().active!) === key) await get().reload();
    } catch (e) {
      set({ error: `Couldn't delete message: ${errorMessage(e)}` });
    }
  },

  recallMessage: async (target) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const c = get().active;
    if (!c) return;
    const key = convKey(c);
    try {
      await chat.recallMessage(c.id, target, isChannelConv(c));
      if (lease.current() && get().active && convKey(get().active!) === key)
        await get().reload();
    } catch (e) {
      set({ error: `Couldn't recall message: ${errorMessage(e)}` });
    }
  },

  clearConversation: async () => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return false;
    const c = get().active;
    if (!c) return false;
    const key = convKey(c);
    const snapshot = get();
    const ids = Object.keys(snapshot.ledger[key] ?? {});
    const overflow = snapshot.overflowUnread[key] ?? 0;
    const legacy = Math.max(
      0,
      (snapshot.unread[key] ?? 0) -
        (snapshot.unreadIds[key]?.length ?? 0) -
        overflow,
    );
    try {
      await chat.clearConversation(c.id, isChannelConv(c));
      if (!lease.current()) return false;
      set((state) => {
        const next = suppressIdentities(state, key, ids);
        const remainingOverflow = Math.max(
          0,
          (state.overflowUnread[key] ?? 0) - overflow,
        );
        const patch = receptionPatch(state, {
          ...emptyReception(
            next.ledger,
            { ...next.overflowUnread, [key]: remainingOverflow },
            next.accountSeen,
          ),
          reception: next.reception,
        });
        return {
          ...clearConversationMessages(state, key),
          ...patch,
          unread: {
            ...(patch.unread ?? state.unread),
            [key]: Math.max(
              0,
              (patch.unread?.[key] ?? state.unread[key] ?? 0) - legacy,
            ),
          },
        };
      });
      if (get().active && convKey(get().active!) === key) await get().reload();
      return true;
    } catch (e) {
      set({ error: `Couldn't clear history: ${errorMessage(e)}` });
      return false;
    }
  },

  sendSticker: async (stickerId, fallback, expiresAtMs) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const c = get().active;
    if (!get().ready || !c) return;
    const installed = stickerById(stickerId, usePacks.getState().packs);
    if (installed) {
      let stagedPath: string | undefined;
      let transferred = false;
      try {
        const snapshot = snapshotSticker(installed, fallback);
        stagedPath = await chat.writeTempFile(
          Array.from(snapshot.bytes),
          snapshot.ext,
          snapshot.name,
        );
        if (
          !lease.current() ||
          !get().active ||
          convKey(get().active!) !== convKey(c)
        )
          return;
        const sending = beginSendIntent(set, get, c, {
          kind: "file",
          path: stagedPath,
          media: true,
          expiresAtMs,
          stickerMedia: {
            name: snapshot.name,
            size: snapshot.bytes.length,
            mime: snapshot.mime,
          },
        });
        if (!sending) return;
        // The ordinary file intent owns this immutable path through retry.
        transferred = true;
        await sending;
      } catch (error) {
        set({ error: `Couldn't send sticker: ${errorMessage(error)}` });
      } finally {
        if (stagedPath && !transferred)
          await chat.deleteTempFile(stagedPath).catch(() => {});
      }
      return;
    }
    await sendIntent(set, get, c, {
      kind: "sticker",
      stickerId,
      fallback,
      expiresAtMs,
    });
  },

  ...createChannelActions({
    read: get,
    write: rawSet,
    capture: () => captureChat(get),
    commands: chat,
    refreshRoster: async () => {
      await get().refreshChannels();
      await get().refreshRoster();
    },
    open: (conversation) => get().open(conversation),
    reportError: (message) => rawSet({ error: message }),
  }),
}));

function refreshChannelRecovery(set: Set, get: Get) {
  const recoveryEntries = get()
    .channels.filter(
      (channel) =>
        Object.prototype.hasOwnProperty.call(channel, "recovery") ||
        channel.dissolved ||
        channel.left ||
        channel.archived,
    )
    .map((channel): [string, ConversationReception] => {
      const key = `channel:${channel.channel_id}`;
      const prior = get().reception[key];
      const evidence =
        channel.dissolved || channel.left || channel.archived
          ? null
          : channel.recovery;
      const phase = prior?.processing
        ? ("catching_up" as const)
        : evidence?.phase === "waiting_for_peer"
          ? ("waiting" as const)
          : evidence?.phase === "catching_up"
            ? ("catching_up" as const)
            : ("idle" as const);
      return [
        key,
        {
          ...prior,
          phase,
          received: prior?.received ?? 0,
          checkedAt: evidence?.checked_at ?? undefined,
        },
      ];
    });
  if (recoveryEntries.length)
    set((state) => ({
      reception: Object.fromEntries(
        Object.entries({
          ...state.reception,
          ...Object.fromEntries(recoveryEntries),
        }).slice(-64),
      ),
    }));
}

// --- incoming event handlers (module fns to keep the store object lean) -----

export type Set = (
  partial: Partial<ChatState> | ((s: ChatState) => Partial<ChatState>),
) => void;
export type Get = () => ChatState;

function captureChat(get: Get, identitySensitive = true) {
  return captureChatOwner(get, identitySensitive);
}

registerRuntimeSnapshot(() => ({
  runEpoch: useChat.getState().runEpoch,
  identityEpoch: useChat.getState().identityEpoch,
  accountId: useChat.getState().myAccountId,
}));

export function captureChatOwnership() {
  const lease = captureChat(useChat.getState);
  return {
    ...lease,
    current: () => lease.current() && useChat.getState().ready,
  };
}

function guardedSet(set: Set, current: () => boolean): Set {
  return (partial) => {
    if (current()) set(partial);
  };
}

function bump(set: Set, key: string, active: boolean) {
  if (!active) {
    set((s) => ({ unread: { ...s.unread, [key]: (s.unread[key] ?? 0) + 1 } }));
  }
}

function canReviewUnread(state: ChatState): boolean {
  return (
    Boolean(state.myAccountId) &&
    state.unreadOwner === useAuth.getState().user?.id &&
    isConversationVisible()
  );
}

function isConversationVisible(): boolean {
  return (
    typeof document !== "undefined" &&
    document.visibilityState === "visible" &&
    document.hasFocus()
  );
}

let lastUnknownSenderRefresh = 0;

function queueUnknownUnread(set: Set, sender: string) {
  set((state) => {
    const pending = state.pendingUnknownUnread;
    if (!pending[sender] && Object.keys(pending).length >= 256) return {};
    return {
      pendingUnknownUnread: {
        ...pending,
        [sender]: Math.min(1_000_000, (pending[sender] ?? 0) + 1),
      },
    };
  });
}

function get_handleDm(set: Set, get: Get, e: DmReceivedEvent) {
  // Route to the sender's account conversation (multi-device aware).
  const peer = get().peers.find((p) => p.user_id === e.from);
  const accountId = e.from_account ?? peer?.account_id;
  if (!accountId) {
    // Unknown sender (not yet discovered). Throttle: a burst from an undiscovered account
    // would otherwise fire one roster refresh (3 invokes) per message. The 4s interval
    // poll also covers this.
    queueUnknownUnread(set, e.from);
    const now = Date.now();
    if (now - lastUnknownSenderRefresh > 2000) {
      lastUnknownSenderRefresh = now;
      void get().refreshRoster();
    }
    return;
  }
  const conv: Conversation = {
    kind: "account",
    id: accountId,
    name: e.from_name || peer?.name || "",
  };
  const key = convKey(conv);
  const isActive = get().active != null && convKey(get().active!) === key;
  const messageId = e.message_id;
  if (messageId) {
    // DMs and their history share the logical envelope ID, not each device's
    // transport-event ID. Realtime and history must use one receipt ledger.
    set((state) =>
      receptionPatch(
        state,
        receiveIdentity(state, key, {
          event_id: messageId,
          arrival_kind: "live",
          own_device: accountId === state.myAccountId || e.from === state.myId,
        }),
      ),
    );
  } else {
    // Compatibility for event providers predating logical DM identities.
    bump(set, key, isActive && isConversationVisible());
  }
  if (isActive) void get().reload();
}

function get_handleChannel(set: Set, get: Get, e: ChannelMessageEvent) {
  const conv: Conversation = {
    kind: "channel",
    id: e.channel_id,
    name: e.channel_name,
  };
  const key = convKey(conv);
  if (e.reception_only) {
    set((state) => receptionPatch(state, receiveIdentity(state, key, e)));
    return;
  }
  const isActive = get().active != null && convKey(get().active!) === key;
  if (e.event_id) {
    const own =
      e.own_device ||
      e.from === get().myId ||
      get().peers.some(
        (peer) =>
          peer.user_id === e.from && peer.account_id === get().myAccountId,
      );
    set((state) =>
      receptionPatch(
        state,
        receiveIdentity(
          state,
          key,
          { ...e, own_device: own },
          !e.protection &&
            mentionsName(
              e.text,
              useAuth.getState().user?.display_name ||
                useAuth.getState().user?.username ||
                "",
            ),
        ),
      ),
    );
  } else if (!e.own_device) bump(set, key, isActive && isConversationVisible());
  if (isActive) void get().reload();
}

function get_handleFile(set: Set, get: Get, e: FileReceivedEvent) {
  const peer = get().peers.find((p) => p.user_id === e.from);
  const own =
    e.own_device ||
    e.from === get().myId ||
    Boolean(peer?.account_id && peer.account_id === get().myAccountId);
  const key = e.is_channel
    ? `channel:${e.conv}`
    : peer?.account_id
      ? `account:${peer.account_id}`
      : null;
  if (e.reception_only) {
    if (key)
      set((state) => receptionPatch(state, receiveIdentity(state, key, e)));
    return;
  }
  const knownIdentity = Boolean(
    key &&
    e.event_id &&
    Object.prototype.hasOwnProperty.call(get().ledger[key] ?? {}, e.event_id),
  );
  const knownTray = get().incomingFiles.some((f) => f.fileConv === e.file_conv);
  // Identity controls unread/recovery; byte-transfer completion has its own events.
  // A duplicate final callback may still finish its batch after tray deduplication.
  if (key && e.event_id)
    set((state) =>
      receptionPatch(
        state,
        expireReception(
          receiveIdentity(state, key, { ...e, own_device: own }),
          Math.max(Date.now(), state.expiryNow),
        ),
      ),
    );
  if (!e.event_id && knownTray) return;
  if (e.expires_at_ms != null && e.expires_at_ms <= Date.now()) return;
  if (!own && !knownTray && !knownIdentity)
    set((state) => ({
      incomingFiles: [
        {
          fromName: peer?.name || e.from,
          expiresAtMs: e.expires_at_ms,
          name: e.name,
          size: e.size,
          mime: e.mime,
          fileConv: e.file_conv,
          media: e.media,
          protection: e.protection,
        },
        ...state.incomingFiles,
      ].slice(0, INCOMING_FILES_CAP),
    }));
  if (!key) {
    if (!e.is_channel && !own) {
      queueUnknownUnread(set, e.from);
      const now = Date.now();
      if (now - lastUnknownSenderRefresh > 2000) {
        lastUnknownSenderRefresh = now;
        void get().refreshRoster();
      }
    }
    return;
  }
  const isActive = get().active !== null && convKey(get().active!) === key;
  if (!e.event_id && !own) bump(set, key, isActive && isConversationVisible());
  if (isActive && !knownIdentity) void get().reload();
}

useChat.subscribe((state, previous) => {
  if (
    state.unreadOwner !== previous.unreadOwner ||
    state.myAccountId !== previous.myAccountId ||
    state.runEpoch !== previous.runEpoch
  )
    flushReceptionPersistence();
  if (!state.unreadOwner || !state.myAccountId) return;
  queueReceptionPersistence(state.unreadOwner, state.myAccountId, {
    ...(state.unread !== previous.unread ? { unread: state.unread } : {}),
    ...(state.pendingUnknownUnread !== previous.pendingUnknownUnread
      ? { pendingUnknownUnread: state.pendingUnknownUnread }
      : {}),
    ...(state.ledger !== previous.ledger ? { ledger: state.ledger } : {}),
    ...(state.overflowUnread !== previous.overflowUnread
      ? { overflowUnread: state.overflowUnread }
      : {}),
    ...(state.historyFullBaselines !== previous.historyFullBaselines
      ? { historyFullBaselines: state.historyFullBaselines }
      : {}),
  });
});
useAuth.subscribe((state, previous) => {
  if (
    state.user?.id !== previous.user?.id ||
    state.generation !== previous.generation
  )
    flushReceptionPersistence();
});
