// Pure classification + filtering helpers for the per-conversation history panel
// (ConversationHistoryDialog). Kept framework-free so the categorization logic is
// unit-testable in the node test environment, mirroring lib/mentions.
import type { ChatMessage } from "@/store/chat";
import { messageSegments } from "@/lib/mentions";

// Image/video extension sniffing — mirrors mediaFile.tsx's IMAGE_EXT/VIDEO_EXT. Duplicated
// here (rather than imported) so this module stays React-free and unit-testable under the
// node vitest environment (mediaFile.tsx pulls in JSX + lucide-react).
const IMAGE_EXT = /\.(png|jpe?g|gif|webp|bmp|avif|svg)$/i;
const VIDEO_EXT = /\.(mp4|mov|webm|m4v|ogv)$/i;
const isMedia = (name: string) => IMAGE_EXT.test(name) || VIDEO_EXT.test(name);

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
    return isMedia(m.file.name) ? "media" : "files";
  }
  return hasLink(m.text) ? "links" : "other";
}

/** Case-insensitive substring match of `term` against a message's text (or file name). */
export function matchesQuery(m: ChatMessage, term: string): boolean {
  const q = term.trim().toLowerCase();
  if (!q) return true;
  if (m.text.toLowerCase().includes(q)) return true;
  return m.file ? m.file.name.toLowerCase().includes(q) : false;
}
