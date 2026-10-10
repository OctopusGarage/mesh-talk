import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArrowUpRight,
  Check,
  Eraser,
  Pencil,
  RotateCcw,
  Square,
  Type,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isMacOverlay } from "@/lib/platform";

type Point = { x: number; y: number };
type Region = Point & { width: number; height: number };
type Mark =
  | { kind: "pen"; points: Point[] }
  | { kind: "rect" | "arrow"; from: Point; to: Point }
  | { kind: "text"; at: Point; value: string };
type Tool = "select" | "pen" | "rect" | "arrow" | "text";

const INK = "#f43f5e";
const lineWidth = 3;

function regionBetween(a: Point, b: Point): Region {
  return {
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    width: Math.abs(a.x - b.x),
    height: Math.abs(a.y - b.y),
  };
}

function paintMark(ctx: CanvasRenderingContext2D, mark: Mark) {
  ctx.strokeStyle = INK;
  ctx.fillStyle = INK;
  ctx.lineWidth = lineWidth;
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  if (mark.kind === "text") {
    ctx.font = "bold 24px sans-serif";
    ctx.fillText(mark.value, mark.at.x, mark.at.y);
  } else if (mark.kind === "pen") {
    if (!mark.points.length) return;
    ctx.beginPath();
    ctx.moveTo(mark.points[0].x, mark.points[0].y);
    for (const point of mark.points.slice(1)) ctx.lineTo(point.x, point.y);
    ctx.stroke();
  } else if (mark.kind === "rect") {
    const rect = regionBetween(mark.from, mark.to);
    ctx.strokeRect(rect.x, rect.y, rect.width, rect.height);
  } else {
    const angle = Math.atan2(mark.to.y - mark.from.y, mark.to.x - mark.from.x);
    ctx.beginPath();
    ctx.moveTo(mark.from.x, mark.from.y);
    ctx.lineTo(mark.to.x, mark.to.y);
    ctx.moveTo(mark.to.x, mark.to.y);
    ctx.lineTo(
      mark.to.x - 14 * Math.cos(angle - Math.PI / 6),
      mark.to.y - 14 * Math.sin(angle - Math.PI / 6),
    );
    ctx.moveTo(mark.to.x, mark.to.y);
    ctx.lineTo(
      mark.to.x - 14 * Math.cos(angle + Math.PI / 6),
      mark.to.y - 14 * Math.sin(angle + Math.PI / 6),
    );
    ctx.stroke();
  }
}

export function ScreenshotEditor({
  bytes,
  initialRegion = false,
  onCancel,
  onSend,
}: {
  bytes: Uint8Array;
  initialRegion?: boolean;
  onCancel: () => void;
  onSend: (bytes: Uint8Array) => Promise<void>;
}) {
  const { t } = useTranslation();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const imageRef = useRef<HTMLImageElement | null>(null);
  const dragRef = useRef<{ start: Point; points: Point[] } | null>(null);
  const [ready, setReady] = useState(false);
  const [region, setRegion] = useState<Region | null>(null);
  const [marks, setMarks] = useState<Mark[]>([]);
  const [preview, setPreview] = useState<Mark | null>(null);
  const [tool, setTool] = useState<Tool>("select");
  const [label, setLabel] = useState("");
  const [sending, setSending] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const invalidImageMessage = useRef(t("screenshot.invalidImage"));
  invalidImageMessage.current = t("screenshot.invalidImage");

  useEffect(() => {
    dialogRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
    // The macOS native selector already spans the desktop. On Windows and Linux, show
    // the frozen screenshot across the display while the user selects a region.
    if (!("__TAURI_INTERNALS__" in window) || isMacOverlay()) return;
    const nativeWindow = getCurrentWindow();
    let active = true;
    let expanded = false;
    void nativeWindow
      .isFullscreen()
      .then(async (wasFullscreen) => {
        if (!active || wasFullscreen) return;
        await nativeWindow.setFullscreen(true);
        expanded = true;
        if (!active) await nativeWindow.setFullscreen(false);
      })
      .catch(() => {});
    return () => {
      active = false;
      if (expanded) void nativeWindow.setFullscreen(false).catch(() => {});
    };
  }, []);

  useEffect(() => {
    let active = true;
    const url = URL.createObjectURL(
      new Blob([new Uint8Array(bytes)], { type: "image/png" }),
    );
    const image = new Image();
    image.onload = () => {
      if (!active) return;
      imageRef.current = image;
      const canvas = canvasRef.current;
      if (canvas) {
        canvas.width = image.naturalWidth;
        canvas.height = image.naturalHeight;
      }
      if (initialRegion) {
        setRegion({
          x: 0,
          y: 0,
          width: image.naturalWidth,
          height: image.naturalHeight,
        });
        setTool("pen");
      }
      setReady(true);
    };
    image.onerror = () => {
      if (active) setFailure(invalidImageMessage.current);
    };
    image.src = url;
    return () => {
      active = false;
      image.onload = null;
      image.onerror = null;
      URL.revokeObjectURL(url);
    };
  }, [bytes, initialRegion]);

  useEffect(() => {
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !sending) onCancel();
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [onCancel, sending]);

  useEffect(() => {
    const canvas = canvasRef.current;
    const image = imageRef.current;
    if (!canvas || !image || !ready) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(image, 0, 0);
    if (region) {
      ctx.fillStyle = "rgba(9, 14, 24, 0.52)";
      ctx.fillRect(0, 0, canvas.width, region.y);
      ctx.fillRect(0, region.y, region.x, region.height);
      ctx.fillRect(
        region.x + region.width,
        region.y,
        canvas.width - region.x - region.width,
        region.height,
      );
      ctx.fillRect(
        0,
        region.y + region.height,
        canvas.width,
        canvas.height - region.y - region.height,
      );
      ctx.strokeStyle = "#ffffff";
      ctx.lineWidth = 2;
      ctx.setLineDash([8, 5]);
      ctx.strokeRect(region.x, region.y, region.width, region.height);
      ctx.setLineDash([]);
      ctx.save();
      ctx.beginPath();
      ctx.rect(region.x, region.y, region.width, region.height);
      ctx.clip();
      marks.forEach((mark) => paintMark(ctx, mark));
      if (preview) paintMark(ctx, preview);
      ctx.restore();
    }
  }, [ready, region, marks, preview]);

  const point = useCallback(
    (event: React.PointerEvent<HTMLCanvasElement>): Point => {
      const canvas = event.currentTarget;
      const rect = canvas.getBoundingClientRect();
      return {
        x: Math.max(
          0,
          Math.min(
            canvas.width,
            Math.round(
              ((event.clientX - rect.left) * canvas.width) / rect.width,
            ),
          ),
        ),
        y: Math.max(
          0,
          Math.min(
            canvas.height,
            Math.round(
              ((event.clientY - rect.top) * canvas.height) / rect.height,
            ),
          ),
        ),
      };
    },
    [],
  );

  const within = (p: Point) =>
    region &&
    p.x >= region.x &&
    p.y >= region.y &&
    p.x <= region.x + region.width &&
    p.y <= region.y + region.height;

  const pointerDown = (event: React.PointerEvent<HTMLCanvasElement>) => {
    if (!ready || sending) return;
    const p = point(event);
    if (tool !== "select" && !within(p)) return;
    if (tool === "text") {
      if (label.trim())
        setMarks((items) => [
          ...items,
          { kind: "text", at: p, value: label.trim() },
        ]);
      return;
    }
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = { start: p, points: [p] };
    if (tool === "select") {
      setMarks([]);
      setRegion({ ...p, width: 0, height: 0 });
    }
  };

  const pointerMove = (event: React.PointerEvent<HTMLCanvasElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const p = point(event);
    if (tool === "select") setRegion(regionBetween(drag.start, p));
    else if (tool === "pen") {
      drag.points.push(p);
      setPreview({ kind: "pen", points: [...drag.points] });
    } else
      setPreview({ kind: tool as "rect" | "arrow", from: drag.start, to: p });
  };

  const pointerUp = (event: React.PointerEvent<HTMLCanvasElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    dragRef.current = null;
    const p = point(event);
    if (tool === "select") {
      const next = regionBetween(drag.start, p);
      setRegion(next.width >= 4 && next.height >= 4 ? next : null);
      if (next.width >= 4 && next.height >= 4) setTool("pen");
    } else {
      const mark: Mark =
        tool === "pen"
          ? { kind: "pen", points: [...drag.points, p] }
          : { kind: tool as "rect" | "arrow", from: drag.start, to: p };
      setMarks((items) => [...items, mark]);
      setPreview(null);
    }
  };

  const send = async () => {
    const image = imageRef.current;
    if (!image || !region || sending) return;
    setSending(true);
    try {
      const output = document.createElement("canvas");
      output.width = region.width;
      output.height = region.height;
      const ctx = output.getContext("2d");
      if (!ctx) throw new Error("Canvas unavailable");
      ctx.drawImage(
        image,
        region.x,
        region.y,
        region.width,
        region.height,
        0,
        0,
        region.width,
        region.height,
      );
      ctx.translate(-region.x, -region.y);
      marks.forEach((mark) => paintMark(ctx, mark));
      const blob = await new Promise<Blob>((resolve, reject) =>
        output.toBlob(
          (value) =>
            value ? resolve(value) : reject(new Error("PNG encoding failed")),
          "image/png",
        ),
      );
      await onSend(new Uint8Array(await blob.arrayBuffer()));
    } catch (error) {
      setFailure(error instanceof Error ? error.message : String(error));
    } finally {
      setSending(false);
    }
  };

  return (
    <div
      ref={dialogRef}
      data-testid="screenshot-editor"
      role="dialog"
      aria-modal="true"
      aria-label={t("screenshot.editorTitle")}
      className="fixed inset-0 z-[100] flex flex-col bg-slate-950 text-white"
    >
      <header className="flex items-center justify-between gap-4 border-b border-white/15 px-5 py-3">
        <div>
          <h2 className="font-semibold">{t("screenshot.editorTitle")}</h2>
          <p className="text-xs text-slate-300">
            {t(
              initialRegion
                ? "screenshot.editorHintSelected"
                : "screenshot.editorHint",
            )}
          </p>
        </div>
        <button
          type="button"
          onClick={onCancel}
          disabled={sending}
          aria-label={t("screenshot.cancel")}
          className="flex h-11 w-11 items-center justify-center rounded-md hover:bg-white/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/70"
        >
          <X size={20} />
        </button>
      </header>
      <div className="flex min-h-0 flex-1 items-center justify-center overflow-hidden p-5">
        <canvas
          ref={canvasRef}
          data-testid="screenshot-canvas"
          onPointerDown={pointerDown}
          onPointerMove={pointerMove}
          onPointerUp={pointerUp}
          onPointerCancel={() => {
            dragRef.current = null;
            setPreview(null);
          }}
          className="max-h-full max-w-full cursor-crosshair touch-none shadow-2xl"
          aria-label={t("screenshot.canvasLabel")}
        />
      </div>
      <footer className="flex flex-wrap items-center justify-center gap-2 border-t border-white/15 px-4 py-3">
        {failure && (
          <p role="alert" className="w-full text-center text-sm text-red-300">
            {failure}
          </p>
        )}
        {(
          [
            ["select", <Eraser size={17} />, "selectRegion"],
            ["pen", <Pencil size={17} />, "pen"],
            ["rect", <Square size={17} />, "rectangle"],
            ["arrow", <ArrowUpRight size={17} />, "arrow"],
            ["text", <Type size={17} />, "text"],
          ] as const
        ).map(([value, icon, key]) => (
          <button
            key={value}
            type="button"
            title={t(`screenshot.${key}`)}
            aria-label={t(`screenshot.${key}`)}
            aria-pressed={tool === value}
            disabled={value !== "select" && !region}
            onClick={() => setTool(value)}
            className={`flex h-11 w-11 items-center justify-center rounded-md focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/70 disabled:opacity-40 ${tool === value ? "bg-white text-slate-950" : "hover:bg-white/15"}`}
          >
            {icon}
          </button>
        ))}
        {tool === "text" && (
          <input
            value={label}
            onChange={(event) => setLabel(event.target.value)}
            maxLength={80}
            aria-label={t("screenshot.textValue")}
            placeholder={t("screenshot.textValue")}
            className="w-36 rounded-md border border-white/25 bg-slate-900 px-2 py-1 text-sm"
          />
        )}
        <button
          type="button"
          title={t("screenshot.undo")}
          aria-label={t("screenshot.undo")}
          disabled={!marks.length}
          onClick={() => setMarks((items) => items.slice(0, -1))}
          className="flex h-11 w-11 items-center justify-center rounded-md hover:bg-white/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/70 disabled:opacity-40"
        >
          <RotateCcw size={17} />
        </button>
        <span className="mx-2 h-5 w-px bg-white/20" />
        <button
          type="button"
          onClick={onCancel}
          disabled={sending}
          className="min-h-11 rounded-md px-3 py-2 text-sm hover:bg-white/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/70"
        >
          {t("screenshot.cancel")}
        </button>
        <button
          type="button"
          data-testid="screenshot-send"
          disabled={!region || sending}
          onClick={() => void send()}
          className="flex min-h-11 items-center gap-2 rounded-md bg-blue-500 px-4 py-2 text-sm font-semibold text-white hover:bg-blue-400 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/70 disabled:cursor-not-allowed disabled:bg-slate-800 disabled:text-slate-300"
        >
          <Check size={16} />
          {t("screenshot.send")}
        </button>
      </footer>
    </div>
  );
}
