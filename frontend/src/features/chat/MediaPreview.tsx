import {
  useCallback,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type SyntheticEvent,
} from "react";
import { AnimatePresence, motion } from "framer-motion";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { Download, FolderOpen, Play, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { attachmentLabel } from "@/lib/directoryAttachment";
import { saveAttachment } from "@/lib/attachmentSave";
import { useSavedDownloads } from "@/lib/savedDownloads";
import { errorMessage } from "@/lib/error";
import { humanSize } from "@/lib/format";
import { ease, useMotionOK } from "@/lib/motion";
import { useChat, captureChatOwnership } from "@/store/chat";
import {
  useFileStatus,
  markUnavailableFileError,
} from "@/store/fileAvailability";
import { ReceiveProgress } from "./ReceiveProgress";
import { isVideo } from "@/lib/mediaTypes";
import { isMacOverlay } from "@/lib/platform";
import { copySensitiveText } from "@/lib/sensitiveClipboard";
import {
  blobMime,
  useFileObjectUrl,
  useVideoPoster,
  withinInlineCap,
} from "./mediaFile";

// Geometry follows the live Blob URL, so retirement cannot reuse another file's size.
// Bound numeric metadata just like ordinary media reuse; no plaintext bytes are retained.
const imageGeometry = new Map<string, { width: number; height: number }>();

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
  const displayName = attachmentLabel(
    name,
    mime ?? "",
    { media: true, size },
    t("message.sticker"),
  );
  const motionOK = useMotionOK();
  const setError = useChat((s) => s.setError);
  const savedPath = useSavedDownloads()[fileConv];
  const status = useFileStatus(savedPath ? undefined : fileConv);
  const available = !!savedPath || !!status?.ready;
  const video = isVideo(name);
  const withinCap = withinInlineCap(name, size);
  const url = useFileObjectUrl(
    fileConv,
    available && withinCap,
    blobMime(name, mime),
    { reuse: true },
  );
  const [decoded, setDecoded] = useState<{
    url: string;
    width: number;
    height: number;
  } | null>(null);
  const geometry = url
    ? decoded?.url === url
      ? decoded
      : imageGeometry.get(url)
    : undefined;
  const frameClass = `relative w-80 max-w-full overflow-hidden rounded-lg bg-muted/40 ${video ? "aspect-video" : "aspect-[4/3]"}`;
  const imageFrameClass =
    "relative block max-w-full overflow-hidden rounded-lg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring";
  // A provisional 4:3 frame becomes the actual aspect ratio on the first decode.
  // Reused URLs reserve the decoded geometry immediately on virtualized remounts.
  const imageStyle: CSSProperties | undefined = geometry
    ? {
        width: Math.min(
          geometry.width,
          320,
          (320 * geometry.width) / geometry.height,
        ),
        aspectRatio: `${geometry.width} / ${geometry.height}`,
      }
    : undefined;
  const rememberImage = (event: SyntheticEvent<HTMLImageElement>) => {
    const { naturalWidth: width, naturalHeight: height } = event.currentTarget;
    if (!url || !width || !height) return;
    if (imageGeometry.size >= 64 && !imageGeometry.has(url))
      imageGeometry.delete(imageGeometry.keys().next().value!);
    imageGeometry.set(url, { width, height });
    setDecoded({ url, width, height });
  };
  const [lightbox, setLightbox] = useState(false);
  const [saving, setSaving] = useState(false);
  const nativeImageText = isMacOverlay();
  const backdropPointerDown = useRef(false);
  const playerRef = useRef<HTMLVideoElement>(null);
  const closeLightbox = useCallback(() => {
    playerRef.current?.pause();
    setLightbox(false);
  }, []);
  useLayoutEffect(() => {
    if (!lightbox) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      closeLightbox();
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("keydown", onKeyDown, true);
      // WebKit's recognized text belongs to this preview, including during exit.
      window.getSelection()?.removeAllRanges();
    };
  }, [lightbox, closeLightbox]);
  const layoutId =
    motionOK && (video || geometry) ? `media-${fileConv}` : undefined;
  const layoutTransition = { duration: 0.22, ease };
  const backdropTransition = { duration: motionOK ? 0.16 : 0.08, ease };
  // An inline image whose codec the webview can't decode (e.g. HEIC outside Safari) fires
  // <img onError>; we then degrade to the same "can't preview — Save" card as video.
  const [imgFailed, setImgFailed] = useState(false);

  // Save the media to disk. The filename/size/download live here in the detail view (not
  // under every bubble) — the chat shows just the media.
  const saveAs = async () => {
    const lease = captureChatOwnership();
    if (!lease.current() || !available || saving) return;
    try {
      setSaving(true);
      await saveAttachment({ fileConv, name, mime, size, media: true }, lease, {
        mode: "as",
      });
    } catch (e) {
      if (lease.current() && markUnavailableFileError(fileConv, e)) return;
      if (lease.current())
        setError(t("files.couldntSave", { error: errorMessage(e) }));
    } finally {
      setSaving(false);
    }
  };
  const reveal = async () => {
    if (!savedPath) return;
    const lease = captureChatOwnership();
    try {
      await revealItemInDir(savedPath);
    } catch (e) {
      if (lease.current())
        setError(t("files.couldntOpen", { error: errorMessage(e) }));
    }
  };
  // The detail bar shown at the bottom of the lightbox: filename · size · download. Clicks
  // here don't dismiss the lightbox.
  const detailBar = (
    <div
      onClick={(e) => e.stopPropagation()}
      className="absolute inset-x-0 bottom-0 flex items-center gap-3 bg-gradient-to-t from-black/70 to-transparent px-5 pb-5 pt-12 text-white"
    >
      <span className="min-w-0 flex-1 truncate text-sm" title={displayName}>
        {displayName}
      </span>
      <span className="shrink-0 font-mono text-xs text-white/70">
        {humanSize(size)}
      </span>
      {savedPath && (
        <span
          className="min-w-0 max-w-48 truncate text-xs text-white/70"
          title={savedPath}
        >
          {savedPath}
        </span>
      )}
      <button
        type="button"
        data-testid="media-detail-save"
        disabled={saving}
        onClick={() => void (savedPath ? reveal() : saveAs())}
        title={
          savedPath ? t("files.savedTo", { path: savedPath }) : t("common.save")
        }
        className="flex shrink-0 items-center gap-1.5 rounded-md bg-white/15 px-2.5 py-1 text-xs font-medium hover:bg-white/25"
      >
        {savedPath ? (
          <FolderOpen className="h-3.5 w-3.5" />
        ) : (
          <Download className="h-3.5 w-3.5" />
        )}
        {savedPath ? t("files.reveal") : t("common.save")}
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
      className={`${frameClass} flex items-center gap-2 border px-3 py-2`}
      data-testid={testid}
    >
      <p className="min-w-0 flex-1 text-xs text-muted-foreground">
        {t(msgKey)}
      </p>
      {!readOnly && (
        <button
          type="button"
          data-testid="media-fallback-save"
          disabled={saving}
          onClick={() => void (savedPath ? reveal() : saveAs())}
          title={
            savedPath
              ? t("files.savedTo", { path: savedPath })
              : t("common.save")
          }
          aria-label={savedPath ? t("files.reveal") : t("common.save")}
          className="flex shrink-0 items-center gap-1.5 rounded-md border px-2 py-1 text-xs font-medium hover:bg-accent"
        >
          {savedPath ? (
            <FolderOpen className="h-3.5 w-3.5" />
          ) : (
            <Download className="h-3.5 w-3.5" />
          )}
          {savedPath ? t("files.reveal") : t("common.save")}
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
  if (!available)
    return (
      <div
        className={`${frameClass} flex flex-col justify-center p-2 text-xs`}
        title={displayName}
      >
        <span className="block truncate">{displayName}</span>
        <ReceiveProgress status={status} />
      </div>
    );
  if (!url)
    return (
      <div
        className={`${frameClass} flex items-center justify-center px-3 text-center text-xs text-muted-foreground`}
        role="status"
      >
        {t("common.loading")} {displayName}
      </div>
    );

  if (video) {
    // A captured poster frame (see useVideoPoster); a neutral box while it's still loading.
    const thumb = poster ? (
      <img
        src={poster}
        alt={displayName}
        data-testid="file-video"
        className="h-full w-full object-contain"
      />
    ) : (
      <div data-testid="file-video" className="h-full w-full bg-muted" />
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
        <div className={frameClass}>
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
          className={`${frameClass} block`}
        >
          <motion.div
            layoutId={layoutId}
            transition={layoutTransition}
            className="relative h-full w-full"
          >
            {thumb}
            {playBadge}
          </motion.div>
        </button>
        <AnimatePresence>
          {lightbox && (
            <motion.div
              role="dialog"
              aria-modal="true"
              aria-label={displayName}
              data-testid="file-video-lightbox"
              onClick={closeLightbox}
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0, pointerEvents: "none" }}
              transition={backdropTransition}
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
              <motion.div
                layoutId={layoutId}
                transition={layoutTransition}
                className="flex max-h-full max-w-full items-center justify-center"
              >
                <video
                  ref={playerRef}
                  src={url}
                  controls
                  autoPlay
                  playsInline
                  data-testid="file-video-player"
                  onClick={(e) => e.stopPropagation()}
                  className="max-h-full max-w-full rounded-lg bg-black"
                />
              </motion.div>
              {detailBar}
            </motion.div>
          )}
        </AnimatePresence>
      </>
    );
  }

  // Read-only (history viewer): the image as-is, no click-to-enlarge, no lightbox.
  if (readOnly) {
    return (
      <div
        className={geometry ? imageFrameClass : frameClass}
        style={imageStyle}
      >
        <img
          src={url}
          alt={displayName}
          data-testid="file-image"
          onLoad={rememberImage}
          onError={() => setImgFailed(true)}
          className="h-full w-full select-none object-contain"
        />
      </div>
    );
  }

  return (
    <>
      <button
        type="button"
        onClick={() => setLightbox(true)}
        title={t("files.viewFullSize")}
        aria-label={t("files.viewFullSize")}
        className={geometry ? imageFrameClass : `${frameClass} block`}
        style={imageStyle}
      >
        <motion.img
          src={url}
          alt={displayName}
          layoutId={layoutId}
          transition={layoutTransition}
          data-testid="file-image"
          onLoad={rememberImage}
          onError={() => setImgFailed(true)}
          className="h-full w-full select-none object-contain"
        />
      </button>
      <AnimatePresence>
        {lightbox && (
          <motion.div
            role="dialog"
            aria-modal="true"
            aria-label={displayName}
            data-testid="file-image-lightbox"
            onPointerDown={(event) => {
              backdropPointerDown.current =
                event.target === event.currentTarget;
            }}
            onClick={(event) => {
              // A selection drag that ends outside the image must not dismiss it.
              if (
                event.target === event.currentTarget &&
                backdropPointerDown.current
              )
                closeLightbox();
            }}
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0, pointerEvents: "none" }}
            transition={backdropTransition}
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 p-6"
          >
            <button
              type="button"
              aria-label={t("common.close")}
              onClick={closeLightbox}
              className="absolute right-4 top-4 rounded-full bg-background/80 p-2 text-foreground hover:bg-background"
            >
              <X className="h-4 w-4" />
            </button>
            {/* Stop clicks on the photo from closing the lightbox (matches the video player). */}
            <motion.img
              src={url}
              alt={displayName}
              layoutId={layoutId}
              transition={layoutTransition}
              onClick={(e) => e.stopPropagation()}
              onContextMenu={
                nativeImageText ? (event) => event.stopPropagation() : undefined
              }
              onCopy={
                nativeImageText
                  ? (event) => {
                      const text = window.getSelection()?.toString();
                      if (!text) return;
                      event.preventDefault();
                      event.stopPropagation();
                      const lease = captureChatOwnership();
                      if (!lease.current()) return;
                      void copySensitiveText(
                        text,
                        t("settings.confirmCopyPrompt"),
                        (value) =>
                          event.clipboardData.setData("text/plain", value),
                      ).catch(() => {
                        if (lease.current()) setError(t("common.copyFailed"));
                      });
                    }
                  : undefined
              }
              className={`max-h-full max-w-full rounded-lg object-contain${nativeImageText ? " select-text" : ""}`}
            />
            {detailBar}
          </motion.div>
        )}
      </AnimatePresence>
    </>
  );
}
