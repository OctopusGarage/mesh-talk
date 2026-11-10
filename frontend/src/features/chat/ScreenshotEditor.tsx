import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { isLinuxDesktop } from "@/lib/platform";
import { editorToolForKey } from "@/lib/screenshotShortcuts";
import { expandCaptureWindow } from "./screenshot/captureWindow";
import { ScreenshotToolbar } from "./screenshot/ScreenshotToolbar";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  adjustRegion,
  handlePoint,
  handles,
  hitHandle,
  regionBetween,
  toolbarPosition,
  type Adjustment,
  type Point,
  type Region,
} from "./screenshot/geometry";
import {
  colors,
  paintMark,
  type Ink,
  type Mark,
  type Tool,
} from "./screenshot/annotations";

type Drag = {
  start: Point;
  points: Point[];
  region?: Region;
  adjustment?: Adjustment;
};
/** Editing stays in image pixels; confirmation hands the PNG back to the composer. */
export function ScreenshotEditor({
  bytes,
  onCancel,
  onSend,
}: {
  bytes: Uint8Array;
  onCancel: () => void;
  onSend: (bytes: Uint8Array) => Promise<void>;
}) {
  const { t } = useTranslation();
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const toolbarRef = useRef<HTMLDivElement>(null);
  const imageRef = useRef<HTMLImageElement | null>(null);
  const dragRef = useRef<Drag | null>(null);
  const busy = useRef(false);
  const [ready, setReady] = useState(false);
  const [region, setRegion] = useState<Region | null>(null);
  const [marks, setMarks] = useState<Mark[]>([]);
  const [preview, setPreview] = useState<Mark | null>(null);
  const [tool, setTool] = useState<Tool>("select");
  const [ink, setInk] = useState<Ink>({ color: colors[0], width: 3 });
  const [label, setLabel] = useState("");
  const [sending, setSending] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [layout, setLayout] = useState({
    left: 8,
    top: 8,
    canvasX: 0,
    canvasY: 0,
    scaleX: 1,
    scaleY: 1,
  });
  const invalidImageMessage = useRef(t("screenshot.invalidImage"));
  invalidImageMessage.current = t("screenshot.invalidImage");

  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const restoreFocus = () => {
      if (previous?.isConnected && previous !== document.body) previous.focus();
      else
        document
          .querySelector<HTMLTextAreaElement>(
            '[data-testid="composer"] textarea',
          )
          ?.focus();
    };
    dialogRef.current?.focus();
    // Portal images have no display identity; Linux edits the chosen image in-app.
    if (!("__TAURI_INTERNALS__" in window) || isLinuxDesktop())
      return restoreFocus;
    const restore = expandCaptureWindow(getCurrentWindow());
    return () => {
      restore();
      restoreFocus();
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
  }, [bytes]);

  useLayoutEffect(() => {
    const measure = () => {
      const canvas = canvasRef.current;
      const toolbar = toolbarRef.current;
      if (!canvas || !toolbar) return;
      const rect = canvas.getBoundingClientRect();
      const sx = rect.width / canvas.width;
      const sy = rect.height / canvas.height;
      const selected = region
        ? {
            x: rect.left + region.x * sx,
            y: rect.top + region.y * sy,
            width: region.width * sx,
            height: region.height * sy,
          }
        : {
            x: 8,
            y: window.innerHeight - 170,
            width: window.innerWidth - 16,
            height: 0,
          };
      const position = toolbarPosition(
        selected,
        { width: window.innerWidth, height: window.innerHeight },
        { width: toolbar.offsetWidth, height: toolbar.offsetHeight },
      );
      setLayout({
        ...position,
        canvasX: rect.left,
        canvasY: rect.top,
        scaleX: sx,
        scaleY: sy,
      });
    };
    measure();
    const observer = new ResizeObserver(measure);
    if (canvasRef.current) observer.observe(canvasRef.current);
    if (toolbarRef.current) observer.observe(toolbarRef.current);
    window.addEventListener("resize", measure);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [ready, region, tool]);

  useEffect(() => {
    const canvas = canvasRef.current;
    const image = imageRef.current;
    if (!canvas || !image || !ready) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(image, 0, 0);
    ctx.save();
    ctx.fillStyle = "rgba(0,0,0,0.45)";
    if (!region) ctx.fillRect(0, 0, canvas.width, canvas.height);
    else {
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
      ctx.beginPath();
      ctx.rect(region.x, region.y, region.width, region.height);
      ctx.clip();
      marks.forEach((mark) => paintMark(ctx, mark, image));
      if (preview) paintMark(ctx, preview, image);
    }
    ctx.restore();
  }, [ready, region, marks, preview]);

  const point = useCallback(
    (event: React.MouseEvent<HTMLCanvasElement>): Point => {
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
  const imageInk = () => {
    const canvas = canvasRef.current;
    return {
      ...ink,
      scale: canvas ? canvas.width / canvas.getBoundingClientRect().width : 1,
    };
  };
  const markBetween = (drag: Drag, p: Point): Mark =>
    tool === "pen"
      ? { ...imageInk(), kind: "pen", points: [...drag.points, p] }
      : {
          ...imageInk(),
          kind: tool as "rect" | "ellipse" | "arrow" | "mosaic",
          from: drag.start,
          to: p,
        };
  const adjustmentAt = (p: Point): Adjustment | null => {
    const canvas = canvasRef.current;
    const tolerance = canvas
      ? (6 * canvas.width) / canvas.getBoundingClientRect().width
      : 6;
    return region
      ? (hitHandle(region, p, tolerance) ??
          (tool === "select" && within(p) ? "move" : null))
      : null;
  };
  const pointerDown = (event: React.PointerEvent<HTMLCanvasElement>) => {
    if (!ready || busy.current || event.button !== 0) return;
    const p = point(event);
    const adjustment = adjustmentAt(p);
    if (adjustment && region) {
      event.currentTarget.setPointerCapture(event.pointerId);
      dragRef.current = { start: p, points: [], region, adjustment };
      return;
    }
    if (tool !== "select" && !within(p)) return;
    if (tool === "text") {
      if (label.trim())
        setMarks((items) => [
          ...items,
          { ...imageInk(), kind: "text", at: p, value: label.trim() },
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
    const p = point(event);
    if (!drag) {
      const adjustment = adjustmentAt(p);
      event.currentTarget.style.cursor = adjustment
        ? adjustment === "move"
          ? "move"
          : `${adjustment}-resize`
        : "crosshair";
      return;
    }
    if (drag.region && drag.adjustment)
      setRegion(
        adjustRegion(
          drag.region,
          drag.adjustment,
          { x: p.x - drag.start.x, y: p.y - drag.start.y },
          event.currentTarget,
        ),
      );
    else if (tool === "select") setRegion(regionBetween(drag.start, p));
    else {
      if (tool === "pen") drag.points.push(p);
      setPreview(markBetween(drag, p));
    }
  };
  const pointerUp = (event: React.PointerEvent<HTMLCanvasElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const p = point(event);
    dragRef.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
    if (drag.region && drag.adjustment)
      setRegion(
        adjustRegion(
          drag.region,
          drag.adjustment,
          { x: p.x - drag.start.x, y: p.y - drag.start.y },
          event.currentTarget,
        ),
      );
    else if (tool === "select") {
      const next = regionBetween(drag.start, p);
      setRegion(next.width >= 4 && next.height >= 4 ? next : null);
    } else setMarks((items) => [...items, markBetween(drag, p)]);
    setPreview(null);
  };
  const confirm = useCallback(async () => {
    const image = imageRef.current;
    if (
      !image ||
      !region ||
      region.width < 4 ||
      region.height < 4 ||
      busy.current ||
      dragRef.current
    )
      return;
    busy.current = true;
    setSending(true);
    setFailure(null);
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
      marks.forEach((mark) => paintMark(ctx, mark, image));
      const png = new Promise<Blob>((resolve, reject) =>
        output.toBlob(
          (value) =>
            value ? resolve(value) : reject(new Error("PNG encoding failed")),
          "image/png",
        ),
      );
      // Start the clipboard write during the confirmation gesture. WebKit needs
      // user activation even though PNG encoding completes asynchronously.
      const copied = navigator.clipboard.write([
        new ClipboardItem({ "image/png": png }),
      ]);
      const [blob] = await Promise.all([png, copied]);
      await onSend(new Uint8Array(await blob.arrayBuffer()));
    } catch (error) {
      setFailure(error instanceof Error ? error.message : String(error));
    } finally {
      busy.current = false;
      setSending(false);
    }
  }, [region, marks, onSend]);

  useEffect(() => {
    const keyboard = (event: KeyboardEvent) => {
      if (event.isComposing || event.repeat) return;
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        if (!busy.current) onCancel();
        return;
      }
      const input =
        event.target instanceof HTMLElement &&
        !!event.target.closest("input, textarea, [contenteditable=true]");
      const interactive =
        event.target instanceof HTMLElement &&
        !!event.target.closest(
          "button, input, textarea, select, a, [role=button], [contenteditable=true]",
        );
      const shortcutTool = editorToolForKey(event);
      if (
        shortcutTool &&
        !input &&
        !busy.current &&
        (region || shortcutTool === "select")
      ) {
        event.preventDefault();
        event.stopPropagation();
        setTool(shortcutTool);
        return;
      }
      if (event.key === "Enter" && !interactive) {
        event.preventDefault();
        event.stopPropagation();
        void confirm();
      }
      if (
        (event.metaKey || event.ctrlKey) &&
        event.key.toLowerCase() === "z" &&
        !event.shiftKey &&
        !event.altKey &&
        !input &&
        !busy.current
      ) {
        event.preventDefault();
        setMarks((items) => items.slice(0, -1));
      }
      if (event.key === "Tab") {
        const buttons = Array.from(
          dialogRef.current?.querySelectorAll<HTMLElement>(
            "button:not(:disabled), input:not(:disabled)",
          ) ?? [],
        );
        const first = buttons[0];
        const last = buttons[buttons.length - 1];
        if (
          event.shiftKey &&
          (document.activeElement === first ||
            document.activeElement === dialogRef.current)
        ) {
          event.preventDefault();
          last?.focus();
        } else if (
          !event.shiftKey &&
          (document.activeElement === last ||
            document.activeElement === dialogRef.current)
        ) {
          event.preventDefault();
          first?.focus();
        }
      }
    };
    window.addEventListener("keydown", keyboard, true);
    return () => window.removeEventListener("keydown", keyboard, true);
  }, [confirm, onCancel, region]);

  const visibleRegion = region && {
    left: layout.canvasX + region.x * layout.scaleX,
    top: layout.canvasY + region.y * layout.scaleY,
    width: region.width * layout.scaleX,
    height: region.height * layout.scaleY,
  };
  return (
    <div
      ref={dialogRef}
      tabIndex={-1}
      data-testid="screenshot-editor"
      role="dialog"
      aria-modal="true"
      aria-label={t("screenshot.editorTitle")}
      className="fixed inset-0 z-[100] select-none overflow-hidden bg-black text-neutral-800 outline-none"
      onContextMenu={(event) => {
        event.preventDefault();
        if (!busy.current) onCancel();
      }}
    >
      <div className="absolute inset-0 flex items-center justify-center overflow-hidden">
        <canvas
          ref={canvasRef}
          data-testid="screenshot-canvas"
          onPointerDown={pointerDown}
          onPointerMove={pointerMove}
          onPointerUp={pointerUp}
          onDoubleClick={(event) => {
            if (tool === "select" && within(point(event))) void confirm();
          }}
          onPointerCancel={() => {
            if (dragRef.current)
              setRegion(
                dragRef.current.region ?? (tool === "select" ? null : region),
              );
            dragRef.current = null;
            setPreview(null);
          }}
          className="max-h-full max-w-full cursor-crosshair touch-none"
          aria-label={t("screenshot.canvasLabel")}
        />
      </div>
      {!region && (
        <p className="pointer-events-none absolute left-1/2 top-8 -translate-x-1/2 rounded bg-black/75 px-4 py-2 text-sm text-white">
          {t("screenshot.editorHint")}
        </p>
      )}
      {visibleRegion && (
        <>
          <div
            className="pointer-events-none absolute border border-[#07c160]"
            style={visibleRegion}
          />
          <output
            data-testid="screenshot-dimensions"
            className="pointer-events-none absolute rounded bg-black/75 px-2 py-1 text-xs tabular-nums text-white"
            style={{
              left: Math.max(8, visibleRegion.left),
              top: Math.max(8, visibleRegion.top - 30),
            }}
          >
            {region.width} × {region.height}
          </output>
          {handles.map((handle) => {
            const p = handlePoint(region, handle);
            return (
              <span
                key={handle}
                aria-hidden="true"
                className="pointer-events-none absolute h-[7px] w-[7px] -translate-x-1/2 -translate-y-1/2 border border-[#07c160] bg-white"
                style={{
                  left: layout.canvasX + p.x * layout.scaleX,
                  top: layout.canvasY + p.y * layout.scaleY,
                }}
              />
            );
          })}
        </>
      )}
      <ScreenshotToolbar
        ref={toolbarRef}
        position={{ left: layout.left, top: layout.top }}
        selected={!!region && region.width >= 4 && region.height >= 4}
        sending={sending}
        tool={tool}
        setTool={setTool}
        ink={ink}
        setInk={setInk}
        label={label}
        setLabel={setLabel}
        canUndo={marks.length > 0}
        onUndo={() => setMarks((items) => items.slice(0, -1))}
        onCancel={onCancel}
        onConfirm={() => void confirm()}
        failure={failure}
      />
    </div>
  );
}
