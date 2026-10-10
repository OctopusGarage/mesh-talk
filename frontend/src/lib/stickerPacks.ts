import type { CustomizationPack } from "@/lib/pack";

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
  label?: string;
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

export function installedStickers(packs: CustomizationPack[]): Sticker[] {
  return packs.flatMap((pack) =>
    pack.kind === "sticker"
      ? pack.stickers.map((item) => ({
          id: `pack:${pack.id}:${item.id}`,
          url: item.url,
          emoji: item.fallback,
          label: item.label,
        }))
      : [],
  );
}

/** Resolve a built-in or installed sticker; unknown packs show the sent fallback. */
export function stickerById(
  id: string,
  packs: CustomizationPack[] = [],
): Sticker | undefined {
  // Messages sent by the earlier bundled cat collection used unprefixed IDs.
  const lookupId = id.startsWith("cat-") ? `pack:cats:${id}` : id;
  if (!lookupId.startsWith("pack:")) return BY_ID.get(lookupId);
  const separator = lookupId.lastIndexOf(":");
  if (separator <= 5) return undefined;
  const pack = packs.find(
    (item) =>
      item.kind === "sticker" && item.id === lookupId.slice(5, separator),
  );
  if (pack?.kind !== "sticker") return undefined;
  const sticker = pack.stickers.find(
    (item) => item.id === lookupId.slice(separator + 1),
  );
  return (
    sticker && {
      id,
      url: sticker.url,
      emoji: sticker.fallback,
      label: sticker.label,
    }
  );
}
