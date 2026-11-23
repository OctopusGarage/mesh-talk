import type { createRosterObservation } from "./rosterObservation";
import type { RosterObservation } from "./types";
export type RosterTicket = ReturnType<
  typeof createRosterObservation<RosterObservation>
>;
/** Local fan-out after the chat owner validates a native invalidation. */
export type SyncInvalidation = {
  owner: string;
  rosterObservation?: RosterTicket;
  scope?: "documents" | "sync_status" | "read_receipts";
  kind: "state" | "roster" | "delivery" | "files" | "contacts";
};
const listeners = new Set<(change: SyncInvalidation) => void>();
export function subscribeSyncInvalidation(
  listener: (change: SyncInvalidation) => void,
) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
/** Qualified status and receipt scopes exclude generic saved-data readers. Legacy, unknown
 * and missing scopes retain conservative document/preference recovery. */
export function isDocumentInvalidation(change: SyncInvalidation): boolean {
  return (
    change.kind === "state" &&
    change.scope !== "sync_status" &&
    change.scope !== "read_receipts"
  );
}

/** One lifecycle-owned subscription for saved documents and preferences. */
export function subscribeDocumentInvalidation(
  owner: string,
  refresh: () => void,
) {
  return subscribeSyncInvalidation((change) => {
    if (change.owner === owner && isDocumentInvalidation(change)) refresh();
  });
}

export function notifySyncInvalidation(change: SyncInvalidation) {
  for (const listener of listeners) listener(change);
}

let pushOwner: string | null = null;
export function setPushStateSupport(owner: string | null) {
  pushOwner = owner;
}
export function supportsPushState(owner?: string | null) {
  return pushOwner !== null && (owner === undefined || pushOwner === owner);
}
