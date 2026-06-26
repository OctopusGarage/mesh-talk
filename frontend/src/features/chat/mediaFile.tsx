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

// Single source of truth for media extensions — the type predicates, the MIME map, AND the
// image-button file-dialog filter (ConversationView) are all derived from these, so the three
// can't drift apart (e.g. a file that previews when dropped but not when picked).
export const IMAGE_EXTENSIONS = [
  "png",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "bmp",
  "avif",
  "svg",
  "heic",
  "heif",
];
export const VIDEO_EXTENSIONS = ["mp4", "mov", "webm", "m4v", "ogv"];
const IMAGE_EXT = new RegExp(`\\.(${IMAGE_EXTENSIONS.join("|")})$`, "i");
const VIDEO_EXT = new RegExp(`\\.(${VIDEO_EXTENSIONS.join("|")})$`, "i");
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
    heic: "image/heic",
    heif: "image/heif",
    mp4: "video/mp4",
    mov: "video/quicktime",
    webm: "video/webm",
    m4v: "video/x-m4v",
    ogv: "video/ogg",
  };
  return map[ext];
}

/** The MIME to type the inline blob with. Prefer the name-derived type for known image/video
 * extensions, because the manifest MIME has historically been a generic
 * `application/octet-stream` — and an `<video>` blob typed octet-stream silently won't decode
 * in WKWebView (an `<img>` byte-sniffs, so images were unaffected). Falls back to the manifest
 * MIME for unknown extensions. */
export function blobMime(
  name: string,
  manifestMime?: string,
): string | undefined {
  return mimeFromName(name) ?? manifestMime;
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

// Captured posters are cached by the file's stable conversation id, so a video row that
// scrolls out of and back into the virtualized list (Virtuoso unmounts it) reuses the JPEG
// instead of decoding the whole clip again. Failures are remembered too, to skip re-decoding
// a known-undecodable clip. Bounded by a small FIFO cap (posters are tiny but unbounded
// growth over a long session is avoided).
const POSTER_CACHE = new Map<string, string>();
const POSTER_FAILED = new Set<string>();
const POSTER_CACHE_MAX = 64;
function rememberPoster(key: string, data: string) {
  if (POSTER_CACHE.size >= POSTER_CACHE_MAX) {
    const oldest = POSTER_CACHE.keys().next().value;
    if (oldest !== undefined) POSTER_CACHE.delete(oldest);
  }
  POSTER_CACHE.set(key, data);
}

/** Capture a poster frame for a video object URL. `<video preload="metadata">` paints a
 * blank/black frame in the webview, and frame 0 is often a black fade-in — so we decode the
 * video off-screen, seek a little past the start, and draw that frame to a canvas (a JPEG
 * data URL). `failed` is set when the codec can't be decoded (poster never arrives), which
 * the caller also treats as "can't preview". Blob URLs are same-origin, so the canvas isn't
 * tainted. `cacheKey` (the file's stable conversation id) memoizes the result across remounts.
 * Returns `{ poster: null, failed: false }` while still working. */
export function useVideoPoster(
  url: string | null,
  cacheKey?: string | null,
): {
  poster: string | null;
  failed: boolean;
} {
  const [poster, setPoster] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    setPoster(null);
    setFailed(false);
    if (!url) return;
    // Reuse a previously captured poster / known failure for this file (no re-decode).
    if (cacheKey) {
      const cached = POSTER_CACHE.get(cacheKey);
      if (cached) {
        setPoster(cached);
        return;
      }
      if (POSTER_FAILED.has(cacheKey)) {
        setFailed(true);
        return;
      }
    }
    let alive = true;
    let settled = false;
    const v = document.createElement("video");
    v.muted = true;
    v.playsInline = true;
    v.preload = "auto";
    v.src = url;

    // Exactly-once teardown: stop the safety timer and the error listener, then release the
    // element. CRITICAL: this must run on SUCCESS too — otherwise the 5s timer (or the
    // `error` that `release()` itself can dispatch) later fires `fail()` and flips `failed`
    // true on an already-captured, perfectly good video, swapping the player for the
    // "unsupported" hint a few seconds in.
    const finish = () => {
      if (settled) return;
      settled = true;
      v.removeEventListener("loadeddata", onLoaded);
      v.removeEventListener("error", fail);
      v.removeEventListener("seeked", grab);
      v.removeAttribute("src");
      v.load();
    };
    function fail() {
      if (settled) return;
      finish();
      if (cacheKey) POSTER_FAILED.add(cacheKey);
      if (alive) setFailed(true);
    }
    function grab() {
      if (settled) return;
      try {
        const w = v.videoWidth;
        const h = v.videoHeight;
        if (!w || !h) return fail();
        const canvas = document.createElement("canvas");
        canvas.width = w;
        canvas.height = h;
        const c2d = canvas.getContext("2d");
        if (!c2d) return fail();
        c2d.drawImage(v, 0, 0, w, h);
        const data = canvas.toDataURL("image/jpeg", 0.7);
        finish(); // settle BEFORE setState so a release-triggered error can't undo it
        if (cacheKey) rememberPoster(cacheKey, data);
        if (alive) setPoster(data);
      } catch {
        fail();
      }
    }
    function onLoaded() {
      // Seek a touch past the start to dodge a black intro frame (clamped to the duration).
      const target = Number.isFinite(v.duration)
        ? Math.min(0.5, v.duration / 2)
        : 0.1;
      v.addEventListener("seeked", grab, { once: true });
      try {
        v.currentTime = target;
      } catch {
        grab();
      }
    }
    v.addEventListener("loadeddata", onLoaded);
    v.addEventListener("error", fail);
    // Stuck-decode safety net: if NOTHING fires, treat it as undecodable. After a success
    // (or any settle) `fail` no-ops via `settled`, so a late tick is harmless; we still
    // clear it on unmount.
    const timer = window.setTimeout(fail, 5000);
    return () => {
      alive = false;
      clearTimeout(timer);
      finish();
    };
  }, [url, cacheKey]);
  return { poster, failed };
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
