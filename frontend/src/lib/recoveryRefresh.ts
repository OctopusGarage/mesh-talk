/** Semantic invalidations own prompt updates. This is activation and lost-event
 * recovery only; delivery, expiry and calls must retain their own deadlines. */
export const RECOVERY_REFRESH_MS = 5 * 60_000;

export function startRecoveryRefresh(
  refresh: () => void,
  {
    background = true,
    intervalMs = () => RECOVERY_REFRESH_MS,
  }: {
    background?: boolean;
    intervalMs?: () => number;
  } = {},
): () => void {
  let active = true;
  let queued = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const visible = () =>
    typeof document === "undefined" || document.visibilityState !== "hidden";
  const arm = () => {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    if (!active || (!background && !visible())) return;
    timer = setTimeout(
      () => {
        timer = undefined;
        arm();
        if (active) refresh();
      },
      Math.max(1, intervalMs()),
    );
  };
  const activate = () => {
    arm();
    if (!active || !visible() || queued) return;
    queued = true;
    queueMicrotask(() => {
      queued = false;
      if (active && visible()) refresh();
    });
  };
  const host = typeof window === "undefined" ? undefined : window;
  const page = typeof document === "undefined" ? undefined : document;
  host?.addEventListener?.("focus", activate);
  page?.addEventListener?.("visibilitychange", activate);
  arm();
  return () => {
    active = false;
    if (timer !== undefined) clearTimeout(timer);
    host?.removeEventListener?.("focus", activate);
    page?.removeEventListener?.("visibilitychange", activate);
  };
}
