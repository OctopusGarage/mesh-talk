import type { ReactionInfo } from "@/lib/types";
import type {
  ChatMessage,
  ChatState,
  Conversation,
  SearchTarget,
} from "./chat";
import type { SendIntent } from "./outgoingIntent";
import type { HistoryItem } from "@/lib/types";

type ConversationSnapshot = Pick<
  ChatState,
  | "cacheOrder"
  | "messages"
  | "reactions"
  | "historyRequests"
  | "deleted"
  | "unread"
  | "intents"
>;

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

/** Keep the active Conversation while bounding message and reaction caches. */
function evictCaches(
  messages: Record<string, ChatMessage[]>,
  reactions: Record<string, ReactionInfo[]>,
  order: string[],
  keep: string,
) {
  if (order.length <= CONV_CACHE_LIMIT) return { messages, reactions, order };
  const excess = order.length - CONV_CACHE_LIMIT;
  const evicted = new Set<string>();
  for (let i = 0; i < order.length && evicted.size < excess; i++) {
    if (order[i] !== keep) evicted.add(order[i]);
  }
  const nextMessages: Record<string, ChatMessage[]> = {};
  for (const key of Object.keys(messages))
    if (!evicted.has(key)) nextMessages[key] = messages[key];
  const nextReactions: Record<string, ReactionInfo[]> = {};
  for (const key of Object.keys(reactions))
    if (!evicted.has(key)) nextReactions[key] = reactions[key];
  return {
    messages: nextMessages,
    reactions: nextReactions,
    order: order.filter((key) => !evicted.has(key)),
  };
}

export function openConversationState(
  state: ConversationSnapshot,
  conversation: Conversation,
  target: SearchTarget | undefined,
  request: number,
): Partial<ChatState> {
  const key = convKey(conversation);
  const order = [...state.cacheOrder.filter((item) => item !== key), key];
  const trimmed = evictCaches(state.messages, state.reactions, order, key);
  const retained = new Set(trimmed.order);
  return {
    active: conversation,
    searchTarget: target ? { ...target, key, request } : null,
    historyError: null,
    activeRequest: request,
    unread: { ...state.unread, [key]: 0 },
    members: [],
    channelOwner: "",
    messages: trimmed.messages,
    reactions: trimmed.reactions,
    cacheOrder: trimmed.order,
    historyRequests: Object.fromEntries(
      Object.entries(state.historyRequests).filter(([item]) =>
        retained.has(item),
      ),
    ),
    deleted: Object.fromEntries(
      Object.entries(state.deleted).filter(([item]) => retained.has(item)),
    ),
  };
}

export function deleteConversationMessage(
  state: ConversationSnapshot,
  key: string,
  target: string,
): Partial<ChatState> {
  return {
    deleted: boundedConversationMap(
      state.deleted,
      key,
      [...(state.deleted[key] ?? []), target].slice(-256),
    ),
    historyRequests: boundedConversationMap(
      state.historyRequests,
      key,
      (state.historyRequests[key] ?? 0) + 1,
    ),
    intents: Object.fromEntries(
      Object.entries(state.intents)
        .filter(
          ([, intent]) =>
            convKey(intent.conversation) !== key ||
            intent.message.id !== target,
        )
        .map(([id, intent]) => [
          id,
          convKey(intent.conversation) === key
            ? protectIntentDeletion(intent, target)
            : intent,
        ]),
    ),
    messages: state.messages[key]
      ? {
          ...state.messages,
          [key]: state.messages[key].filter((message) => message.id !== target),
        }
      : state.messages,
  };
}

export function clearConversationMessages(
  state: ConversationSnapshot,
  key: string,
): Partial<ChatState> {
  return {
    deleted: boundedConversationMap(
      state.deleted,
      key,
      [
        ...new Set([
          ...(state.deleted[key] ?? []),
          ...(state.messages[key] ?? []).flatMap((message) =>
            message.id ? [message.id] : [],
          ),
        ]),
      ].slice(-256),
    ),
    historyRequests: boundedConversationMap(
      state.historyRequests,
      key,
      (state.historyRequests[key] ?? 0) + 1,
    ),
    intents: Object.fromEntries(
      Object.entries(state.intents).filter(
        ([, intent]) => convKey(intent.conversation) !== key,
      ),
    ),
    messages: state.messages[key]
      ? { ...state.messages, [key]: [] }
      : state.messages,
  };
}

export function invalidateConversationIdentity(): Partial<ChatState> {
  return {
    ready: false,
    messages: {},
    searchTarget: null,
    reactions: {},
    intents: {},
    deleted: {},
    historyRequests: {},
    cacheOrder: [],
    members: [],
    channelOwner: "",
    statusBusy: false,
    loading: false,
    historyError: null,
  };
}
