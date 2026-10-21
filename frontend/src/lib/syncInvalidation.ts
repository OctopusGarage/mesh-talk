/** Local fan-out after the chat owner validates a native invalidation. */
export type SyncInvalidation = {
  owner: string;
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
