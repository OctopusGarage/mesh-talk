import { useEffect } from "react";
import { create } from "zustand";
import {
  accountProfile,
  type AccountProfileView,
} from "@/lib/api/accountProfile";
import { useAuth } from "./auth";
import { captureRuntimeOwner } from "./ownership";
import { readWithDeadline } from "@/lib/readDeadline";

const requests = new Map<string, Promise<AccountProfileView | null>>();
const versions = new Map<string, number>();
let epoch = 0;
interface ProfilesState {
  epoch: number;
  profiles: Record<string, AccountProfileView | null>;
  load: (accountId: string) => Promise<AccountProfileView | null>;
  invalidate: (accountId: string) => Promise<void>;
  clear: () => void;
}
/** Only profiles requested by visible UI are read; live events refresh those observations. */
export const useAccountProfiles = create<ProfilesState>((set, get) => ({
  profiles: {},
  epoch: 0,
  clear: () => {
    epoch++;
    requests.clear();
    versions.clear();
    set({ profiles: {}, epoch });
  },
  load: (accountId) => {
    const existing = requests.get(accountId);
    if (existing) return existing;
    const lease = captureRuntimeOwner();
    if (!accountId || !lease.current()) return Promise.resolve(null);
    const requestEpoch = epoch;
    const version = (versions.get(accountId) ?? 0) + 1;
    versions.set(accountId, version);
    // A subscriber must receive the same accepted snapshot as the cache, even when
    // an event replaces its initial request while that request is still in flight.
    const authoritative = ():
      AccountProfileView | null | Promise<AccountProfileView | null> => {
      if (!lease.current() || epoch !== requestEpoch) return null;
      const latest = requests.get(accountId);
      if (latest && latest !== pending) return latest;
      return get().profiles[accountId] ?? null;
    };
    const pending: Promise<AccountProfileView | null> = readWithDeadline(() =>
      accountProfile.get(lease.owner!, accountId),
    )
      .then((value) => {
        if (
          lease.current() &&
          epoch === requestEpoch &&
          versions.get(accountId) === version
        )
          set((state) => ({
            profiles: { ...state.profiles, [accountId]: value },
          }));
        return authoritative();
      })
      .catch((error) => {
        if (
          lease.current() &&
          epoch === requestEpoch &&
          versions.get(accountId) !== version
        )
          return authoritative();
        throw error;
      })
      .finally(() => {
        if (requests.get(accountId) === pending) requests.delete(accountId);
      });
    requests.set(accountId, pending);
    return pending;
  },
  invalidate: (accountId) => {
    if (
      !Object.prototype.hasOwnProperty.call(get().profiles, accountId) &&
      !requests.has(accountId) &&
      !versions.has(accountId)
    )
      return Promise.resolve();
    requests.delete(accountId);
    return get()
      .load(accountId)
      .then(() => {})
      .catch(() => {});
  },
}));
useAuth.subscribe((state, previous) => {
  if (
    state.user?.id !== previous.user?.id ||
    state.generation !== previous.generation
  )
    useAccountProfiles.getState().clear();
});
export function useAccountProfile(accountId: string | undefined) {
  const owner = useAuth((state) => state.user?.id);
  const generation = useAuth((state) => state.generation);
  const profile = useAccountProfiles((state) =>
    accountId ? state.profiles[accountId] : undefined,
  );
  const runtimeEpoch = useAccountProfiles((state) => state.epoch);
  const missing = profile === undefined;
  useEffect(() => {
    if (accountId && owner && missing)
      void useAccountProfiles
        .getState()
        .load(accountId)
        .catch(() => {});
  }, [accountId, owner, generation, missing, runtimeEpoch]);
  return profile;
}
