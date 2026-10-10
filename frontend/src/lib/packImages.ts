import type { CustomizationPack } from "@/lib/pack";

const MAX_SIDE = 4096;
const MAX_PIXELS = 16_000_000;

export async function verifyPackImages(pack: CustomizationPack): Promise<void> {
  const urls =
    pack.kind === "avatar"
      ? pack.avatars.map((item) => item.url)
      : pack.kind === "sticker"
        ? pack.stickers.map((item) => item.url)
        : [
            pack.wallpaper,
            pack.crest,
            ...(pack.wallpapers ?? []).map((item) => item.url),
          ].filter((url): url is string => !!url);
  for (const url of new Set(urls)) {
    const image = new Image();
    image.src = url;
    try {
      await image.decode();
      if (
        !image.naturalWidth ||
        !image.naturalHeight ||
        image.naturalWidth > MAX_SIDE ||
        image.naturalHeight > MAX_SIDE ||
        image.naturalWidth * image.naturalHeight > MAX_PIXELS
      ) {
        throw new Error("Pack image dimensions exceed the safety limit");
      }
    } catch (error) {
      if (error instanceof Error && error.message.includes("safety limit"))
        throw error;
      throw new Error("A pack image cannot be decoded");
    } finally {
      image.src = "";
    }
  }
}
