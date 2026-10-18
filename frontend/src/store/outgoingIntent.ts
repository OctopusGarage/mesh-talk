import { chat } from "@/lib/api";
import { errorMessage, sendFailReason } from "@/lib/error";
import { captureChatOwner } from "./ownership";
import { useTransfers } from "./transfers";
import {
  boundedConversationMap,
  convKey,
  fromHistoryItem,
  reconcileMessages,
  applyProjection,
  completeSendIntentState,
} from "./conversationState";
export {
  reconcileMessages,
  applyProjection,
  fromHistoryItem,
} from "./conversationState";
import type { ChatMessage, Conversation, Set, Get } from "./chat";

export const SEND_INTENT_CAP = 256;
export type SendPayload =
  | {
      kind: "text";
      text: string;
      replyTo: string | null;
      expiresAtMs?: number | null;
    }
  | {
      kind: "sticker";
      stickerId: string;
      fallback: string;
      expiresAtMs?: number | null;
    }
  | { kind: "file"; path: string; media: boolean; expiresAtMs?: number | null };
export interface SendIntent {
  conversation: Conversation;
  payload: SendPayload;
  message: ChatMessage;
  /** Exact deletion protection owned by the bounded intent, never by an LRU scope. */
  deletedIds?: string[];
  /** Capacity fallback: accepted messages need canonical history, never an overlay. */
  acceptanceHistoryOnly?: boolean;
}

let clientIdCounter = 0;
/** A process-unique id for an optimistic message bubble. */
function nextClientId(): string {
  clientIdCounter += 1;
  return `c${clientIdCounter}`;
}

function sendFileFor(
  c: Conversation,
  path: string,
  media: boolean,
  progressKey: string,
  expiresAtMs?: number | null,
) {
  return c.kind === "account"
    ? chat.sendFileToAccount(c.id, path, media)
    : chat.sendFileChannel(c.id, path, media, progressKey, expiresAtMs);
}
function sendStickerFor(
  c: Conversation,
  stickerId: string,
  fallback: string,
  expiresAtMs?: number | null,
) {
  return chat.sendSticker(
    c.id,
    stickerId,
    fallback,
    c.kind === "channel",
    expiresAtMs,
  );
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
  if (!lease.current()) return false;
  const c = get().active;
  if (!c) return false;
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
      return false;
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
    return lease.current();
  } catch {
    if (
      get().loadingRequest === loadingRequest &&
      get().active &&
      convKey(get().active!) === key
    )
      set({ loading: false, historyError: key });
    return false;
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
  if (!lease.current() || !c || get().statusBusy) return;
  const key = convKey(c);
  set({ statusBusy: true });
  try {
    if (c.kind === "channel") {
      const ids = [
        ...new Set(
          (get().messages[key] ?? [])
            .filter((m) => m.fromMe && m.id && !m.file)
            .map((m) => m.id!),
        ),
      ];
      const replicas = new Map<string, number>();
      for (let i = 0; i < ids.length; i += 256) {
        const rows = await chat.channelCustodyStatuses(
          c.id,
          ids.slice(i, i + 256),
        );
        if (!lease.current()) return;
        for (const row of rows) replicas.set(row.id, row.replicas);
      }
      set((s) => ({
        messages: s.messages[key]
          ? {
              ...s.messages,
              [key]: s.messages[key].map((m) =>
                m.id && replicas.has(m.id)
                  ? { ...m, replicas: replicas.get(m.id) }
                  : m,
              ),
            }
          : s.messages,
      }));
      return;
    }
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
    const fileIds = new Set(
      [
        ...(get().messages[key] ?? []),
        ...Object.values(get().intents)
          .filter((i) => convKey(i.conversation) === key)
          .map((i) => i.message),
      ]
        .filter((m) => m.fromMe && m.id && m.file)
        .map((m) => m.id!),
    );
    const projection = new Map<string, "awaiting" | "delivered">();
    for (let i = 0; i < ids.length; i += 256) {
      if (!lease.current()) return;
      const rows = await chat.deliveryStatuses(
        lease.owner!,
        c.id,
        ids.slice(i, i + 256),
      );
      if (!lease.current()) return;
      for (const row of rows) {
        if (!ids.includes(row.id)) continue;
        projection.set(row.id, row.status);
        if (fileIds.has(row.id)) {
          if (row.file_progress)
            useTransfers
              .getState()
              .applyUploadStatus(row.id, row.file_progress);
          else if (row.status === "delivered")
            // Older status providers cannot report chunk custody. End the
            // placeholder without ever presenting an unverified 100%.
            useTransfers.getState().applyUploadStatus(row.id, {
              done: 0,
              total: 0,
              delivered: true,
            });
        }
      }
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
  options: { waitForHydration?: boolean } = {},
): Promise<boolean> | null {
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
  return dispatchIntent(set, get, intent, options);
}

export async function dispatchIntent(
  set: Set,
  get: Get,
  intent: SendIntent,
  { waitForHydration = true }: { waitForHydration?: boolean } = {},
): Promise<boolean> {
  const lease = captureChatOwner(get);
  const guarded: Set = (partial) => {
    if (lease.current()) set(partial);
  };
  const { conversation: c, payload, message } = intent;
  const key = convKey(c);
  let admitted = false;
  const clientId = message.clientId!;
  const update = (next: ChatMessage | null) =>
    guarded((state) => completeSendIntentState(state, c, clientId, next));
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
            payload.expiresAtMs,
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
            payload.expiresAtMs,
          ),
          pending: false,
        };
      else {
        const result = await chat.enqueueFile(
          owner,
          c.id,
          payload.path,
          payload.media,
          clientId,
          payload.expiresAtMs,
        );
        accepted = {
          ...message,
          id: result.id,
          pending: false,
          file: { ...message.file!, fileConv: result.fileConv },
        };
      }
      admitted = true;
      if (!lease.current()) return false;
      const hadIntent = !!get().intents[clientId];
      update(accepted);
      if (!hadIntent) return true;
    } else {
      if (payload.kind === "text")
        await chat.sendChannelMessage(
          c.id,
          payload.text,
          payload.replyTo,
          payload.expiresAtMs,
        );
      else if (payload.kind === "sticker")
        await sendStickerFor(
          c,
          payload.stickerId,
          payload.fallback,
          payload.expiresAtMs,
        );
      else
        await sendFileFor(
          c,
          payload.path,
          payload.media,
          clientId,
          payload.expiresAtMs,
        );
      admitted = true;
      if (!lease.current()) return false;
      update(null); // Legacy void never claims acceptance/receipt for a placeholder.
    }
    if (!lease.current()) return false;
    if (accepted?.id) {
      // The durable ID settles admission. Optional UI hydration must not hold the draft.
      const hydration =
        get().active && convKey(get().active!) === key
          ? get().reload()
          : get().refreshStatuses(c);
      if (waitForHydration) await hydration;
      else
        void hydration.catch(() => {
          /* Durable acceptance is not a retryable send failure. */
        });
      return lease.current();
    }
    if (get().active && convKey(get().active!) === key) await get().reload();
    return lease.current();
  } catch (e) {
    if (!lease.current()) return false;
    // Durable acceptance survives hydration failure, even if history already removed the intent.
    if (admitted) return true;
    if (payload.kind === "file") useTransfers.getState().clear(clientId);
    update({
      ...message,
      pending: false,
      failed: true,
      failReason: sendFailReason(e),
    });
    if (payload.kind === "file")
      guarded({
        error: /file manifest exceeds transport frame/i.test(errorMessage(e))
          ? "This attachment has too much metadata to send. Try a shorter name or a smaller folder."
          : `Couldn't send file: ${errorMessage(e)}`,
      });
    return false;
  }
}
