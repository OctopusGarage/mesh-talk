import { create } from "zustand";
import { chat, favorites as favoritesApi } from "@/lib/api";
import { errorMessage, sendFailReason, type SendFailReason } from "@/lib/error";
import { subscribeNodeEvents } from "@/lib/events";
import { notifyInbound } from "@/lib/notify";
import { useAvatars } from "@/store/avatars";
import { useTransfers } from "@/store/transfers";
import type {
  AccountInfo,
  ChannelInfo,
  ChannelMemberInfo,
  ChannelMessageEvent,
  DmReceivedEvent,
  FavoriteInfo,
  FileReceivedEvent,
  HistoryItem,
  PeerInfo,
  ReactionInfo,
} from "@/lib/types";

const HISTORY_LIMIT = 200;

// Cap how many conversations keep cached message/reaction arrays in memory at once.
// Touching hundreds of conversations in a session would otherwise retain them all until
// logout. Only the cached arrays for the least-recently-opened conversations beyond this
// many are evicted; `unread` (tiny, drives sidebar badges) is never touched, and a reopen
// repopulates from the log via reload(). Active conversation is always retained.
const CONV_CACHE_LIMIT = 50;

/**
 * Evict message/reaction caches for the least-recently-opened conversations, keeping at
 * most CONV_CACHE_LIMIT entries. `order` is most-recent-last; `keep` (the active key) is
 * never evicted. Returns the trimmed maps plus the pruned order list. Pure — no set().
 */
function evictCaches(
  messages: Record<string, ChatMessage[]>,
  reactions: Record<string, ReactionInfo[]>,
  order: string[],
  keep: string,
): {
  messages: Record<string, ChatMessage[]>;
  reactions: Record<string, ReactionInfo[]>;
  order: string[];
} {
  if (order.length <= CONV_CACHE_LIMIT) return { messages, reactions, order };
  const excess = order.length - CONV_CACHE_LIMIT;
  const evicted = new Set<string>();
  for (let i = 0; i < order.length && evicted.size < excess; i++) {
    if (order[i] !== keep) evicted.add(order[i]);
  }
  const nextMessages: Record<string, ChatMessage[]> = {};
  for (const k of Object.keys(messages))
    if (!evicted.has(k)) nextMessages[k] = messages[k];
  const nextReactions: Record<string, ReactionInfo[]> = {};
  for (const k of Object.keys(reactions))
    if (!evicted.has(k)) nextReactions[k] = reactions[k];
  return {
    messages: nextMessages,
    reactions: nextReactions,
    order: order.filter((k) => !evicted.has(k)),
  };
}

export type ConvKind = "account" | "channel";

export interface Conversation {
  kind: ConvKind;
  id: string; // account_id (hex) | channel_id (hex)
  name: string;
}

export interface ChatMessage {
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
  file?: { name: string; size: number; mime: string; fileConv: string } | null;
}

export interface IncomingFile {
  fromName: string;
  name: string;
  size: number;
  fileConv: string;
}

export const convKey = (c: Conversation) => `${c.kind}:${c.id}`;

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

let clientIdCounter = 0;
/** A process-unique id for an optimistic message bubble. */
function nextClientId(): string {
  clientIdCounter += 1;
  return `c${clientIdCounter}`;
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
      const id = await chat.myId();
      const acct = await chat.accountId();
      set({ myId: id, myAccountId: acct, ready: true });
      // Tell the avatars store who "we" are (so setting our own avatar publishes it) and
      // pull avatars peers already propagated to us (durable across restart).
      useAvatars.getState().setOwnId(acct);
      void useAvatars.getState().loadPeers();
      await get().refreshRoster();
      return true;
    } catch {
      await new Promise((r) => setTimeout(r, BOOT_POLL_MS));
    }
  }
  return false;
}

export function fromHistoryItem(h: HistoryItem): ChatMessage {
  return {
    id: h.id,
    fromMe: h.from_me,
    who: h.who,
    text: h.text,
    wallClock: h.wall_clock,
    replyTo: h.reply_to,
    file: h.file
      ? {
          name: h.file.name,
          size: h.file.size,
          mime: h.file.mime,
          fileConv: h.file.file_conv,
        }
      : null,
  };
}

// --- per-conversation API dispatch ----------------------------------------

function historyFor(c: Conversation) {
  return c.kind === "account"
    ? chat.accountHistory(c.id, HISTORY_LIMIT)
    : chat.channelHistory(c.id, HISTORY_LIMIT);
}
function reactionsFor(c: Conversation) {
  return c.kind === "account"
    ? chat.accountReactions(c.id)
    : chat.channelReactions(c.id);
}
function sendTextFor(c: Conversation, text: string, replyTo: string | null) {
  return c.kind === "account"
    ? chat.sendToAccount(c.id, text, replyTo)
    : chat.sendChannelMessage(c.id, text, replyTo);
}
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
function sendFileFor(c: Conversation, path: string) {
  return c.kind === "account"
    ? chat.sendFileToAccount(c.id, path)
    : chat.sendFileChannel(c.id, path);
}

interface ChatState {
  ready: boolean;
  myId: string;
  myAccountId: string;
  peers: PeerInfo[];
  accounts: AccountInfo[];
  channels: ChannelInfo[];
  active: Conversation | null;
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
  open: (c: Conversation) => Promise<void>;
  reload: () => Promise<void>;
  send: (text: string, replyTo: string | null) => Promise<void>;
  retry: (clientId: string) => Promise<void>;
  sendFile: (path: string) => Promise<void>;
  saveFile: (fileConv: string, dest: string) => Promise<void>;
  toggleReaction: (target: string, emoji: string) => Promise<void>;
  createChannel: (name: string, memberIds: string[]) => Promise<void>;
  addMember: (memberId: string) => Promise<void>;
  removeMember: (memberId: string) => Promise<void>;
}

export const useChat = create<ChatState>((set, get) => ({
  ready: false,
  myId: "",
  myAccountId: "",
  peers: [],
  accounts: [],
  channels: [],
  active: null,
  messages: {},
  reactions: {},
  unread: {},
  cacheOrder: [],
  members: [],
  channelOwner: "",
  incomingFiles: [],
  favorites: NO_FAVORITES,
  loading: false,
  error: null,
  bootFailed: false,

  clearError: () => set({ error: null }),
  setError: (msg) => set({ error: msg }),

  loadFavorites: async () => {
    try {
      const list = await favoritesApi.get();
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
    await get().loadFavorites();
  },

  setAlias: async (id, alias) => {
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
    await get().loadFavorites();
  },

  // Re-attempt the node-id poll after a boot failure (events + roster interval from the
  // original start() are still live, so we only need to re-resolve my_id/account_id).
  retryBoot: () => {
    set({ bootFailed: false, ready: false });
    void (async () => {
      // No cancel token here: retryBoot is a one-shot user action with no teardown hook,
      // so it always runs to completion (matching the prior behavior).
      const ok = await pollUntilReady(set, get, () => false);
      if (!ok) set({ bootFailed: true });
    })();
  },

  saveFile: async (fileConv, dest) => {
    try {
      await chat.saveFile(fileConv, dest);
      get().dismissFile(fileConv);
    } catch (e) {
      set({ error: `Couldn't save file: ${errorMessage(e)}` });
    }
  },

  dismissFile: (fileConv) =>
    set((s) => ({
      incomingFiles: s.incomingFiles.filter((f) => f.fileConv !== fileConv),
    })),

  start: () => {
    // Fresh slate per login (the store survives logout/login of a different account).
    lastUnknownSenderRefresh = 0;
    useTransfers.getState().reset();
    set({
      ready: false,
      error: null,
      bootFailed: false,
      myId: "",
      myAccountId: "",
      peers: [],
      accounts: [],
      channels: [],
      active: null,
      messages: {},
      reactions: {},
      unread: {},
      cacheOrder: [],
      members: [],
      channelOwner: "",
      incomingFiles: [],
      favorites: NO_FAVORITES,
    });
    // Favorites are local UI prefs (no node needed) — load them right away.
    void get().loadFavorites();
    // Poll my_id until the node finishes opening (post-login KDF unlock takes a moment).
    let cancelled = false;
    void (async () => {
      const ok = await pollUntilReady(set, get, () => cancelled);
      // Exhausted the boot window without the node coming up — surface it so the UI can
      // offer a retry instead of sitting on "starting…" forever. (Skip if cancelled by
      // teardown, so a logout mid-boot doesn't flash a spurious failure.)
      if (!ok && !cancelled) set({ bootFailed: true });
    })();

    const roster = setInterval(() => {
      if (get().ready) void get().refreshRoster();
    }, 4000);

    const unlisten = subscribeNodeEvents({
      onDm: (e) => get_handleDm(set, get, e),
      onChannelMessage: (e) => get_handleChannel(set, get, e),
      onFile: (e) => get_handleFile(set, get, e),
      onFileProgress: (e) => useTransfers.getState().applyProgress(e),
      onProfile: (e) =>
        useAvatars.getState().mergeReceived(e.account_id, e.avatar),
    });

    return () => {
      cancelled = true;
      clearInterval(roster);
      unlisten();
    };
  },

  refreshRoster: async () => {
    try {
      const [peers, accounts, channels] = await Promise.all([
        chat.listPeers(),
        chat.listAccounts(),
        chat.listChannels(),
      ]);
      set({ peers, accounts, channels });
    } catch {
      // node may still be starting; ignore
    }
  },

  open: async (c) => {
    const key = convKey(c);
    set((s) => {
      // Mark this conversation most-recently-opened, then evict the message/reaction
      // caches of the least-recently-opened beyond the cap (never the active one).
      const order = [...s.cacheOrder.filter((k) => k !== key), key];
      const trimmed = evictCaches(s.messages, s.reactions, order, key);
      return {
        active: c,
        unread: { ...s.unread, [key]: 0 },
        members: [],
        channelOwner: "",
        messages: trimmed.messages,
        reactions: trimmed.reactions,
        cacheOrder: trimmed.order,
      };
    });
    await get().reload();
    if (c.kind === "channel") {
      try {
        const info = await chat.channelMembers(c.id);
        set({ members: info.members, channelOwner: info.owner });
      } catch {
        /* ignore */
      }
    }
  },

  reload: async () => {
    const c = get().active;
    if (!c) return;
    const key = convKey(c);
    set({ loading: true });
    try {
      const [items, reacts] = await Promise.all([
        historyFor(c),
        reactionsFor(c),
      ]);
      set((s) => ({
        messages: { ...s.messages, [key]: items.map(fromHistoryItem) },
        reactions: { ...s.reactions, [key]: reacts },
        loading: false,
      }));
    } catch {
      set({ loading: false });
    }
  },

  send: async (text, replyTo) => {
    const c = get().active;
    if (!c || !text.trim()) return;
    const key = convKey(c);
    const clientId = nextClientId();
    const optimistic: ChatMessage = {
      id: null,
      fromMe: true,
      who: get().myId,
      text,
      wallClock: Date.now(),
      replyTo,
      pending: true,
      clientId,
    };
    set((s) => ({
      messages: {
        ...s.messages,
        [key]: [...(s.messages[key] ?? []), optimistic],
      },
    }));
    await dispatchSend(set, get, c, key, optimistic);
  },

  // Re-send a previously-failed optimistic bubble (reusing its clientId/text/replyTo).
  // Clears the failed state, shows pending again, then runs the same dispatch as send().
  retry: async (clientId) => {
    const c = get().active;
    if (!c) return;
    const key = convKey(c);
    const arr = get().messages[key] ?? [];
    const orig = arr.find((m) => m.clientId === clientId);
    if (!orig || !orig.failed) return;
    const pending: ChatMessage = {
      ...orig,
      pending: true,
      failed: false,
      failReason: undefined,
      wallClock: Date.now(),
    };
    set((s) => ({
      messages: {
        ...s.messages,
        [key]: (s.messages[key] ?? []).map((m) =>
          m.clientId === clientId ? pending : m,
        ),
      },
    }));
    await dispatchSend(set, get, c, key, pending);
  },

  sendFile: async (path) => {
    const c = get().active;
    if (!c) return;
    try {
      await sendFileFor(c, path);
    } catch (e) {
      set({ error: `Couldn't send file: ${errorMessage(e)}` });
      return;
    }
    if (get().active && convKey(get().active!) === convKey(c))
      await get().reload();
  },

  toggleReaction: async (target, emoji) => {
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
      if (get().active && convKey(get().active!) === key) await get().reload();
    } catch (e) {
      set({ error: `Couldn't update reaction: ${errorMessage(e)}` });
    }
  },

  createChannel: async (name, memberIds) => {
    try {
      const id = await chat.createChannel(name, memberIds);
      await get().refreshRoster();
      await get().open({ kind: "channel", id, name });
    } catch (e) {
      set({ error: `Couldn't create channel: ${errorMessage(e)}` });
    }
  },

  addMember: async (memberId) => {
    const c = get().active;
    if (!c || c.kind !== "channel") return;
    try {
      await chat.addChannelMember(c.id, memberId);
      const info = await chat.channelMembers(c.id);
      set({ members: info.members, channelOwner: info.owner });
      void get().refreshRoster();
    } catch (e) {
      set({ error: `Couldn't add member: ${errorMessage(e)}` });
    }
  },

  removeMember: async (memberId) => {
    const c = get().active;
    if (!c || c.kind !== "channel") return;
    try {
      await chat.removeChannelMember(c.id, memberId);
      const info = await chat.channelMembers(c.id);
      set({ members: info.members, channelOwner: info.owner });
      void get().refreshRoster();
    } catch (e) {
      set({ error: `Couldn't remove member: ${errorMessage(e)}` });
    }
  },
}));

// --- incoming event handlers (module fns to keep the store object lean) -----

type Set = (
  partial: Partial<ChatState> | ((s: ChatState) => Partial<ChatState>),
) => void;
type Get = () => ChatState;

function bump(set: Set, key: string, active: boolean) {
  if (!active) {
    set((s) => ({ unread: { ...s.unread, [key]: (s.unread[key] ?? 0) + 1 } }));
  }
}

// Shared optimistic-send dispatch for send() and retry(): fire the per-conversation send;
// on failure mark the (already-present) bubble failed with a coarse reason; on success
// re-sync from the log so the message gets its real id. The bubble (matched by clientId)
// is assumed to already be in `messages[key]` as pending.
async function dispatchSend(
  set: Set,
  get: Get,
  c: Conversation,
  key: string,
  bubble: ChatMessage,
) {
  try {
    await sendTextFor(c, bubble.text, bubble.replyTo);
  } catch (e) {
    // Keep the bubble visible, marked failed, instead of silently dropping it. Match by
    // clientId (not object identity), and re-append if a concurrent reload() already
    // replaced the array — so an inbound message mid-send can't make the failure vanish.
    const reason = sendFailReason(e);
    set((s) => {
      const arr = s.messages[key] ?? [];
      const failed: ChatMessage = {
        ...bubble,
        pending: false,
        failed: true,
        failReason: reason,
      };
      const next = arr.some((m) => m.clientId === bubble.clientId)
        ? arr.map((m) => (m.clientId === bubble.clientId ? failed : m))
        : [...arr, failed];
      return { messages: { ...s.messages, [key]: next } };
    });
    return;
  }
  // Success: re-sync from the log so the message gets its real id (needed for reactions).
  if (get().active && convKey(get().active!) === key) await get().reload();
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
          incomingFiles: [
            { fromName, name: e.name, size: e.size, fileConv: e.file_conv },
            ...s.incomingFiles,
          ],
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
