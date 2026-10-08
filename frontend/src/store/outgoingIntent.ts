import { chat } from "@/lib/api";
import type { HistoryItem } from "@/lib/types";
import { errorMessage, sendFailReason } from "@/lib/error";
import { captureChatOwner } from "./ownership";
import type { ChatMessage, Conversation, Set, Get } from "./chat";

// Cap how many conversations keep cached message/reaction arrays in memory at once.
// Touching hundreds of conversations in a session would otherwise retain them all until
// logout. Only the cached arrays for the least-recently-opened conversations beyond this
// many are evicted; `unread` (tiny, drives sidebar badges) is never touched, and a reopen
// repopulates from the log via reload(). Active conversation is always retained.
export const CONV_CACHE_LIMIT = 50;

export function boundedConversationMap<T>(
  previous: Record<string, T>,
  key: string,
  value: T,
): Record<string, T> {
  return Object.fromEntries([
    ...Object.entries(previous)
      .filter(([k]) => k !== key)
      .slice(-(CONV_CACHE_LIMIT - 1)),
    [key, value],
  ]);
}

export const convKey = (c: Conversation) => `${c.kind}:${c.id}`;

export function fromHistoryItem(h: HistoryItem): ChatMessage {
  return {
    id: h.id,
    fromMe: h.from_me,
    who: h.who,
    text: h.text,
    wallClock: h.wall_clock,
    replyTo: h.reply_to,
    recalled: h.recalled,
    recalledText: h.recalled_text,
    sticker: h.sticker,
    file: h.file
      ? {
          name: h.file.name,
          size: h.file.size,
          mime: h.file.mime,
          fileConv: h.file.file_conv,
          media: h.file.media,
        }
      : null,
  };
}

export const SEND_INTENT_CAP = 256;
export type SendPayload =
  | { kind: "text"; text: string; replyTo: string | null }
  | { kind: "sticker"; stickerId: string; fallback: string }
  | { kind: "file"; path: string; media: boolean };
export interface SendIntent {
  conversation: Conversation;
  payload: SendPayload;
  message: ChatMessage;
  /** Exact deletion protection owned by the bounded intent, never by an LRU scope. */
  deletedIds?: string[];
  /** Capacity fallback: accepted messages need canonical history, never an overlay. */
  acceptanceHistoryOnly?: boolean;
}

export function protectIntentDeletion(
  intent: SendIntent,
  id: string,
): SendIntent {
  if (
    intent.message.id ||
    intent.acceptanceHistoryOnly ||
    intent.deletedIds?.includes(id)
  )
    return intent;
  const deletedIds = intent.deletedIds ?? [];
  return deletedIds.length < 256
    ? { ...intent, deletedIds: [...deletedIds, id] }
    : { ...intent, acceptanceHistoryOnly: true };
}

/** Exact event ID only. Never associate identical content, paths or filenames. */
export function reconcileMessages(
  history: ChatMessage[],
  previous: ChatMessage[],
  intents: SendIntent[],
  deleted: string[],
): ChatMessage[] {
  const tombstones = new Set(deleted);
  const overlays = intents
    .filter((i) => !i.acceptanceHistoryOnly || !i.message.id)
    .map((i) => i.message);
  const known = [...previous, ...intents.map((i) => i.message)];
  const result: ChatMessage[] = history
    .filter((m) => !m.id || !tombstones.has(m.id))
    .map((m) => {
      const old = m.id ? known.find((p) => p.id === m.id) : undefined;
      return {
        ...m,
        clientId: old?.clientId,
        delivery: old?.delivery,
        pending: false,
        metadataPending: false,
      };
    });
  for (const m of overlays) {
    if (m.id && tombstones.has(m.id)) continue;
    if (!m.id || !result.some((h) => h.id === m.id)) result.push(m);
  }
  return result;
}

export function applyProjection(
  message: ChatMessage,
  statuses: Map<string, "awaiting" | "delivered">,
): ChatMessage {
  if (!message.fromMe || !message.id) return message;
  const projected = statuses.get(message.id);
  return {
    ...message,
    delivery: message.delivery === "delivered" ? "delivered" : projected,
  };
}

let clientIdCounter = 0;
/** A process-unique id for an optimistic message bubble. */
function nextClientId(): string {
  clientIdCounter += 1;
  return `c${clientIdCounter}`;
}

function sendFileFor(c: Conversation, path: string, media: boolean) {
  return c.kind === "account"
    ? chat.sendFileToAccount(c.id, path, media)
    : chat.sendFileChannel(c.id, path, media);
}
function sendStickerFor(c: Conversation, stickerId: string, fallback: string) {
  return chat.sendSticker(c.id, stickerId, fallback, c.kind === "channel");
}

const HISTORY_LIMIT = 200;

function historyFor(c: Conversation, owner: string) {
  return c.kind === "account"
    ? chat.ownerHistory(owner, c.id, HISTORY_LIMIT)
    : chat.channelHistory(c.id, HISTORY_LIMIT);
}
function reactionsFor(c: Conversation) {
  return c.kind === "account"
    ? chat.accountReactions(c.id)
    : chat.channelReactions(c.id);
}

export async function reloadConversation(rawSet: Set, get: Get) {
  const lease = captureChatOwner(get);
  const set: Set = (partial) => {
    if (lease.current()) rawSet(partial);
  };
  if (!lease.current()) return;
  const c = get().active;
  if (!c) return;
  const key = convKey(c);
  const loadingRequest = get().loadingRequest + 1;
  const request = loadingRequest;
  set({
    loading: true,
    loadingRequest,
    historyRequests: boundedConversationMap(
      get().historyRequests,
      key,
      request,
    ),
  });
  try {
    const [items, reacts] = await Promise.all([
      historyFor(c, lease.owner!),
      reactionsFor(c),
    ]);
    if (
      !lease.current() ||
      get().historyRequests[key] !== request ||
      (!get().cacheOrder.includes(key) &&
        get().active &&
        convKey(get().active!) !== key)
    )
      return;
    set((s) => ({
      messages: {
        ...s.messages,
        [key]: reconcileMessages(
          items.map(fromHistoryItem),
          s.messages[key] ?? [],
          Object.values(s.intents).filter(
            (i) => convKey(i.conversation) === key,
          ),
          s.deleted[key] ?? [],
        ),
      },
      intents: Object.fromEntries(
        Object.entries(s.intents).filter(
          ([, intent]) =>
            convKey(intent.conversation) !== key ||
            !intent.message.id ||
            (!intent.acceptanceHistoryOnly &&
              !items.some((h) => h.id === intent.message.id)),
        ),
      ),
      reactions: { ...s.reactions, [key]: reacts },
      ...(s.loadingRequest === loadingRequest
        ? { loading: false, historyError: null }
        : {}),
    }));
    if (lease.current() && get().active && convKey(get().active!) === key)
      await get().refreshStatuses();
  } catch {
    if (
      get().loadingRequest === loadingRequest &&
      get().active &&
      convKey(get().active!) === key
    )
      set({ loading: false, historyError: key });
  }
}

export async function refreshDeliveryStatuses(
  rawSet: Set,
  get: Get,
  conversation?: Conversation,
) {
  const lease = captureChatOwner(get);
  const set: Set = (partial) => {
    if (lease.current()) rawSet(partial);
  };
  const c = conversation ?? get().active;
  if (!lease.current() || !c || c.kind !== "account" || get().statusBusy)
    return;
  const key = convKey(c);
  set({ statusBusy: true });
  try {
    // The shared serial ticker also recovers capacity-fallback accepted intents.
    // reload's status refresh sees statusBusy and cannot recursively poll.
    if (
      get().active &&
      convKey(get().active!) === key &&
      Object.values(get().intents).some(
        (i) =>
          convKey(i.conversation) === key &&
          i.acceptanceHistoryOnly &&
          i.message.id,
      )
    ) {
      await get().reload();
      if (!lease.current()) return;
    }
    const ids = [
      ...new Set(
        [
          ...(get().messages[key] ?? []),
          ...Object.values(get().intents)
            .filter((i) => convKey(i.conversation) === key)
            .map((i) => i.message),
        ]
          .filter((m) => m.fromMe && m.id)
          .map((m) => m.id!),
      ),
    ];
    const projection = new Map<string, "awaiting" | "delivered">();
    for (let i = 0; i < ids.length; i += 256) {
      if (!lease.current()) return;
      const rows = await chat.deliveryStatuses(
        lease.owner!,
        c.id,
        ids.slice(i, i + 256),
      );
      if (!lease.current()) return;
      for (const row of rows)
        if (ids.includes(row.id)) projection.set(row.id, row.status);
    }
    set((s) => ({
      messages: s.messages[key]
        ? {
            ...s.messages,
            [key]: s.messages[key].map((m) => applyProjection(m, projection)),
          }
        : s.messages,
      intents: Object.fromEntries(
        Object.entries(s.intents).map(([id, intent]) => [
          id,
          convKey(intent.conversation) === key
            ? {
                ...intent,
                message: applyProjection(intent.message, projection),
              }
            : intent,
        ]),
      ),
    }));
  } catch {
    /* A missing projection response is not a delivery claim or send failure. */
  } finally {
    set({ statusBusy: false });
  }
}

/** Bounded local intent storage is independent of the 50-conversation cache. */
export async function sendIntent(
  set: Set,
  get: Get,
  c: Conversation,
  payload: SendPayload,
) {
  const dispatch = beginSendIntent(set, get, c, payload);
  if (dispatch) await dispatch;
}

export function beginSendIntent(
  set: Set,
  get: Get,
  c: Conversation,
  payload: SendPayload,
): Promise<void> | null {
  if (Object.keys(get().intents).length >= SEND_INTENT_CAP) {
    set({
      error:
        "Too many pending messages. Resolve a failed send before sending more.",
    });
    return null;
  }
  const clientId = nextClientId();
  const message: ChatMessage = {
    id: null,
    clientId,
    fromMe: true,
    who: get().myId,
    text:
      payload.kind === "text"
        ? payload.text
        : payload.kind === "sticker"
          ? payload.fallback
          : "",
    replyTo: payload.kind === "text" ? payload.replyTo : null,
    wallClock: Date.now(),
    pending: true,
    sticker: payload.kind === "sticker" ? payload.stickerId : null,
    metadataPending: payload.kind === "file",
    file:
      payload.kind === "file"
        ? {
            name: payload.path.split(/[\\\\/]/).pop() || "File",
            size: 0,
            mime: "",
            fileConv: "",
            media: payload.media,
          }
        : null,
  };
  const intent = { conversation: { ...c }, payload, message };
  const key = convKey(c);
  set((s) => ({
    intents: { ...s.intents, [clientId]: intent },
    messages: { ...s.messages, [key]: [...(s.messages[key] ?? []), message] },
  }));
  return dispatchIntent(set, get, intent);
}

export async function dispatchIntent(set: Set, get: Get, intent: SendIntent) {
  const lease = captureChatOwner(get);
  const guarded: Set = (partial) => {
    if (lease.current()) set(partial);
  };
  const { conversation: c, payload, message } = intent;
  const key = convKey(c);
  const clientId = message.clientId!;
  const update = (next: ChatMessage | null) =>
    guarded((s) => {
      if (!s.intents[clientId]) return {};
      const intents = { ...s.intents };
      const currentIntent = s.intents[clientId];
      if (
        next?.id &&
        (s.deleted[key]?.includes(next.id) ||
          currentIntent.deletedIds?.includes(next.id))
      ) {
        delete intents[clientId];
        return {
          intents,
          messages: s.messages[key]
            ? {
                ...s.messages,
                [key]: s.messages[key].filter(
                  (m) => m.clientId !== clientId && m.id !== next.id,
                ),
              }
            : s.messages,
        };
      }
      const existing = next?.id
        ? s.messages[key]?.find((m) => m.id === next.id)
        : undefined;
      const resolved = next
        ? {
            ...next,
            delivery:
              existing?.delivery === "delivered"
                ? ("delivered" as const)
                : next.delivery,
            ...(next.file && existing?.file && !existing.metadataPending
              ? { file: existing.file, metadataPending: false }
              : {}),
          }
        : null;
      if (resolved) intents[clientId] = { ...currentIntent, message: resolved };
      else delete intents[clientId];
      // Completion must not repopulate an evicted conversation's cache.
      const messages = { ...s.messages };
      if (messages[key]) {
        const rows = messages[key].filter(
          (m) => m.clientId !== clientId && (!next?.id || m.id !== next.id),
        );
        messages[key] =
          resolved && !(resolved.id && currentIntent.acceptanceHistoryOnly)
            ? [...rows, resolved]
            : rows;
      }
      return { intents, messages };
    });
  try {
    let accepted: ChatMessage | null = null;
    if (c.kind === "account") {
      const owner = lease.owner!;
      if (payload.kind === "text")
        accepted = {
          ...message,
          id: await chat.enqueueText(
            owner,
            c.id,
            payload.text,
            payload.replyTo,
          ),
          pending: false,
        };
      else if (payload.kind === "sticker")
        accepted = {
          ...message,
          id: await chat.enqueueSticker(
            owner,
            c.id,
            payload.stickerId,
            payload.fallback,
          ),
          pending: false,
        };
      else {
        const result = await chat.enqueueFile(
          owner,
          c.id,
          payload.path,
          payload.media,
        );
        accepted = {
          ...message,
          id: result.id,
          pending: false,
          file: { ...message.file!, fileConv: result.fileConv },
        };
      }
      if (!lease.current()) return;
      if (!get().intents[clientId]) {
        if (accepted?.id)
          guarded((s) => ({
            deleted: boundedConversationMap(
              s.deleted,
              key,
              [...(s.deleted[key] ?? []), accepted!.id!].slice(-256),
            ),
          }));
        return;
      }
      update(accepted);
    } else {
      if (payload.kind === "text")
        await chat.sendChannelMessage(c.id, payload.text, payload.replyTo);
      else if (payload.kind === "sticker")
        await sendStickerFor(c, payload.stickerId, payload.fallback);
      else await sendFileFor(c, payload.path, payload.media);
      if (!lease.current()) return;
      update(null); // Legacy void never claims acceptance/receipt for a placeholder.
    }
    if (!lease.current()) return;
    if (get().active && convKey(get().active!) === key) await get().reload();
    else if (accepted?.id) await get().refreshStatuses(c);
  } catch (e) {
    if (!lease.current()) return;
    // Once a stable ID was accepted, hydration/projection failure is NOT a retryable send.
    if (get().intents[clientId]?.message.id) return;
    update({
      ...message,
      pending: false,
      failed: true,
      failReason: sendFailReason(e),
    });
    if (payload.kind === "file")
      guarded({ error: `Couldn't send file: ${errorMessage(e)}` });
  }
}
