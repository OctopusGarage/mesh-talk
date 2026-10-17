interface CaptureWindow {
  readonly label?: string;
  isFullscreen(): Promise<boolean>;
  setSimpleFullscreen(fullscreen: boolean): Promise<void>;
}
interface CaptureSession {
  window: CaptureWindow;
  owners: Set<symbol>;
  queue: Promise<void>;
  originalFullscreen?: boolean;
  expanded: boolean;
}
// Tauri returns separate JS handles for the same native window. Use its label as identity.
const sessions = new Map<string | CaptureWindow, CaptureSession>();

/** Serialize transitions and retain original state until the final editor releases it. */
export function expandCaptureWindow(window: CaptureWindow): () => void {
  const key = window.label ?? window;
  let session = sessions.get(key);
  if (!session) {
    session = {
      window,
      owners: new Set(),
      queue: Promise.resolve(),
      expanded: false,
    };
    sessions.set(key, session);
  }
  const state = session;
  const owner = Symbol("capture");
  state.owners.add(owner);
  const enqueue = (transition: () => Promise<void>) => {
    state.queue = state.queue.then(transition).catch(() => {});
  };
  enqueue(async () => {
    if (!state.owners.has(owner)) return;
    if (state.originalFullscreen === undefined)
      state.originalFullscreen = await state.window.isFullscreen();
    if (!state.owners.has(owner)) return;
    if (!state.originalFullscreen && !state.expanded) {
      // macOS stays in its current Space; other platforms use regular fullscreen.
      await state.window.setSimpleFullscreen(true);
      state.expanded = true;
    }
  });
  return () => {
    if (!state.owners.delete(owner)) return;
    enqueue(async () => {
      if (state.owners.size) return;
      if (state.expanded) {
        await state.window.setSimpleFullscreen(false);
        state.expanded = false;
      }
      if (!state.owners.size && sessions.get(key) === state)
        sessions.delete(key);
    });
  };
}
