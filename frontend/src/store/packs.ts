import { create } from "zustand";
import { parsePack, type CustomizationPack } from "@/lib/pack";
import { verifyPackImages } from "@/lib/packImages";
import type { DocumentVersion } from "@/lib/api/accountDocuments";
import { captureRuntimeOwner } from "./ownership";

const LEGACY_DB = "mesh-talk-customization";
const PUBLIC_DB = "mesh-talk-customization-bundled-v2";
const STORE = "packs";
const BUNDLED_IDS = (import.meta.env.VITE_BUNDLED_PACK_IDS as string)
  .split(",")
  .filter(Boolean);
const bundledOrder = new Map(BUNDLED_IDS.map((id, index) => [id, index]));
export const isBundledPack = (id: string) => bundledOrder.has(id);
export class PackIdConflictError extends Error {
  constructor() {
    super("A different library type already uses this pack ID.");
  }
}
export interface PackScope {
  owner: string;
  account: string;
  session?: string;
}
export interface StoredPack {
  id: string;
  pack: CustomizationPack | null;
  archive: Uint8Array | null;
  sequence: number;
  pending: boolean;
  version: DocumentVersion | null;
}
export interface PackStorage {
  publicPacks(): Promise<CustomizationPack[]>;
  legacyPacks(): Promise<CustomizationPack[]>;
  list(scope: PackScope): Promise<StoredPack[]>;
  get?(scope: PackScope, id: string): Promise<StoredPack | undefined>;
  update(
    scope: PackScope,
    id: string,
    mutate: (previous: StoredPack | undefined) => StoredPack | undefined,
  ): Promise<StoredPack | undefined>;
}
interface PacksState {
  scope: PackScope | null;
  packs: CustomizationPack[];
  loaded: boolean;
  setScope(
    owner: string | null,
    account: string | null,
    session?: string,
  ): void;
  load(selectedThemeId?: string): Promise<void>;
  install(
    bytes: Uint8Array,
    requirePack?: (pack: CustomizationPack) => void,
  ): Promise<CustomizationPack>;
  remove(id: string): Promise<void>;
  pendingChanges(): Promise<StoredPack[]>;
  acknowledge(
    id: string,
    sequence: number,
    version: DocumentVersion,
  ): Promise<void>;
  applyLinked(
    id: string,
    bytes: Uint8Array | null,
    version: DocumentVersion,
    current: () => boolean,
  ): Promise<boolean>;
  legacyPacks(): Promise<CustomizationPack[]>;
}
function ordered(packs: CustomizationPack[]) {
  return packs.sort(
    (a, b) =>
      (bundledOrder.get(a.id) ?? Number.MAX_SAFE_INTEGER) -
        (bundledOrder.get(b.id) ?? Number.MAX_SAFE_INTEGER) ||
      a.name.localeCompare(b.name),
  );
}
function conflict(
  previous: CustomizationPack | undefined | null,
  next: CustomizationPack,
) {
  return (
    previous &&
    (previous.kind !== next.kind ||
      (previous.kind === "avatar" &&
        next.kind === "avatar" &&
        previous.category !== next.category))
  );
}
function compareVersions(a: DocumentVersion, b: DocumentVersion): number {
  if (a.revision !== b.revision) return a.revision > b.revision ? 1 : -1;
  if (a.writer_device !== b.writer_device)
    return a.writer_device > b.writer_device ? 1 : -1;
  for (
    let index = 0;
    index < Math.max(a.content_hash.length, b.content_hash.length);
    index++
  ) {
    const left = a.content_hash[index] ?? -1;
    const right = b.content_hash[index] ?? -1;
    if (left !== right) return left > right ? 1 : -1;
  }
  return 0;
}
function dbName(scope: PackScope) {
  return `mesh-talk-customization-account:${encodeURIComponent(scope.owner)}:${encodeURIComponent(scope.account)}`;
}
function database(
  name: string,
  existingOnly = false,
): Promise<IDBDatabase | null> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(name, existingOnly ? undefined : 1);
    let absent = false;
    request.onupgradeneeded = () => {
      if (existingOnly) {
        absent = true;
        request.transaction?.abort();
      } else if (!request.result.objectStoreNames.contains(STORE))
        request.result.createObjectStore(STORE, { keyPath: "id" });
    };
    request.onerror = () => (absent ? resolve(null) : reject(request.error));
    request.onsuccess = () => resolve(request.result);
  });
}
async function all<T>(name: string, existingOnly = false): Promise<T[]> {
  const db = await database(name, existingOnly);
  if (!db) return [];
  try {
    if (!db.objectStoreNames.contains(STORE)) return [];
    return await new Promise<T[]>((resolve, reject) => {
      const request = db
        .transaction(STORE, "readonly")
        .objectStore(STORE)
        .getAll();
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
  } finally {
    db.close();
  }
}
async function update<T extends { id: string }>(
  name: string,
  id: string,
  mutate: (previous: T | undefined) => T | undefined,
): Promise<T | undefined> {
  const db = await database(name);
  if (!db) throw new Error("Pack storage unavailable");
  try {
    return await new Promise<T | undefined>((resolve, reject) => {
      const tx = db.transaction(STORE, "readwrite");
      const store = tx.objectStore(STORE);
      const request = store.get(id);
      let result: T | undefined;
      let failure: unknown;
      request.onsuccess = () => {
        try {
          result = mutate(request.result as T | undefined);
          if (result) store.put(result);
        } catch (error) {
          failure = error;
          tx.abort();
        }
      };
      tx.oncomplete = () => resolve(result);
      tx.onerror = () => reject(failure ?? tx.error);
      tx.onabort = () => reject(failure ?? tx.error);
    });
  } finally {
    db.close();
  }
}
const storage: PackStorage = {
  async publicPacks() {
    const existing = await all<CustomizationPack>(PUBLIC_DB);
    const present = new Set(existing.map((pack) => pack.id));
    for (const id of BUNDLED_IDS) {
      if (present.has(id)) continue;
      try {
        const response = await fetch(
          new URL(`builtin-packs/${id}.zip`, document.baseURI),
        );
        if (!response.ok) throw new Error(`Could not load bundled pack: ${id}`);
        const pack = parsePack(new Uint8Array(await response.arrayBuffer()));
        if (pack.id !== id) throw new Error("Bundled pack ID mismatch");
        await verifyPackImages(pack);
        await update<CustomizationPack>(
          PUBLIC_DB,
          id,
          (previous) => previous ?? pack,
        );
      } catch (error) {
        console.warn(`Could not load bundled pack ${id}; will retry`, error);
      }
    }
    return all<CustomizationPack>(PUBLIC_DB);
  },
  async legacyPacks() {
    return (await all<CustomizationPack>(LEGACY_DB, true)).filter(
      (pack) => !isBundledPack(pack.id),
    );
  },
  list: (scope) => all<StoredPack>(dbName(scope)),
  async get(scope, id) {
    const db = await database(dbName(scope));
    if (!db) return undefined;
    try {
      return await new Promise<StoredPack | undefined>((resolve, reject) => {
        const request = db
          .transaction(STORE, "readonly")
          .objectStore(STORE)
          .get(id);
        request.onsuccess = () =>
          resolve(request.result as StoredPack | undefined);
        request.onerror = () => reject(request.error);
      });
    } finally {
      db.close();
    }
  },
  update: (scope, id, mutate) => update<StoredPack>(dbName(scope), id, mutate),
};

/** Account namespaces and durable pending edits share one storage owner. */
export function createPacksStore(
  io: PackStorage,
  captureCurrent: () => () => boolean = () => () => true,
) {
  let epoch = 0;
  let loading: Promise<void> | undefined;
  const busy = new Map<string, symbol>();
  let publicSnapshot: CustomizationPack[] = [];
  return create<PacksState>((set, get) => {
    const capture = () => {
      const scope = get().scope;
      const generation = epoch;
      const externalCurrent = scope ? captureCurrent() : () => true;
      const current = () => epoch === generation && externalCurrent();
      const requireCurrent = () => {
        if (!current())
          throw new Error("Account changed during library update");
      };
      return { scope, current, requireCurrent };
    };
    const reload = async (selectedThemeId?: string) => {
      const lease = capture();
      // Only an authenticated account scope can restore private appearance. Read
      // its selected theme without cloning every unrelated gallery asset first.
      const selected =
        lease.scope && selectedThemeId && io.get
          ? io
              .get(lease.scope, selectedThemeId)
              .then((row) => {
                if (!lease.current() || get().loaded || !row) return;
                const bundled = publicSnapshot.filter(
                  (pack) => pack.id !== row.id,
                );
                set({
                  packs: ordered([
                    ...bundled,
                    ...(row.pack?.kind === "theme" ? [row.pack] : []),
                  ]),
                });
              })
              .catch((error) => {
                console.warn(
                  "Could not restore selected theme; loading library",
                  error,
                );
              })
          : Promise.resolve();
      const [bundled, rows] = await Promise.all([
        io.publicPacks(),
        lease.scope ? io.list(lease.scope) : Promise.resolve([]),
        selected,
      ]);
      if (!lease.current()) return;
      publicSnapshot = bundled;
      const overrides = new Set(rows.map((row) => row.id));
      set({
        packs: ordered([
          ...bundled.filter((pack) => !overrides.has(pack.id)),
          ...rows.flatMap((row) => (row.pack ? [row.pack] : [])),
        ]),
        loaded: true,
      });
    };
    return {
      scope: null,
      packs: [],
      loaded: false,
      setScope(owner, account, session) {
        const previous = get().scope;
        if (
          previous?.owner === owner &&
          previous?.account === account &&
          previous?.session === session
        )
          return;
        if (!previous && (!owner || !account)) return;
        epoch++;
        loading = undefined;
        busy.clear();
        set({
          scope: owner && account ? { owner, account, session } : null,
          packs: ordered(publicSnapshot.slice()),
          loaded: false,
        });
      },
      async load(selectedThemeId) {
        if (loading) return loading;
        if (get().loaded) return;
        const task = reload(selectedThemeId);
        loading = task;
        try {
          await task;
        } finally {
          if (loading === task) loading = undefined;
        }
      },
      async install(bytes, requirePack) {
        const lease = capture();
        if (!lease.scope) throw new Error("Account identity is not ready");
        const pack = parsePack(bytes);
        requirePack?.(pack);
        const token = Symbol();
        busy.set(pack.id, token);
        try {
          await get().load();
          lease.requireCurrent();
          await verifyPackImages(pack);
          lease.requireCurrent();
          await io.update(lease.scope, pack.id, (previous) => {
            lease.requireCurrent();
            if (busy.get(pack.id) !== token)
              throw new Error("Library edit superseded");
            if (
              conflict(
                previous?.pack ??
                  get().packs.find((item) => item.id === pack.id),
                pack,
              )
            )
              throw new PackIdConflictError();
            return {
              id: pack.id,
              pack,
              archive: bytes.slice(),
              sequence: (previous?.sequence ?? 0) + 1,
              pending: true,
              version: previous?.version ?? null,
            };
          });
          lease.requireCurrent();
          await reload();
          return pack;
        } finally {
          if (busy.get(pack.id) === token) busy.delete(pack.id);
        }
      },
      async remove(id) {
        const lease = capture();
        if (!lease.scope) throw new Error("Account identity is not ready");
        const token = Symbol();
        busy.set(id, token);
        try {
          await io.update(lease.scope, id, (previous) => {
            lease.requireCurrent();
            if (busy.get(id) !== token)
              throw new Error("Library edit superseded");
            return {
              id,
              pack: null,
              archive: null,
              sequence: (previous?.sequence ?? 0) + 1,
              pending: true,
              version: previous?.version ?? null,
            };
          });
          lease.requireCurrent();
          await reload();
        } finally {
          if (busy.get(id) === token) busy.delete(id);
        }
      },
      async pendingChanges() {
        const lease = capture();
        if (!lease.scope) return [];
        const rows = await io.list(lease.scope);
        return lease.current()
          ? rows.filter((row) => row.pending && !busy.has(row.id))
          : [];
      },
      async acknowledge(id, sequence, version) {
        const lease = capture();
        if (!lease.scope) return;
        await io.update(lease.scope, id, (previous) => {
          if (
            !lease.current() ||
            !previous ||
            previous.sequence !== sequence ||
            busy.has(id)
          )
            return undefined;
          return { ...previous, pending: false, version };
        });
      },
      async applyLinked(id, bytes, version, current) {
        const lease = capture();
        if (!lease.scope || !current() || busy.has(id)) return false;
        // Remote validation never owns the local-edit token; a new edit can
        // invalidate it while image decoding or IndexedDB admission is pending.
        const pack = bytes ? parsePack(bytes) : null;
        if (pack && pack.id !== id)
          throw new Error("Linked library ID mismatch");
        if (pack) await verifyPackImages(pack);
        if (!lease.current() || !current() || busy.has(id)) return false;
        let applied = false;
        let retained = false;
        await io.update(lease.scope, id, (previous) => {
          if (
            !lease.current() ||
            !current() ||
            busy.has(id) ||
            previous?.pending
          )
            return undefined;
          if (
            previous?.version &&
            compareVersions(previous.version, version) >= 0
          ) {
            retained = true;
            return undefined;
          }
          if (pack && conflict(previous?.pack, pack))
            throw new PackIdConflictError();
          applied = true;
          return {
            id,
            pack,
            archive: bytes?.slice() ?? null,
            sequence: previous?.sequence ?? 0,
            pending: false,
            version,
          };
        });
        if (applied && lease.current() && current()) await reload();
        return applied || retained;
      },
      legacyPacks: () => io.legacyPacks(),
    };
  });
}
export const usePacks = createPacksStore(
  storage,
  () => captureRuntimeOwner().current,
);
export function installedTheme(id: string) {
  const pack = usePacks.getState().packs.find((item) => item.id === id);
  return pack?.kind === "theme" ? pack : undefined;
}
