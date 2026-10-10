// Sticker IDs are sent over the wire, so asset filenames must remain stable.
// Noto IDs encode an emoji; cat IDs have explicit emoji fallbacks for older builds.

export type StickerPackId = "noto" | "cats";

export interface Sticker {
  id: string;
  url: string;
  emoji: string;
  label: string;
  pack: StickerPackId;
}

export const STICKER_PACKS: StickerPackId[] = ["noto", "cats"];

/** "1f602" → 😂, "2764_fe0f" → ❤️. */
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

// Row-major order within each of the five source sheets.
const CAT_DETAILS: readonly (readonly (readonly [string, string])[])[] = [
  [
    ["Cool cat", "😎"],
    ["Confused tabby", "🤔"],
    ["Shocked cat", "😱"],
    ["Crying cat", "😭"],
    ["Side-eye tabby", "😒"],
    ["Deadpan cat", "🙄"],
    ["Silly cat", "😛"],
    ["Cat hug", "💕"],
    ["Rocket cat", "🚀"],
  ],
  [
    ["Late-night typing", "😵‍💫"],
    ["Too-small box", "📦"],
    ["Fish victory", "🐟"],
    ["Noodle argument", "😡"],
    ["Cookie heist", "🍪"],
    ["Sick day", "🤒"],
    ["Zoomies", "💨"],
    ["Video call", "💻"],
    ["Plant accident", "🙈"],
  ],
  [
    ["Mouse nap", "😴"],
    ["Box spy", "👀"],
    ["Couch flop", "😮‍💨"],
    ["Robot vacuum inspector", "🤖"],
    ["Cat tree melt", "🫠"],
    ["Feather insult", "😾"],
    ["Blanket burrito", "🥶"],
    ["Snack tug-of-war", "🍟"],
    ["Keyboard burnout", "😵"],
  ],
  [
    ["Laundry king", "👑"],
    ["Birdwatching", "🐦"],
    ["Water paw", "💧"],
    ["Empty bowl", "😑"],
    ["Bag escape", "🛍️"],
    ["Mirror crisis", "🪞"],
    ["Yoga cat", "🧘"],
    ["Plant guard", "🌿"],
    ["Interrupted grooming", "😳"],
  ],
  [
    ["Chair boss", "😎"],
    ["Curtain spy", "👀"],
    ["Book block", "📖"],
    ["Door protest", "🚪"],
    ["Pencil drop", "✏️"],
    ["Sock mystery", "🧦"],
    ["Alarm slap", "⏰"],
    ["Sink sovereign", "🛁"],
    ["Crooked crown", "👑"],
  ],
];

const noto = Object.entries(
  import.meta.glob("../assets/stickers/noto/*.webp", {
    eager: true,
    query: "?url",
    import: "default",
  }) as Record<string, string>,
)
  .sort(([a], [b]) => a.localeCompare(b))
  .map(([path, url]): Sticker => {
    const id = idFromPath(path);
    const emoji = emojiFromId(id);
    return { id, url, emoji, label: emoji, pack: "noto" };
  });

const cats = Object.entries(
  import.meta.glob("../assets/stickers/cats/*.webp", {
    eager: true,
    query: "?url",
    import: "default",
  }) as Record<string, string>,
)
  .sort(([a], [b]) => a.localeCompare(b))
  .map(([path, url]): Sticker => {
    const id = idFromPath(path);
    const match = /^cat-(0[1-5])-(0[1-9])$/.exec(id);
    if (!match) throw new Error(`Invalid cat sticker ID: ${id}`);
    const packNumber = Number(match[1]);
    const itemNumber = Number(match[2]);
    const detail = CAT_DETAILS[packNumber - 1]?.[itemNumber - 1];
    if (!detail) throw new Error(`Missing cat sticker metadata: ${id}`);
    const [label, emoji] = detail;
    return {
      id,
      url,
      emoji,
      label,
      pack: "cats",
    };
  });

export const STICKERS: Sticker[] = [...noto, ...cats];

const BY_ID = new Map(STICKERS.map((sticker) => [sticker.id, sticker]));

export function stickerById(id: string): Sticker | undefined {
  return BY_ID.get(id);
}
