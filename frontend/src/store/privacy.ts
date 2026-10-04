import { create } from "zustand";
import { privacy } from "@/lib/api";
import type { PrivacySnapshot } from "@/lib/types";

interface PrivacyState {
  owner: string | null;
  snapshot: PrivacySnapshot | null;
  loaded: boolean;
  loading: boolean;
  busy: boolean;
  error: "load" | "save" | null;
  reset: () => void;
  load: (owner: string) => Promise<void>;
  setInvisible: (owner: string, invisible: boolean) => Promise<boolean>;
  setAllowed: (
    owner: string,
    account: string,
    allowed: boolean,
  ) => Promise<boolean>;
}
let generation = 0;
const empty = () => ({
  owner: null,
  snapshot: null,
  loaded: false,
  loading: false,
  busy: false,
  error: null,
});
function validate(value: PrivacySnapshot, owner: string): PrivacySnapshot {
  if (
    !value ||
    value.owner !== owner ||
    value.version !== 1 ||
    typeof value.invisible !== "boolean" ||
    !Array.isArray(value.allowed_accounts) ||
    value.allowed_accounts.length > 1024
  )
    throw new Error("Invalid privacy snapshot");
  const ids = new Set<string>();
  for (const account of value.allowed_accounts) {
    if (
      !account ||
      typeof account.id !== "string" ||
      !/^[0-9a-f]{32}$/.test(account.id) ||
      typeof account.name !== "string" ||
      [...account.name].length > 256 ||
      /[\p{Cc}]/u.test(account.name) ||
      (account.source !== "Manual" && account.source !== "Initiated") ||
      ids.has(account.id)
    )
      throw new Error("Invalid privacy permission");
    ids.add(account.id);
  }
  return value;
}

/** Backend owns enforcement. Never show a change as saved before its acknowledgement. */
export const usePrivacy = create<PrivacyState>((set, get) => {
  const change = async (
    owner: string,
    operation: () => Promise<PrivacySnapshot>,
  ) => {
    if (get().owner !== owner || !get().loaded || get().loading || get().busy)
      return false;
    const request = generation;
    set({ busy: true, error: null });
    try {
      const snapshot = validate(await operation(), owner);
      if (request !== generation) return false;
      set({ snapshot, busy: false });
      return true;
    } catch {
      if (request === generation) set({ busy: false, error: "save" });
      return false;
    }
  };
  return {
    ...empty(),
    reset: () => {
      generation++;
      set(empty());
    },
    load: async (owner) => {
      if (get().owner === owner && get().busy) return;
      const request = ++generation;
      set({
        ...(get().owner === owner ? {} : empty()),
        owner,
        loading: true,
        error: null,
      });
      try {
        const snapshot = validate(await privacy.get(owner), owner);
        if (request === generation)
          set({ snapshot, loaded: true, loading: false });
      } catch {
        if (request === generation)
          set({ loaded: false, loading: false, error: "load" });
      }
    },
    setInvisible: (owner, invisible) =>
      change(owner, () => privacy.setInvisible(owner, invisible)),
    setAllowed: (owner, account, allowed) =>
      change(owner, () => privacy.setAllowed(owner, account, allowed)),
  };
});
