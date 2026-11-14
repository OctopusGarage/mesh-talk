import {
  subscribeSyncInvalidation,
  supportsPushState,
  type SyncInvalidation,
} from "@/lib/syncInvalidation";
import { readWithDeadline } from "@/lib/readDeadline";
import { useEffect, useRef } from "react";

/** Event-owned refresh with one visible recovery deadline and one actual read. */
export function useSerialPoll<T>(
  enabled: boolean,
  intervalMs: number,
  request: () => Promise<T>,
  onResult: (result: T) => void,
  onError: (error: unknown) => void,
  wakeOn?: SyncInvalidation["kind"],
  acceptsInvalidation?: (change: SyncInvalidation) => boolean,
) {
  const handlers = useRef({ request, onResult, onError, acceptsInvalidation });
  useEffect(() => {
    handlers.current = { request, onResult, onError, acceptsInvalidation };
  }, [request, onResult, onError, acceptsInvalidation]);

  useEffect(() => {
    if (!enabled) return;
    let active = true;
    let busy = false;
    let dirty = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const schedule = () => {
      clearTimeout(timer);
      if (!active || busy || document.hidden) return;
      const delay = wakeOn && supportsPushState() ? 300_000 : intervalMs;
      timer = setTimeout(() => poll(), delay);
    };
    const poll = (invalidate = false) => {
      if (!active) return;
      if (busy || document.hidden) {
        dirty ||= invalidate;
        return;
      }
      busy = true;
      dirty = false;
      clearTimeout(timer);
      let physical: Promise<void> | undefined;
      void readWithDeadline(() => {
        const operation = Promise.resolve(handlers.current.request());
        physical = operation.then(
          () => {},
          () => {},
        );
        return operation;
      })
        .then(
          (result) => {
            if (active) handlers.current.onResult(result);
          },
          (error) => {
            if (active) handlers.current.onError(error);
          },
        )
        .finally(() => {
          const release = () => {
            busy = false;
            if (!active) return;
            if (dirty && !document.hidden) poll();
            else schedule();
          };
          // A UI timeout cannot cancel native work. Drain after actual settlement
          // and after the global read admission has released its original slot.
          if (physical) void physical.then(() => queueMicrotask(release));
          else release();
        });
    };
    const unlisten = subscribeSyncInvalidation((change) => {
      if (
        change.kind === wakeOn &&
        (handlers.current.acceptsInvalidation?.(change) ?? true)
      )
        poll(true);
    });
    const foreground = () => {
      if (!document.hidden) poll(true);
      else clearTimeout(timer);
    };
    window.addEventListener("focus", foreground);
    document.addEventListener("visibilitychange", foreground);
    poll();
    return () => {
      active = false;
      clearTimeout(timer);
      window.removeEventListener("focus", foreground);
      document.removeEventListener("visibilitychange", foreground);
      unlisten();
    };
  }, [enabled, intervalMs, wakeOn]);
}
