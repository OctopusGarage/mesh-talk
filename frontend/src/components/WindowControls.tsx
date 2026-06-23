import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Square, X } from "lucide-react";
import { needsCustomWindowControls } from "@/lib/platform";

/**
 * Custom minimize/maximize/close controls for the frameless window. We drop the native
 * title bar on Windows/Linux (macOS keeps its overlay traffic-lights), so the app draws its
 * own controls top-right. Renders nothing on macOS or in a plain browser (e2e/dev tab).
 *
 * Close routes through the normal window close → the Rust `on_window_event` handler honors
 * "minimize to tray" (hide vs quit), so this behaves exactly like the old native button.
 */
export function WindowControls() {
  if (!needsCustomWindowControls()) return null;
  const win = getCurrentWindow();
  const btn =
    "flex h-8 w-12 items-center justify-center text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none";
  return (
    <div className="fixed right-0 top-0 z-[60] flex select-none">
      <button
        type="button"
        aria-label="Minimize"
        className={btn}
        onClick={() => void win.minimize()}
      >
        <Minus className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        aria-label="Maximize"
        className={btn}
        onClick={() => void win.toggleMaximize()}
      >
        <Square className="h-3 w-3" />
      </button>
      <button
        type="button"
        aria-label="Close"
        className={`${btn} hover:bg-red-600 hover:text-white`}
        onClick={() => void win.close()}
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  );
}
