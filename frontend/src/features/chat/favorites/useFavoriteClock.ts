import { useCallback, useEffect, useRef, useState } from "react";

/** Display retirement only; original/native authority is checked independently. */
export function useFavoriteClock(
  expiries: readonly (number | null | undefined)[],
  calendar: boolean,
  current: () => boolean,
) {
  const origin = useRef({
    wall: Date.now(),
    monotonic: performance.now(),
    floor: 0,
  });
  const readNow = useCallback(() => {
    const clock = origin.current;
    clock.floor = Math.max(
      clock.floor,
      Date.now(),
      clock.wall + Math.floor(performance.now() - clock.monotonic),
    );
    return clock.floor;
  }, []);
  const [published, publish] = useState(readNow);
  const now = Math.max(published, readNow());
  let expiry = Infinity;
  for (const at of expiries)
    if (at != null && Number.isFinite(at) && at > now)
      expiry = Math.min(expiry, at);
  const midnight = new Date(now);
  midnight.setHours(24, 0, 0, 0);
  const next = Math.min(expiry, calendar ? midnight.getTime() : Infinity);
  // Only pending expiring content needs the existing sparse wall-jump check.
  const clockCheck = expiry === Infinity ? Infinity : 300_000;
  useEffect(() => {
    if (!Number.isFinite(next) || !current()) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const arm = () => {
      timer = setTimeout(
        tick,
        Math.min(
          2_147_483_647,
          clockCheck,
          Math.max(1, Math.ceil(next - readNow())),
        ),
      );
    };
    const tick = () => {
      timer = undefined;
      if (!active || !current()) return;
      const at = readNow();
      publish(at);
      if (at < next) arm();
    };
    const foreground = () => {
      if (active && current() && document.visibilityState !== "hidden")
        publish(readNow());
    };
    arm();
    window.addEventListener("focus", foreground);
    document.addEventListener("visibilitychange", foreground);
    return () => {
      active = false;
      clearTimeout(timer);
      window.removeEventListener("focus", foreground);
      document.removeEventListener("visibilitychange", foreground);
    };
  }, [next, clockCheck, current, readNow]);
  return now;
}
