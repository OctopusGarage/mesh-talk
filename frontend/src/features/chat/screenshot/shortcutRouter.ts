import {
  matchesShortcut,
  shortcutPlatformIsMac,
  type ScreenshotShortcuts,
} from "@/lib/screenshotShortcuts";
import type { ScreenshotShortcutEvent } from "@/lib/api/screenshotShortcuts";

interface Snapshot {
  current: boolean;
  available: boolean;
  blocked: boolean;
  writable: boolean;
  owner: string | null;
  generation: number | null;
  globalActive: boolean;
  bindings: ScreenshotShortcuts;
}

/** One dispatcher for OS and webview input. No pixels or conversation leases cross it. */
export function installScreenshotShortcutRouter({
  read,
  capture,
  wake,
  listen,
  report = () => {},
  mac = shortcutPlatformIsMac(),
}: {
  read: () => Snapshot;
  capture: (hideWindow: boolean) => Promise<void>;
  wake: () => Promise<void>;
  listen: (
    callback: (event: ScreenshotShortcutEvent) => void,
  ) => Promise<() => void>;
  report?: (error: unknown) => void;
  mac?: boolean;
}) {
  let disposed = false;
  let acquiring = false;
  const blocked = () =>
    !!document.querySelector(
      '[role="dialog"], [role="alertdialog"], [data-shortcut-recording]',
    );
  const run = (hideWindow: boolean) => {
    const state = read();
    if (disposed || acquiring || !state.current || state.blocked || blocked())
      return;
    acquiring = true;
    void (state.available && state.writable ? capture(hideWindow) : wake())
      .catch((error) => {
        if (!disposed && read().current) report(error);
      })
      .finally(() => {
        acquiring = false;
      });
  };
  const keyboard = (event: KeyboardEvent) => {
    const state = read();
    if (
      disposed ||
      event.defaultPrevented ||
      !state.current ||
      state.globalActive ||
      !document.hasFocus() ||
      blocked() ||
      !state.available ||
      state.blocked
    )
      return;
    const hidden = matchesShortcut(event, state.bindings.capture_hidden, mac);
    if (!hidden && !matchesShortcut(event, state.bindings.capture, mac)) return;
    event.preventDefault();
    run(hidden);
  };
  let unlisten: (() => void) | undefined;
  window.addEventListener("keydown", keyboard);
  const ready = listen((event) => {
    const state = read();
    if (
      !state.globalActive ||
      event.owner !== state.owner ||
      event.generation !== state.generation
    )
      return;
    run(event.hide_window);
  }).then(
    (cleanup) => {
      if (disposed) {
        cleanup();
        return false;
      }
      unlisten = cleanup;
      return true;
    },
    (error) => {
      if (!disposed && read().current) report(error);
      return false;
    },
  );
  const cleanup = () => {
    if (disposed) return;
    disposed = true;
    window.removeEventListener("keydown", keyboard);
    unlisten?.();
  };
  return Object.assign(cleanup, { ready });
}
