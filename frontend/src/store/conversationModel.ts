import type { SendFailReason } from "@/lib/error";

export type ConvKind = "account" | "channel";

export interface Conversation {
  kind: ConvKind;
  id: string; // Kind-dependent account/channel ID; not a raw protocol ConversationId.
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
