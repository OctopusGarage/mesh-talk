import { create } from "zustand";
import { parsePack, type CustomizationPack } from "@/lib/pack";

const DB_NAME = "mesh-talk-customization";
const STORE_NAME = "packs";

interface PacksState {
  packs: CustomizationPack[];
  loaded: boolean;
  load: () => Promise<void>;
  install: (bytes: Uint8Array) => Promise<CustomizationPack>;
  remove: (id: string) => Promise<void>;
}

let loadPromise: Promise<void> | undefined;

function database(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, 1);
    request.onupgradeneeded = () =>
      request.result.createObjectStore(STORE_NAME, { keyPath: "id" });
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);
  });
}

async function transaction<T>(
  mode: IDBTransactionMode,
  run: (store: IDBObjectStore) => IDBRequest<T>,
): Promise<T> {
  const db = await database();
  try {
    return await new Promise<T>((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, mode);
      const request = run(tx.objectStore(STORE_NAME));
      tx.oncomplete = () => resolve(request.result);
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
  } finally {
    db.close();
  }
}

export const usePacks = create<PacksState>((set, get) => ({
  packs: [],
  loaded: false,
  load: async () => {
    if (get().loaded) return;
    loadPromise ??= transaction<CustomizationPack[]>("readonly", (store) =>
      store.getAll(),
    )
      .then((packs) => set({ packs, loaded: true }))
      .finally(() => {
        loadPromise = undefined;
      });
    await loadPromise;
  },
  install: async (bytes) => {
    await get().load();
    const pack = parsePack(bytes);
    await transaction("readwrite", (store) => store.put(pack));
    set((state) => ({
      packs: [...state.packs.filter((item) => item.id !== pack.id), pack],
    }));
    return pack;
  },
  remove: async (id) => {
    await get().load();
    await transaction("readwrite", (store) => store.delete(id));
    set((state) => ({ packs: state.packs.filter((item) => item.id !== id) }));
  },
}));

export function installedTheme(id: string) {
  const pack = usePacks.getState().packs.find((item) => item.id === id);
  return pack?.kind === "theme" ? pack : undefined;
}
