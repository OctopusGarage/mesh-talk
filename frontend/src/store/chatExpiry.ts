import type { ChatState } from "./chat";

type ExpiryState = Pick<
  ChatState,
  | "messages"
  | "intents"
  | "ledger"
  | "incomingFiles"
  | "searchTarget"
  | "expiryNow"
>;

/** One actual-deadline timer per chat owner. No data means no timer and no clock
 * publication. The host retains authoritative expiry; this retires presentation. */
export function startChatExpiry(ports: {
  get: () => ExpiryState;
  current: () => boolean;
  expire: (now: number) => void;
}) {
  const wallStart = Date.now();
  const monotonicStart = performance.now();
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const now = () =>
    Math.max(
      ports.get().expiryNow,
      Date.now(),
      wallStart + Math.floor(performance.now() - monotonicStart),
    );
  const nextDeadline = () => {
    const state = ports.get();
    let next = Infinity;
    const take = (at: number | null | undefined) => {
      if (at != null && Number.isFinite(at)) next = Math.min(next, at);
    };
    for (const rows of Object.values(state.messages))
      for (const row of rows) take(row.expiresAtMs);
    for (const intent of Object.values(state.intents))
      take(intent.message.expiresAtMs);
    for (const entries of Object.values(state.ledger))
      for (const entry of Object.values(entries))
        if (!entry.suppressed) take(entry.expiresAtMs);
    for (const file of state.incomingFiles) take(file.expiresAtMs);
    if (state.searchTarget && "context" in state.searchTarget)
      for (const message of state.searchTarget.context)
        if (
          message.expiresAtMs != null &&
          message.expiresAtMs > state.expiryNow
        )
          take(message.expiresAtMs);
    return next;
  };
  const schedule = () => {
    if (timer !== undefined) clearTimeout(timer);
    timer = undefined;
    if (stopped || !ports.current()) return;
    const deadline = nextDeadline();
    if (deadline === Infinity) return;
    // A sparse clock check only while expiring content exists also bounds wall-clock jumps.
    timer = setTimeout(
      refresh,
      Math.min(5 * 60_000, Math.max(0, deadline - now())),
    );
  };
  const refresh = () => {
    if (stopped || !ports.current()) return;
    const at = now();
    if (nextDeadline() <= at) ports.expire(at);
    schedule();
  };
  schedule();
  return {
    schedule,
    refresh,
    stop: () => {
      stopped = true;
      if (timer !== undefined) clearTimeout(timer);
    },
  };
}
