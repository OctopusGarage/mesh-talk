import { readWithDeadline } from "./readDeadline";

/** Ephemeral input to one qualified invalidation, never a cross-event cache. */
export function createRosterObservation<T>(
  owner: string,
  observe: () => Promise<T>,
  current: () => boolean = () => true,
) {
  let pending: Promise<{ value: T; requestedAt: number }> | undefined;
  return {
    current,
    read(requestOwner: string) {
      if (requestOwner !== owner)
        return Promise.reject(new Error("Roster observation owner mismatch"));
      if (!current())
        return Promise.reject(new Error("Roster observation runtime retired"));
      return (pending ??= readWithDeadline(() => {
        const requestedAt = performance.now();
        return observe().then((value) => {
          if (!current()) throw new Error("Roster observation runtime retired");
          return { value, requestedAt };
        });
      }));
    },
  };
}
