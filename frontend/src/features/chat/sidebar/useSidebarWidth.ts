import { useCallback, useEffect, useRef, useState } from "react";

// Sidebar width: user-resizable via the right-edge drag handle, persisted to localStorage,
// clamped to a sensible range, double-click to reset. Default mirrors the old `w-72`.
const WIDTH_KEY = "mesh-talk-sidebar-width";
const DEFAULT_WIDTH = 284;
export const MIN_WIDTH = 230;
export const MAX_WIDTH = 460;

const clampWidth = (w: number) =>
  Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(w)));

function readWidth(): number {
  try {
    const v = Number(localStorage.getItem(WIDTH_KEY));
    return Number.isFinite(v) && v > 0 ? clampWidth(v) : DEFAULT_WIDTH;
  } catch {
    // Storage may be disabled by the webview or browser privacy policy.
    return DEFAULT_WIDTH;
  }
}

export function useSidebarWidth() {
  const [width, setWidth] = useState(readWidth);
  const dragCleanup = useRef<(() => void) | null>(null);

  const persist = useCallback((w: number) => {
    setWidth(w);
    try {
      localStorage.setItem(WIDTH_KEY, String(w));
    } catch {
      // Resizing remains usable even if this device cannot persist the preference.
    }
  }, []);

  useEffect(() => () => dragCleanup.current?.(), []);

  // Pointer capture keeps the drag continuous outside the narrow handle. Persist only
  // the settled width so a long drag does not write storage on every pointer event.
  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (e.button !== 0 || dragCleanup.current) return;
      e.preventDefault();
      const handle = e.currentTarget as HTMLElement;
      const pointerId = e.pointerId;
      const aside = (e.currentTarget as HTMLElement).parentElement;
      const left = aside?.getBoundingClientRect().left ?? 0;
      const prevCursor = document.body.style.cursor;
      const prevSelect = document.body.style.userSelect;
      let latest = clampWidth(e.clientX - left);
      let done = false;
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
      handle.setPointerCapture(pointerId);
      const onMove = (ev: PointerEvent) => {
        if (ev.pointerId !== pointerId || done) return;
        latest = clampWidth(ev.clientX - left);
        setWidth(latest);
      };
      const cleanup = () => {
        if (done) return;
        done = true;
        document.body.style.cursor = prevCursor;
        document.body.style.userSelect = prevSelect;
        handle.removeEventListener("pointermove", onMove);
        handle.removeEventListener("pointerup", onEnd);
        handle.removeEventListener("pointercancel", onEnd);
        handle.removeEventListener("lostpointercapture", onEnd);
        window.removeEventListener("blur", onEnd);
        if (handle.hasPointerCapture(pointerId))
          handle.releasePointerCapture(pointerId);
        if (dragCleanup.current === cleanup) dragCleanup.current = null;
      };
      const onEnd = (ev?: PointerEvent | Event) => {
        if (ev instanceof PointerEvent && ev.pointerId !== pointerId) return;
        cleanup();
        persist(latest);
      };
      dragCleanup.current = cleanup;
      handle.addEventListener("pointermove", onMove);
      handle.addEventListener("pointerup", onEnd);
      handle.addEventListener("pointercancel", onEnd);
      handle.addEventListener("lostpointercapture", onEnd);
      window.addEventListener("blur", onEnd);
    },
    [persist],
  );

  const reset = useCallback(() => persist(DEFAULT_WIDTH), [persist]);

  // Keyboard resize for accessibility (focus the handle, arrow to nudge).
  const onKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "ArrowLeft") persist(clampWidth(width - 16));
      else if (e.key === "ArrowRight") persist(clampWidth(width + 16));
      else return;
      e.preventDefault();
    },
    [persist, width],
  );

  return { width, onPointerDown, reset, onKeyDown };
}
