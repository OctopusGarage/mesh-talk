import type { ChatMessage, Conversation } from "./chat";

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
