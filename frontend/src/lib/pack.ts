import { unzipSync, strFromU8 } from "fflate";

export interface PackBase {
  format: 1;
  id: string;
  version: string;
  name: string;
  kind: "avatar" | "theme" | "sticker";
}

export interface AvatarPack extends PackBase {
  kind: "avatar";
  category: "personal" | "group";
  fit: "cover" | "contain";
  avatars: { label: string; url: string }[];
}

export interface ThemePack extends PackBase {
  kind: "theme";
  base: "light" | "dark";
  colors: Record<string, string>;
  wallpaper?: string;
  crest?: string;
  wallpapers?: { id: string; title: string; url: string }[];
}

export interface StickerPack extends PackBase {
  kind: "sticker";
  stickers: { id: string; label: string; fallback: string; url: string }[];
}

export type CustomizationPack = AvatarPack | ThemePack | StickerPack;

export const MAX_PACK_ZIP_BYTES = 12 * 1024 * 1024;
export const MAX_IMAGE_SIDE = 4096;
export const MAX_IMAGE_PIXELS = 16_000_000;
const MAX_PACK_ENTRIES = 256;
const MAX_UNPACKED = 24 * 1024 * 1024;
const MAX_IMAGE = 3 * 1024 * 1024;
const TOKEN_NAMES = new Set([
  "background",
  "foreground",
  "card",
  "card-foreground",
  "popover",
  "popover-foreground",
  "primary",
  "primary-foreground",
  "secondary",
  "secondary-foreground",
  "muted",
  "muted-foreground",
  "accent",
  "accent-foreground",
  "destructive",
  "destructive-fill",
  "destructive-foreground",
  "border",
  "input",
  "ring",
  "signal",
  "attention",
  "verified",
  "mention",
  "bubble-own",
  "bubble-own-foreground",
  "bubble-own-link",
  "message-link",
  "shell-rail",
  "conversation-surface",
  "composer-surface",
]);

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Invalid pack manifest");
  }
  return value as Record<string, unknown>;
}

function named(value: unknown, field: string, max = 80): string {
  if (typeof value !== "string" || !value.trim() || value.length > max) {
    throw new Error(`Invalid ${field}`);
  }
  return value.trim();
}

function path(value: unknown, sticker = false): string {
  if (
    typeof value !== "string" ||
    !(sticker
      ? /^images\/[a-zA-Z0-9_./-]+\.(png|jpe?g|webp|gif)$/.test(value)
      : /^images\/[a-zA-Z0-9_./-]+\.(png|jpe?g|webp)$/.test(value)) ||
    value.split("/").some((part) => part === ".." || part === "." || !part)
  ) {
    throw new Error("Invalid image path");
  }
  return value;
}

/** The only image files a creator tool should include when building a ZIP. */
export function referencedPackImages(manifest: unknown): string[] {
  const raw = object(manifest);
  let references: unknown[] = [];
  if (raw.kind === "avatar" && Array.isArray(raw.avatars)) {
    references = raw.avatars.map((item) => object(item).file);
  } else if (raw.kind === "sticker" && Array.isArray(raw.stickers)) {
    references = raw.stickers.map((item) => object(item).file);
  } else if (raw.kind === "theme") {
    references = [raw.wallpaper, raw.crest];
    if (Array.isArray(raw.wallpapers)) {
      references.push(...raw.wallpapers.map((item) => object(item).file));
    }
  }
  return [
    ...new Set(
      references
        .filter((value) => value !== undefined)
        .map((value) => path(value, raw.kind === "sticker")),
    ),
  ];
}

function image(
  files: Record<string, Uint8Array>,
  value: unknown,
  sticker = false,
): string {
  const file = files[path(value, sticker)];
  if (!file || !file.length || file.length > MAX_IMAGE)
    throw new Error("Missing or oversized image");
  const name = value as string;
  const mime = name.endsWith(".png")
    ? "image/png"
    : name.endsWith(".webp")
      ? "image/webp"
      : name.endsWith(".gif")
        ? "image/gif"
        : "image/jpeg";
  const valid =
    mime === "image/png"
      ? file.length >= 8 &&
        [137, 80, 78, 71, 13, 10, 26, 10].every((byte, i) => file[i] === byte)
      : mime === "image/webp"
        ? strFromU8(file.subarray(0, 4)) === "RIFF" &&
          strFromU8(file.subarray(8, 12)) === "WEBP"
        : mime === "image/gif"
          ? ["GIF87a", "GIF89a"].includes(strFromU8(file.subarray(0, 6)))
          : file[0] === 0xff && file[1] === 0xd8 && file[2] === 0xff;
  if (!valid) throw new Error("Invalid image data");
  const dimensions = imageDimensions(file, mime);
  if (
    dimensions &&
    (!dimensions.width ||
      !dimensions.height ||
      dimensions.width > MAX_IMAGE_SIDE ||
      dimensions.height > MAX_IMAGE_SIDE ||
      dimensions.width * dimensions.height > MAX_IMAGE_PIXELS)
  ) {
    throw new Error("Pack image dimensions exceed the safety limit");
  }
  let binary = "";
  for (let i = 0; i < file.length; i += 8192) {
    binary += String.fromCharCode(...file.subarray(i, i + 8192));
  }
  return `data:${mime};base64,${btoa(binary)}`;
}

function imageDimensions(
  bytes: Uint8Array,
  mime: string,
): { width: number; height: number } | undefined {
  const u16le = (at: number) => bytes[at] | (bytes[at + 1] << 8);
  const u16be = (at: number) => (bytes[at] << 8) | bytes[at + 1];
  const u32be = (at: number) =>
    (bytes[at] * 0x1000000 +
      (bytes[at + 1] << 16) +
      (bytes[at + 2] << 8) +
      bytes[at + 3]) >>>
    0;
  if (mime === "image/png") {
    if (bytes.length < 24 || strFromU8(bytes.subarray(12, 16)) !== "IHDR")
      return undefined;
    return { width: u32be(16), height: u32be(20) };
  }
  if (mime === "image/gif") {
    if (bytes.length < 10) return undefined;
    return { width: u16le(6), height: u16le(8) };
  }
  if (mime === "image/webp") {
    if (bytes.length < 30) return undefined;
    const kind = strFromU8(bytes.subarray(12, 16));
    if (kind === "VP8X") {
      return {
        width: 1 + bytes[24] + (bytes[25] << 8) + (bytes[26] << 16),
        height: 1 + bytes[27] + (bytes[28] << 8) + (bytes[29] << 16),
      };
    }
    if (
      kind === "VP8 " &&
      bytes[23] === 0x9d &&
      bytes[24] === 0x01 &&
      bytes[25] === 0x2a
    ) {
      return { width: u16le(26) & 0x3fff, height: u16le(28) & 0x3fff };
    }
    if (kind === "VP8L" && bytes[20] === 0x2f) {
      return {
        width: 1 + bytes[21] + ((bytes[22] & 0x3f) << 8),
        height:
          1 + (bytes[22] >> 6) + (bytes[23] << 2) + ((bytes[24] & 0x0f) << 10),
      };
    }
    return undefined;
  }
  if (mime === "image/jpeg") {
    let at = 2;
    while (at + 3 < bytes.length) {
      if (bytes[at++] !== 0xff) return undefined;
      while (bytes[at] === 0xff) at++;
      const marker = bytes[at++];
      if (marker === 0xd9 || marker === 0xda) return undefined;
      if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd8)) continue;
      if (at + 2 > bytes.length) return undefined;
      const length = u16be(at);
      if (length < 2 || at + length > bytes.length) return undefined;
      if (
        (marker >= 0xc0 && marker <= 0xc3) ||
        (marker >= 0xc5 && marker <= 0xc7) ||
        (marker >= 0xc9 && marker <= 0xcb) ||
        (marker >= 0xcd && marker <= 0xcf)
      ) {
        if (length < 7) return undefined;
        return { width: u16be(at + 5), height: u16be(at + 3) };
      }
      at += length;
    }
  }
  return undefined;
}

export function parsePack(bytes: Uint8Array): CustomizationPack {
  if (bytes.length > MAX_PACK_ZIP_BYTES)
    throw new Error("Pack ZIP is too large");
  let total = 0;
  const seen = new Set<string>();
  const files = unzipSync(bytes, {
    filter: (entry) => {
      if (seen.has(entry.name)) throw new Error("Duplicate ZIP entry");
      seen.add(entry.name);
      if (seen.size > MAX_PACK_ENTRIES) throw new Error("Too many ZIP entries");
      total += entry.originalSize;
      if (
        entry.name.startsWith("/") ||
        entry.name.includes("\\") ||
        entry.name.split("/").some((part) => part === ".." || part === ".")
      ) {
        throw new Error("Invalid ZIP path");
      }
      if (
        entry.name !== "manifest.json" &&
        entry.name !== "credits.json" &&
        !/^images\/(?:[a-zA-Z0-9_-]+\/)*$/.test(entry.name) &&
        !/^images\/[a-zA-Z0-9_./-]+\.(png|jpe?g|webp|gif)$/.test(entry.name)
      ) {
        throw new Error("Unsupported ZIP file");
      }
      if (
        total > MAX_UNPACKED ||
        (entry.originalSize > MAX_IMAGE && entry.name !== "manifest.json") ||
        ((entry.name === "manifest.json" || entry.name === "credits.json") &&
          entry.originalSize > 64 * 1024)
      ) {
        throw new Error("Pack content is too large");
      }
      return (
        entry.name === "manifest.json" ||
        entry.name === "credits.json" ||
        (entry.name.startsWith("images/") && !entry.name.endsWith("/"))
      );
    },
  });
  const manifestBytes = files["manifest.json"];
  if (!manifestBytes || manifestBytes.length > 64 * 1024)
    throw new Error("Missing manifest.json");
  if (files["credits.json"]) JSON.parse(strFromU8(files["credits.json"]));
  const raw = object(JSON.parse(strFromU8(manifestBytes)));
  if (raw.format !== 1) throw new Error("Unsupported pack format");
  const referencedImages = new Set(referencedPackImages(raw));
  if (
    Object.keys(files).some(
      (name) => name.startsWith("images/") && !referencedImages.has(name),
    )
  ) {
    throw new Error("Unreferenced image in pack");
  }
  if (
    typeof raw.id !== "string" ||
    !/^[a-z0-9][a-z0-9._-]{2,79}$/.test(raw.id)
  ) {
    throw new Error("Invalid pack id");
  }
  if (["dark", "light", "oled"].includes(raw.id))
    throw new Error("Reserved pack id");
  if (typeof raw.version !== "string" || !/^\d+\.\d+\.\d+$/.test(raw.version)) {
    throw new Error("Invalid pack version");
  }
  const base = {
    format: 1 as const,
    id: raw.id,
    version: raw.version,
    name: named(raw.name, "name"),
  };
  if (raw.kind === "avatar") {
    if (raw.category !== "personal" && raw.category !== "group")
      throw new Error("Invalid avatar category");
    if (raw.fit !== "cover" && raw.fit !== "contain")
      throw new Error("Invalid avatar fit");
    if (
      !Array.isArray(raw.avatars) ||
      !raw.avatars.length ||
      raw.avatars.length > 100
    ) {
      throw new Error("Invalid avatar list");
    }
    return {
      ...base,
      kind: "avatar",
      category: raw.category,
      fit: raw.fit,
      avatars: raw.avatars.map((item) => {
        const entry = object(item);
        return {
          label: named(entry.label, "avatar label"),
          url: image(files, entry.file),
        };
      }),
    };
  }
  if (raw.kind === "theme") {
    if (raw.base !== "light" && raw.base !== "dark")
      throw new Error("Invalid theme base");
    const colors = object(raw.colors);
    if (
      !Object.keys(colors).length ||
      Object.keys(colors).some((token) => {
        const value = colors[token];
        if (!TOKEN_NAMES.has(token) || typeof value !== "string") return true;
        const match =
          /^(\d{1,3}(?:\.\d+)?)\s+(\d{1,3}(?:\.\d+)?)%\s+(\d{1,3}(?:\.\d+)?)%$/.exec(
            value,
          );
        return (
          !match ||
          Number(match[1]) > 360 ||
          Number(match[2]) > 100 ||
          Number(match[3]) > 100
        );
      })
    ) {
      throw new Error("Invalid theme color");
    }
    let wallpapers: ThemePack["wallpapers"];
    if (raw.wallpapers !== undefined) {
      if (
        !Array.isArray(raw.wallpapers) ||
        !raw.wallpapers.length ||
        raw.wallpapers.length > 60
      ) {
        throw new Error("Invalid wallpaper list");
      }
      const wallpaperIds = new Set<string>();
      wallpapers = raw.wallpapers.map((item) => {
        const entry = object(item);
        const id = named(entry.id, "wallpaper id");
        if (!/^[a-z0-9-]+$/.test(id) || wallpaperIds.has(id))
          throw new Error("Invalid or duplicate wallpaper id");
        wallpaperIds.add(id);
        return {
          id,
          title: named(entry.title, "wallpaper title"),
          url: image(files, entry.file),
        };
      });
    }
    return {
      ...base,
      kind: "theme",
      base: raw.base,
      colors: colors as Record<string, string>,
      ...(raw.wallpaper !== undefined
        ? { wallpaper: image(files, raw.wallpaper) }
        : {}),
      ...(raw.crest !== undefined ? { crest: image(files, raw.crest) } : {}),
      ...(wallpapers ? { wallpapers } : {}),
    };
  }
  if (raw.kind === "sticker") {
    if (
      !Array.isArray(raw.stickers) ||
      !raw.stickers.length ||
      raw.stickers.length > 100
    )
      throw new Error("Invalid sticker list");
    const ids = new Set<string>();
    const stickers = raw.stickers.map((item) => {
      const entry = object(item);
      const id = named(entry.id, "sticker id", 40);
      if (!/^[a-z0-9][a-z0-9_-]*$/.test(id) || ids.has(id))
        throw new Error("Invalid or duplicate sticker id");
      ids.add(id);
      const fallback = named(entry.fallback, "sticker fallback", 32);
      if (Array.from(fallback).length > 8)
        throw new Error("Sticker fallback is too long");
      return {
        id,
        label: named(entry.label, "sticker label"),
        fallback,
        url: image(files, entry.file, true),
      };
    });
    return { ...base, kind: "sticker", stickers };
  }
  throw new Error("Unknown pack kind");
}
