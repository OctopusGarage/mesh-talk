import { create } from "zustand";
import { draftsApi } from "@/lib/api/drafts";

interface DraftState {
  owner: string | null;
  generation: number;
  texts: Record<string, string>;
  loaded: boolean;
  error: "load" | "save" | null;
  start: (owner: string, generation: number) => Promise<void>;
  reset: () => void;
  edit: (owner: string, generation: number, key: string, text: string) => void;
  flush: () => Promise<void>;
}

/** One idle timer for all drafts; disk work never runs on the keystroke path. */
export function createDraftStore(api: typeof draftsApi) {
  let epoch = 0;
  let session = "";
  let dirty = false;
  let idle: ReturnType<typeof setTimeout> | undefined;
  let writes = Promise.resolve();
  const cancelTimers = () => {
    clearTimeout(idle);
    idle = undefined;
  };
  return create<DraftState>((set, get) => ({
    owner: null,
    generation: 0,
    texts: {},
    loaded: false,
    error: null,
    reset: () => {
      epoch++;
      cancelTimers();
      dirty = false;
      session = "";
      writes = Promise.resolve();
      set({ owner: null, texts: {}, loaded: false, error: null });
    },
    start: async (owner, generation) => {
      get().reset();
      const run = epoch;
      set({ owner, generation });
      try {
        const snapshot = await api.load(owner);
        if (epoch !== run) return;
        session = snapshot.session;
        set({ texts: snapshot.drafts, loaded: true });
      } catch {
        if (epoch === run) set({ error: "load" });
      }
    },
    edit: (owner, generation, key, text) => {
      const state = get();
      if (
        !state.loaded ||
        state.owner !== owner ||
        state.generation !== generation
      )
        return;
      if ((state.texts[key] ?? "") === text) return;
      const texts = { ...state.texts };
      if (text) texts[key] = text;
      else delete texts[key];
      set({ texts });
      dirty = true;
      clearTimeout(idle);
      idle = setTimeout(() => void get().flush(), 10000);
      // Clearing/sending should not resurrect already-submitted text after a restart.
      if (!text) void get().flush();
    },
    flush: () => {
      cancelTimers();
      const { owner, texts, loaded } = get();
      if (!dirty || !owner || !loaded) return writes;
      dirty = false;
      const run = epoch;
      const savedSession = session;
      writes = writes.then(async () => {
        if (epoch !== run) return;
        try {
          await api.save(owner, savedSession, texts);
          if (epoch === run) {
            if (get().texts === texts) dirty = false;
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
  }));
}

export const useDrafts = createDraftStore(draftsApi);
