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
  let binary = "";
  for (let i = 0; i < file.length; i += 8192) {
    binary += String.fromCharCode(...file.subarray(i, i + 8192));
  }
  return `data:${mime};base64,${btoa(binary)}`;
}

export function parsePack(bytes: Uint8Array): CustomizationPack {
  if (bytes.length > MAX_PACK_ZIP_BYTES)
    throw new Error("Pack ZIP is too large");
  let total = 0;
  const files = unzipSync(bytes, {
    filter: (entry) => {
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
      Object.keys(colors).some(
        (token) =>
          !TOKEN_NAMES.has(token) ||
          typeof colors[token] !== "string" ||
          !/^\d{1,3}(?:\.\d+)?\s+\d{1,3}(?:\.\d+)?%\s+\d{1,3}(?:\.\d+)?%$/.test(
            colors[token],
          ),
      )
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
      wallpapers = raw.wallpapers.map((item) => {
        const entry = object(item);
        const id = named(entry.id, "wallpaper id");
        if (!/^[a-z0-9-]+$/.test(id)) throw new Error("Invalid wallpaper id");
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
      ...(raw.wallpaper ? { wallpaper: image(files, raw.wallpaper) } : {}),
      ...(raw.crest ? { crest: image(files, raw.crest) } : {}),
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
