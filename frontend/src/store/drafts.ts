import { create } from "zustand";
import { draftsApi } from "@/lib/api/drafts";
import type { DraftChange, DraftVersion } from "@/lib/api/drafts";
import type { MessageProtection } from "@/lib/types";
import { captureRuntimeOwner, currentRuntimeAccountId } from "./ownership";

type Scope = [owner: string, generation: number, key: string];
interface DraftAdmissionResult {
  accepted: boolean;
  cleared: boolean;
}
interface DraftState {
  owner: string | null;
  generation: number;
  texts: Record<string, string>;
  protections: Record<string, MessageProtection>;
  pending: Record<string, boolean>;
  loaded: boolean;
  error: "load" | "save" | null;
  start: (
    owner: string,
    generation: number,
    accountId?: string,
  ) => Promise<void>;
  refresh: () => Promise<void>;
  reset: () => void;
  discard: (...args: Scope) => void;
  edit: (...args: [...Scope, text: string]) => void;
  setProtection: (
    ...args: [...Scope, protection: MessageProtection | undefined]
  ) => void;
  touch: (...args: Scope) => void;
  captureProtection: (...args: Scope) => {
    protection?: MessageProtection;
    accept: () => boolean;
  };
  admit: (
    ...args: [...Scope, text: string, submit: () => Promise<boolean>]
  ) => Promise<DraftAdmissionResult> | null;
  flush: () => Promise<void>;
}

/** Owns durable text/policy and transient admission; none of its callbacks cross sessions. */
export function createDraftStore(api: Pick<typeof draftsApi, "load" | "save">) {
  let epoch = 0;
  let session = "";
  let dirty = false;
  let revisions: Record<string, number> = {};
  let versions: Record<string, DraftVersion> = {};
  let dirtyKeys = new Set<string>();
  let accountId: string | undefined;
  let refreshRequest = 0;
  let idle: ReturnType<typeof setTimeout> | undefined;
  let writes = Promise.resolve();
  const cancelTimers = () => {
    clearTimeout(idle);
    idle = undefined;
  };
  return create<DraftState>((set, get) => {
    const current = (owner: string, generation: number) => {
      const state = get();
      return (
        state.loaded && state.owner === owner && state.generation === generation
      );
    };
    const touch = (key: string) => {
      revisions[key] = (revisions[key] ?? 0) + 1;
    };
    const locallyChanged = (key: string) => {
      touch(key);
      dirtyKeys.add(key);
    };
    const changed = (immediate = false) => {
      dirty = true;
      cancelTimers();
      idle = setTimeout(() => void get().flush(), 10000);
      if (immediate) void get().flush();
    };
    return {
      owner: null,
      generation: 0,
      texts: {},
      protections: {},
      pending: {},
      loaded: false,
      error: null,
      discard: (owner, generation, key) => {
        if (get().owner !== owner || get().generation !== generation) return;
        touch(key);
        dirtyKeys.delete(key);
        const { texts, protections, pending } = get();
        const nextTexts = { ...texts };
        delete nextTexts[key];
        const nextProtections = { ...protections };
        delete nextProtections[key];
        const nextPending = { ...pending };
        delete nextPending[key];
        set({
          texts: nextTexts,
          protections: nextProtections,
          pending: nextPending,
        });
      },
      reset: () => {
        epoch++;
        cancelTimers();
        dirty = false;
        revisions = {};
        versions = {};
        dirtyKeys = new Set();
        accountId = undefined;
        session = "";
        writes = Promise.resolve();
        set({
          owner: null,
          texts: {},
          protections: {},
          pending: {},
          loaded: false,
          error: null,
        });
      },
      start: async (owner, generation, currentAccount) => {
        get().reset();
        accountId = currentAccount;
        const run = epoch;
        set({ owner, generation });
        try {
          const snapshot = await api.load(owner, currentAccount);
          if (epoch !== run) return;
          if (snapshot.account_id && snapshot.account_id !== currentAccount)
            throw new Error("Draft account changed");
          session = snapshot.session;
          versions = snapshot.versions ?? {};
          const texts = { ...snapshot.drafts };
          const protections = { ...(snapshot.protections ?? {}) };
          for (const key of Object.keys(revisions)) {
            // A touched key belongs to the current local view, including an
            // absent key discarded while this initial snapshot was in flight.
            if (get().texts[key]) texts[key] = get().texts[key];
            else delete texts[key];
            if (get().protections[key])
              protections[key] = get().protections[key];
            else delete protections[key];
          }
          set({ texts, protections, loaded: true });
        } catch {
          if (epoch === run) set({ error: "load" });
        }
      },
      refresh: async () => {
        const run = epoch;
        const request = ++refreshRequest;
        const readRevisions = { ...revisions };
        const owner = get().owner;
        if (!owner || !get().loaded) return;
        try {
          const snapshot = await api.load(owner, accountId);
          if (
            epoch !== run ||
            request !== refreshRequest ||
            snapshot.session !== session ||
            (snapshot.account_id && snapshot.account_id !== accountId)
          )
            return;
          const texts = { ...snapshot.drafts };
          const protections = { ...(snapshot.protections ?? {}) };
          const nextVersions = { ...(snapshot.versions ?? {}) };
          for (const key of new Set([
            ...dirtyKeys,
            ...Object.keys(get().pending),
            ...Object.keys(revisions).filter(
              (key) => revisions[key] !== readRevisions[key],
            ),
          ])) {
            if (get().texts[key]) texts[key] = get().texts[key];
            else delete texts[key];
            if (get().protections[key])
              protections[key] = get().protections[key];
            else delete protections[key];
            if (versions[key]) nextVersions[key] = versions[key];
            else delete nextVersions[key];
          }
          for (const key of new Set([
            ...Object.keys(get().texts),
            ...Object.keys(texts),
            ...Object.keys(get().protections),
            ...Object.keys(protections),
          ])) {
            if (
              get().texts[key] !== texts[key] ||
              get().protections[key] !== protections[key]
            )
              touch(key);
          }
          versions = nextVersions;
          set({ texts, protections });
        } catch {
          /* Retain the last readable snapshot and all local edits. */
        }
      },
      edit: (owner, generation, key, text) => {
        if (!current(owner, generation) || (get().texts[key] ?? "") === text)
          return;
        const texts = { ...get().texts };
        if (text) texts[key] = text;
        else delete texts[key];
        locallyChanged(key);
        set({ texts });
        changed(!text);
      },
      setProtection: (owner, generation, key, protection) => {
        if (
          !current(owner, generation) ||
          get().protections[key] === protection
        )
          return;
        const protections = { ...get().protections };
        if (protection) protections[key] = protection;
        else delete protections[key];
        locallyChanged(key);
        set({ protections });
        changed(!protection);
      },
      touch: (owner, generation, key) => {
        if (current(owner, generation)) touch(key);
      },
      captureProtection: (owner, generation, key) => {
        const run = epoch;
        const revision = revisions[key] ?? 0;
        const protection = current(owner, generation)
          ? get().protections[key]
          : undefined;
        return {
          protection,
          accept: () => {
            if (
              epoch !== run ||
              !current(owner, generation) ||
              (revisions[key] ?? 0) !== revision
            )
              return false;
            get().setProtection(owner, generation, key, undefined);
            return true;
          },
        };
      },
      admit: (owner, generation, key, text, submit) => {
        if (
          !current(owner, generation) ||
          get().pending[key] ||
          !text.trim() ||
          (get().texts[key] ?? "").trim() !== text.trim()
        )
          return null;
        const run = epoch;
        const revision = revisions[key] ?? 0;
        set({ pending: { ...get().pending, [key]: true } });
        const finish = (accepted: boolean): DraftAdmissionResult => {
          if (epoch !== run || !current(owner, generation))
            return { accepted, cleared: false };
          const pending = { ...get().pending };
          delete pending[key];
          const cleared = accepted && (revisions[key] ?? 0) === revision;
          if (cleared) {
            const texts = { ...get().texts };
            const protections = { ...get().protections };
            delete texts[key];
            delete protections[key];
            locallyChanged(key);
            set({ texts, protections, pending });
            changed(true);
          } else set({ pending });
          return { accepted, cleared };
        };
        try {
          return submit().then(finish, () => finish(false));
        } catch {
          return Promise.resolve(finish(false));
        }
      },
      flush: () => {
        cancelTimers();
        const { owner, texts, protections, loaded } = get();
        if (!dirty || !owner || !loaded) return writes;
        dirty = false;
        const run = epoch;
        const savedSession = session;
        const savedAccount = accountId;
        const keys = [...dirtyKeys];
        const savedRevisions = { ...revisions };
        writes = writes.then(async () => {
          if (epoch !== run) return;
          const currentKeys = keys.filter(
            (key) => revisions[key] === savedRevisions[key],
          );
          if (!currentKeys.length) return;
          try {
            const changes: Record<string, DraftChange> = Object.fromEntries(
              currentKeys.map((key) => [
                key,
                {
                  text: texts[key] ?? "",
                  ...(protections[key] ? { protection: protections[key] } : {}),
                  expected: versions[key] ?? null,
                },
              ]),
            );
            const snapshot = savedAccount
              ? await api.save(
                  owner,
                  savedSession,
                  texts,
                  protections,
                  savedAccount,
                  changes,
                )
              : await api.save(
                  owner,
                  savedSession,
                  get().texts,
                  get().protections,
                );
            if (epoch === run) {
              if (snapshot?.versions)
                for (const key of keys) {
                  if (snapshot.versions[key])
                    versions[key] = snapshot.versions[key];
                  else delete versions[key];
                }
              const conflicts = new Set(snapshot?.conflicts ?? []);
              for (const key of keys)
                if (
                  !conflicts.has(key) &&
                  revisions[key] === savedRevisions[key]
                )
                  dirtyKeys.delete(key);
              if (conflicts.size) {
                dirty = true;
                set({ error: "save" });
                return;
              }
              if (get().texts === texts && get().protections === protections)
                dirty = false;
              set({ error: null });
            }
          } catch {
            if (epoch === run) {
              dirty = true;
              set({ error: "save" });
            }
          }
        });
        return writes;
      },
    };
  });
}

export const useDrafts = createDraftStore(draftsApi);

/** Explicit kept-source action; ordinary account startup never imports local legacy drafts. */
export async function publishDraftsBeforeLink(owner: string) {
  const lease = captureRuntimeOwner();
  const account = currentRuntimeAccountId();
  const current = () =>
    lease.owner === owner &&
    lease.current() &&
    currentRuntimeAccountId() === account;
  if (!account || !current()) throw new Error("Draft account is not ready");
  await useDrafts.getState().flush();
  if (!current()) throw new Error("Draft account changed");
  if (useDrafts.getState().error)
    throw new Error("Unfinished drafts could not be saved");
  const result = await draftsApi.publishLegacy(owner, account);
  if (!current()) throw new Error("Draft account changed");
  await useDrafts.getState().refresh();
  return result;
}
