/** Component-local reveal lifetime. The body never enters application stores. */
export function createProtectedReveal(options: {
  reveal: (
    signal: AbortSignal,
  ) => Promise<{ text: string; visible_for_ms: number }>;
  current: () => boolean;
  foreground: () => boolean;
  nativeAuthentication: boolean;
  disposeBody?: (body: string) => void;
  changed: (state: {
    text: string | null;
    remaining: number;
    pending: boolean;
    error: boolean;
  }) => void;
}) {
  let epoch = 0;
  let requestAbort: AbortController | null = null;
  let timer: ReturnType<typeof setInterval> | undefined;
  let body: string | null = null;
  let wallDeadline = 0;
  let monotonicDeadline = 0;
  let visible = false;
  let duration = 30000;
  let pending = false;
  let failed = false;
  const remainingMs = () =>
    Math.min(wallDeadline - Date.now(), monotonicDeadline - performance.now());
  const startDeadline = () => {
    wallDeadline = Date.now() + duration;
    monotonicDeadline = performance.now() + duration;
  };
  const publish = () =>
    options.changed({
      text: visible && options.foreground() ? body : null,
      remaining: Math.max(0, Math.ceil(remainingMs() / 1000)),
      pending,
      error: failed,
    });
  const hide = () => {
    epoch++;
    requestAbort?.abort();
    requestAbort = null;
    clearInterval(timer);
    if (body !== null) options.disposeBody?.(body);
    body = null;
    visible = false;
    wallDeadline = monotonicDeadline = 0;
    pending = false;
    failed = false;
    publish();
  };
  const focus = () => {
    if (!options.current() || (body !== null && remainingMs() <= 0)) {
      hide();
      return;
    }
    if (body !== null && !visible && options.foreground()) {
      visible = true;
      startDeadline();
    }
    publish();
  };
  return {
    hide,
    blur() {
      if (visible || !options.nativeAuthentication) hide();
    },
    focus,
    async reveal() {
      hide();
      const request = epoch;
      const abort = new AbortController();
      requestAbort = abort;
      pending = true;
      publish();
      try {
        const result = await options.reveal(abort.signal);
        if (request !== epoch || !options.current()) {
          options.disposeBody?.(result.text);
          return;
        }
        pending = false;
        body = result.text;
        duration = Math.min(30000, Math.max(0, result.visible_for_ms));
        // A native prompt can resolve before the focus event. Keep its result local
        // for at most thirty seconds, then start the visible lifetime on focus.
        startDeadline();
        visible = options.foreground();
        timer = setInterval(() => {
          if (
            !options.current() ||
            remainingMs() <= 0 ||
            (visible && !options.foreground())
          )
            hide();
          else publish();
        }, 250);
        publish();
      } catch {
        if (request !== epoch || !options.current()) return;
        pending = false;
        failed = true;
        publish();
      }
    },
  };
}
