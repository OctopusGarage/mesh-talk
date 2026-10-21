import {
  subscribeSyncInvalidation,
  supportsPushState,
  type SyncInvalidation,
} from "@/lib/syncInvalidation";
import { readWithDeadline } from "@/lib/readDeadline";
import { useEffect, useRef } from "react";

/** Poll while visible, without overlapping requests or applying results after close. */
export function useSerialPoll<T>(
  enabled: boolean,
  intervalMs: number,
  request: () => Promise<T>,
  onResult: (result: T) => void,
  onError: (error: unknown) => void,
  wakeOn?: SyncInvalidation["kind"],
) {
  const handlers = useRef({ request, onResult, onError });
  useEffect(() => {
    handlers.current = { request, onResult, onError };
  }, [request, onResult, onError]);

  useEffect(() => {
    if (!enabled) return;
    let active = true;
    let busy = false;
    let dirty = false;
    let nextFallback = 0;
    const poll = (invalidate = false) => {
      if (busy) {
        dirty ||= invalidate;
        return;
      }
      busy = true;
      nextFallback = performance.now() + 30000;
      void Promise.resolve()
        .then(() => readWithDeadline(() => handlers.current.request()))
        .then(
          (result) => {
            if (active) handlers.current.onResult(result);
          },
          (error) => {
            if (active) handlers.current.onError(error);
          },
        )
        .finally(() => {
          busy = false;
          if (active && dirty) {
            dirty = false;
            poll();
          }
        });
    };
    poll();
    const unlisten = subscribeSyncInvalidation((change) => {
      if (change.kind === wakeOn) poll(true);
    });
    const timer = setInterval(() => {
      if (!wakeOn || !supportsPushState() || performance.now() >= nextFallback)
        poll();
    }, intervalMs);
    return () => {
      active = false;
      clearInterval(timer);
      unlisten();
    };
  }, [enabled, intervalMs, wakeOn]);
}
