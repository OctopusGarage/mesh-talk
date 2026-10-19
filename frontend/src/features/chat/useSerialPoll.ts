import { useEffect, useRef } from "react";

/** Poll while visible, without overlapping requests or applying results after close. */
export function useSerialPoll<T>(
  enabled: boolean,
  intervalMs: number,
  request: () => Promise<T>,
  onResult: (result: T) => void,
  onError: (error: unknown) => void,
) {
  const handlers = useRef({ request, onResult, onError });
  useEffect(() => {
    handlers.current = { request, onResult, onError };
  }, [request, onResult, onError]);

  useEffect(() => {
    if (!enabled) return;
    let active = true;
    let busy = false;
    const poll = () => {
      if (busy) return;
      busy = true;
      void Promise.resolve()
        .then(() => handlers.current.request())
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
        });
    };
    poll();
    const timer = setInterval(poll, intervalMs);
    return () => {
      active = false;
      clearInterval(timer);
    };
  }, [enabled, intervalMs]);
}
