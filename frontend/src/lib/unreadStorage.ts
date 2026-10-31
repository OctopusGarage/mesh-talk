const PREFIX = "mesh-talk-unread-v1";
const PENDING_PREFIX = "mesh-talk-pending-unread-v1";
const MAX_SNAPSHOT_BYTES = 1_000_000;

type ReceptionSnapshot = Partial<{
  unread: Record<string, number>;
  pendingUnknownUnread: Record<string, number>;
  ledger: ReceptionLedger;
  overflowUnread: Record<string, number>;
  historyFullBaselines: string[];
}>;
let pendingSnapshot:
  { owner: string; account: string; value: ReceptionSnapshot } | undefined;
let persistenceTimer: ReturnType<typeof setTimeout> | undefined;

/** One latest snapshot per current account; a fixed short window cannot starve
 * persistence under continuous reception. Retirement and restoration flush it. */
export function queueReceptionPersistence(
  owner: string,
  account: string,
  value: ReceptionSnapshot,
) {
  if (!Object.keys(value).length) return;
  if (
    pendingSnapshot &&
    (pendingSnapshot.owner !== owner || pendingSnapshot.account !== account)
  )
    flushReceptionPersistence();
  pendingSnapshot = {
    owner,
    account,
    value: { ...pendingSnapshot?.value, ...value },
  };
  persistenceTimer ??= setTimeout(flushReceptionPersistence, 100);
}
export function flushReceptionPersistence() {
  clearTimeout(persistenceTimer);
  persistenceTimer = undefined;
  const pending = pendingSnapshot;
  pendingSnapshot = undefined;
  if (!pending) return;
  const { owner, account, value } = pending;
  if (value.unread) saveUnread(owner, account, value.unread);
  if (value.pendingUnknownUnread)
    savePendingUnread(owner, account, value.pendingUnknownUnread);
  if (value.ledger) saveReceptionLedger(owner, account, value.ledger);
  if (value.overflowUnread)
    saveUnreadOverflow(owner, account, value.overflowUnread);
  if (value.historyFullBaselines)
    saveFullHistoryBaselines(owner, account, value.historyFullBaselines);
}

function key(owner: string, accountId: string): string {
  return `${PREFIX}:${owner}:${accountId}`;
}

function pendingKey(owner: string, accountId: string): string {
  return `${PENDING_PREFIX}:${owner}:${accountId}`;
}

function validEntry(conversation: string, count: unknown): count is number {
  return (
    conversation.length <= 200 &&
    (conversation.startsWith("account:") ||
      conversation.startsWith("channel:")) &&
    Number.isSafeInteger(count) &&
    Number(count) > 0
  );
}

/** Persist only opaque conversation keys and counts, scoped to both host and mesh account. */
export function loadUnread(
  owner: string,
  accountId: string,
): Record<string, number> {
  flushReceptionPersistence();
  try {
    const encoded = localStorage.getItem(key(owner, accountId));
    if (!encoded || encoded.length > MAX_SNAPSHOT_BYTES) return {};
    const parsed: unknown = JSON.parse(encoded);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
      return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(([conversation, count]) =>
        validEntry(conversation, count),
      ),
    );
  } catch {
    return {};
  }
}

export function saveUnread(
  owner: string,
  accountId: string,
  unread: Record<string, number>,
): void {
  flushReceptionPersistence();
  try {
    const entries = Object.entries(unread).filter(([conversation, count]) =>
      validEntry(conversation, count),
    );
    localStorage.setItem(
      key(owner, accountId),
      JSON.stringify(Object.fromEntries(entries)),
    );
  } catch {
    // Storage may be unavailable; the in-memory counters still work this session.
  }
}

/** Keep early events until discovery maps their sender device to a conversation. */
export function loadPendingUnread(
  owner: string,
  accountId: string,
): Record<string, number> {
  flushReceptionPersistence();
  try {
    const encoded = localStorage.getItem(pendingKey(owner, accountId));
    if (!encoded || encoded.length > MAX_SNAPSHOT_BYTES) return {};
    const parsed: unknown = JSON.parse(encoded);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
      return {};
    return Object.fromEntries(
      Object.entries(parsed)
        .filter(
          ([sender, count]) =>
            sender.length > 0 &&
            sender.length <= 200 &&
            Number.isSafeInteger(count) &&
            Number(count) > 0 &&
            Number(count) <= 1_000_000,
        )
        .slice(0, 256),
    );
  } catch {
    return {};
  }
}

export function savePendingUnread(
  owner: string,
  accountId: string,
  pending: Record<string, number>,
): void {
  flushReceptionPersistence();
  try {
    const entries = Object.entries(pending)
      .filter(
        ([sender, count]) =>
          sender.length > 0 &&
          sender.length <= 200 &&
          Number.isSafeInteger(count) &&
          count > 0 &&
          count <= 1_000_000,
      )
      .slice(0, 256);
    localStorage.setItem(
      pendingKey(owner, accountId),
      JSON.stringify(Object.fromEntries(entries)),
    );
  } catch {
    // Storage may be unavailable; in-memory discovery still works this session.
  }
}

const LEDGER_PREFIX = "mesh-talk-reception-v2";
const LEDGER_SNAPSHOT_LIMIT = 3_000_000;
import {
  boundLedger,
  validMessageAliases,
  type ReceptionLedger,
  type ReceptionEntry,
} from "@/store/reception";

export function loadReceptionLedger(
  owner: string,
  accountId: string,
): ReceptionLedger {
  flushReceptionPersistence();
  try {
    const encoded = localStorage.getItem(
      `${LEDGER_PREFIX}:${owner}:${accountId}`,
    );
    if (!encoded || encoded.length > LEDGER_SNAPSHOT_LIMIT) return {};
    const parsed: unknown = JSON.parse(encoded);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
      return {};
    const ledger: ReceptionLedger = {};
    for (const [conversation, entries] of Object.entries(parsed)) {
      if (
        !validEntry(conversation, 1) ||
        !entries ||
        typeof entries !== "object" ||
        Array.isArray(entries)
      )
        continue;
      const valid: Record<string, ReceptionEntry> = Object.create(null);
      for (const [id, entry] of Object.entries(entries)) {
        if (!id || id.length > 200 || !entry || typeof entry !== "object")
          continue;
        const e = entry as ReceptionEntry;
        if (typeof e.seen !== "boolean" || typeof e.late !== "boolean")
          continue;
        const aliases = validMessageAliases(e.messageAliases, id);
        valid[id] = {
          ...(aliases.length ? { messageAliases: aliases } : {}),
          ...(e.mentioned === true ? { mentioned: true } : {}),
          ...(e.suppressed === true ? { suppressed: true } : {}),
          ...(Number.isFinite(e.expiresAtMs)
            ? { expiresAtMs: e.expiresAtMs }
            : {}),
          seen: e.seen,
          late: e.late,
          ...(Number.isFinite(e.receivedAt)
            ? { receivedAt: e.receivedAt }
            : {}),
          ...(["live", "catch_up", "history_sync"].includes(e.arrivalKind ?? "")
            ? { arrivalKind: e.arrivalKind }
            : {}),
        };
      }
      ledger[conversation] = valid;
    }
    return boundLedger(ledger);
  } catch {
    return {};
  }
}
export function saveReceptionLedger(
  owner: string,
  accountId: string,
  ledger: ReceptionLedger,
): void {
  flushReceptionPersistence();
  try {
    let bounded = boundLedger(ledger);
    // Optional bindings yield before canonical unread state when the snapshot is full.
    let encoded = JSON.stringify(bounded);
    if (encoded.length > LEDGER_SNAPSHOT_LIMIT) {
      bounded = Object.fromEntries(
        Object.entries(bounded).map(([conversation, entries]) => [
          conversation,
          Object.fromEntries(
            Object.entries(entries).map(([id, entry]) => {
              const { messageAliases: _aliases, ...metadata } = entry;
              return [id, metadata];
            }),
          ),
        ]),
      );
      encoded = JSON.stringify(bounded);
    }
    if (encoded.length > LEDGER_SNAPSHOT_LIMIT) return;
    localStorage.setItem(`${LEDGER_PREFIX}:${owner}:${accountId}`, encoded);
  } catch {
    /* Private mode/quota failures must not interrupt reception. */
  }
}

const OVERFLOW_PREFIX = "mesh-talk-unread-overflow-v2";
export function loadUnreadOverflow(
  owner: string,
  accountId: string,
): Record<string, number> {
  flushReceptionPersistence();
  try {
    const encoded = localStorage.getItem(
      `${OVERFLOW_PREFIX}:${owner}:${accountId}`,
    );
    if (!encoded || encoded.length > MAX_SNAPSHOT_BYTES) return {};
    const parsed: unknown = JSON.parse(encoded);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
      return {};
    return Object.fromEntries(
      Object.entries(parsed)
        .filter(
          ([key, count]) =>
            validEntry(key, count) && Number(count) <= 1_000_000,
        )
        .slice(-4096),
    );
  } catch {
    return {};
  }
}
export function saveUnreadOverflow(
  owner: string,
  accountId: string,
  overflow: Record<string, number>,
): void {
  flushReceptionPersistence();
  try {
    const entries = Object.entries(overflow)
      .filter(([key, count]) => validEntry(key, count) && count <= 1_000_000)
      .slice(-4096);
    localStorage.setItem(
      `${OVERFLOW_PREFIX}:${owner}:${accountId}`,
      JSON.stringify(Object.fromEntries(entries)),
    );
  } catch {
    /* In-memory overflow remains usable if storage is unavailable. */
  }
}

const FULL_BASELINE_PREFIX = "mesh-talk-history-coverage-v2";
export function loadFullHistoryBaselines(
  owner: string,
  accountId: string,
): string[] {
  flushReceptionPersistence();
  try {
    const encoded = localStorage.getItem(
      `${FULL_BASELINE_PREFIX}:${owner}:${accountId}`,
    );
    if (!encoded || encoded.length > MAX_SNAPSHOT_BYTES) return [];
    const parsed: unknown = JSON.parse(encoded);
    return Array.isArray(parsed)
      ? parsed
          .filter(
            (key): key is string =>
              typeof key === "string" && validEntry(key, 1),
          )
          .slice(-4096)
      : [];
  } catch {
    return [];
  }
}
export function saveFullHistoryBaselines(
  owner: string,
  accountId: string,
  keys: string[],
): void {
  flushReceptionPersistence();
  try {
    localStorage.setItem(
      `${FULL_BASELINE_PREFIX}:${owner}:${accountId}`,
      JSON.stringify(keys.filter((key) => validEntry(key, 1)).slice(-4096)),
    );
  } catch {
    /* Coverage remains available for this session if storage is unavailable. */
  }
}
