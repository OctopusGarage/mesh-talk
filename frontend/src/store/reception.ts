import type { ArrivalKind, ReceptionMetadata } from "@/lib/types";

export interface ReceptionEntry {
  /** Core-authenticated recipient event identities for this logical file message. */
  messageAliases?: string[];
  /** Content-free evidence that this external message mentioned the local user. */
  mentioned?: boolean;
  /** Confirmed local removal; never published as a viewed receipt. */
  suppressed?: boolean;
  expiresAtMs?: number;
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
  /** Account documents outlive the viewed-entry LRU and may precede history. */
  accountSeen?: Record<string, string[]>;
  ledger: ReceptionLedger;
  overflowUnread: Record<string, number>;
  unreadIds: Record<string, string[]>;
  lateUnreadIds: Record<string, string[]>;
  reception: Record<string, ConversationReception>;
}
export const LEDGER_CONVERSATIONS = 64;
export const LEDGER_IDENTITIES = 1024;
export const LEDGER_TOTAL = 8192;
export const LEDGER_ALIAS_BYTES = 128_000;
const MESSAGE_ALIAS_LIMIT = 64;

export function validMessageAliases(
  value: unknown,
  canonical?: string,
): string[] {
  return Array.isArray(value)
    ? [
        ...new Set(
          value.filter(
            (id): id is string =>
              typeof id === "string" &&
              id !== canonical &&
              validAccountSeenIdentity(id),
          ),
        ),
      ].slice(0, MESSAGE_ALIAS_LIMIT)
    : [];
}

/** Resolve only explicit identities inside this account/conversation's bounded ledger. */
function receptionIdentity(
  entries: Record<string, ReceptionEntry>,
  id: string,
): string {
  if (Object.prototype.hasOwnProperty.call(entries, id)) return id;
  return (
    Object.keys(entries).find((canonical) =>
      entries[canonical].messageAliases?.includes(id),
    ) ?? id
  );
}

/** Immutable conversation maps share their enumeration and unread projections. */
const conversationSnapshots = new WeakMap<
  Record<string, ReceptionEntry>,
  {
    entries: [string, ReceptionEntry][];
    unread: string[];
    late: string[];
  }
>();
function conversationSnapshot(entries: Record<string, ReceptionEntry>) {
  let snapshot = conversationSnapshots.get(entries);
  if (!snapshot) {
    const rows = Object.entries(entries);
    const unread = rows.filter(([, entry]) => !entry.seen && !entry.suppressed);
    snapshot = {
      entries: rows,
      unread: unread.map(([id]) => id),
      late: unread.filter(([, entry]) => entry.late).map(([id]) => id),
    };
    conversationSnapshots.set(entries, snapshot);
  }
  return snapshot;
}

/** Exact identities, scoped to account by persistence. Unread entries retain
 * priority over viewed LRU entries without sorting/cloning the entire ledger. */
export function boundLedger(ledger: ReceptionLedger): ReceptionLedger {
  const ordered = Object.entries(ledger);
  const conversations = ordered
    .filter(([, entries]) => conversationSnapshot(entries).unread.length === 0)
    .concat(
      ordered.filter(
        ([, entries]) => conversationSnapshot(entries).unread.length > 0,
      ),
    )
    .slice(-LEDGER_CONVERSATIONS);
  const retained = new Map<string, [string, ReceptionEntry][]>(
    conversations.map(([key]) => [key, []]),
  );
  let count = 0;
  let aliasBytes = 0;
  // The old stable priority sort visited newest unread entries, then newest
  // viewed entries. Two reverse passes preserve that eviction/alias policy.
  for (const viewed of [false, true]) {
    for (
      let c = conversations.length - 1;
      c >= 0 && count < LEDGER_TOTAL;
      c--
    ) {
      const [key, entries] = conversations[c];
      const rows = conversationSnapshot(entries).entries;
      const kept = retained.get(key)!;
      for (
        let i = rows.length - 1;
        i >= 0 && count < LEDGER_TOTAL && kept.length < LEDGER_IDENTITIES;
        i--
      ) {
        const [id, entry] = rows[i];
        if (Boolean(entry.seen || entry.suppressed) !== viewed) continue;
        let value = entry;
        if (entry.messageAliases !== undefined) {
          const aliases = validMessageAliases(entry.messageAliases, id).filter(
            (alias) => {
              const bytes = JSON.stringify(alias).length + 1;
              if (aliasBytes + bytes > LEDGER_ALIAS_BYTES) return false;
              aliasBytes += bytes;
              return true;
            },
          );
          if (
            !aliases.length ||
            aliases.length !== entry.messageAliases.length ||
            aliases.some(
              (alias, index) => alias !== entry.messageAliases![index],
            )
          ) {
            const { messageAliases: _aliases, ...metadata } = entry;
            value = aliases.length
              ? { ...metadata, messageAliases: aliases }
              : metadata;
          }
        }
        kept.push(value === entry ? rows[i] : [id, value]);
        count++;
      }
    }
  }
  return Object.fromEntries(
    conversations.map(([key, entries]) => {
      const kept = retained.get(key)!.reverse();
      const original = conversationSnapshot(entries).entries;
      const unchanged =
        kept.length === original.length &&
        kept.every(
          ([id, entry], i) => id === original[i][0] && entry === original[i][1],
        );
      return [key, unchanged ? entries : Object.fromEntries(kept)];
    }),
  );
}
export function emptyReception(
  ledger: ReceptionLedger = {},
  overflowUnread: Record<string, number> = {},
  accountSeen?: Record<string, string[]>,
): ReceptionState {
  const bounded = boundLedger(ledger);
  const overflow = { ...overflowUnread };
  for (const [key, entries] of Object.entries(ledger)) {
    if (bounded[key] === entries) continue;
    const dropped = conversationSnapshot(entries).entries.filter(
      ([id, entry]) =>
        !entry.seen &&
        !entry.suppressed &&
        !Object.prototype.hasOwnProperty.call(bounded[key] ?? {}, id),
    ).length;
    if (dropped)
      overflow[key] = Math.min(1_000_000, (overflow[key] ?? 0) + dropped);
  }
  return {
    ...(accountSeen ? { accountSeen } : {}),
    ledger: bounded,
    overflowUnread: Object.fromEntries(
      Object.entries(overflow)
        .filter(([, count]) => Number.isSafeInteger(count) && count > 0)
        .slice(-4096),
    ),
    unreadIds: Object.fromEntries(
      Object.entries(bounded).map(([key, entries]) => [
        key,
        conversationSnapshot(entries).unread,
      ]),
    ),
    lateUnreadIds: Object.fromEntries(
      Object.entries(bounded).map(([key, entries]) => [
        key,
        conversationSnapshot(entries).late,
      ]),
    ),
    reception: {},
  };
}

type CountedReception = ReceptionState & { unread: Record<string, number> };
const trackedUnread = (state: ReceptionState, key: string) =>
  (state.unreadIds[key]?.length ?? 0) + (state.overflowUnread[key] ?? 0);

/** Install disk state without losing same-owner identities received during boot. */
export function mergeReception(
  persisted: CountedReception,
  pending: CountedReception,
): CountedReception {
  const keys = new Set([
    ...Object.keys(persisted.ledger),
    ...Object.keys(pending.ledger),
  ]);
  const ledger = Object.fromEntries(
    [...keys].map((key) => {
      const saved = persisted.ledger[key] ?? {};
      const live = pending.ledger[key] ?? {};
      return [
        key,
        Object.fromEntries(
          [...new Set([...Object.keys(saved), ...Object.keys(live)])].map(
            (id) => {
              const old = saved[id],
                incoming = live[id];
              return [
                id,
                old && incoming
                  ? {
                      ...incoming,
                      ...old,
                      seen: old.seen || incoming.seen,
                      suppressed: old.suppressed || incoming.suppressed,
                      expiresAtMs: old.expiresAtMs ?? incoming.expiresAtMs,
                      late: old.late || incoming.late,
                      mentioned: old.mentioned || incoming.mentioned,
                      messageAliases: validMessageAliases(
                        [
                          ...(old.messageAliases ?? []),
                          ...(incoming.messageAliases ?? []),
                        ],
                        id,
                      ),
                      receivedAt:
                        old.receivedAt === undefined
                          ? incoming.receivedAt
                          : incoming.receivedAt === undefined
                            ? old.receivedAt
                            : Math.min(old.receivedAt, incoming.receivedAt),
                      arrivalKind: old.arrivalKind ?? incoming.arrivalKind,
                    }
                  : (old ?? incoming),
              ];
            },
          ),
        ),
      ];
    }),
  );
  const overflow = Object.fromEntries(
    [
      ...new Set([
        ...Object.keys(persisted.overflowUnread),
        ...Object.keys(pending.overflowUnread),
      ]),
    ].map((key) => [
      key,
      Math.min(
        1_000_000,
        (persisted.overflowUnread[key] ?? 0) +
          (pending.overflowUnread[key] ?? 0),
      ),
    ]),
  );
  let merged = emptyReception(
    ledger,
    overflow,
    persisted.accountSeen || pending.accountSeen
      ? Object.fromEntries(
          [
            ...new Set([
              ...Object.keys(persisted.accountSeen ?? {}),
              ...Object.keys(pending.accountSeen ?? {}),
            ]),
          ].map((key) => [
            key,
            [
              ...new Set([
                ...(persisted.accountSeen?.[key] ?? []),
                ...(pending.accountSeen?.[key] ?? []),
              ]),
            ],
          ]),
        )
      : undefined,
  );
  // Disk may already know a binding while startup callbacks still carry an alias.
  for (const [key, entries] of Object.entries(merged.ledger)) {
    merged = reconcileHistory(
      merged,
      key,
      Object.entries(entries)
        .filter(([, entry]) => entry.messageAliases?.length)
        .map(([id, entry]) => ({
          id,
          fromMe: false,
          fileMessageAliases: entry.messageAliases,
        })),
      false,
    );
  }
  const unread = Object.fromEntries(
    [
      ...new Set([
        ...Object.keys(persisted.unread),
        ...Object.keys(pending.unread),
        ...Object.keys(merged.unreadIds),
        ...Object.keys(merged.overflowUnread),
      ]),
    ].map((key) => [
      key,
      Math.max(
        0,
        (persisted.unread[key] ?? 0) - trackedUnread(persisted, key),
      ) +
        Math.max(0, (pending.unread[key] ?? 0) - trackedUnread(pending, key)) +
        trackedUnread(merged, key),
    ]),
  );
  return {
    ...merged,
    unread,
    reception: Object.fromEntries(
      Object.entries({ ...persisted.reception, ...pending.reception }).filter(
        ([key]) => Object.prototype.hasOwnProperty.call(merged.ledger, key),
      ),
    ),
  };
}

export function receiveIdentity(
  state: ReceptionState,
  key: string,
  event: ReceptionMetadata,
  mentioned = false,
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
  const eventId = event.event_id;
  if (!eventId || eventId.length > 200) return state;
  const id = receptionIdentity(state.ledger[key] ?? {}, eventId);
  const old = Object.prototype.hasOwnProperty.call(state.ledger[key] ?? {}, id)
    ? state.ledger[key][id]
    : undefined;
  if (old) {
    let next = state;
    if (
      (old.receivedAt === undefined && event.received_at !== undefined) ||
      (old.expiresAtMs === undefined && event.expires_at_ms != null) ||
      (mentioned && !old.mentioned && !event.own_device)
    ) {
      next = {
        ...state,
        ledger: {
          ...state.ledger,
          [key]: {
            ...state.ledger[key],
            [id]: {
              ...old,
              expiresAtMs: old.expiresAtMs ?? event.expires_at_ms ?? undefined,
              mentioned: !event.own_device && (old.mentioned || mentioned),
              receivedAt: old.receivedAt ?? event.received_at,
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
      ...(mentioned && !event.own_device ? { mentioned: true } : {}),
      seen:
        event.own_device === true ||
        state.accountSeen?.[key]?.includes(id) === true,
      ...(Number.isFinite(event.expires_at_ms)
        ? { expiresAtMs: event.expires_at_ms! }
        : {}),
      late: kind !== "live",
      receivedAt: event.received_at,
      arrivalKind: kind,
    },
  };
  const next = emptyReception(
    { ...state.ledger, [key]: entries },
    state.overflowUnread,
    state.accountSeen,
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
  for (const requested of ids) {
    const id = receptionIdentity(entries, requested);
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
  }
  return {
    ...emptyReception(
      { ...state.ledger, [key]: entries },
      state.overflowUnread,
      state.accountSeen,
    ),
    reception: state.reception,
  };
}
export function reconcileHistory(
  state: ReceptionState,
  key: string,
  rows: {
    id: string | null;
    fromMe: boolean;
    mentioned?: boolean;
    fileMessageAliases?: string[];
    expiresAtMs?: number | null;
    suppressed?: boolean;
  }[],
  discover: boolean,
): ReceptionState {
  const entries = { ...(state.ledger[key] ?? {}) };
  for (const row of rows) {
    if (!row.id || row.id.length > 200) continue;
    const aliases = validMessageAliases(row.fileMessageAliases, row.id);
    const ids = new Set([row.id, ...aliases]);
    // Preserve previously learned aliases when history pages expose a subset.
    for (const id of ids) {
      if (Object.prototype.hasOwnProperty.call(entries, id))
        for (const alias of entries[id].messageAliases ?? []) ids.add(alias);
    }
    const related = [...ids].flatMap((id) =>
      Object.prototype.hasOwnProperty.call(entries, id) ? [entries[id]] : [],
    );
    const received = related.flatMap((entry) =>
      entry.receivedAt === undefined ? [] : [entry.receivedAt],
    );
    const receivedAt = received.length ? Math.min(...received) : undefined;
    const oldest = related.find((entry) => entry.receivedAt === receivedAt);
    const knownRead =
      related.some((entry) => entry.seen) ||
      [...ids].some((id) => state.accountSeen?.[key]?.includes(id));
    const retainedAliases = validMessageAliases([...ids], row.id);
    for (const id of ids) if (id !== row.id) delete entries[id];
    Object.defineProperty(entries, row.id, {
      value: {
        ...(retainedAliases.length ? { messageAliases: retainedAliases } : {}),
        ...(!row.fromMe &&
        (row.mentioned || related.some((entry) => entry.mentioned))
          ? { mentioned: true }
          : {}),
        ...(row.suppressed || related.some((entry) => entry.suppressed)
          ? { suppressed: true }
          : {}),
        ...(Number.isFinite(row.expiresAtMs)
          ? { expiresAtMs: row.expiresAtMs! }
          : related.some((entry) => entry.expiresAtMs !== undefined)
            ? {
                expiresAtMs: related.find(
                  (entry) => entry.expiresAtMs !== undefined,
                )!.expiresAtMs,
              }
            : {}),
        seen: row.fromMe || knownRead || (!related.length && !discover),
        late: related.length ? related.some((entry) => entry.late) : true,
        ...(receivedAt === undefined ? {} : { receivedAt }),
        arrivalKind:
          oldest?.arrivalKind ?? related[0]?.arrivalKind ?? "history_sync",
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
      state.accountSeen,
    ),
    reception: state.reception,
  };
}

/** Exact monotone sibling reads never advance a timestamp or another conversation. */
export function validAccountSeenIdentity(id: string): boolean {
  return (
    id.length > 0 &&
    id.length <= 200 &&
    !Array.from(id).some((character) => {
      const code = character.codePointAt(0)!;
      return code < 32 || (code >= 127 && code <= 159);
    })
  );
}

export function applyAccountSeen(
  state: ReceptionState,
  key: string,
  ids: string[],
): ReceptionState {
  const existing = state.accountSeen?.[key] ?? [];
  const valid = ids.filter(validAccountSeenIdentity);
  const count = Object.values(state.accountSeen ?? {}).reduce(
    (total, entries) => total + entries.length,
    0,
  );
  const additions = [...new Set(valid)]
    .filter((id) => !existing.includes(id))
    .slice(0, Math.max(0, 65536 - count));
  const accountSeen = {
    ...state.accountSeen,
    [key]: [...existing, ...additions],
  };
  return {
    ...viewIdentities({ ...state, accountSeen }, key, valid),
    accountSeen,
  };
}

/** Merge identity counts without losing legacy-host unread counters. */
export function receptionPatch(
  state: ReceptionState & { unread: Record<string, number> },
  next: ReceptionState,
): Partial<ReceptionState & { unread: Record<string, number> }> {
  if (next === state) return {};
  const unread = { ...state.unread };
  for (const key of new Set([
    ...Object.keys(state.unreadIds),
    ...Object.keys(next.unreadIds),
    ...Object.keys(state.overflowUnread),
    ...Object.keys(next.overflowUnread),
  ])) {
    unread[key] = Math.max(
      0,
      (unread[key] ?? 0) + trackedUnread(next, key) - trackedUnread(state, key),
    );
  }
  return { ...next, unread };
}

/** Clear explicit sources in one pass so ledger bounding cannot reintroduce overflow. */
export function markConversationsRead(
  state: ReceptionState & { unread: Record<string, number> },
  keys: string[],
) {
  const selected = new Set(keys);
  const ledger = Object.fromEntries(
    Object.entries(state.ledger).map(([key, entries]) => [
      key,
      selected.has(key)
        ? Object.fromEntries(
            Object.entries(entries).map(([id, entry]) => [
              id,
              entry.seen || entry.suppressed ? entry : { ...entry, seen: true },
            ]),
          )
        : entries,
    ]),
  );
  const next = emptyReception(
    ledger,
    Object.fromEntries(
      Object.entries(state.overflowUnread).filter(
        ([key]) => !selected.has(key),
      ),
    ),
    state.accountSeen,
  );
  return {
    ...next,
    reception: state.reception,
    unread: {
      ...state.unread,
      ...Object.fromEntries(keys.map((key) => [key, 0])),
    },
  };
}

/** Keep bounded removal evidence so delayed live/history replay cannot restore unread. */
export function suppressIdentities(
  state: ReceptionState,
  key: string,
  ids: string[],
): ReceptionState {
  const entries = { ...state.ledger[key] };
  let changed = false;
  for (const requested of ids) {
    const id = receptionIdentity(entries, requested);
    const entry = Object.prototype.hasOwnProperty.call(entries, id)
      ? entries[id]
      : undefined;
    if (!entry || entry.suppressed) continue;
    entries[id] = { ...entry, suppressed: true };
    changed = true;
  }
  if (!changed) return state;
  return {
    ...emptyReception(
      { ...state.ledger, [key]: entries },
      state.overflowUnread,
      state.accountSeen,
    ),
    reception: state.reception,
  };
}

/** Expiry evidence survives message-cache eviction; absence from a page proves nothing. */
export function expireReception(
  state: ReceptionState,
  now: number,
): ReceptionState {
  let changed = false;
  const ledger = Object.fromEntries(
    Object.entries(state.ledger).map(([key, entries]) => {
      const expired = Object.keys(entries).filter(
        (id) =>
          !entries[id].suppressed &&
          entries[id].expiresAtMs !== undefined &&
          entries[id].expiresAtMs! <= now,
      );
      if (!expired.length) return [key, entries];
      changed = true;
      const next = { ...entries };
      for (const id of expired) next[id] = { ...entries[id], suppressed: true };
      return [key, next];
    }),
  );
  return changed
    ? {
        ...emptyReception(ledger, state.overflowUnread, state.accountSeen),
        reception: state.reception,
      }
    : state;
}
