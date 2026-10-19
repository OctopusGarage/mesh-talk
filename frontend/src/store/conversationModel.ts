import type { SendFailReason } from "@/lib/error";

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
