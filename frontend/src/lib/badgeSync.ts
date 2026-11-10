import { errorMessage } from "./error";

interface BadgePorts {
  write: (count: number) => Promise<void>;
  current: () => boolean;
  /** Content-free diagnostic, cleared only by an acknowledged update. */
  status: (error: string | null) => void;
}

/** One latest-value writer; bounded backoff and admission survive native stalls. */
export function createBadgeSync(ports: BadgePorts) {
  let active = true;
  let busy = false;
  let desired: number | null = null;
  let revision = 0;
  let applied = -1;
  let failures = 0;
  let pending = 0;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let deadline: ReturnType<typeof setTimeout> | undefined;
  const current = () => active && ports.current();
  const cancelRetry = () => {
    clearTimeout(retryTimer);
    retryTimer = undefined;
  };
  const schedule = () => {
    if (!current()) return;
    cancelRetry();
    retryTimer = setTimeout(
      () => {
        retryTimer = undefined;
        void flush();
      },
      Math.min(30_000, 1000 * 2 ** Math.min(5, failures - 1)),
    );
  };
  const flush = async () => {
    if (!current() || busy || desired === null || applied === revision) return;
    // A webview deadline cannot cancel native work. Never admit unlimited hung writes.
    if (pending >= 4) {
      schedule();
      return;
    }
    busy = true;
    const attempt = revision;
    const count = desired;
    pending++;
    try {
      await new Promise<void>((resolve, reject) => {
        deadline = setTimeout(
          () => reject(new Error("Badge update timed out")),
          5000,
        );
        try {
          Promise.resolve(ports.write(count)).then(
            () => {
              pending--;
              resolve();
            },
            (error: unknown) => {
              pending--;
              reject(error);
            },
          );
        } catch (error) {
          pending--;
          reject(error);
        }
      });
      if (!current()) return;
      applied = attempt;
      failures = 0;
      ports.status(null);
    } catch (error) {
      if (!current()) return;
      failures++;
      ports.status(errorMessage(error));
    } finally {
      clearTimeout(deadline);
      deadline = undefined;
      busy = false;
      if (current() && attempt !== revision) void flush();
      else if (current() && applied !== revision) schedule();
    }
  };
  return {
    update(count: number, force = false) {
      if (!current()) return;
      if (desired !== count || force) {
        desired = count;
        revision++;
      }
      if (applied === revision) return;
      cancelRetry();
      void flush();
    },
    stop() {
      active = false;
      cancelRetry();
      clearTimeout(deadline);
    },
  };
}
