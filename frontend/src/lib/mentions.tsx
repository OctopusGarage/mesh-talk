import { Fragment } from "react";

export const EMOJIS = ["👍", "❤️", "😂", "🎉", "🙏", "🔥", "😮", "😢"];

// Mentions or http(s) URLs, as a single capturing alternation so split() preserves order.
const TOKEN_RE = /(@[\p{L}\p{N}_-]+|https?:\/\/[^\s]+)/u;

export type Segment = { text: string; kind: "plain" | "mention" | "link" };

/** Split `text` into ordered plain / @mention / URL segments (pure — unit-testable). */
export function messageSegments(text: string): Segment[] {
  return text
    .split(TOKEN_RE)
    .filter((s) => s.length > 0)
    .map((s) => ({
      text: s,
      kind: s.startsWith("@")
        ? "mention"
        : /^https?:\/\//.test(s)
          ? "link"
          : "plain",
    }));
}

function openExternal(url: string) {
  // Open in the OS default browser, never the app webview. Lazy import so this module stays
  // importable in the node unit-test environment (no Tauri runtime there).
  import("@tauri-apps/plugin-shell").then((m) => m.open(url)).catch(() => {});
}

/** Render `text` with @mentions highlighted and URLs as external links. */
export function renderWithMentions(text: string) {
  return messageSegments(text).map((seg, i) => {
    if (seg.kind === "mention")
      return (
        <span key={i} className="font-bold text-foreground">
          {seg.text}
        </span>
      );
    if (seg.kind === "link")
      return (
        <a
          key={i}
          href={seg.text}
          onClick={(e) => {
            e.preventDefault();
            openExternal(seg.text);
          }}
          className="break-all text-primary underline underline-offset-2 hover:opacity-80"
        >
          {seg.text}
        </a>
      );
    return <Fragment key={i}>{seg.text}</Fragment>;
  });
}

/** Does this text @-mention the given display name? */
export function mentionsName(text: string, name: string): boolean {
  if (!name) return false;
  const re = new RegExp(
    `@${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`,
    "i",
  );
  return re.test(text);
}
