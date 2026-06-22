import { useCallback, useEffect, useRef, useState } from "react";
import { ZoomIn, ZoomOut, Loader2 } from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { extractAvatar, loadImage, type CropRegion } from "@/lib/avatarImage";

/**
 * AvatarCropDialog — interactive crop/preview step modeled on WeChat/Telegram/Slack avatar
 * upload. After the caller picks an image File, this dialog opens with a fixed square
 * viewport: the user drags to reposition and zooms (slider + wheel/pinch), with a circular
 * mask guide (avatars render rounded) and a live circular preview. Confirm draws the visible
 * square to a 256×256 canvas → JPEG data-URL via `extractAvatar`; Cancel/Esc aborts.
 *
 * Custom implementation (pointer drag + zoom math + canvas extraction) — no cropper library
 * — to keep the dependency surface lean (audit-ci / Scorecard).
 */

/** Edge of the square crop viewport, in CSS px. */
const VIEWPORT = 280;
/** How far past cover-fit the user may zoom in. */
const MAX_ZOOM = 4;

interface Vec {
  x: number;
  y: number;
}

export function AvatarCropDialog({
  file,
  onConfirm,
  onCancel,
}: {
  /** The picked image; when null the dialog is closed. */
  file: File | null;
  /** Called with the cropped 256×256 JPEG data-URL on Confirm. */
  onConfirm: (dataUrl: string) => void;
  /** Called when the user cancels (button, Esc, or backdrop). */
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  const [img, setImg] = useState<HTMLImageElement | null>(null);
  // A LIVE object URL for the viewport <img>. We can't reuse `img.src` because loadImage
  // (used below for the decoded bitmap → cover-fit math + preview canvas) revokes its URL
  // once decoded — leaving `img.src` dead, so the viewport would render blank. Keep our own
  // URL alive for as long as the dialog is open, revoked on cleanup.
  const [srcUrl, setSrcUrl] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Cover-fit scale: the smallest scale that fully covers the viewport at zoom = 1.
  const baseScaleRef = useRef(1);
  // User zoom multiplier on top of the base cover-fit scale.
  const [zoom, setZoom] = useState(1);
  // Top-left offset (CSS px) of the scaled image relative to the viewport's top-left.
  const [offset, setOffset] = useState<Vec>({ x: 0, y: 0 });
  // Live circular preview, redrawn whenever the crop changes.
  const previewRef = useRef<HTMLCanvasElement | null>(null);

  // Decode the picked file and center it cover-fit.
  useEffect(() => {
    if (!file) return;
    let cancelled = false;
    setImg(null);
    setZoom(1);
    setOffset({ x: 0, y: 0 });
    void loadImage(file).then((loaded) => {
      if (cancelled) return;
      const base = Math.max(
        VIEWPORT / loaded.naturalWidth,
        VIEWPORT / loaded.naturalHeight,
      );
      baseScaleRef.current = base;
      // Center the cover-fit image inside the viewport.
      const w = loaded.naturalWidth * base;
      const h = loaded.naturalHeight * base;
      setOffset({ x: (VIEWPORT - w) / 2, y: (VIEWPORT - h) / 2 });
      setImg(loaded);
    });
    return () => {
      cancelled = true;
    };
  }, [file]);

  // Hold a live object URL for the viewport <img> (revoked on file change / unmount).
  useEffect(() => {
    if (!file) {
      setSrcUrl(null);
      return;
    }
    const url = URL.createObjectURL(file);
    setSrcUrl(url);
    return () => URL.revokeObjectURL(url);
  }, [file]);

  // Clamp an offset so the scaled image always fully covers the viewport (no gaps).
  const clamp = useCallback(
    (next: Vec, z: number): Vec => {
      if (!img) return next;
      const scale = baseScaleRef.current * z;
      const w = img.naturalWidth * scale;
      const h = img.naturalHeight * scale;
      // Offset range: image edge must not cross the viewport edge inward.
      const minX = VIEWPORT - w;
      const minY = VIEWPORT - h;
      return {
        x: Math.min(0, Math.max(minX, next.x)),
        y: Math.min(0, Math.max(minY, next.y)),
      };
    },
    [img],
  );

  // Apply a zoom change while keeping the viewport center anchored.
  const applyZoom = useCallback(
    (nextZoom: number) => {
      const z = Math.min(MAX_ZOOM, Math.max(1, nextZoom));
      setZoom((prevZoom) => {
        setOffset((prevOffset) => {
          const center = VIEWPORT / 2;
          const ratio = z / prevZoom;
          // Scale the offset about the viewport center so the focal point stays put.
          const scaled = {
            x: center - (center - prevOffset.x) * ratio,
            y: center - (center - prevOffset.y) * ratio,
          };
          return clamp(scaled, z);
        });
        return z;
      });
    },
    [clamp],
  );

  // --- Pointer drag (mouse + touch via Pointer Events) ----------------------
  const dragRef = useRef<{ start: Vec; origin: Vec } | null>(null);
  const onPointerDown = (e: React.PointerEvent) => {
    if (!img) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    dragRef.current = {
      start: { x: e.clientX, y: e.clientY },
      origin: offset,
    };
  };
  const onPointerMove = (e: React.PointerEvent) => {
    const d = dragRef.current;
    if (!d) return;
    const next = {
      x: d.origin.x + (e.clientX - d.start.x),
      y: d.origin.y + (e.clientY - d.start.y),
    };
    setOffset(clamp(next, zoom));
  };
  const endDrag = (e: React.PointerEvent) => {
    if (dragRef.current && e.currentTarget.hasPointerCapture(e.pointerId)) {
      e.currentTarget.releasePointerCapture(e.pointerId);
    }
    dragRef.current = null;
  };

  // Wheel / trackpad pinch → zoom.
  const onWheel = (e: React.WheelEvent) => {
    if (!img) return;
    applyZoom(zoom - e.deltaY * 0.002);
  };

  // Redraw the live circular preview on every crop change.
  useEffect(() => {
    const canvas = previewRef.current;
    if (!canvas || !img) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const r = canvas.width;
    ctx.clearRect(0, 0, r, r);
    ctx.save();
    ctx.beginPath();
    ctx.arc(r / 2, r / 2, r / 2, 0, Math.PI * 2);
    ctx.clip();
    const scale = baseScaleRef.current * zoom;
    // Map the viewport-space placement onto the (smaller) preview canvas.
    const k = r / VIEWPORT;
    ctx.drawImage(
      img,
      offset.x * k,
      offset.y * k,
      img.naturalWidth * scale * k,
      img.naturalHeight * scale * k,
    );
    ctx.restore();
  }, [img, zoom, offset]);

  const confirm = () => {
    if (!img) return;
    setBusy(true);
    try {
      const scale = baseScaleRef.current * zoom;
      // Map the viewport square back into source pixels.
      const region: CropRegion = {
        x: -offset.x / scale,
        y: -offset.y / scale,
        size: VIEWPORT / scale,
      };
      onConfirm(extractAvatar(img, region));
    } finally {
      setBusy(false);
    }
  };

  const scale = baseScaleRef.current * zoom;

  return (
    <Dialog
      open={file != null}
      onOpenChange={(o) => {
        if (!o) onCancel();
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("avatar.crop.title")}</DialogTitle>
          <DialogDescription>{t("avatar.crop.description")}</DialogDescription>
        </DialogHeader>

        <div className="flex flex-col items-center gap-4">
          {/* Crop viewport */}
          <div
            className="relative touch-none select-none overflow-hidden rounded-xl bg-muted"
            style={{ width: VIEWPORT, height: VIEWPORT }}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={endDrag}
            onPointerCancel={endDrag}
            onWheel={onWheel}
            role="application"
            aria-label={t("avatar.crop.viewportLabel")}
          >
            {img ? (
              <img
                src={srcUrl ?? ""}
                alt=""
                draggable={false}
                aria-hidden
                className="pointer-events-none absolute max-w-none origin-top-left"
                style={{
                  left: 0,
                  top: 0,
                  width: img.naturalWidth * scale,
                  height: img.naturalHeight * scale,
                  transform: `translate(${offset.x}px, ${offset.y}px)`,
                }}
              />
            ) : (
              <div className="flex h-full w-full items-center justify-center">
                <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
              </div>
            )}
            {/* Circular mask guide: dim everything outside the inscribed circle. */}
            <div
              aria-hidden
              className="pointer-events-none absolute inset-0"
              style={{
                background: "rgba(0,0,0,0.5)",
                WebkitMaskImage:
                  "radial-gradient(circle at center, transparent 0, transparent calc(50% - 1px), black 50%)",
                maskImage:
                  "radial-gradient(circle at center, transparent 0, transparent calc(50% - 1px), black 50%)",
              }}
            />
            <div
              aria-hidden
              className="pointer-events-none absolute inset-0 rounded-full ring-1 ring-white/60"
            />
          </div>

          {/* Zoom control + live preview */}
          <div className="flex w-full items-center gap-3">
            <ZoomOut
              className="h-4 w-4 shrink-0 text-muted-foreground"
              aria-hidden
            />
            <input
              type="range"
              min={1}
              max={MAX_ZOOM}
              step={0.01}
              value={zoom}
              disabled={!img}
              onChange={(e) => applyZoom(Number(e.target.value))}
              aria-label={t("avatar.crop.zoomLabel")}
              className="h-1.5 flex-1 cursor-pointer appearance-none rounded-full bg-muted accent-signal"
            />
            <ZoomIn
              className="h-4 w-4 shrink-0 text-muted-foreground"
              aria-hidden
            />
            <canvas
              ref={previewRef}
              width={56}
              height={56}
              aria-label={t("avatar.crop.previewLabel")}
              className="h-14 w-14 shrink-0 rounded-full border bg-muted"
            />
          </div>
        </div>

        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onCancel}>
            {t("common.cancel")}
          </Button>
          <Button disabled={!img || busy} onClick={confirm}>
            {busy && <Loader2 className="h-4 w-4 animate-spin" />}
            {t("avatar.crop.confirm")}
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
