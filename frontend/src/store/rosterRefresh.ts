import type { AccountInfo } from "@/lib/types";

/** Every roster caller shares one owner-scoped lane and one pending invalidation. */
export function createRosterRefresh<Input = void>(context: {
  capture(): { current(): boolean };
  refresh(hasPending: () => boolean, input: Input | undefined): Promise<void>;
}): (input?: Input) => Promise<void> {
  type Run = {
    current(): boolean;
    dirty: boolean;
    input?: Input;
    promise?: Promise<void>;
  };
  let run: Run | undefined;
  const start = (work: Run): Promise<void> => {
    const drain = async () => {
      do {
        work.dirty = false;
        const input = work.input;
        work.input = undefined;
        await context.refresh(() => work.dirty, input);
      } while (work.current() && work.dirty);
    };
    const promise = drain().finally(() => {
      work.promise = undefined;
      if (work.current() && work.dirty) return start(work);
    });
    work.promise = promise;
    return promise;
  };
  return (input?: Input) => {
    if (!run?.current()) run = { ...context.capture(), dirty: false };
    if (!run.current()) return Promise.resolve();
    run.input = input;
    if (run.promise) {
      run.dirty = true;
      return run.promise;
    }
    return start(run);
  };
}

export const UNKNOWN_SENDER_REPAIR_MS = 2_000;
const MAX_CONTACT_HINTS = 256;

/** A first message can affect an account row before its sender device is discovered. */
export function createContactHistoryRefresh(refresh: () => Promise<void>) {
  const sources = new Map<string, { next: number; pending: boolean }>();
  let due = Infinity;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let armed = Infinity;
  let active: Promise<void> | undefined;

  function disarm() {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    armed = Infinity;
  }

  function schedule() {
    // Let the current read publish. Its completion drains hints received after
    // it started, including when a native read outlasts the two-second window.
    if (active || due === armed) return;
    disarm();
    armed = due;
    timer = setTimeout(run, Math.max(0, due - performance.now()));
  }

  function run() {
    disarm();
    due = Infinity;
    const next = performance.now() + UNKNOWN_SENDER_REPAIR_MS;
    // One whole account-list read covers all hints received before this call.
    for (const entry of sources.values()) {
      if (entry.pending) {
        entry.next = next;
        entry.pending = false;
      }
    }
    const read = refresh();
    if (active === read) return;
    active = read;
    void read.finally(() => {
      if (active !== read) return;
      active = undefined;
      schedule();
    });
  }

  return {
    observe(kind: "account" | "device", id: string) {
      const key = `${kind}:${id}`;
      const now = performance.now();
      const entry = sources.get(key);
      if (!active && !entry && sources.size === MAX_CONTACT_HINTS) {
        for (const [key, observed] of sources) {
          if (!observed.pending && observed.next <= now) sources.delete(key);
        }
      }
      if (
        entry ? !active && entry.next <= now : sources.size < MAX_CONTACT_HINTS
      ) {
        sources.set(key, {
          next: now + UNKNOWN_SENDER_REPAIR_MS,
          pending: false,
        });
        run();
        return;
      }
      // Additional hint sources share a single trailing request without allocating
      // more keys. Repeated hints cannot postpone an existing deadline.
      if (entry) entry.pending = true;
      due = Math.min(due, entry?.next ?? now + UNKNOWN_SENDER_REPAIR_MS);
      schedule();
    },
    reset() {
      disarm();
      due = Infinity;
      active = undefined;
      sources.clear();
    },
  };
}

/** One native contact-list recheck at the earliest advertised deadline. */
export function createContactExpiry(recheck: () => void) {
  let deadline = Infinity;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const arm = () => {
    // A backward wall-clock adjustment can move even a one-day deadline far
    // beyond the browser timer range. Recheck the clock before native reads.
    timer = setTimeout(
      () => {
        timer = undefined;
        if (Date.now() < deadline) arm();
        else recheck();
      },
      Math.min(86_400_000, Math.max(0, deadline - Date.now())),
    );
  };
  return {
    observe(rows: readonly AccountInfo[]) {
      let next = Infinity;
      for (const row of rows) {
        const at = row.contact_expires_at_ms;
        if (at != null && Number.isFinite(at)) next = Math.min(next, at);
      }
      // A consumed deadline stays parked on a failed read. The existing
      // activation and recovery refreshes retry it without a zero-delay loop.
      if (next === deadline) return;
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
      deadline = next;
      if (next !== Infinity) arm();
    },
    stop() {
      if (timer !== undefined) clearTimeout(timer);
    },
  };
}
