// Built-in animated stickers: Google's Noto Animated Emoji (open-source), bundled as
// downscaled animated WebP. The manifest is built automatically from the bundled assets
// via `import.meta.glob` — each file is named by its emoji codepoint(s) joined with `_`
// (e.g. `1f602.webp`, `2764_fe0f.webp`), so the sticker id and its fallback emoji char
// are both derived from the filename. Drop a `.webp` in the folder and it appears.

export interface Sticker {
  /** Stable id sent over the wire — the codepoint string, e.g. "1f602" / "2764_fe0f". */
  id: string;
  /** Bundled animated-WebP URL (hashed by Vite). */
  url: string;
  /** The emoji char this sticker depicts — the fallback shown if a peer lacks the file. */
  emoji: string;
}

/** "1f602" → 😂, "2764_fe0f" → ❤️ (joins the codepoints; invalid parts are dropped). */
function emojiFromId(id: string): string {
  try {
    const cps = id.split("_").map((h) => parseInt(h, 16));
    if (cps.some((n) => Number.isNaN(n))) return "";
    return String.fromCodePoint(...cps);
  } catch {
    return "";
  }
}

function idFromPath(path: string): string {
  return (path.split("/").pop() ?? "").replace(/\.[^.]+$/, "");
}

/** All bundled animated stickers, sorted by id for a stable picker order. */
export const STICKERS: Sticker[] = Object.entries(
  import.meta.glob("../assets/stickers/noto/*.webp", {
    eager: true,
    query: "?url",
    import: "default",
  }) as Record<string, string>,
)
  .sort(([a], [b]) => a.localeCompare(b))
  .map(([path, url]) => {
    const id = idFromPath(path);
    return { id, url, emoji: emojiFromId(id) };
  });

const BY_ID = new Map(STICKERS.map((s) => [s.id, s]));

/** Look up a bundled sticker by id (undefined if this build doesn't bundle it). */
export function stickerById(id: string): Sticker | undefined {
  return BY_ID.get(id);
}
