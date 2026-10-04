import { create } from "zustand";
import { contactPolicy } from "@/lib/api";
import type { HiddenContact, HiddenContactsSnapshot } from "@/lib/types";

interface ContactPolicyState {
  owner: string | null;
  contacts: Record<string, HiddenContact>;
  loaded: boolean;
  loading: boolean;
  busy: boolean;
  error: "load" | "save" | null;
  reset: () => void;
  clearSaveError: () => void;
  load: (owner: string) => Promise<void>;
  change: (
    owner: string,
    account: string,
    hidden: boolean,
    name: string,
  ) => Promise<boolean>;
}
let generation = 0;
const empty = () => ({
  owner: null,
  contacts: {},
  loaded: false,
  loading: false,
  busy: false,
  error: null,
});
function contactsFrom(snapshot: HiddenContactsSnapshot, owner: string) {
  if (snapshot?.owner !== owner || !Array.isArray(snapshot.contacts))
    throw new Error("Invalid contact policy response");
  const entries = snapshot.contacts.map((c) => {
    if (typeof c.account_id !== "string" || typeof c.name !== "string")
      throw new Error("Invalid contact policy entry");
    return [c.account_id, c] as const;
  });
  return Object.fromEntries(entries);
}

/** Presentation only: never remove a peer from the protocol/chat roster. */
export const useContactPolicy = create<ContactPolicyState>((set, get) => ({
  ...empty(),
  clearSaveError: () => {
    if (get().error === "save") set({ error: null });
  },
  reset: () => {
    generation++;
    set(empty());
  },
  load: async (owner) => {
    if (get().busy && get().owner === owner) return;
    const request = ++generation;
    const sameOwner = get().owner === owner;
    set({ ...(sameOwner ? {} : empty()), owner, loading: true, error: null });
    try {
      const contacts = contactsFrom(await contactPolicy.get(owner), owner);
      if (request === generation)
        set({ contacts, loaded: true, loading: false });
    } catch {
      if (request === generation) set({ loading: false, error: "load" });
    }
  },
  change: async (owner, account, hidden, name) => {
    if (get().owner !== owner || !get().loaded || get().loading || get().busy)
      return false;
    const request = generation;
    set({ busy: true, error: null });
    try {
      const contacts = contactsFrom(
        await contactPolicy.set(owner, account, hidden, name),
        owner,
      );
      if (request !== generation) return false;
      set({ contacts, busy: false });
      return true;
    } catch {
      if (request === generation) set({ busy: false, error: "save" });
      return false;
    }
  },
}));
