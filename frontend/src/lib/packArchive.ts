import { strToU8, zipSync } from "fflate";
import { MAX_PACK_ZIP_BYTES, parsePack, type CustomizationPack } from "./pack";

/** Reconstitute legacy decoded libraries without accessing external URLs. */
export function archivePack(pack: CustomizationPack): Uint8Array {
  if (
    pack.id.length > 80 ||
    pack.name.length > 80 ||
    pack.version.length > 128 ||
    (pack.kind === "avatar" && pack.avatars.length > 100) ||
    (pack.kind === "sticker" && pack.stickers.length > 100) ||
    (pack.kind === "theme" &&
      ((pack.wallpapers?.length ?? 0) > 60 ||
        Object.keys(pack.colors).length > 64))
  )
    throw new Error("Saved pack content exceeds the safety limit");
  const files: Record<string, Uint8Array> = {};
  const references = new Map<string, string>();
  let total = 0;
  const image = (url: string): string => {
    const previous = references.get(url);
    if (previous) return previous;
    if (typeof url !== "string" || url.length > 4 * 1024 * 1024 + 100)
      throw new Error("Invalid or oversized saved pack image");
    const match =
      /^data:image\/(png|jpeg|webp|gif);base64,([A-Za-z0-9+/]*={0,2})$/.exec(
        url,
      );
    if (
      !match ||
      match[2].length > 4 * 1024 * 1024 ||
      match[2].length % 4 !== 0
    )
      throw new Error("Invalid or oversized saved pack image");
    let binary: string;
    try {
      binary = atob(match[2]);
    } catch {
      throw new Error("Invalid saved pack image");
    }
    total += binary.length;
    if (
      !binary.length ||
      binary.length > 3 * 1024 * 1024 ||
      total > 24 * 1024 * 1024
    )
      throw new Error("Saved pack images exceed the safety limit");
    const name = `images/asset-${references.size}.${match[1] === "jpeg" ? "jpg" : match[1]}`;
    files[name] = Uint8Array.from(binary, (character) =>
      character.charCodeAt(0),
    );
    references.set(url, name);
    return name;
  };
  const base = {
    format: pack.format,
    id: pack.id,
    version: pack.version,
    name: pack.name,
    kind: pack.kind,
  };
  const manifest =
    pack.kind === "avatar"
      ? {
          ...base,
          category: pack.category,
          fit: pack.fit,
          avatars: pack.avatars.map(({ label, url }) => ({
            label,
            file: image(url),
          })),
        }
      : pack.kind === "sticker"
        ? {
            ...base,
            stickers: pack.stickers.map(({ id, label, fallback, url }) => ({
              id,
              label,
              fallback,
              file: image(url),
            })),
          }
        : {
            ...base,
            base: pack.base,
            colors: pack.colors,
            ...(pack.wallpaper ? { wallpaper: image(pack.wallpaper) } : {}),
            ...(pack.crest ? { crest: image(pack.crest) } : {}),
            ...(pack.wallpapers
              ? {
                  wallpapers: pack.wallpapers.map(({ id, title, url }) => ({
                    id,
                    title,
                    file: image(url),
                  })),
                }
              : {}),
          };
  files["manifest.json"] = strToU8(JSON.stringify(manifest));
  if (files["manifest.json"].length > 64 * 1024)
    throw new Error("Saved pack manifest exceeds the safety limit");
  const bytes = zipSync(files);
  if (bytes.length > MAX_PACK_ZIP_BYTES)
    throw new Error("Pack ZIP is too large");
  parsePack(bytes);
  return bytes;
}
