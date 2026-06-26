import { useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { Download, Play, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { chat } from "@/lib/api";
import { defaultSavePath } from "@/lib/download";
import { errorMessage } from "@/lib/error";
import { humanSize } from "@/lib/format";
import { useChat } from "@/store/chat";
import {
  blobMime,
  isVideo,
  useFileObjectUrl,
  useVideoPoster,
  withinInlineCap,
} from "./mediaFile";

/** Inline preview for a received image or small video. Both are clickable to open a
 * full-size lightbox — an image shows large, a video autoplays with controls. Renders
 * nothing while the bytes are still loading. Bytes are fetched only when under the
 * relevant size cap. */
export function MediaPreview({
  fileConv,
  name,
  size,
  mime,
  readOnly = false,
}: {
  fileConv: string;
  name: string;
  size: number;
  /** The manifest MIME; needed so the inline <video> blob is typed (else macOS won't play). */
  mime?: string;
  /** Non-interactive: render just the image / video poster — no click-to-enlarge, no play,
   *  no Save. Used by the history viewer, where media should look like the chat but isn't
   *  clickable. */
  readOnly?: boolean;
}) {
  const { t } = useTranslation();
  const setError = useChat((s) => s.setError);
  const video = isVideo(name);
  const withinCap = withinInlineCap(name, size);
  const url = useFileObjectUrl(fileConv, withinCap, blobMime(name, mime));
  const [lightbox, setLightbox] = useState(false);
  // An inline image whose codec the webview can't decode (e.g. HEIC outside Safari) fires
  // <img onError>; we then degrade to the same "can't preview — Save" card as video.
  const [imgFailed, setImgFailed] = useState(false);

  // Save the media to disk. The filename/size/download live here in the detail view (not
  // under every bubble) — the chat shows just the media.
  const saveAs = async () => {
    try {
      const dest = await save({ defaultPath: await defaultSavePath(name) });
      if (typeof dest === "string") await chat.saveFile(fileConv, dest);
    } catch (e) {
      setError(t("files.couldntSave", { error: errorMessage(e) }));
    }
  };
  // The detail bar shown at the bottom of the lightbox: filename · size · download. Clicks
  // here don't dismiss the lightbox.
  const detailBar = (
    <div
      onClick={(e) => e.stopPropagation()}
      className="absolute inset-x-0 bottom-0 flex items-center gap-3 bg-gradient-to-t from-black/70 to-transparent px-5 pb-5 pt-12 text-white"
    >
      <span className="min-w-0 flex-1 truncate text-sm" title={name}>
        {name}
      </span>
      <span className="shrink-0 font-mono text-xs text-white/70">
        {humanSize(size)}
      </span>
      <button
        type="button"
        data-testid="media-detail-save"
        onClick={() => void saveAs()}
        className="flex shrink-0 items-center gap-1.5 rounded-md bg-white/15 px-2.5 py-1 text-xs font-medium hover:bg-white/25"
      >
        <Download className="h-3.5 w-3.5" />
        {t("common.save")}
      </button>
    </div>
  );
  // Capture a real (non-black) poster frame for the inline video. If the codec can't be
  // decoded (notably iPhone .mov / HEVC), the poster never arrives — treat that as
  // unsupported and show a hint instead of a dead black box.
  const { poster, failed: videoFailed } = useVideoPoster(
    video ? url : null,
    fileConv,
  );

  // Shared "can't preview inline" card: a hint plus a reachable Save button, so a file is
  // never a dead end (no empty bubble, no broken-image icon).
  const unpreviewable = (msgKey: string, testid?: string) => (
    <div
      className="mb-1.5 flex items-center gap-2 rounded-lg border bg-muted/40 px-3 py-2"
      data-testid={testid}
    >
      <p className="min-w-0 flex-1 text-xs text-muted-foreground">
        {t(msgKey)}
      </p>
      {!readOnly && (
        <button
          type="button"
          data-testid="media-fallback-save"
          onClick={() => void saveAs()}
          title={t("common.save")}
          aria-label={t("common.save")}
          className="flex shrink-0 items-center gap-1.5 rounded-md border px-2 py-1 text-xs font-medium hover:bg-accent"
        >
          <Download className="h-3.5 w-3.5" />
          {t("common.save")}
        </button>
      )}
    </div>
  );

  // A video we can't preview inline (too large, or an undecodable codec like HEVC). Over-cap
  // media is normally handled by FileBubble's file card; this also covers a decode failure.
  if (video && (!withinCap || videoFailed)) {
    return unpreviewable(
      videoFailed ? "files.videoUnsupported" : "files.largeVideo",
      videoFailed ? "file-video-unsupported" : undefined,
    );
  }
  // An image the webview couldn't decode (e.g. HEIC outside Safari).
  if (!video && imgFailed) {
    return unpreviewable("files.imageUnsupported", "file-image-unsupported");
  }
  if (!url) return null;

  if (video) {
    // A captured poster frame (see useVideoPoster); a neutral box while it's still loading.
    const thumb = poster ? (
      <img
        src={poster}
        alt={name}
        data-testid="file-video"
        className="max-h-80 w-full rounded-lg object-cover"
      />
    ) : (
      <div
        data-testid="file-video"
        className="flex aspect-video w-full items-center justify-center rounded-lg bg-muted"
      />
    );
    // A play badge marks it as a video (purely visual in read-only mode).
    const playBadge = (
      <span
        aria-hidden
        className="absolute inset-0 flex items-center justify-center"
      >
        <span className="rounded-full bg-black/55 p-3 text-white">
          <Play className="h-6 w-6 fill-current" />
        </span>
      </span>
    );
    // Read-only (history viewer): show the poster + badge, no click-to-play, no lightbox.
    if (readOnly) {
      return (
        <div className="relative">
          {thumb}
          {playBadge}
        </div>
      );
    }
    return (
      <>
        {/* Inline preview: the poster with a play badge. Click opens the fullscreen player —
            same interaction as an image's lightbox. */}
        <button
          type="button"
          onClick={() => setLightbox(true)}
          title={t("files.playVideo")}
          aria-label={t("files.playVideo")}
          className="relative mb-1.5 block w-full"
        >
          {thumb}
          {playBadge}
        </button>
        {lightbox && (
          <div
            role="dialog"
            aria-modal="true"
            aria-label={name}
            data-testid="file-video-lightbox"
            onClick={() => setLightbox(false)}
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 p-6"
          >
            <button
              type="button"
              aria-label={t("common.close")}
              className="absolute right-4 top-4 rounded-full bg-background/80 p-2 text-foreground hover:bg-background"
            >
              <X className="h-4 w-4" />
            </button>
            {/* Stop clicks on the player from closing the lightbox (so controls work). */}
            <video
              src={url}
              controls
              autoPlay
              playsInline
              data-testid="file-video-player"
              onClick={(e) => e.stopPropagation()}
              className="max-h-full max-w-full rounded-lg bg-black"
            />
            {detailBar}
          </div>
        )}
      </>
    );
  }

  // Read-only (history viewer): the image as-is, no click-to-enlarge, no lightbox.
  if (readOnly) {
    return (
      <img
        src={url}
        alt={name}
        data-testid="file-image"
        onError={() => setImgFailed(true)}
        className="max-h-80 w-full rounded-lg object-cover"
      />
    );
  }

  return (
    <>
      <button
        type="button"
        onClick={() => setLightbox(true)}
        title={t("files.viewFullSize")}
        aria-label={t("files.viewFullSize")}
        className="mb-1.5 block w-full"
      >
        <img
          src={url}
          alt={name}
          data-testid="file-image"
          onError={() => setImgFailed(true)}
          className="max-h-80 w-full rounded-lg object-cover"
        />
      </button>
      {lightbox && (
        <div
          role="dialog"
          aria-modal="true"
          aria-label={name}
          data-testid="file-image-lightbox"
          onClick={() => setLightbox(false)}
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 p-6"
        >
          <button
            type="button"
            aria-label={t("common.close")}
            className="absolute right-4 top-4 rounded-full bg-background/80 p-2 text-foreground hover:bg-background"
          >
            <X className="h-4 w-4" />
          </button>
          {/* Stop clicks on the photo from closing the lightbox (matches the video player). */}
          <img
            src={url}
            alt={name}
            onClick={(e) => e.stopPropagation()}
            className="max-h-full max-w-full rounded-lg object-contain"
          />
          {detailBar}
        </div>
      )}
    </>
  );
}
