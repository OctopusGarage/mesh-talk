// Pure classification + filtering helpers for the per-conversation history panel
// (ConversationHistoryDialog). Kept framework-free so the categorization logic is
// unit-testable in the node test environment, mirroring lib/mentions.
import type { ChatMessage } from "@/store/chat";
import { messageSegments } from "@/lib/mentions";
import { isImage, isVideo } from "@/lib/mediaTypes";
import { messageSticker } from "./messagePresentation";
import type { CustomizationPack } from "./pack";

export type HistoryCategory = "media" | "files" | "links" | "other";

/** True when `text` contains at least one http(s) URL (reuses the shared URL detection). */
export function hasLink(text: string): boolean {
  return messageSegments(text).some((s) => s.kind === "link");
}

/**
 * Which history tab a message belongs to (besides "All", which holds everything):
 * - "media": an image/video file message
 * - "files": any other file message
 * - "links": a text message containing a URL
 * - "other": plain text with no link and no file
 * A file message is never also classified as a link (file bubbles carry no link text).
 */
export function categorize(m: ChatMessage): HistoryCategory {
  if (m.file) {
    return isImage(m.file.name) || isVideo(m.file.name) ? "media" : "files";
  }
  return hasLink(m.text) ? "links" : "other";
}

/** Case-insensitive substring match of `term` against a message's text (or file name). */
export function matchesQuery(
  m: ChatMessage,
  term: string,
  packs: CustomizationPack[] = [],
): boolean {
  const q = term.trim().toLowerCase();
  if (!q) return true;
  if (m.protection || m.recalled) return false;
  const sticker = messageSticker(m, packs);
  if (sticker)
    return `${sticker.label ?? ""} ${sticker.fallback}`
      .toLowerCase()
      .includes(q);
  return (
    m.text.toLowerCase().includes(q) || !!m.file?.name.toLowerCase().includes(q)
  );
}
