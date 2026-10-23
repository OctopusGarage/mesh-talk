import { create } from "zustand";
import { draftsApi } from "@/lib/api/drafts";
import type { MessageProtection } from "@/lib/types";

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
  start: (owner: string, generation: number) => Promise<void>;
  reset: () => void;
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
export function createDraftStore(api: typeof draftsApi) {
  let epoch = 0;
  let session = "";
  let dirty = false;
  let revisions: Record<string, number> = {};
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
      reset: () => {
        epoch++;
        cancelTimers();
        dirty = false;
        revisions = {};
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
      start: async (owner, generation) => {
        get().reset();
        const run = epoch;
        set({ owner, generation });
        try {
          const snapshot = await api.load(owner);
          if (epoch !== run) return;
          session = snapshot.session;
          set({
            texts: snapshot.drafts,
            protections: snapshot.protections ?? {},
            loaded: true,
          });
        } catch {
          if (epoch === run) set({ error: "load" });
        }
      },
      edit: (owner, generation, key, text) => {
        if (!current(owner, generation) || (get().texts[key] ?? "") === text)
          return;
        const texts = { ...get().texts };
        if (text) texts[key] = text;
        else delete texts[key];
        touch(key);
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
        touch(key);
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
            touch(key);
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
        writes = writes.then(async () => {
          if (epoch !== run) return;
          try {
            await api.save(owner, savedSession, texts, protections);
            if (epoch === run) {
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
