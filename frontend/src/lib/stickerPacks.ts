import type { CustomizationPack } from "@/lib/pack";

// Earlier app versions sent unprefixed Noto IDs. Keep those messages readable
// after moving the artwork into the removable Google Noto pack.

export interface Sticker {
  /** Stable id sent over the wire — the codepoint string, e.g. "1f602" / "2764_fe0f". */
  id: string;
  /** Artwork URL; empty when the optional artwork pack is not installed. */
  url: string;
  /** The emoji char this sticker depicts — the fallback shown if a peer lacks the file. */
  emoji: string;
  label?: string;
}

const LEGACY_NOTO_IDS = new Set([
  "1f389",
  "1f44b",
  "1f44c",
  "1f44d",
  "1f44e",
  "1f44f",
  "1f496",
  "1f4a9",
  "1f4aa",
  "1f4af",
  "1f525",
  "1f602",
  "1f608",
  "1f609",
  "1f60a",
  "1f60b",
  "1f60d",
  "1f60e",
  "1f618",
  "1f621",
  "1f622",
  "1f62d",
  "1f631",
  "1f634",
  "1f644",
  "1f648",
  "1f64f",
  "1f680",
  "1f914",
  "1f917",
  "1f923",
  "1f92a",
  "1f92f",
  "1f970",
  "1f971",
  "1f973",
  "1f97a",
  "270c_fe0f",
  "2764_fe0f",
  "2b50",
]);

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

/** Resolve installed artwork; removed packs show the sent fallback. */
export function stickerById(
  id: string,
  packs: CustomizationPack[] = [],
): Sticker | undefined {
  // Messages sent by the earlier bundled cat collection used unprefixed IDs.
  const lookupId = id.startsWith("cat-") ? `pack:cats:${id}` : id;
  if (!lookupId.startsWith("pack:")) {
    if (!LEGACY_NOTO_IDS.has(lookupId)) return undefined;
    const pack =
      packs.find(
        (item) => item.kind === "sticker" && item.id === "google-noto",
      ) ??
      packs.find(
        (item) => item.kind === "sticker" && item.id === "noto-favorites",
      );
    const sticker =
      pack?.kind === "sticker"
        ? pack.stickers.find((item) => item.id === lookupId)
        : undefined;
    return (
      sticker && {
        id,
        url: sticker.url,
        emoji: sticker.fallback,
        label: sticker.label,
      }
    );
  }
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
