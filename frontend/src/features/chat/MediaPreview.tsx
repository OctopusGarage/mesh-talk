import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type SyntheticEvent,
} from "react";
import { AnimatePresence, motion } from "framer-motion";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  Download,
  FolderOpen,
  Play,
  X,
  ZoomIn,
  ZoomOut,
  Maximize,
  Copy,
  Share2,
  ImageOff,
} from "lucide-react";
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
import {
  ContextMenu,
  ContextMenuTrigger,
  ContextMenuContent,
  ContextMenuItem,
} from "@/components/ui/context-menu";
import { canCopyImage, copyImage } from "@/lib/imageClipboard";
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
 * full-size lightbox — an image shows large, a video autoplays with controls. Images
 * retain a quiet status frame until the current source can display. Bytes are fetched
 * only when under the relevant size cap. */
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
  const frameClass = `relative w-80 max-w-full overflow-hidden rounded-lg ${video ? "aspect-video bg-muted/40" : "aspect-[4/3] bg-card text-card-foreground ring-1 ring-inset ring-border"}`;
  const imageFrameClass =
    "relative block max-w-full overflow-hidden rounded-lg";
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
  const imageSource = useRef<{ fileConv: string; url: string | null } | null>(
    null,
  );
  useLayoutEffect(() => {
    imageSource.current = { fileConv, url };
    return () => {
      imageSource.current = null;
    };
  }, [fileConv, url]);
  const imageLease = captureChatOwnership();
  const currentImage = (image: HTMLImageElement) =>
    !!url &&
    imageSource.current?.fileConv === fileConv &&
    imageSource.current.url === url &&
    image.getAttribute("src") === url &&
    imageLease.current();
  const rememberImage = (event: SyntheticEvent<HTMLImageElement>) => {
    const { naturalWidth: width, naturalHeight: height } = event.currentTarget;
    if (!url || !currentImage(event.currentTarget) || !width || !height) return;
    if (imageGeometry.size >= 64 && !imageGeometry.has(url))
      imageGeometry.delete(imageGeometry.keys().next().value!);
    imageGeometry.set(url, { width, height });
    setDecoded({ url, width, height });
  };
  const [lightbox, setLightbox] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const imageRef = useRef<HTMLImageElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const viewRef = useRef({ scale: 1, x: 0, y: 0 });
  const [view, setView] = useState(viewRef.current);
  const drag = useRef<{ id: number; x: number; y: number } | null>(null);
  const [dragging, setDragging] = useState(false);
  const updateView = useCallback((scale: number, x = 0, y = 0) => {
    const stage = stageRef.current;
    const image = imageRef.current;
    scale = Math.max(1, Math.min(8, scale));
    const limitX = Math.max(
      0,
      ((image?.offsetWidth ?? 0) * scale - (stage?.clientWidth ?? 0)) / 2,
    );
    const limitY = Math.max(
      0,
      ((image?.offsetHeight ?? 0) * scale - (stage?.clientHeight ?? 0)) / 2,
    );
    const next = {
      scale,
      x: Math.max(-limitX, Math.min(limitX, x)),
      y: Math.max(-limitY, Math.min(limitY, y)),
    };
    viewRef.current = next;
    setView(next);
  }, []);
  const zoom = useCallback(
    (scale: number, clientX?: number, clientY?: number) => {
      const bounds = stageRef.current?.getBoundingClientRect();
      const current = viewRef.current;
      const next = Math.max(1, Math.min(8, scale));
      const ratio = next / current.scale;
      const x =
        bounds && clientX !== undefined
          ? clientX - bounds.left - bounds.width / 2
          : 0;
      const y =
        bounds && clientY !== undefined
          ? clientY - bounds.top - bounds.height / 2
          : 0;
      updateView(
        next,
        x - (x - current.x) * ratio,
        y - (y - current.y) * ratio,
      );
    },
    [updateView],
  );
  const [selectedText, setSelectedText] = useState("");
  const [shareFile, setShareFile] = useState<File | null>(null);
  const [copying, setCopying] = useState(false);
  const [saving, setSaving] = useState(false);
  const nativeImageText = isMacOverlay();
  const backdropPointerDown = useRef(false);
  const playerRef = useRef<HTMLVideoElement>(null);
  const closeLightbox = useCallback(() => {
    playerRef.current?.pause();
    setLightbox(false);
    triggerRef.current?.focus();
  }, []);
  useLayoutEffect(() => {
    if (!lightbox) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (document.querySelector('[data-image-actions][data-state="open"]'))
        return;
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
  useEffect(() => {
    if (!lightbox || video) return;
    drag.current = null;
    setDragging(false);
    updateView(1);
    closeRef.current?.focus();
    const stage = stageRef.current;
    if (!stage) return;
    const wheel = (event: WheelEvent) => {
      event.preventDefault();
      const delta =
        event.deltaY *
        (event.deltaMode === 1
          ? 16
          : event.deltaMode === 2
            ? stage.clientHeight
            : 1);
      zoom(
        viewRef.current.scale *
          Math.exp(-Math.max(-100, Math.min(100, delta)) * 0.002),
        event.clientX,
        event.clientY,
      );
    };
    const resize = new ResizeObserver(() => {
      const current = viewRef.current;
      updateView(current.scale, current.x, current.y);
    });
    resize.observe(stage);
    stage.addEventListener("wheel", wheel, { passive: false });
    return () => {
      resize.disconnect();
      stage.removeEventListener("wheel", wheel);
    };
  }, [lightbox, video, zoom, updateView]);
  useEffect(() => {
    setShareFile(null);
    if (!lightbox || video || !url || !navigator.canShare || !navigator.share)
      return;
    let active = true;
    const lease = captureChatOwnership();
    void fetch(url)
      .then((response) => response.blob())
      .then((blob) => {
        const file = new File([blob], displayName, { type: blob.type });
        if (active && lease.current() && navigator.canShare({ files: [file] }))
          setShareFile(file);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [lightbox, video, url, displayName]);
  const copyFullImage = async () => {
    const lease = captureChatOwnership();
    if (!imageRef.current || copying || !lease.current()) return;
    setCopying(true);
    try {
      await copyImage(
        imageRef.current,
        t("settings.confirmCopyPrompt"),
        lease.current,
      );
    } catch {
      if (lease.current()) setError(t("common.copyFailed"));
    } finally {
      setCopying(false);
    }
  };
  const shareImage = async () => {
    const lease = captureChatOwnership();
    if (!shareFile || !lease.current()) return;
    try {
      await navigator.share({ files: [shareFile] });
    } catch (error) {
      if (
        lease.current() &&
        !(error instanceof DOMException && error.name === "AbortError")
      )
        setError(t("files.shareFailed"));
    }
  };
  const layoutId =
    motionOK && (video || geometry) ? `media-${fileConv}` : undefined;
  const layoutTransition = { duration: 0.22, ease };
  const backdropTransition = { duration: motionOK ? 0.16 : 0.08, ease };
  // An inline image whose codec the webview can't decode (e.g. HEIC outside Safari) fires
  // <img onError>; we then degrade to the same "can't preview — Save" card as video.
  const [failedImageUrl, setFailedImageUrl] = useState<string | null>(null);
  const imgFailed = !!url && failedImageUrl === url;
  const failImage = (event: SyntheticEvent<HTMLImageElement>) => {
    if (!url || !currentImage(event.currentTarget)) return;
    imageGeometry.delete(url);
    setFailedImageUrl(url);
  };

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
      className={`${frameClass} flex items-center gap-3 ${video ? "border px-3 py-2" : "flex-col justify-center p-5 text-center"}`}
      data-testid={testid}
    >
      {!video && (
        <ImageOff
          className="h-6 w-6 text-muted-foreground"
          aria-hidden="true"
        />
      )}
      <p
        role={!video ? "status" : undefined}
        className={`min-w-0 text-xs text-muted-foreground ${video ? "flex-1" : "max-w-full [overflow-wrap:anywhere]"}`}
      >
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
  if (!video && (!available || !url))
    return (
      <div
        className={`${frameClass} flex items-center justify-center p-5`}
        title={displayName}
      >
        <ReceiveProgress
          status={status}
          presentation="image"
          size={size}
          preparing={available}
        />
      </div>
    );
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
                className="absolute right-4 top-4 flex h-11 w-11 items-center justify-center rounded-full bg-background/80 text-foreground hover:bg-background"
              >
                <X className="h-4 w-4" />
              </button>
              {/* Stop clicks on the player from closing the lightbox (so controls work). */}
              <motion.div
                layoutId={layoutId}
                transition={layoutTransition}
                className="pointer-events-none flex h-full w-full items-center justify-center"
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

  // An available Blob URL is not yet a displayable image. Keep the same local
  // surface until onLoad; cached geometry skips preparation on virtualized remounts.
  const imageOpacity: CSSProperties = {
    opacity: geometry ? 1 : 0,
  };
  const preparingImage = (
    <div
      aria-hidden={!!geometry}
      className="pointer-events-none absolute inset-0 flex items-center justify-center rounded-lg bg-card p-5 text-card-foreground ring-1 ring-inset ring-border"
      style={{
        // Reveal the fully painted image beneath one dissolving surface. Fading
        // both layers would let the wallpaper bleed through halfway through.
        opacity: geometry ? 0 : 1,
        transition: `opacity ${motionOK ? 180 : 80}ms cubic-bezier(0.22, 1, 0.36, 1)`,
      }}
    >
      {!geometry && (
        <ReceiveProgress
          status={status}
          presentation="image"
          size={size}
          preparing
        />
      )}
    </div>
  );

  // Read-only (history viewer): no click-to-enlarge, no play, no lightbox.
  if (readOnly) {
    return (
      <div
        className={geometry ? imageFrameClass : frameClass}
        style={imageStyle}
      >
        <img
          key={url}
          src={url}
          alt={displayName}
          data-testid="file-image"
          onLoad={rememberImage}
          onError={failImage}
          className="h-full w-full select-none object-contain"
          style={imageOpacity}
        />
        {preparingImage}
      </div>
    );
  }

  return (
    <>
      <div
        data-inline-image=""
        className={geometry ? imageFrameClass : frameClass}
        style={imageStyle}
      >
        <button
          type="button"
          ref={triggerRef}
          onClick={() => setLightbox(true)}
          title={t("files.viewFullSize")}
          aria-label={t("files.viewFullSize")}
          disabled={!geometry}
          className="block h-full w-full rounded-lg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
        >
          <motion.img
            key={url}
            src={url}
            alt={displayName}
            layoutId={layoutId}
            transition={layoutTransition}
            data-testid="file-image"
            onLoad={rememberImage}
            onError={failImage}
            className="h-full w-full select-none object-contain"
            style={imageOpacity}
          />
        </button>
        {preparingImage}
      </div>
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
            onKeyDown={(event) => {
              if (["+", "=", "-", "0"].includes(event.key)) {
                event.preventDefault();
                if (event.key === "0") updateView(1);
                else
                  zoom(
                    viewRef.current.scale * (event.key === "-" ? 0.8 : 1.25),
                  );
              }
              if (event.key === "Tab") {
                const buttons =
                  event.currentTarget.querySelectorAll<HTMLButtonElement>(
                    "button:not(:disabled)",
                  );
                const first = buttons[0],
                  last = buttons[buttons.length - 1];
                if (event.shiftKey && document.activeElement === first) {
                  event.preventDefault();
                  last?.focus();
                } else if (!event.shiftKey && document.activeElement === last) {
                  event.preventDefault();
                  first?.focus();
                }
              }
            }}
            className="fixed inset-0 z-50 flex items-center justify-center overflow-hidden bg-black/80 p-6"
          >
            <button
              ref={closeRef}
              type="button"
              aria-label={t("common.close")}
              onClick={closeLightbox}
              className="absolute right-4 top-4 flex h-11 w-11 items-center justify-center rounded-full bg-background/80 text-foreground hover:bg-background"
            >
              <X className="h-4 w-4" />
            </button>
            <div
              ref={stageRef}
              className="absolute inset-x-6 bottom-24 top-16 flex items-center justify-center overflow-hidden"
              style={{ touchAction: "none" }}
              onDoubleClick={(event) => {
                event.stopPropagation();
                zoom(
                  viewRef.current.scale > 1 ? 1 : 2,
                  event.clientX,
                  event.clientY,
                );
              }}
              onPointerDown={(event) => {
                // Portalled action-menu events also bubble through the React stage.
                if (!event.currentTarget.contains(event.target as Node)) return;
                backdropPointerDown.current =
                  event.target === event.currentTarget;
                if (
                  event.button !== 0 ||
                  viewRef.current.scale <= 1 ||
                  !event.isPrimary
                )
                  return;
                // Native text selection remains available with Option/Alt held.
                if (event.altKey) return;
                event.preventDefault();
                event.currentTarget.setPointerCapture(event.pointerId);
                drag.current = {
                  id: event.pointerId,
                  x: event.clientX,
                  y: event.clientY,
                };
                setDragging(true);
              }}
              onPointerMove={(event) => {
                const start = drag.current;
                if (!start || start.id !== event.pointerId) return;
                const current = viewRef.current;
                updateView(
                  current.scale,
                  current.x + event.clientX - start.x,
                  current.y + event.clientY - start.y,
                );
                drag.current = {
                  id: start.id,
                  x: event.clientX,
                  y: event.clientY,
                };
              }}
              onPointerUp={(event) => {
                drag.current = null;
                setDragging(false);
                if (event.currentTarget.hasPointerCapture(event.pointerId))
                  event.currentTarget.releasePointerCapture(event.pointerId);
              }}
              onPointerCancel={() => {
                drag.current = null;
                setDragging(false);
              }}
              onLostPointerCapture={() => {
                drag.current = null;
                setDragging(false);
              }}
              onClick={(event) => {
                if (
                  event.target !== event.currentTarget ||
                  viewRef.current.scale > 1
                )
                  event.stopPropagation();
                else if (backdropPointerDown.current) closeLightbox();
              }}
            >
              <ContextMenu
                onOpenChange={(open) => {
                  if (open)
                    setSelectedText(window.getSelection()?.toString() ?? "");
                }}
              >
                <ContextMenuTrigger
                  asChild
                  onContextMenu={(event) => event.stopPropagation()}
                >
                  <div
                    data-testid="image-transform"
                    className="pointer-events-none flex h-full w-full items-center justify-center"
                    style={{
                      transform: `translate(${view.x}px, ${view.y}px) scale(${view.scale})`,
                      cursor:
                        view.scale > 1
                          ? dragging
                            ? "grabbing"
                            : "grab"
                          : "zoom-in",
                    }}
                  >
                    <motion.img
                      ref={imageRef}
                      draggable={false}
                      onLoad={() => updateView(1)}
                      src={url}
                      alt={displayName}
                      layoutId={layoutId}
                      transition={layoutTransition}
                      onClick={(e) => e.stopPropagation()}
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
                                  event.clipboardData.setData(
                                    "text/plain",
                                    value,
                                  ),
                              ).catch(() => {
                                if (lease.current())
                                  setError(t("common.copyFailed"));
                              });
                            }
                          : undefined
                      }
                      className={`pointer-events-auto max-h-full max-w-full rounded-lg object-contain${nativeImageText ? " select-text" : ""}`}
                    />
                  </div>
                </ContextMenuTrigger>
                <ContextMenuContent
                  data-image-actions=""
                  className="z-[60]"
                  onCloseAutoFocus={(event) => {
                    event.preventDefault();
                    closeRef.current?.focus();
                  }}
                  onClick={(event) => event.stopPropagation()}
                >
                  {canCopyImage() && (
                    <ContextMenuItem
                      disabled={copying}
                      onSelect={() => void copyFullImage()}
                    >
                      <Copy className="h-4 w-4" />
                      {t("files.copyImage")}
                    </ContextMenuItem>
                  )}
                  {nativeImageText && selectedText && (
                    <ContextMenuItem
                      onSelect={() => {
                        const lease = captureChatOwnership();
                        const text = selectedText;
                        if (text && lease.current())
                          void copySensitiveText(
                            text,
                            t("settings.confirmCopyPrompt"),
                          ).catch(() => {
                            if (lease.current())
                              setError(t("common.copyFailed"));
                          });
                      }}
                    >
                      <Copy className="h-4 w-4" />
                      {t("files.copyText")}
                    </ContextMenuItem>
                  )}
                  <ContextMenuItem
                    disabled={saving}
                    onSelect={() => void saveAs()}
                  >
                    <Download className="h-4 w-4" />
                    {t("files.saveAs")}
                  </ContextMenuItem>
                  {savedPath && (
                    <ContextMenuItem onSelect={() => void reveal()}>
                      <FolderOpen className="h-4 w-4" />
                      {t("files.reveal")}
                    </ContextMenuItem>
                  )}
                  {shareFile && (
                    <ContextMenuItem onSelect={() => void shareImage()}>
                      <Share2 className="h-4 w-4" />
                      {t("files.shareImage")}
                    </ContextMenuItem>
                  )}
                </ContextMenuContent>
              </ContextMenu>
            </div>
            <div
              role="group"
              aria-label={t("files.imageZoom")}
              className="pointer-events-none absolute inset-x-0 top-3 flex justify-center gap-1 text-white"
              onClick={(event) => event.stopPropagation()}
            >
              {(
                [
                  [
                    "zoomOut",
                    ZoomOut,
                    () => zoom(viewRef.current.scale / 1.25),
                    view.scale <= 1,
                  ],
                  ["fitToWindow", Maximize, () => updateView(1), false],
                  [
                    "zoomIn",
                    ZoomIn,
                    () => zoom(viewRef.current.scale * 1.25),
                    view.scale >= 8,
                  ],
                ] as const
              ).map(([label, Icon, action, disabled]) => (
                <button
                  key={label}
                  type="button"
                  aria-label={t(`files.${label}`)}
                  title={t(`files.${label}`)}
                  disabled={disabled}
                  onClick={action}
                  className="pointer-events-auto flex h-11 w-11 items-center justify-center rounded-md bg-black/60 hover:bg-white/20 disabled:opacity-40 focus-visible:outline-2 focus-visible:outline-white"
                >
                  <Icon className="h-4 w-4" />
                </button>
              ))}
              <span
                className="flex min-w-14 items-center justify-center rounded-md bg-black/60 text-xs tabular-nums"
                aria-live="polite"
              >
                {Math.round(view.scale * 100)}%
              </span>
            </div>
            {detailBar}
          </motion.div>
        )}
      </AnimatePresence>
    </>
  );
}
