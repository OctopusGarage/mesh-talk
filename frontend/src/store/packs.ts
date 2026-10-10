import { create } from "zustand";
import { parsePack, type CustomizationPack } from "@/lib/pack";
import { verifyPackImages } from "@/lib/packImages";

const DB_NAME = "mesh-talk-customization";
const STORE_NAME = "packs";
const META_NAME = "meta";
const BUNDLED_SEED_KEY = "bundled-packs-v1";
const BUNDLED_PACK_IDS = (import.meta.env.VITE_BUNDLED_PACK_IDS as string)
  .split(",")
  .filter(Boolean);
const bundledOrder = new Map(BUNDLED_PACK_IDS.map((id, index) => [id, index]));

export class PackIdConflictError extends Error {
  constructor() {
    super("A different library type already uses this pack ID.");
  }
}

interface PacksState {
  packs: CustomizationPack[];
  loaded: boolean;
  load: () => Promise<void>;
  install: (
    bytes: Uint8Array,
    requirePack?: (pack: CustomizationPack) => void,
  ) => Promise<CustomizationPack>;
  remove: (id: string) => Promise<void>;
}

let loadPromise: Promise<void> | undefined;

function database(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, 2);
    request.onupgradeneeded = () => {
      if (!request.result.objectStoreNames.contains(STORE_NAME))
        request.result.createObjectStore(STORE_NAME, { keyPath: "id" });
      if (!request.result.objectStoreNames.contains(META_NAME))
        request.result.createObjectStore(META_NAME);
    };
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);
  });
}

async function seedBundledPacks(): Promise<void> {
  if (BUNDLED_PACK_IDS.length === 0) return;
  const db = await database();
  try {
    if (await seeded(BUNDLED_SEED_KEY, db)) return;
    for (const id of BUNDLED_PACK_IDS) {
      const key = `${BUNDLED_SEED_KEY}:${id}`;
      if (await seeded(key, db)) continue;
      try {
        const response = await fetch(
          new URL(`builtin-packs/${id}.zip`, document.baseURI),
        );
        if (!response.ok) throw new Error(`Could not load bundled pack: ${id}`);
        const pack = parsePack(new Uint8Array(await response.arrayBuffer()));
        if (pack.id !== id) throw new Error(`Bundled pack ID mismatch: ${id}`);
        await verifyPackImages(pack);
        await new Promise<void>((resolve, reject) => {
          const tx = db.transaction([STORE_NAME, META_NAME], "readwrite");
          const store = tx.objectStore(STORE_NAME);
          const existing = store.get(id);
          existing.onsuccess = () => {
            if (existing.result === undefined) store.put(pack);
            tx.objectStore(META_NAME).put(true, key);
          };
          tx.oncomplete = () => resolve();
          tx.onerror = () => reject(tx.error);
          tx.onabort = () => reject(tx.error);
        });
      } catch (error) {
        console.warn(
          `Could not seed bundled pack ${id}; will retry next launch`,
          error,
        );
      }
    }
  } finally {
    db.close();
  }
}

function seeded(key: string, db: IDBDatabase): Promise<boolean> {
  return new Promise((resolve, reject) => {
    const request = db
      .transaction(META_NAME, "readonly")
      .objectStore(META_NAME)
      .get(key);
    request.onsuccess = () => resolve(request.result === true);
    request.onerror = () => reject(request.error);
  });
}

function orderedPacks(packs: CustomizationPack[]): CustomizationPack[] {
  return packs.sort((a, b) => {
    const aOrder = bundledOrder.get(a.id);
    const bOrder = bundledOrder.get(b.id);
    if (aOrder !== undefined && bOrder !== undefined) return aOrder - bOrder;
    if (aOrder !== undefined) return -1;
    if (bOrder !== undefined) return 1;
    return a.name.localeCompare(b.name);
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

async function savePack(pack: CustomizationPack): Promise<void> {
  const db = await database();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, "readwrite");
      const store = tx.objectStore(STORE_NAME);
      let conflictingType = false;
      const existing = store.get(pack.id);
      existing.onsuccess = () => {
        const previous = existing.result as CustomizationPack | undefined;
        conflictingType =
          previous !== undefined &&
          (previous.kind !== pack.kind ||
            (previous.kind === "avatar" &&
              pack.kind === "avatar" &&
              previous.category !== pack.category));
        if (conflictingType) tx.abort();
        else store.put(pack);
      };
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
      tx.onabort = () =>
        reject(conflictingType ? new PackIdConflictError() : tx.error);
    });
  } finally {
    db.close();
  }
}

export const usePacks = create<PacksState>((set, get) => ({
  packs: [],
  loaded: false,
  load: async () => {
    if (loadPromise) return loadPromise;
    if (get().loaded) return;
    loadPromise = transaction<CustomizationPack[]>("readonly", (store) =>
      store.getAll(),
    )
      .then((packs) => set({ packs: orderedPacks(packs), loaded: true }))
      .then(seedBundledPacks)
      .then(() =>
        transaction<CustomizationPack[]>("readonly", (store) => store.getAll()),
      )
      .then((packs) => set({ packs: orderedPacks(packs) }))
      .finally(() => {
        loadPromise = undefined;
      });
    await loadPromise;
  },
  install: async (bytes, requirePack) => {
    await get().load();
    const pack = parsePack(bytes);
    requirePack?.(pack);
    await verifyPackImages(pack);
    await savePack(pack);
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
