import { create } from "zustand";
import { chat, favorites as favoritesApi } from "@/lib/api";
import { errorMessage, type SendFailReason } from "@/lib/error";
import { subscribeNodeEvents } from "@/lib/events";
import { notifyInbound } from "@/lib/notify";
import { useAvatars } from "@/store/avatars";
import { useCalls } from "@/store/calls";
import { useTransfers } from "@/store/transfers";
import { resetFileAvailability } from "@/store/fileAvailability";
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
  delivery?: "awaiting" | "delivered";
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
      set({ myId: identity.device_id, myAccountId: acct, ready: true });
      // Tell the avatars store who "we" are (so setting our own avatar publishes it) and
      // pull avatars peers already propagated to us (durable across restart).
      useAvatars.getState().setOwnId(acct);
      void useAvatars.getState().loadPeers(() => !isCancelled());
      // Re-publish our own avatar so the node re-holds it and propagates to peers after a
      // restart (otherwise contacts can't pull it until we manually re-set the photo).
      void useAvatars.getState().reassertOwn(() => !isCancelled());
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

export interface ChatState {
  identityEpoch: number;
  bootBusy: boolean;
  bootRequest: number;
  intents: Record<string, SendIntent>;
  deleted: Record<string, string[]>;
  statusBusy: boolean;
  runEpoch: number;
  historyRequests: Record<string, number>;
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

  start: () => () => void;
  retryBoot: () => void;
  loadFavorites: () => Promise<void>;
  togglePinned: (id: string, pinned: boolean) => Promise<void>;
  setAlias: (id: string, alias: string | null) => Promise<void>;
  dismissFile: (fileConv: string) => void;
  clearError: () => void;
  setError: (msg: string) => void;
  refreshRoster: () => Promise<void>;
  open: (c: Conversation, target?: SearchTarget) => Promise<void>;
  reload: () => Promise<void>;
  refreshStatuses: (conversation?: Conversation) => Promise<void>;
  send: (text: string, replyTo: string | null) => Promise<void>;
  admitText: (text: string, replyTo: string | null) => boolean;
  retry: (clientId: string) => Promise<void>;
  sendFile: (path: string, media: boolean) => Promise<void>;
  saveFile: (fileConv: string, dest: string) => Promise<void>;
  toggleReaction: (target: string, emoji: string) => Promise<void>;
  /** Delete one message from this device only (local). */
  deleteMessage: (target: string) => Promise<void>;
  /** Recall (unsend) one of my own messages within the 2-minute window. */
  recallMessage: (target: string) => Promise<void>;
  /** Clear all local history for the active conversation. */
  clearConversation: () => Promise<void>;
  /** Send an animated sticker (by id) as its own message; `fallback` is its emoji char. */
  sendSticker: (stickerId: string, fallback: string) => Promise<void>;
  createChannel: (name: string, memberIds: string[]) => Promise<boolean>;
  addMember: (memberId: string) => Promise<void>;
  removeMember: (memberId: string) => Promise<void>;
  /** Rename a channel for everyone (owner-only; the new name syncs to all members). */
  renameChannel: (channelId: string, name: string) => Promise<void>;
}

export const useChat = create<ChatState>((rawSet, get) => ({
  identityEpoch: 0,
  bootBusy: false,
  bootRequest: 0,
  intents: {},
  deleted: {},
  statusBusy: false,
  runEpoch: 0,
  historyRequests: {},
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
  cacheOrder: [],
  members: [],
  channelOwner: "",
  incomingFiles: [],
  favorites: NO_FAVORITES,
  loading: false,
  historyError: null,
  error: null,
  bootFailed: false,

  clearError: () => rawSet({ error: null }),
  setError: (msg) => rawSet({ error: msg }),

  loadFavorites: async () => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const request = get().favoritesRequest + 1;
    set({ favoritesRequest: request });
    try {
      const list = await favoritesApi.get();
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
          [id]: { id, pinned, custom_alias: prev?.custom_alias ?? null },
        },
      };
    });
    try {
      await favoritesApi.setFavorite(id, pinned);
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
          [id]: { id, pinned: prev?.pinned ?? false, custom_alias: trimmed },
        },
      };
    });
    try {
      await favoritesApi.setAlias(id, trimmed);
    } catch (e) {
      set({ error: `Couldn't rename contact: ${errorMessage(e)}` });
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
    useTransfers.getState().reset();
    resetFileAvailability();
    useCalls.getState().teardown();
    set({
      ready: false,
      error: null,
      bootFailed: false,
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
      cacheOrder: [],
      members: [],
      channelOwner: "",
      incomingFiles: [],
      favorites: NO_FAVORITES,
      historyRequests: {},
      activeRequest: 0,
      rosterRequest: 0,
      favoritesRequest: 0,
      loadingRequest: 0,
      loading: false,
      historyError: null,
      identityEpoch: 0,
      bootBusy: true,
      bootRequest: 1,
      intents: {},
      deleted: {},
      statusBusy: false,
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
        if (lease.current()) useTransfers.getState().applyProgress(e);
      },
      onProfile: (e) => {
        if (lease.current())
          useAvatars.getState().mergeReceived(e.account_id, e.avatar);
      },
      onCallSignal: (e) => {
        if (lease.current()) useCalls.getState().onSignal(e);
      },
    });

    return () => {
      cancelled = true;
      clearInterval(roster);
      unlisten();
      if (get().runEpoch === lease.run) {
        useCalls.getState().teardown();
        rawSet((s) => ({ runEpoch: s.runEpoch + 1, ready: false }));
      }
    };
  },

  refreshRoster: async () => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    if (!get().ready) return;
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
        resetFileAvailability();
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
      if (
        JSON.stringify([
          previous.peers,
          previous.accounts,
          previous.channels,
        ]) !== JSON.stringify([peers, accounts, channels])
      )
        set({ peers, accounts, channels });
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
  admitText: (text, replyTo) => {
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

  sendFile: async (path, media) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const c = get().active;
    if (!get().ready || !c) return;
    await sendIntent(set, get, c, { kind: "file", path, media });
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
    if (!lease.current()) return;
    const c = get().active;
    if (!c) return;
    const key = convKey(c);
    try {
      await chat.clearConversation(c.id, isChannelConv(c));
      if (!lease.current()) return;
      set((state) => clearConversationMessages(state, key));
      if (get().active && convKey(get().active!) === key) await get().reload();
    } catch (e) {
      set({ error: `Couldn't clear history: ${errorMessage(e)}` });
    }
  },

  sendSticker: async (stickerId, fallback) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const c = get().active;
    if (!get().ready || !c) return;
    await sendIntent(set, get, c, { kind: "sticker", stickerId, fallback });
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
    if (!lease.current()) return;
    const c = get().active;
    if (!c || c.kind !== "channel") return;
    const request = get().activeRequest + 1;
    set({ activeRequest: request });
    try {
      await chat.addChannelMember(c.id, memberId);
      if (!lease.current() || get().activeRequest !== request) return;
      const info = await chat.channelMembers(c.id);
      if (!lease.current() || get().activeRequest !== request) return;
      set({ members: info.members, channelOwner: info.owner });
      void get().refreshRoster();
    } catch (e) {
      if (get().activeRequest === request)
        set({ error: `Couldn't add member: ${errorMessage(e)}` });
    }
  },

  removeMember: async (memberId) => {
    const lease = captureChat(get);
    const set = guardedSet(rawSet, lease.current);
    if (!lease.current()) return;
    const c = get().active;
    if (!c || c.kind !== "channel") return;
    const request = get().activeRequest + 1;
    set({ activeRequest: request });
    try {
      await chat.removeChannelMember(c.id, memberId);
      if (!lease.current() || get().activeRequest !== request) return;
      const info = await chat.channelMembers(c.id);
      if (!lease.current() || get().activeRequest !== request) return;
      set({ members: info.members, channelOwner: info.owner });
      void get().refreshRoster();
    } catch (e) {
      if (get().activeRequest === request)
        set({ error: `Couldn't remove member: ${errorMessage(e)}` });
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

let lastUnknownSenderRefresh = 0;

function get_handleDm(set: Set, get: Get, e: DmReceivedEvent) {
  // Route to the sender's account conversation (multi-device aware).
  const peer = get().peers.find((p) => p.user_id === e.from);
  const accountId = peer?.account_id;
  if (!accountId) {
    // Unknown sender (not yet discovered). Throttle: a burst from an undiscovered account
    // would otherwise fire one roster refresh (3 invokes) per message. The 4s interval
    // poll also covers this.
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
  bump(set, key, isActive);
  void notifyInbound(conv.name || "New message", e.text, isActive);
}

function get_handleChannel(set: Set, get: Get, e: ChannelMessageEvent) {
  const conv: Conversation = {
    kind: "channel",
    id: e.channel_id,
    name: e.channel_name,
  };
  const key = convKey(conv);
  const isActive = get().active != null && convKey(get().active!) === key;
  if (isActive) void get().reload();
  bump(set, key, isActive);
  void notifyInbound(e.channel_name, e.text, isActive);
}

function get_handleFile(set: Set, get: Get, e: FileReceivedEvent) {
  // Surface received files in the dedicated tray (quick Save via a native dialog), and —
  // since files are now first-class conversation messages — reload the active conversation
  // so the new media bubble appears inline. De-dupe the tray by file_conv.
  const peer = get().peers.find((p) => p.user_id === e.from);
  const fromName = peer?.name || e.from;
  set((s) =>
    s.incomingFiles.some((f) => f.fileConv === e.file_conv)
      ? {}
      : {
          // Cap the tray list so a long session in a busy channel can't grow it without
          // bound (entries otherwise leave only on explicit save/dismiss).
          incomingFiles: [
            {
              fromName,
              name: e.name,
              size: e.size,
              fileConv: e.file_conv,
              media: e.media,
            },
            ...s.incomingFiles,
          ].slice(0, INCOMING_FILES_CAP),
        },
  );

  // Route the file to its conversation so its bubble lands in the stream. A channel file's
  // `conv` is the channel id; a DM file is filed (on both sides) under the sender's ACCOUNT
  // conversation, matching get_handleDm. If that conversation is active, reload it so the
  // new bubble appears; otherwise bump its unread badge.
  const isChannelFile = get().channels.some((c) => c.channel_id === e.conv);
  let key: string | null = null;
  if (isChannelFile) {
    key = convKey({ kind: "channel", id: e.conv, name: "" });
  } else if (peer?.account_id) {
    key = convKey({ kind: "account", id: peer.account_id, name: "" });
  }
  if (!key) return;
  const isActive = get().active != null && convKey(get().active!) === key;
  if (isActive) void get().reload();
  bump(set, key, isActive);
}
