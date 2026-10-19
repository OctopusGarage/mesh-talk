import type { ArrivalKind, ReceptionMetadata } from "@/lib/types";

export interface ReceptionEntry {
  seen: boolean;
  late: boolean;
  receivedAt?: number;
  arrivalKind?: ArrivalKind;
}
export type ReceptionLedger = Record<string, Record<string, ReceptionEntry>>;
export interface ConversationReception {
  phase: "idle" | "catching_up" | "waiting";
  received: number;
  batchId?: string;
  checkedAt?: number;
  processing?: boolean;
}
export interface ReceptionState {
  ledger: ReceptionLedger;
  overflowUnread: Record<string, number>;
  unreadIds: Record<string, string[]>;
  lateUnreadIds: Record<string, string[]>;
  reception: Record<string, ConversationReception>;
}
export const LEDGER_CONVERSATIONS = 64;
export const LEDGER_IDENTITIES = 1024;
export const LEDGER_TOTAL = 8192;

/** Exact identities, scoped to account by persistence. No author-time read watermark. */
export function boundLedger(ledger: ReceptionLedger): ReceptionLedger {
  const ordered = Object.entries(ledger);
  // Viewed-only conversation caches yield before conversations with unread work.
  const conversations = ordered
    .filter(
      ([, entries]) => !Object.values(entries).some((entry) => !entry.seen),
    )
    .concat(
      ordered.filter(([, entries]) =>
        Object.values(entries).some((entry) => !entry.seen),
      ),
    )
    .slice(-LEDGER_CONVERSATIONS);
  // Unread exceptions consume the budget before viewed LRU entries.
  const candidates = conversations
    .flatMap(([key, entries]) =>
      Object.entries(entries).map(([id, entry]) => ({ key, id, entry })),
    )
    .reverse();
  candidates.sort((a, b) => Number(a.entry.seen) - Number(b.entry.seen));
  const result: ReceptionLedger = Object.fromEntries(
    conversations.map(([key]) => [key, {}]),
  );
  let retained = 0;
  const counts: Record<string, number> = {};
  for (const { key, id, entry } of candidates) {
    if (retained >= LEDGER_TOTAL) break;
    if ((counts[key] ?? 0) >= LEDGER_IDENTITIES) continue;
    Object.defineProperty(result[key], id, {
      value: entry,
      enumerable: true,
      configurable: true,
      writable: true,
    });
    counts[key] = (counts[key] ?? 0) + 1;
    retained++;
  }
  return Object.fromEntries(
    Object.entries(result).map(([key, entries]) => [
      key,
      Object.fromEntries(Object.entries(entries).reverse()),
    ]),
  );
}
export function emptyReception(
  ledger: ReceptionLedger = {},
  overflowUnread: Record<string, number> = {},
): ReceptionState {
  const bounded = boundLedger(ledger);
  const overflow = { ...overflowUnread };
  for (const [key, entries] of Object.entries(ledger)) {
    const dropped = Object.entries(entries).filter(
      ([id, entry]) =>
        !entry.seen &&
        !Object.prototype.hasOwnProperty.call(bounded[key] ?? {}, id),
    ).length;
    if (dropped)
      overflow[key] = Math.min(1_000_000, (overflow[key] ?? 0) + dropped);
  }
  return {
    ledger: bounded,
    overflowUnread: Object.fromEntries(
      Object.entries(overflow)
        .filter(([, count]) => Number.isSafeInteger(count) && count > 0)
        .slice(-4096),
    ),
    unreadIds: Object.fromEntries(
      Object.entries(bounded).map(([key, entries]) => [
        key,
        Object.keys(entries).filter((id) => !entries[id].seen),
      ]),
    ),
    lateUnreadIds: Object.fromEntries(
      Object.entries(bounded).map(([key, entries]) => [
        key,
        Object.keys(entries).filter(
          (id) => !entries[id].seen && entries[id].late,
        ),
      ]),
    ),
    reception: {},
  };
}
export function receiveIdentity(
  state: ReceptionState,
  key: string,
  event: ReceptionMetadata,
): ReceptionState {
  if (event.reception_only) {
    const batch = state.reception[key];
    if (
      !event.batch_complete ||
      !batch ||
      batch.batchId !== (event.batch_id ?? undefined)
    )
      return state;
    return {
      ...state,
      reception: {
        ...state.reception,
        [key]: { ...batch, phase: "idle", processing: false },
      },
    };
  }
  const id = event.event_id;
  if (!id || id.length > 200) return state;
  const old = Object.prototype.hasOwnProperty.call(state.ledger[key] ?? {}, id)
    ? state.ledger[key][id]
    : undefined;
  if (old) {
    let next = state;
    if (old.receivedAt === undefined && event.received_at !== undefined) {
      next = {
        ...state,
        ledger: {
          ...state.ledger,
          [key]: {
            ...state.ledger[key],
            [id]: {
              ...old,
              receivedAt: event.received_at,
              arrivalKind: event.arrival_kind ?? old.arrivalKind,
            },
          },
        },
      };
    }
    const batch = next.reception[key];
    return event.batch_complete && batch?.batchId === event.batch_id
      ? {
          ...next,
          reception: {
            ...next.reception,
            [key]: { ...batch, phase: "idle", processing: false },
          },
        }
      : next;
  }
  const kind = event.arrival_kind ?? "live";
  const entries = {
    ...(state.ledger[key] ?? {}),
    [id]: {
      seen: event.own_device === true,
      late: kind !== "live",
      receivedAt: event.received_at,
      arrivalKind: kind,
    },
  };
  const next = emptyReception(
    { ...state.ledger, [key]: entries },
    state.overflowUnread,
  );
  const previous = state.reception[key];
  const batch = event.batch_id ?? undefined;
  const received =
    kind === "catch_up"
      ? (previous && previous.batchId === batch ? previous.received : 0) + 1
      : 0;
  return {
    ...next,
    reception: {
      ...Object.fromEntries(
        Object.entries(state.reception).filter(([key]) =>
          Object.prototype.hasOwnProperty.call(next.ledger, key),
        ),
      ),
      ...(kind === "catch_up"
        ? {
            [key]: {
              phase: event.batch_complete === false ? "catching_up" : "idle",
              processing: event.batch_complete === false,
              received,
              checkedAt: previous?.checkedAt,
              batchId: batch,
            },
          }
        : {}),
    },
  };
}
export function viewIdentities(
  state: ReceptionState,
  key: string,
  ids: string[],
): ReceptionState {
  const entries = { ...(state.ledger[key] ?? {}) };
  for (const id of ids)
    if (id && id.length <= 200)
      Object.defineProperty(entries, id, {
        value: {
          ...(Object.prototype.hasOwnProperty.call(entries, id)
            ? entries[id]
            : { late: false }),
          seen: true,
        },
        enumerable: true,
        configurable: true,
        writable: true,
      });
  return {
    ...emptyReception(
      { ...state.ledger, [key]: entries },
      state.overflowUnread,
    ),
    reception: state.reception,
  };
}
export function reconcileHistory(
  state: ReceptionState,
  key: string,
  rows: { id: string | null; fromMe: boolean }[],
  discover: boolean,
): ReceptionState {
  const entries = { ...(state.ledger[key] ?? {}) };
  for (const row of rows) {
    if (
      !row.id ||
      row.id.length > 200 ||
      Object.prototype.hasOwnProperty.call(entries, row.id)
    )
      continue;
    Object.defineProperty(entries, row.id, {
      value: {
        seen: row.fromMe || !discover,
        late: true,
        arrivalKind: "history_sync",
      },
      enumerable: true,
      configurable: true,
      writable: true,
    });
  }
  return {
    ...emptyReception(
      { ...state.ledger, [key]: entries },
      state.overflowUnread,
    ),
    reception: state.reception,
  };
}
