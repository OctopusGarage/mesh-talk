import type { ReceptionLedger, ConversationReception } from "@/store/reception";
import type {
  ReceptionCursor,
  ReceptionScope,
} from "@/store/receptionDelivery";

export interface ReceptionSnapshot {
  cursor: ReceptionCursor;
  ledger: ReceptionLedger;
  overflowUnread: Record<string, number>;
  unread: Record<string, number>;
  pendingUnknownUnread: Record<string, number>;
  historyFullBaselines: string[];
  reception: Record<string, ConversationReception>;
}

const MAX_SNAPSHOT_CHARS = 8_000_000;
function key(scope: ReceptionScope) {
  return `mesh-talk-reception-commit-v1:${scope.owner}:${scope.account_id}:${scope.device_id}`;
}
function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function counts(value: unknown, limit: number) {
  return (
    object(value) &&
    Object.keys(value).length <= limit &&
    Object.entries(value).every(
      ([id, count]) =>
        id.length > 0 &&
        id.length <= 200 &&
        Number.isSafeInteger(count) &&
        Number(count) >= 0,
    )
  );
}
function valid(value: unknown): value is ReceptionSnapshot {
  if (
    !object(value) ||
    !object(value.cursor) ||
    typeof value.cursor.epoch !== "string" ||
    !/^[0-9a-f]{32}$/.test(value.cursor.epoch) ||
    typeof value.cursor.sequence !== "string" ||
    !/^(0|[1-9][0-9]{0,19})$/.test(value.cursor.sequence) ||
    BigInt(value.cursor.sequence) > 18446744073709551615n ||
    !counts(value.unread, 4096) ||
    !counts(value.overflowUnread, 4096) ||
    !counts(value.pendingUnknownUnread, 256) ||
    !Array.isArray(value.historyFullBaselines) ||
    value.historyFullBaselines.length > 4096 ||
    !value.historyFullBaselines.every(
      (id) => typeof id === "string" && id.length <= 200,
    ) ||
    !object(value.reception) ||
    Object.keys(value.reception).length > 4096 ||
    !object(value.ledger) ||
    Object.keys(value.ledger).length > 64
  )
    return false;
  let entries = 0;
  for (const [conversation, rows] of Object.entries(value.ledger)) {
    if (
      conversation.length > 200 ||
      !object(rows) ||
      Object.keys(rows).length > 1024
    )
      return false;
    for (const [id, row] of Object.entries(rows)) {
      if (
        ++entries > 8192 ||
        id.length > 200 ||
        !object(row) ||
        typeof row.seen !== "boolean" ||
        typeof row.late !== "boolean"
      )
        return false;
      if (row.mentioned !== undefined && typeof row.mentioned !== "boolean")
        return false;
      if (row.receivedAt !== undefined && !Number.isFinite(row.receivedAt))
        return false;
      if (
        row.arrivalKind !== undefined &&
        !["live", "catch_up", "history_sync"].includes(String(row.arrivalKind))
      )
        return false;
      if (
        row.messageAliases !== undefined &&
        (!Array.isArray(row.messageAliases) ||
          row.messageAliases.length > 64 ||
          !row.messageAliases.every(
            (alias) => typeof alias === "string" && alias.length <= 200,
          ))
      )
        return false;
    }
  }
  return Object.entries(value.reception).every(
    ([id, entry]) =>
      id.length <= 200 &&
      object(entry) &&
      ["idle", "catching_up", "waiting"].includes(String(entry.phase)) &&
      Number.isSafeInteger(entry.received) &&
      Number(entry.received) >= 0 &&
      (entry.batchId === undefined ||
        (typeof entry.batchId === "string" && entry.batchId.length <= 200)) &&
      (entry.checkedAt === undefined || Number.isFinite(entry.checkedAt)) &&
      (entry.processing === undefined || typeof entry.processing === "boolean"),
  );
}

/** A single storage replacement commits the existing read owner and its receipt prefix. */
export function saveReceptionSnapshot(
  scope: ReceptionScope,
  snapshot: ReceptionSnapshot,
): void {
  if (!valid(snapshot))
    throw new Error("Reception snapshot exceeds its bounds or is invalid");
  const encoded = JSON.stringify(snapshot);
  if (encoded.length > MAX_SNAPSHOT_CHARS)
    throw new Error("Reception snapshot exceeds storage budget");
  // Quota/security failures must reach the pump: an uncommitted cursor cannot be ACKed.
  localStorage.setItem(key(scope), encoded);
}
export function loadReceptionSnapshot(
  scope: ReceptionScope,
): ReceptionSnapshot | null {
  const encoded = localStorage.getItem(key(scope));
  if (encoded === null) return null;
  if (encoded.length > MAX_SNAPSHOT_CHARS)
    throw new Error("Stored reception snapshot exceeds budget");
  const parsed: unknown = JSON.parse(encoded);
  if (!valid(parsed)) throw new Error("Stored reception snapshot is invalid");
  return parsed;
}
