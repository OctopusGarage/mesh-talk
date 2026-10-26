import type { Sticker } from "./stickerPacks";

const PREFIX = "mt-sticker-v1-";
export const STICKER_MEDIA_CAP = 3 * 1024 * 1024;
const MIME = {
  png: "image/png",
  jpg: "image/jpeg",
  webp: "image/webp",
  gif: "image/gif",
} as const;
type Extension = keyof typeof MIME;
type StickerIdentity = { id: string; fallback: string };

function validIdentity(id: string, fallback: string) {
  return (
    /^[a-z0-9_:.-]{1,126}$/.test(id) &&
    [...fallback].length >= 1 &&
    [...fallback].length <= 8 &&
    new TextEncoder().encode(fallback).length <= 32 &&
    !/[\p{Cc}\p{Cs}]/u.test(fallback)
  );
}
function base64(bytes: Uint8Array) {
  // Bounded chunks avoid spreading a multi-megabyte image onto the JS stack.
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 8192)
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
  return btoa(binary);
}
export function encodeStickerMediaName(
  id: string,
  fallback: string,
  ext: Extension,
) {
  if (!validIdentity(id, fallback))
    throw new Error("Invalid sticker identity or fallback");
  const encoded = base64(new TextEncoder().encode(`${id}\n${fallback}`))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
  return `${PREFIX}${encoded}.${ext}`;
}
export function decodeStickerMedia(file: {
  name: string;
  media: boolean;
  size: number;
  mime?: string;
}): StickerIdentity | null {
  if (
    !file.media ||
    !Number.isSafeInteger(file.size) ||
    file.size < 1 ||
    file.size > STICKER_MEDIA_CAP ||
    file.name.length > 233
  )
    return null;
  const match = /^mt-sticker-v1-([A-Za-z0-9_-]+)\.(png|jpg|webp|gif)$/.exec(
    file.name,
  );
  if (!match || file.mime !== MIME[match[2] as Extension]) return null;
  try {
    const encoded = match[1].replace(/-/g, "+").replace(/_/g, "/");
    const bytes = Uint8Array.from(
      atob(encoded + "=".repeat((4 - (encoded.length % 4)) % 4)),
      (char) => char.charCodeAt(0),
    );
    const identity = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    const [id, fallback, extra] = identity.split("\n");
    if (
      extra !== undefined ||
      fallback === undefined ||
      !validIdentity(id, fallback)
    )
      return null;
    if (
      encodeStickerMediaName(id, fallback, match[2] as Extension) !== file.name
    )
      return null;
    return { id, fallback };
  } catch {
    return null;
  }
}
/** Copy the selected artwork synchronously before pack/account state can change. */
export function snapshotSticker(sticker: Sticker, fallback: string) {
  const match =
    /^data:(image\/(?:png|jpeg|webp|gif));base64,([A-Za-z0-9+/]+={0,2})$/.exec(
      sticker.url,
    );
  if (!match || match[2].length > 4 * Math.ceil(STICKER_MEDIA_CAP / 3))
    throw new Error("Sticker artwork is unavailable or too large");
  const mime = match[1];
  const ext = (Object.keys(MIME) as Extension[]).find(
    (key) => MIME[key] === mime,
  )!;
  const binary = atob(match[2]);
  const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
  if (
    !bytes.length ||
    bytes.length > STICKER_MEDIA_CAP ||
    base64(bytes) !== match[2]
  )
    throw new Error("Invalid sticker artwork");
  return {
    bytes,
    mime,
    ext,
    name: encodeStickerMediaName(sticker.id, fallback, ext),
  };
}
