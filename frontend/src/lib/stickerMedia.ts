/// <reference lib="es2022.intl" />
import type { Sticker } from "./stickerPacks";

const PREFIX = "mt-sticker-v1-";
const LABEL_PREFIX = "mt-sticker-v2-";
const MAX_NAME = 255;
const UNSAFE_LABEL =
  /[\p{Cc}\p{Cs}\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069\ufeff]/u;
export const STICKER_MEDIA_CAP = 3 * 1024 * 1024;
const MIME = {
  png: "image/png",
  jpg: "image/jpeg",
  webp: "image/webp",
  gif: "image/gif",
} as const;
type Extension = keyof typeof MIME;
export type StickerIdentity = { id: string; fallback: string; label?: string };

function validLabel(label: string) {
  return (
    !!label &&
    label === label.trim() &&
    [...label].length <= 80 &&
    !UNSAFE_LABEL.test(label)
  );
}

/** Creator text is display data, never markup or a transport filename. */
export function cleanStickerLabel(label: string | undefined): string {
  return (label ?? "")
    .replace(/\s+/gu, " ")
    .replace(new RegExp(UNSAFE_LABEL.source, "gu"), "")
    .trim();
}

export function stickerSummary(
  sticker: Pick<StickerIdentity, "label">,
  generic = "[Sticker]",
) {
  return sticker.label ? `[${sticker.label}]` : generic;
}

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
  label?: string,
) {
  if (!validIdentity(id, fallback))
    throw new Error("Invalid sticker identity or fallback");
  if (label !== undefined && !validLabel(label))
    throw new Error("Invalid sticker label");
  const encoded = base64(
    new TextEncoder().encode(
      `${id}\n${fallback}${label === undefined ? "" : `\n${label}`}`,
    ),
  )
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
  const name = `${label === undefined ? PREFIX : LABEL_PREFIX}${encoded}.${ext}`;
  if (name.length > (label === undefined ? 233 : MAX_NAME))
    throw new Error("Sticker metadata exceeds the filename budget");
  return name;
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
    file.name.length > MAX_NAME
  )
    return null;
  const match =
    /^mt-sticker-v([12])-([A-Za-z0-9_-]+)\.(png|jpg|webp|gif)$/.exec(file.name);
  if (!match || file.mime !== MIME[match[3] as Extension]) return null;
  try {
    const encoded = match[2].replace(/-/g, "+").replace(/_/g, "/");
    const bytes = Uint8Array.from(
      atob(encoded + "=".repeat((4 - (encoded.length % 4)) % 4)),
      (char) => char.charCodeAt(0),
    );
    const identity = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
    const fields = identity.split("\n");
    const [id, fallback, label] = fields;
    if (
      fields.length !== (match[1] === "1" ? 2 : 3) ||
      fallback === undefined ||
      !validIdentity(id, fallback) ||
      (match[1] === "2" && !validLabel(label))
    )
      return null;
    if (
      encodeStickerMediaName(id, fallback, match[3] as Extension, label) !==
      file.name
    )
      return null;
    return label === undefined ? { id, fallback } : { id, fallback, label };
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
  // Fit metadata to a portable filesystem component, at whole grapheme boundaries.
  // A label is immutable presentation; removing/updating the source pack cannot change it.
  const clean = cleanStickerLabel(sticker.label);
  let label = "";
  for (const { segment } of new Intl.Segmenter(undefined, {
    granularity: "grapheme",
  }).segment(clean)) {
    const candidate = label + segment;
    if ([...candidate].length > 80) break;
    try {
      encodeStickerMediaName(sticker.id, fallback, ext, candidate.trimEnd());
      label = candidate;
    } catch {
      break;
    }
  }
  return {
    bytes,
    mime,
    ext,
    name: encodeStickerMediaName(
      sticker.id,
      fallback,
      ext,
      label.trim() || undefined,
    ),
  };
}
