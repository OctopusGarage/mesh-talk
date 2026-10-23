import type { SendFailReason } from "@/lib/error";

export type ConvKind = "account" | "channel";

export interface Conversation {
  kind: ConvKind;
  id: string; // Kind-dependent account/channel ID; not a raw protocol ConversationId.
  name: string;
}

export type SearchTarget =
  | {
      reference: import("@/lib/api/chat").SearchReference;
      /** Read-only context presentation, separate from canonical history and unread evidence. */
      context: ChatMessage[];
    }
  | {
      wallClock: number;
      text: string;
      fromMe: boolean;
    };

export interface ChatMessage {
  protection?: import("@/lib/types").MessageProtection | null;
  /** Authenticated absolute send-time deadline; null/absent means keep. */
  expiresAtMs?: number | null;
  receivedAt?: number;
  arrivalKind?: import("@/lib/types").ArrivalKind;
  delivery?: "awaiting" | "delivered";
  /** Local runtime fact sampled at a monotonic response boundary; never persisted. */
  deliveryObservation?: {
    kind: "attempting" | "retry_scheduled";
    ageMs: number;
    sampledAt: number;
    reason?: "post_office_disabled" | "no_known_route" | "attempt_timed_out";
  };
  replicas?: number;
  postOfficeCopies?: number;
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
      protection?: import("@/lib/types").MessageProtection | null;
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
  | {
      kind: "file";
      path: string;
      media: boolean;
      expiresAtMs?: number | null;
      protection?: import("@/lib/types").MessageProtection | null;
    };
export interface SendIntent {
  conversation: Conversation;
  payload: SendPayload;
  message: ChatMessage;
  /** Exact deletion protection owned by the bounded intent, never by an LRU scope. */
  deletedIds?: string[];
  /** Capacity fallback: accepted messages need canonical history, never an overlay. */
  acceptanceHistoryOnly?: boolean;
}
