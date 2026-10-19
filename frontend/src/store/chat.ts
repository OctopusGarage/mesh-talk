import { create } from "zustand";
import { chat, contactPolicy, favorites as favoritesApi } from "@/lib/api";
import { errorMessage, type SendFailReason } from "@/lib/error";
import { subscribeNodeEvents } from "@/lib/events";
import {
  loadPendingUnread,
  loadUnread,
  loadReceptionLedger,
  saveReceptionLedger,
  loadUnreadOverflow,
  saveUnreadOverflow,
  loadFullHistoryBaselines,
  saveFullHistoryBaselines,
} from "@/lib/unreadStorage";
import {
  emptyReception,
  receiveIdentity,
  viewIdentities,
  reconcileHistory,
  type ReceptionState,
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
  type SendIntent,
  sendIntent,
  beginSendIntent,
  dispatchIntent,
  reloadConversation,
  refreshDeliveryStatuses,
} from "./outgoingIntent";
import {
  convKey,
  boundedConversationMap,
  openConversationState,
  deleteConversationMessage,
  clearConversationMessages,
  invalidateConversationIdentity,
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
  ReactionInfo,
} from "@/lib/types";

/** Cap on the received-files tray list. */
const INCOMING_FILES_CAP = 300;

export type ConvKind = "account" | "channel";

export interface Conversation {
  kind: ConvKind;
  id: string; // account_id (hex) | channel_id (hex)
  name: string;
}

export interface SearchTarget {
  wallClock: number;
  text: string;
  fromMe: boolean;
}

export interface ChatMessage {
  receivedAt?: number;
  arrivalKind?: import("@/lib/types").ArrivalKind;
  delivery?: "awaiting" | "delivered";
  replicas?: number;
  metadataPending?: boolean;
  id: string | null; // hex EventId; null while pending
  fromMe: boolean;
  who: string;
  text: string;
  wallClock: number;
  replyTo: string | null;
  pending?: boolean;
  failed?: boolean; // a send that errored — kept visible (not silently dropped)
  failReason?: SendFailReason; // coarse, frontend-derived cause (drives the label + help)
  clientId?: string; // stable id for an optimistic bubble (survives a concurrent reload)
  recalled?: boolean; // true when recalled → render a placeholder, no content
  recalledText?: string | null; // our own recalled text, for "re-edit"
  sticker?: string | null; // animated-sticker id when this message is a sticker
  file?: {
    name: string;
    size: number;
    mime: string;
    fileConv: string;
    media: boolean;
  } | null;
}

export interface IncomingFile {
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

/** Number of boot poll attempts (≈30s at BOOT_POLL_MS). */
const BOOT_POLL_TRIES = 60;
const BOOT_POLL_MS = 500;

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
): Promise<boolean> {
  for (let i = 0; i < BOOT_POLL_TRIES && !isCancelled(); i++) {
    try {
      const owner = captureOwner().owner;
      if (!owner) return false;
      const identity = await chat.ownerIdentity(owner);
      if (isCancelled()) return false;
      if (identity.owner !== owner)
        throw new Error("Node identity is not ready for this owner");
      const acct = identity.account_id;
      const persistedUnread = loadUnread(owner, acct);
      const persistedPendingUnread = loadPendingUnread(owner, acct);
      const persistedReception = emptyReception(
        loadReceptionLedger(owner, acct),
        loadUnreadOverflow(owner, acct),
      );
      set((state) => {
        const sameScope =
          state.unreadOwner === owner && state.myAccountId === acct;
        const replacedScope = state.unreadOwner !== "" && !sameScope;
        const unread = sameScope ? state.unread : { ...persistedUnread };
        if (!sameScope)
          for (const key of new Set([
            ...Object.keys(persistedReception.unreadIds),
            ...Object.keys(persistedReception.overflowUnread),
          ]))
            unread[key] = Math.max(
              unread[key] ?? 0,
              (persistedReception.unreadIds[key]?.length ?? 0) +
                (persistedReception.overflowUnread[key] ?? 0),
            );
        const pendingUnknownUnread = sameScope
          ? state.pendingUnknownUnread
          : { ...persistedPendingUnread };
        if (!sameScope && !replacedScope)
          for (const [key, count] of Object.entries(state.unread))
            unread[key] = (unread[key] ?? 0) + count;
        if (!sameScope && !replacedScope)
          for (const [sender, count] of Object.entries(
            state.pendingUnknownUnread,
          ))
            pendingUnknownUnread[sender] = Math.min(
              1_000_000,
              (pendingUnknownUnread[sender] ?? 0) + count,
            );
        return {
          ...(replacedScope ? invalidateConversationIdentity() : {}),
          ...(!sameScope
            ? {
                ...persistedReception,
                historyBaselines: Object.keys(persistedReception.ledger),
                historyFullBaselines: loadFullHistoryBaselines(owner, acct),
              }
            : {}),
          identityEpoch: replacedScope
            ? state.identityEpoch + 1
            : state.identityEpoch,
          historyRevision: identity.history_revision ?? null,
          myId: identity.device_id,
          myAccountId: acct,
          unread,
          pendingUnknownUnread,
          unreadOwner: owner,
          ready: true,
        };
      });
      syncChatAvatarIdentity(acct, () => !isCancelled());
      await get().refreshRoster();
      return true;
    } catch {
      if (isCancelled()) return false;
      await new Promise((r) => setTimeout(r, BOOT_POLL_MS));
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

export interface ChatState extends ReceptionState {
  identityEpoch: number;
  historyRevision: number | null;
  historyScanBusy: boolean;
  historyScanQueue: Conversation[];
  historyScanRevision: number | null;
  historyBaselines: string[];
  historyFullBaselines: string[];
  bootBusy: boolean;
  bootRequest: number;
  intents: Record<string, SendIntent>;
  deleted: Record<string, string[]>;
  statusBusy: boolean;
  runEpoch: number;
  historyRequests: Record<string, number>;
  historyLimits: Record<string, number>;
  activeRequest: number;
  rosterRequest: number;
  favoritesRequest: number;
  loadingRequest: number;
  ready: boolean;
  myId: string;
  myAccountId: string;
  peers: PeerInfo[];
  accounts: AccountInfo[];
  channels: ChannelInfo[];
  /** Each channel's members (by channel_id), cached for the composite group avatar. */
  channelMembersById: Record<string, ChannelMemberInfo[]>;
  active: Conversation | null;
  searchTarget: (SearchTarget & { key: string; request: number }) | null;
  messages: Record<string, ChatMessage[]>;
  reactions: Record<string, ReactionInfo[]>;
  unread: Record<string, number>;
  unreadOwner: string;
  pendingUnknownUnread: Record<string, number>;
  // Conversation keys in open-recency order (most-recent-last); drives cache eviction.
  // Not read by any component selector — purely internal LRU bookkeeping.
  cacheOrder: string[];
  members: ChannelMemberInfo[];
  // The active channel's owner (device user_id). Only the owner may change membership.
  channelOwner: string;
  incomingFiles: IncomingFile[];
  // Per-contact UI prefs (pin + custom alias), keyed by account_id/channel_id. Persisted
  // on the Rust side; mirrored here so the sidebar can sort/rename without a roundtrip.
  favorites: Record<string, FavoriteInfo>;
  loading: boolean;
  historyError: string | null;
  error: string | null; // transient action error (file/reaction send), surfaced to the user
  bootFailed: boolean; // the node never came up within the boot window

  resetForLock: () => void;
  start: () => () => void;
  retryBoot: () => void;
  loadFavorites: () => Promise<void>;
  togglePinned: (id: string, pinned: boolean) => Promise<void>;
  setChannelHidden: (id: string, hidden: boolean) => Promise<void>;
  setAlias: (id: string, alias: string | null) => Promise<void>;
  dismissFile: (fileConv: string) => void;
  clearError: () => void;
  setError: (msg: string) => void;
  refreshRoster: () => Promise<void>;
  deleteContact: (account: string) => Promise<boolean>;
  open: (c: Conversation, target?: SearchTarget) => Promise<void>;
  markActiveRead: () => void;
  markLegacyActiveRead: () => void;
  markMessagesViewed: (key: string, ids: string[]) => void;
  revealUnread: (key: string, id: string) => Promise<boolean>;
  reload: () => Promise<boolean>;
  refreshStatuses: (conversation?: Conversation) => Promise<void>;
  send: (text: string, replyTo: string | null) => Promise<void>;
  admitText: (
    text: string,
    replyTo: string | null,
    expiresAtMs?: number | null,
  ) => boolean;
  retry: (clientId: string) => Promise<void>;
  sendFile: (
    path: string,
    media: boolean,
    expiresAtMs?: number | null,
  ) => Promise<void>;
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
  createChannel: (name: string, memberIds: string[]) => Promise<boolean>;
  dissolveChannel: (channelId: string) => Promise<boolean>;
  addMember: (memberId: string) => Promise<boolean>;
  removeMember: (memberId: string) => Promise<boolean>;
  transferChannelOwner: (memberId: string) => Promise<boolean>;
  leaveChannel: () => Promise<boolean>;
  acceptChannelInvite: () => Promise<boolean>;
  /** Rename a channel for everyone (owner-only; the new name syncs to all members). */
  renameChannel: (channelId: string, name: string) => Promise<void>;
}

/** One empty session snapshot for initial state, login, and app lock. */
function emptySessionData() {
  return {
    ...emptyReception(),
    identityEpoch: 0,
    historyRevision: null,
    historyScanBusy: false,
    historyScanQueue: [],
    historyScanRevision: null,
    historyBaselines: [],
    historyFullBaselines: [],
    bootBusy: false,
    bootRequest: 0,
    intents: {},
    deleted: {},
    statusBusy: false,
    historyRequests: {},
    historyLimits: {},
    activeRequest: 0,
    rosterRequest: 0,
    favoritesRequest: 0,
    loadingRequest: 0,
    ready: false,
    myId: "",
    myAccountId: "",
    peers: [],
    accounts: [],
    channels: [],
    channelMembersById: {},
    active: null,
    searchTarget: null,
    messages: {},
    reactions: {},
    unread: {},
    unreadOwner: "",
    pendingUnknownUnread: {},
    cacheOrder: [],
    members: [],
    channelOwner: "",
    incomingFiles: [],
    favorites: NO_FAVORITES,
    loading: false,
    historyError: null,
    error: null,
    bootFailed: false,
  };
}

export const useChat = create<ChatState>((rawSet, get) => ({
  ...emptySessionData(),
  runEpoch: 0,

  clearError: () => rawSet({ error: null }),
  setError: (msg) => rawSet({ error: msg }),

  resetForLock: () => {
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
    } catch (e) {
      set({ error: `Couldn't update channel visibility: ${errorMessage(e)}` });
    }
    if (lease.current()) await get().loadFavorites();
  },

  // Re-attempt the node-id poll after a boot failure (events + roster interval from the
  // original start() are still live, so we only need to re-resolve my_id/account_id).
  retryBoot: () => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    if (get().bootBusy) return;
    const request = get().bootRequest + 1;
    set({
      bootFailed: false,
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
      );
      if (get().bootRequest === request)
        set({ bootBusy: false, bootFailed: !ok });
    })();
  },

  saveFile: async (fileConv, dest) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    try {
      await chat.saveFile(fileConv, dest);
      if (lease.current()) get().dismissFile(fileConv);
    } catch (e) {
      set({ error: `Couldn't save file: ${errorMessage(e)}` });
    }
  },

  dismissFile: (fileConv) =>
    rawSet((s) => ({
      incomingFiles: s.incomingFiles.filter((f) => f.fileConv !== fileConv),
    })),

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

    const roster = setInterval(() => {
      if (lease.current() && get().ready) {
        void get().refreshRoster();
        void get().refreshStatuses();
      } else if (lease.current() && !get().bootBusy && !get().bootFailed)
        get().retryBoot();
    }, 2000);

    const unlisten = subscribeNodeEvents({
      onDm: (e) => {
        if (lease.current()) get_handleDm(set, get, e);
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
      onProfile: (e) => {
        if (lease.current()) applyChatProfile(e);
      },
      onCallSignal: (e) => {
        if (lease.current()) applyChatCallSignal(e);
      },
    });

    return () => {
      cancelled = true;
      clearInterval(roster);
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

  refreshRoster: async () => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    if (!get().ready || get().historyScanBusy) return;
    const request = get().rosterRequest + 1;
    set({ rosterRequest: request });
    try {
      let identity;
      try {
        identity = await chat.ownerIdentity(lease.owner!);
      } catch {
        if (get().rosterRequest === request) set({ ready: false });
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
          ...invalidateConversationIdentity(),
          identityEpoch: state.identityEpoch + 1,
        }));
        get().retryBoot();
        return;
      }
      const [peers, accounts, channels] = await Promise.all([
        chat.listPeers(),
        chat.listAccounts(),
        chat.listChannels(),
      ]);
      if (!lease.current() || get().rosterRequest !== request) return;
      // Polling returns fresh arrays even when discovery has not changed. Preserve
      // the previous references so sidebar subscribers do not redraw every tick.
      const previous = get();
      const rosterChanged =
        JSON.stringify([
          previous.peers,
          previous.accounts,
          previous.channels,
        ]) !== JSON.stringify([peers, accounts, channels]);
      const resolved = peers.filter(
        (peer) =>
          peer.account_id && previous.pendingUnknownUnread[peer.user_id],
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
          ...(rosterChanged ? { peers, accounts, channels } : {}),
          ...(resolved.length ? { unread, pendingUnknownUnread } : {}),
        });
      }
      const recoveryEntries = channels
        .filter(
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
      // Imports are durable before this process-local revision advances. Reuse
      // the owner-scoped poll so an open conversation updates without notifications.
      if (identity.history_revision !== undefined) {
        const changed = get().historyRevision !== identity.history_revision;
        let loaded = true;
        const conversations: Conversation[] = [
          ...accounts.map((account) => ({
            kind: "account" as const,
            id: account.account_id,
            name: "",
          })),
          ...channels.map((channel) => ({
            kind: "channel" as const,
            id: channel.channel_id,
            name: channel.name,
          })),
        ];
        const revision = identity.history_revision;
        // Keep a bounded rotating work queue: later conversations receive a turn,
        // and an individual failure cannot force earlier snapshots to repeat.
        if (get().historyScanRevision !== revision) {
          set({
            historyScanRevision: revision,
            historyScanQueue: [
              ...get().historyScanQueue,
              ...conversations.filter(
                (c) =>
                  !get().historyScanQueue.some(
                    (pending) => convKey(pending) === convKey(c),
                  ) &&
                  (get().historyScanRevision === null ||
                    changed ||
                    !get().historyBaselines.includes(convKey(c))),
              ),
            ].slice(0, 4096),
          });
        } else if (!get().historyScanQueue.length && !changed) {
          const missing = conversations.filter(
            (c) => !get().historyBaselines.includes(convKey(c)),
          );
          if (missing.length) set({ historyScanQueue: missing.slice(0, 4096) });
        }
        const round = get().historyScanQueue.slice(0, 8);
        if (round.length) {
          set({ historyScanBusy: true });
          try {
            for (const conversation of round) {
              const key = convKey(conversation);
              const initialized = Object.prototype.hasOwnProperty.call(
                get().ledger,
                key,
              );
              try {
                const rows =
                  conversation.kind === "channel"
                    ? await chat.channelHistory(conversation.id, 500)
                    : await chat.ownerHistory(
                        lease.owner!,
                        conversation.id,
                        500,
                      );
                if (!lease.current()) return;
                set((state) => ({
                  ...receptionPatch(
                    state,
                    reconcileSnapshotCoverage(state, key, rows, initialized),
                  ),
                  historyFullBaselines: [
                    ...state.historyFullBaselines.filter((k) => k !== key),
                    key,
                  ].slice(-4096),
                  historyScanQueue: state.historyScanQueue.filter(
                    (c) => convKey(c) !== key,
                  ),
                  historyBaselines: [
                    ...state.historyBaselines.filter((k) => k !== key),
                    key,
                  ].slice(-4096),
                }));
              } catch {
                loaded = false;
                set((state) => ({
                  historyScanQueue: [
                    ...state.historyScanQueue.filter((c) => convKey(c) !== key),
                    conversation,
                  ],
                }));
              }
            }
          } finally {
            set({ historyScanBusy: false });
          }
        }
        if (!lease.current()) return;
        if (changed && get().active) loaded = (await get().reload()) && loaded;
        if (!lease.current()) return;
        if (loaded && !get().historyScanQueue.length)
          set({ historyRevision: get().historyScanRevision ?? revision });
      }
      // Cache each channel's members for the composite group avatar (best-effort per
      // channel; a single failure just leaves that channel's montage on its fallback).
      const memberEntries = await Promise.all(
        channels.map(async (c) => {
          try {
            return [
              c.channel_id,
              (await chat.channelMembers(c.channel_id)).members,
            ] as const;
          } catch {
            return [c.channel_id, [] as ChannelMemberInfo[]] as const;
          }
        }),
      );
      if (get().rosterRequest === request) {
        const channelMembersById = Object.fromEntries(memberEntries);
        if (
          JSON.stringify(get().channelMembersById) !==
          JSON.stringify(channelMembersById)
        )
          set({ channelMembersById });
      }
    } catch {
      // node may still be starting; ignore
    }
  },

  open: async (c, target) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const request = get().activeRequest + 1;
    set((state) => openConversationState(state, c, target, request));
    await get().reload();
    if (!lease.current() || get().activeRequest !== request) return;
    if (
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
        const info = await chat.channelMembers(c.id);
        if (!lease.current() || get().activeRequest !== request) return;
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
    rawSet((state) => ({
      ...viewIdentities(state, key, Object.keys(state.ledger[key] ?? {})),
      overflowUnread: { ...state.overflowUnread, [key]: 0 },
      unread: { ...state.unread, [key]: 0 },
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
      (id) => present.has(id) && state.ledger[key]?.[id]?.seen !== true,
    );
    if (!viewed.length) return;
    rawSet((state) =>
      receptionPatch(state, viewIdentities(state, key, viewed)),
    );
  },

  revealUnread: async (key, id) => {
    const lease = captureChat(get);
    if (!lease.current() || !get().active || convKey(get().active!) !== key)
      return false;
    if (get().messages[key]?.some((row) => row.id === id)) return true;
    const request = get().activeRequest;
    rawSet((state) => ({
      historyLimits: boundedConversationMap(state.historyLimits, key, 500),
    }));
    // Expanding an archive is a baseline operation unless an observed import
    // revision already proves that local history is changing.
    const imported =
      get().historyRevision !== null &&
      get().historyScanRevision !== get().historyRevision;
    const loaded = await reloadConversation(rawSet, get, imported);
    return (
      loaded &&
      lease.current() &&
      get().activeRequest === request &&
      get().active !== null &&
      convKey(get().active!) === key &&
      Boolean(get().messages[key]?.some((row) => row.id === id))
    );
  },

  reload: () => reloadConversation(rawSet, get),
  refreshStatuses: (conversation) =>
    refreshDeliveryStatuses(rawSet, get, conversation),

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
  admitText: (text, replyTo, expiresAtMs) => {
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
      },
    );
    if (!dispatch) return false;
    void dispatch;
    return true;
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
      set((state) => deleteConversationMessage(state, key, target));
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
    try {
      await chat.clearConversation(c.id, isChannelConv(c));
      if (!lease.current()) return false;
      set((state) => clearConversationMessages(state, key));
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
    await sendIntent(set, get, c, {
      kind: "sticker",
      stickerId,
      fallback,
      expiresAtMs,
    });
  },

  createChannel: async (name, memberIds) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return false;
    const request = get().activeRequest + 1;
    set({ activeRequest: request });
    const id = await chat.createChannel(name, memberIds);
    if (!lease.current() || get().activeRequest !== request) return false;
    await get().refreshRoster();
    if (!lease.current() || get().activeRequest !== request) return false;
    await get().open({ kind: "channel", id, name });
    return lease.current();
  },

  addMember: async (memberId) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return false;
    const c = get().active;
    if (!c || c.kind !== "channel") return false;
    const request = get().activeRequest + 1;
    set({ activeRequest: request });
    try {
      await chat.addChannelMember(c.id, memberId);
      if (!lease.current() || get().activeRequest !== request) return false;
      const info = await chat.channelMembers(c.id);
      if (!lease.current() || get().activeRequest !== request) return false;
      set({ members: info.members, channelOwner: info.owner });
      void get().refreshRoster();
      return true;
    } catch (e) {
      if (get().activeRequest === request)
        set({ error: `Couldn't add member: ${errorMessage(e)}` });
      return false;
    }
  },

  removeMember: async (memberId) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return false;
    const c = get().active;
    if (!c || c.kind !== "channel") return false;
    const request = get().activeRequest + 1;
    set({ activeRequest: request });
    try {
      await chat.removeChannelMember(c.id, memberId);
      if (!lease.current() || get().activeRequest !== request) return false;
      const info = await chat.channelMembers(c.id);
      if (!lease.current() || get().activeRequest !== request) return false;
      set({ members: info.members, channelOwner: info.owner });
      void get().refreshRoster();
      return true;
    } catch (e) {
      if (get().activeRequest === request)
        set({ error: `Couldn't remove member: ${errorMessage(e)}` });
      return false;
    }
  },

  transferChannelOwner: async (memberId) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return false;
    const c = get().active;
    if (!c || c.kind !== "channel") return false;
    const request = get().activeRequest + 1;
    set({ activeRequest: request });
    try {
      await chat.transferChannelOwner(c.id, memberId);
      if (!lease.current() || get().activeRequest !== request) return false;
      const info = await chat.channelMembers(c.id);
      if (!lease.current() || get().activeRequest !== request) return false;
      set({ members: info.members, channelOwner: info.owner });
      void get().refreshRoster();
      return true;
    } catch (e) {
      if (get().activeRequest === request)
        set({
          error: `Couldn't transfer channel ownership: ${errorMessage(e)}`,
        });
      return false;
    }
  },

  leaveChannel: async () => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return false;
    const c = get().active;
    if (!c || c.kind !== "channel") return false;
    try {
      await chat.leaveChannel(c.id);
      if (!lease.current()) return false;
      set((state) => {
        const unread = { ...state.unread };
        delete unread[convKey(c)];
        const leavingActive =
          state.active?.kind === "channel" && state.active.id === c.id;
        return {
          active: leavingActive ? null : state.active,
          members: leavingActive ? [] : state.members,
          channelOwner: leavingActive ? "" : state.channelOwner,
          unread,
        };
      });
      await get().refreshRoster();
      return lease.current();
    } catch (e) {
      set({ error: `Couldn't leave channel: ${errorMessage(e)}` });
      return false;
    }
  },

  acceptChannelInvite: async () => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return false;
    const c = get().active;
    if (!c || c.kind !== "channel") return false;
    const request = get().activeRequest;
    try {
      await chat.acceptChannelInvite(c.id);
      if (!lease.current()) return false;
      await get().refreshRoster();
      if (
        lease.current() &&
        get().activeRequest === request &&
        get().active?.id === c.id
      ) {
        try {
          const info = await chat.channelMembers(c.id);
          if (
            lease.current() &&
            get().activeRequest === request &&
            get().active?.id === c.id
          )
            set({ members: info.members, channelOwner: info.owner });
        } catch {
          // Acceptance succeeded; opening the member list can retry its details.
        }
      }
      return lease.current();
    } catch (e) {
      set({ error: `Couldn't accept invitation: ${errorMessage(e)}` });
      return false;
    }
  },

  dissolveChannel: async (channelId) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return false;
    try {
      await chat.dissolveChannel(channelId);
      if (!lease.current()) return false;
      set((state) => ({
        channels: state.channels.map((channel) =>
          channel.channel_id === channelId
            ? { ...channel, dissolved: true }
            : channel,
        ),
      }));
      await get().refreshRoster();
      return lease.current();
    } catch (e) {
      set({ error: `Couldn't dissolve channel: ${errorMessage(e)}` });
      return false;
    }
  },

  renameChannel: async (channelId, name) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    try {
      await chat.renameChannel(channelId, name);
      if (!lease.current()) return;
      await get().refreshRoster();
      if (!lease.current()) return;
      // Keep the open conversation's header in sync if it's the one we renamed.
      const active = get().active;
      if (active?.kind === "channel" && active.id === channelId) {
        set({ active: { ...active, name } });
      }
    } catch (e) {
      set({ error: `Couldn't rename channel: ${errorMessage(e)}` });
    }
  },
}));

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
  const accountId = peer?.account_id;
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
  if (isActive) void get().reload();
  bump(set, key, isActive && isConversationVisible());
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
        receiveIdentity(state, key, { ...e, own_device: own }),
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
        receiveIdentity(state, key, { ...e, own_device: own }),
      ),
    );
  if (!e.event_id && knownTray) return;
  if (!own && !knownTray && !knownIdentity)
    set((state) => ({
      incomingFiles: [
        {
          fromName: peer?.name || e.from,
          name: e.name,
          size: e.size,
          mime: e.mime,
          fileConv: e.file_conv,
          media: e.media,
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

/** Merge identity counts without losing legacy-host unread counters. */
export function receptionPatch(
  state: ChatState,
  next: ReceptionState,
): Partial<ChatState> {
  if (next === state) return {};
  const unread = { ...state.unread };
  for (const key of new Set([
    ...Object.keys(state.unreadIds),
    ...Object.keys(next.unreadIds),
    ...Object.keys(state.overflowUnread),
    ...Object.keys(next.overflowUnread),
  ])) {
    unread[key] = Math.max(
      0,
      (unread[key] ?? 0) +
        (next.unreadIds[key]?.length ?? 0) +
        (next.overflowUnread[key] ?? 0) -
        (state.unreadIds[key]?.length ?? 0) -
        (state.overflowUnread[key] ?? 0),
    );
  }
  return { ...next, unread };
}
useChat.subscribe((state, previous) => {
  if (
    state.ledger !== previous.ledger &&
    state.unreadOwner &&
    state.myAccountId
  )
    saveReceptionLedger(state.unreadOwner, state.myAccountId, state.ledger);
  if (
    state.overflowUnread !== previous.overflowUnread &&
    state.unreadOwner &&
    state.myAccountId
  )
    saveUnreadOverflow(
      state.unreadOwner,
      state.myAccountId,
      state.overflowUnread,
    );
  if (
    state.historyFullBaselines !== previous.historyFullBaselines &&
    state.unreadOwner &&
    state.myAccountId
  )
    saveFullHistoryBaselines(
      state.unreadOwner,
      state.myAccountId,
      state.historyFullBaselines,
    );
});

/** Expanded archive coverage is distinct from the latest presentation window. */
function reconcileSnapshotCoverage(
  state: ChatState,
  key: string,
  rows: import("@/lib/types").HistoryItem[],
  initialized: boolean,
): ReceptionState {
  const messages = rows.map((row) => ({ id: row.id, fromMe: row.from_me }));
  const known = initialized && state.historyBaselines.includes(key);
  if (known && !state.historyFullBaselines.includes(key)) {
    // Only the comparable latest window has a baseline. Older expansion rows are
    // preexisting archive until a full snapshot establishes local coverage.
    const recent = reconcileHistory(state, key, messages.slice(-200), true);
    return reconcileHistory(recent, key, messages, false);
  }
  return reconcileHistory(state, key, messages, known);
}
