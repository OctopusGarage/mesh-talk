// Shared, non-component helpers for file/media messages: type sniffing from the file
// name, the inline-render size gate, a file glyph, and the lazy decrypted-bytes blob-URL
// hook. Kept out of MediaPreview.tsx so that file can export only its component (the
// react-refresh rule flags mixing component + non-component exports in one module).
import { useEffect, useState } from "react";
import {
  FileArchive,
  FileAudio,
  FileImage,
  FileText,
  FileVideo,
  File as FileIcon,
} from "lucide-react";
import { chat } from "@/lib/api";

const IMAGE_EXT = /\.(png|jpe?g|gif|webp|bmp|avif|svg)$/i;
const VIDEO_EXT = /\.(mp4|mov|webm|m4v|ogv)$/i;
export const isImage = (name: string) => IMAGE_EXT.test(name);
export const isVideo = (name: string) => VIDEO_EXT.test(name);

/** Best-effort MIME from a file name. The inline <video>/<img> blob URL MUST carry a type,
 * or macOS WKWebView can't pick a decoder and the video silently won't play. Used as a
 * fallback when the manifest's mime is missing/empty. */
export function mimeFromName(name: string): string | undefined {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  const map: Record<string, string> = {
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    gif: "image/gif",
    webp: "image/webp",
    bmp: "image/bmp",
    avif: "image/avif",
    svg: "image/svg+xml",
    mp4: "video/mp4",
    mov: "video/quicktime",
    webm: "video/webm",
    m4v: "video/x-m4v",
    ogv: "video/ogg",
  };
  return map[ext];
}

// Only fetch the whole file into memory for inline rendering when it's small enough.
// `read_file` decrypts and returns the entire file (bounded only by the 4 GiB hard cap),
// so we gate on the manifest-reported size before pulling bytes through IPC.
const IMAGE_INLINE_CAP = 16 * 1024 * 1024; // 16 MB
const VIDEO_INLINE_CAP = 50 * 1024 * 1024; // 50 MB

/** Whether an image/video of `size` bytes is small enough to fetch + render inline. */
export function withinInlineCap(name: string, size: number): boolean {
  if (isImage(name)) return size <= IMAGE_INLINE_CAP;
  if (isVideo(name)) return size <= VIDEO_INLINE_CAP;
  return false;
}

/** Pick an on-brand file glyph from the extension. */
export function fileGlyph(name: string) {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  const cls = "h-4 w-4 shrink-0 text-signal";
  if (IMAGE_EXT.test(name)) return <FileImage className={cls} />;
  if (/^(mp4|mov|mkv|webm|avi|m4v|ogv)$/.test(ext))
    return <FileVideo className={cls} />;
  if (/^(mp3|wav|flac|aac|ogg|m4a)$/.test(ext))
    return <FileAudio className={cls} />;
  if (/^(zip|tar|gz|7z|rar|bz2|xz)$/.test(ext))
    return <FileArchive className={cls} />;
  if (/^(txt|md|pdf|doc|docx|csv|json|log)$/.test(ext))
    return <FileText className={cls} />;
  return <FileIcon className={cls} />;
}

/** Lazily fetch a received file's decrypted bytes and expose them as a short-lived object
 * URL, revoked on unmount (and never refetched on re-render). Returns null until loaded. */
export function useFileObjectUrl(
  fileConv: string,
  enabled: boolean,
  mime?: string,
): string | null {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    let objectUrl: string | null = null;
    const show = (buf: ArrayBuffer) => {
      if (!alive) return;
      // The blob MUST carry a MIME type or macOS WKWebView won't play an inline <video>.
      objectUrl = URL.createObjectURL(
        new Blob([buf], mime ? { type: mime } : {}),
      );
      setUrl(objectUrl);
    };
    // Load from the DURABLE chat-media store first (survives chunk prune + restart); fall
    // back to reassembling the transient chunks only if the store has no copy yet (a
    // just-arrived media file before its receive-complete persist, or legacy history).
    chat
      .readMedia(fileConv)
      .then(show)
      .catch(() =>
        chat
          .readFile(fileConv)
          .then(show)
          .catch(() => {}),
      );
    return () => {
      alive = false;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [fileConv, enabled, mime]);
  return url;
}
