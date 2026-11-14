import { create } from "zustand";
import { parsePack, type PackBase, type CustomizationPack } from "@/lib/pack";
import { verifyPackImages } from "@/lib/packImages";
import type { DocumentVersion } from "@/lib/api/accountDocuments";
import { captureRuntimeOwner } from "./ownership";

const LEGACY_DB = "mesh-talk-customization";
const PUBLIC_DB = "mesh-talk-customization-bundled-v2";
const STORE = "packs";
const CATALOG = "catalog";
const PENDING_INDEX = "pendingSync";
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
/** This catalog never owns image payloads or archives. */
export type PackCatalogEntry = PackBase &
  (
    | {
        kind: "avatar";
        category: "personal" | "group";
        fit: "cover" | "contain";
        avatars: { label: string }[];
      }
    | {
        kind: "sticker";
        stickers: { id: string; label: string; fallback: string }[];
      }
    | {
        kind: "theme";
        base: "light" | "dark";
        colors: Record<string, string>;
        wallpapers?: { id: string; title: string }[];
      }
  );
export function packMetadata(pack: CustomizationPack): PackCatalogEntry {
  const base = {
    format: pack.format,
    id: pack.id,
    version: pack.version,
    name: pack.name,
  };
  if (pack.kind === "avatar")
    return {
      ...base,
      kind: pack.kind,
      category: pack.category,
      fit: pack.fit,
      avatars: pack.avatars.map(({ label }) => ({ label })),
    };
  if (pack.kind === "sticker")
    return {
      ...base,
      kind: pack.kind,
      stickers: pack.stickers.map(({ id, label, fallback }) => ({
        id,
        label,
        fallback,
      })),
    };
  return {
    ...base,
    kind: pack.kind,
    base: pack.base,
    colors: { ...pack.colors },
    ...(pack.wallpapers
      ? { wallpapers: pack.wallpapers.map(({ id, title }) => ({ id, title })) }
      : {}),
  };
}
export type PackCatalogRow = Omit<StoredPack, "archive" | "pack"> & {
  pack: PackCatalogEntry | null;
};
function catalogRow(row: StoredPack): PackCatalogRow {
  return {
    id: row.id,
    pack: row.pack ? packMetadata(row.pack) : null,
    sequence: row.sequence,
    pending: row.pending,
    version: row.version,
  };
}
export interface PackStorage {
  setScope?(scope: PackScope | null): void;
  getCatalog?(
    scope: PackScope,
    id: string,
  ): Promise<PackCatalogRow | undefined>;
  publicCatalog?(): Promise<PackCatalogEntry[]>;
  catalog?(scope: PackScope): Promise<PackCatalogRow[]>;
  publicGet?(id: string): Promise<CustomizationPack | undefined>;
  publicPacks(): Promise<CustomizationPack[]>;
  legacyPacks(): Promise<CustomizationPack[]>;
  list(scope: PackScope): Promise<StoredPack[]>;
  pending?(scope: PackScope): Promise<StoredPack[]>;
  get?(scope: PackScope, id: string): Promise<StoredPack | undefined>;
  update(
    scope: PackScope,
    id: string,
    mutate: (previous: StoredPack | undefined) => StoredPack | undefined,
  ): Promise<StoredPack | undefined>;
}
interface PacksState {
  scope: PackScope | null;
  packs: PackCatalogEntry[];
  loaded: boolean;
  /** A local edit released its token; durable pending work may now be eligible. */
  localEditRevision: number;
  selectedTheme: CustomizationPack | undefined;
  readPack(
    id: string,
    current?: () => boolean,
  ): Promise<CustomizationPack | undefined>;
  prepareTheme(id: string): Promise<boolean>;
  linkedVersion(id: string): DocumentVersion | null;
  contentToken(id: string): string;
  /** Selected point lookup settled for this exact scope, before full enumeration. */
  preparedThemeId: string | null;
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
  forEachPending(
    visit: (row: StoredPack) => Promise<void>,
    current?: () => boolean,
  ): Promise<void>;
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
function ordered(packs: PackCatalogEntry[]) {
  return packs.sort(
    (a, b) =>
      (bundledOrder.get(a.id) ?? Number.MAX_SAFE_INTEGER) -
        (bundledOrder.get(b.id) ?? Number.MAX_SAFE_INTEGER) ||
      a.name.localeCompare(b.name),
  );
}
function conflict(
  previous: PackCatalogEntry | undefined | null,
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
function openDatabase(
  name: string,
  existingOnly = false,
): Promise<IDBDatabase | null> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(name, existingOnly ? undefined : 3);
    let absent = false;
    request.onupgradeneeded = () => {
      if (existingOnly) {
        absent = true;
        request.transaction?.abort();
      } else {
        const store = request.result.objectStoreNames.contains(STORE)
          ? request.transaction!.objectStore(STORE)
          : request.result.createObjectStore(STORE, { keyPath: "id" });
        if (!request.result.objectStoreNames.contains(CATALOG)) {
          const catalog = request.result.createObjectStore(CATALOG, {
            keyPath: "id",
          });
          const cursor = store.openCursor();
          cursor.onsuccess = () => {
            const row = cursor.result;
            if (!row) return;
            const value = row.value;
            catalog.put(
              name.startsWith("mesh-talk-customization-account:")
                ? catalogRow(value)
                : packMetadata(value),
            );
            row.continue();
          };
        }
        if (
          name.startsWith("mesh-talk-customization-account:") &&
          !store.indexNames.contains(PENDING_INDEX)
        ) {
          // Boolean values are not IndexedDB keys. A sparse numeric index
          // selects pending edits without cloning acknowledged archive payloads.
          store.createIndex(PENDING_INDEX, PENDING_INDEX);
          const cursor = store.openCursor();
          cursor.onsuccess = () => {
            const row = cursor.result;
            if (!row) return;
            const value = row.value;
            if (value.pending) value[PENDING_INDEX] = 1;
            else delete value[PENDING_INDEX];
            row.update(value);
            row.continue();
          };
        }
      }
    };
    request.onerror = () => (absent ? resolve(null) : reject(request.error));
    request.onsuccess = () => resolve(request.result);
  });
}
type PackConnection = {
  name: string;
  task: Promise<IDBDatabase | null>;
  db: IDBDatabase | null;
  users: number;
  retired: boolean;
};
// Only the public database and current account may retain a connection. Retired
// operations retain their own lease until their already-admitted work finishes.
const connections = new Map<string, PackConnection>();
let activeAccountDatabase: string | null = null;
function retireConnection(slot: PackConnection) {
  slot.retired = true;
  if (connections.get(slot.name) === slot) connections.delete(slot.name);
  if (!slot.users) slot.db?.close();
}
function setDatabaseScope(scope: PackScope | null) {
  for (const slot of connections.values()) retireConnection(slot);
  activeAccountDatabase = scope ? dbName(scope) : null;
}
async function database(name: string, existingOnly = false) {
  const retain =
    !existingOnly && (name === PUBLIC_DB || name === activeAccountDatabase);
  let slot = retain ? connections.get(name) : undefined;
  if (!slot) {
    slot = {
      name,
      task: openDatabase(name, existingOnly),
      db: null,
      users: 0,
      retired: !retain,
    };
    const opened = slot;
    if (retain) connections.set(name, opened);
    void opened.task.then(
      (db) => {
        opened.db = db;
        if (db) {
          db.onversionchange = () => retireConnection(opened);
          db.onclose = () => retireConnection(opened);
        }
        if (opened.retired && !opened.users) db?.close();
      },
      () => retireConnection(opened),
    );
  }
  const leased = slot;
  leased.users++;
  const release = () => {
    leased.users--;
    if (leased.retired && !leased.users) leased.db?.close();
  };
  try {
    return { db: await leased.task, release };
  } catch (error) {
    release();
    throw error;
  }
}
async function all<T>(
  name: string,
  existingOnly = false,
  storeName = STORE,
): Promise<T[]> {
  const { db, release } = await database(name, existingOnly);
  if (!db) {
    release();
    return [];
  }
  try {
    if (!db.objectStoreNames.contains(storeName)) return [];
    return await new Promise<T[]>((resolve, reject) => {
      const request = db
        .transaction(storeName, "readonly")
        .objectStore(storeName)
        .getAll();
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
  } finally {
    release();
  }
}
async function update<T extends { id: string }>(
  name: string,
  id: string,
  mutate: (previous: T | undefined) => T | undefined,
): Promise<T | undefined> {
  const { db, release } = await database(name);
  if (!db) {
    release();
    throw new Error("Pack storage unavailable");
  }
  try {
    return await new Promise<T | undefined>((resolve, reject) => {
      const tx = db.transaction([STORE, CATALOG], "readwrite");
      const store = tx.objectStore(STORE);
      const request = store.get(id);
      let result: T | undefined;
      let failure: unknown;
      request.onsuccess = () => {
        try {
          result = mutate(request.result as T | undefined);
          if (result) {
            const value = { ...result } as T & {
              pending?: boolean;
              pendingSync?: number;
            };
            if (store.indexNames.contains(PENDING_INDEX)) {
              if (value.pending) value.pendingSync = 1;
              else delete value.pendingSync;
            }
            store.put(value);
            tx.objectStore(CATALOG).put(
              name.startsWith("mesh-talk-customization-account:")
                ? catalogRow(value as unknown as StoredPack)
                : packMetadata(value as unknown as CustomizationPack),
            );
          }
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
    release();
  }
}
async function point<T>(
  name: string,
  id: string,
  storeName = STORE,
): Promise<T | undefined> {
  const { db, release } = await database(name);
  if (!db) {
    release();
    throw new Error("Pack storage unavailable");
  }
  try {
    return await new Promise<T | undefined>((resolve, reject) => {
      const tx = db.transaction(storeName, "readonly");
      const request = tx.objectStore(storeName).get(id);
      tx.oncomplete = () => resolve(request.result as T | undefined);
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
  } finally {
    release();
  }
}
export const indexedPackStorage: PackStorage = {
  setScope: setDatabaseScope,
  getCatalog: (scope, id) => point<PackCatalogRow>(dbName(scope), id, CATALOG),
  async publicCatalog() {
    const existing = await all<PackCatalogEntry>(PUBLIC_DB, false, CATALOG);
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
    return all<PackCatalogEntry>(PUBLIC_DB, false, CATALOG);
  },
  publicPacks: () => all<CustomizationPack>(PUBLIC_DB),
  catalog: (scope) => all<PackCatalogRow>(dbName(scope), false, CATALOG),
  publicGet: (id) => point<CustomizationPack>(PUBLIC_DB, id),
  async legacyPacks() {
    return (await all<CustomizationPack>(LEGACY_DB, true)).filter(
      (pack) => !isBundledPack(pack.id),
    );
  },
  list: (scope) => all<StoredPack>(dbName(scope)),
  async pending(scope) {
    const { db, release } = await database(dbName(scope));
    if (!db) {
      release();
      throw new Error("Pack storage unavailable");
    }
    try {
      return await new Promise<StoredPack[]>((resolve, reject) => {
        const tx = db.transaction(STORE, "readonly");
        const request = tx.objectStore(STORE).index(PENDING_INDEX).getAll(1, 1);
        tx.oncomplete = () => resolve(request.result as StoredPack[]);
        tx.onerror = () => reject(tx.error);
        tx.onabort = () => reject(tx.error);
      });
    } finally {
      release();
    }
  },
  get: (scope, id) => point<StoredPack>(dbName(scope), id),
  update: (scope, id, mutate) => update<StoredPack>(dbName(scope), id, mutate),
};

/** Account namespaces and durable pending edits share one storage owner. */
export function createPacksStore(
  io: PackStorage,
  captureCurrent: () => () => boolean = () => () => true,
) {
  let epoch = 0;
  let loading: Promise<void> | undefined;
  const reads = new Map<
    string,
    {
      task: Promise<CustomizationPack | undefined>;
      demands: Set<() => boolean>;
    }
  >();
  // At most two full rows are cloned at once (24 MiB legal image budget each).
  // Waiters share same-pack work, and retirement is checked before disk admission.
  let reading = 0;
  const waiting: Array<() => void> = [];
  const admitted = async <T>(work: () => Promise<T>): Promise<T> => {
    if (reading >= 2) {
      if (waiting.length >= 64)
        throw new Error(
          "Pack read capacity reached; retry when pending reads finish",
        );
      await new Promise<void>((resolve) => waiting.push(resolve));
    } else reading++;
    try {
      return await work();
    } finally {
      const next = waiting.shift();
      if (next) next();
      else reading--;
    }
  };
  const busy = new Map<string, symbol>();
  const revisions = new Map<string, number>();
  const invalidate = (id: string) =>
    revisions.set(id, (revisions.get(id) ?? 0) + 1);
  let publicSnapshot: PackCatalogEntry[] = [];
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
    let rowsSnapshot: PackCatalogRow[] = [];
    let themeRequest = 0;
    let desiredThemeId: string | undefined;
    let themeLoading: { key: string; task: Promise<boolean> } | undefined;
    let catalogRequest = 0;
    let publicLoading: Promise<PackCatalogEntry[]> | undefined;
    const reload = async (selectedThemeId?: string) => {
      const lease = capture();
      const request = ++catalogRequest;
      const publicRead = io.publicCatalog
        ? io.publicCatalog()
        : io.publicPacks().then((packs) => packs.map(packMetadata));
      publicLoading = publicRead;
      const selectedRead = selectedThemeId
        ? get().prepareTheme(selectedThemeId)
        : Promise.resolve();
      const [bundled, rows] = await Promise.all([
        publicRead,
        lease.scope
          ? io.catalog
            ? io.catalog(lease.scope)
            : io.list(lease.scope).then((rows) => rows.map(catalogRow))
          : Promise.resolve([]),
        selectedRead,
      ]);
      if (!lease.current() || request !== catalogRequest) return;
      const previous = new Map(
        get().packs.map((pack) => [
          pack.id,
          { pack, token: get().contentToken(pack.id) },
        ]),
      );
      publicSnapshot = bundled;
      rowsSnapshot = rows;
      const overrides = new Set(rows.map((row) => row.id));
      set({
        packs: ordered(
          [
            ...publicSnapshot.filter((pack) => !overrides.has(pack.id)),
            ...rows.flatMap((row) => (row.pack ? [row.pack] : [])),
          ].map((pack) => {
            const old = previous.get(pack.id);
            return old?.token === get().contentToken(pack.id) ? old.pack : pack;
          }),
        ),
        loaded: true,
      });
      const themeId = selectedThemeId ?? desiredThemeId;
      if (!selectedThemeId && themeId) await get().prepareTheme(themeId);
      await selectedRead;
    };
    return {
      scope: null,
      packs: [],
      loaded: false,
      localEditRevision: 0,
      selectedTheme: undefined,
      preparedThemeId: null,
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
        io.setScope?.(owner && account ? { owner, account, session } : null);
        loading = undefined;
        busy.clear();
        rowsSnapshot = [];
        revisions.clear();
        desiredThemeId = undefined;
        themeLoading = undefined;
        themeRequest++;
        set({
          scope: owner && account ? { owner, account, session } : null,
          packs: ordered(publicSnapshot.slice()),
          loaded: false,
          selectedTheme: undefined,
          preparedThemeId: null,
        });
      },
      async load(selectedThemeId) {
        if (loading) return loading;
        if (get().loaded) {
          if (selectedThemeId && get().preparedThemeId !== selectedThemeId)
            await get().prepareTheme(selectedThemeId);
          return;
        }
        const task = reload(selectedThemeId);
        loading = task;
        try {
          await task;
        } finally {
          if (loading === task) loading = undefined;
        }
      },
      contentToken(id) {
        const row = rowsSnapshot.find((row) => row.id === id);
        return JSON.stringify(
          row
            ? [id, row.sequence, row.version, Boolean(row.pack)]
            : [
                id,
                "public",
                get().packs.find((pack) => pack.id === id)?.version,
              ],
        );
      },
      linkedVersion(id) {
        const row = rowsSnapshot.find((row) => row.id === id);
        return row && !row.pending ? row.version : null;
      },
      async readPack(id, current) {
        const lease = capture();
        const descriptor = get().packs.find((pack) => pack.id === id);
        const revision = revisions.get(id);
        const wasLoaded = get().loaded;
        if (wasLoaded && !descriptor) return undefined;
        // No inactive cache: callers own only the visible payload. Every read is
        // fenced against deletion/replacement as well as account retirement.
        const key = JSON.stringify([
          epoch,
          id,
          revision,
          get().contentToken(id),
        ]);
        const demand = () => lease.current() && (!current || current());
        const existing = reads.get(key);
        if (existing) {
          if (existing.demands.size >= 64)
            throw new Error(
              "Pack reader capacity reached; retry when pending reads finish",
            );
          existing.demands.add(demand);
          try {
            const pack = await existing.task;
            return demand() ? pack : undefined;
          } finally {
            existing.demands.delete(demand);
          }
        }
        const demands = new Set([demand]);
        const task = admitted(async () => {
          if (
            ![...demands].some((needed) => needed()) ||
            !lease.current() ||
            revisions.get(id) !== revision
          )
            return undefined;
          const row = lease.scope
            ? io.get
              ? await io.get(lease.scope, id)
              : (await io.list(lease.scope)).find((row) => row.id === id)
            : undefined;
          if (!row && publicLoading) await publicLoading;
          const pack = row
            ? (row.pack ?? undefined)
            : io.publicGet
              ? await io.publicGet(id)
              : (await io.publicPacks()).find((pack) => pack.id === id);
          return lease.current() &&
            revisions.get(id) === revision &&
            (!wasLoaded ||
              get().packs.find((pack) => pack.id === id) === descriptor)
            ? pack
            : undefined;
        });
        const flight = { task, demands };
        reads.set(key, flight);
        try {
          const pack = await task;
          return demand() ? pack : undefined;
        } finally {
          demands.delete(demand);
          if (reads.get(key) === flight) reads.delete(key);
        }
      },
      async prepareTheme(id) {
        const lease = capture();
        desiredThemeId = id;
        const revision = revisions.get(id);
        const key = JSON.stringify([
          epoch,
          id,
          revision,
          get().contentToken(id),
        ]);
        if (themeLoading?.key === key) return themeLoading.task;
        const request = ++themeRequest;
        const task = (async () => {
          const pack = ["dark", "light", "oled"].includes(id)
            ? undefined
            : await get().readPack(
                id,
                () => lease.current() && request === themeRequest,
              );
          if (
            !lease.current() ||
            request !== themeRequest ||
            revisions.get(id) !== revision
          )
            return false;
          set({
            selectedTheme: pack?.kind === "theme" ? pack : undefined,
            preparedThemeId: id,
          });
          return true;
        })();
        themeLoading = { key, task };
        try {
          return await task;
        } finally {
          if (themeLoading?.task === task) themeLoading = undefined;
        }
      },
      async install(bytes, requirePack) {
        const lease = capture();
        if (!lease.scope) throw new Error("Account identity is not ready");
        const pack = parsePack(bytes);
        requirePack?.(pack);
        const token = Symbol();
        busy.set(pack.id, token);
        invalidate(pack.id);
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
          if (busy.get(pack.id) === token) {
            busy.delete(pack.id);
            if (lease.current())
              set({ localEditRevision: get().localEditRevision + 1 });
          }
        }
      },
      async remove(id) {
        const lease = capture();
        if (!lease.scope) throw new Error("Account identity is not ready");
        const token = Symbol();
        busy.set(id, token);
        invalidate(id);
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
          if (busy.get(id) === token) {
            busy.delete(id);
            if (lease.current())
              set({ localEditRevision: get().localEditRevision + 1 });
          }
        }
      },
      async forEachPending(visit, current = () => true) {
        const lease = capture();
        if (!lease.scope) return;
        // Select only compact metadata first. A failed publication can be caught
        // by its visitor so a failed first ID never pins later pending edits.
        const entries = io.catalog
          ? await io.catalog(lease.scope)
          : (await io.list(lease.scope)).map(catalogRow);
        for (const entry of entries) {
          if (!lease.current() || !current()) return;
          if (!entry.pending || busy.has(entry.id)) continue;
          const row = io.get
            ? await io.get(lease.scope, entry.id)
            : (await io.list(lease.scope)).find((row) => row.id === entry.id);
          if (!lease.current()) return;
          if (current() && row?.pending && !busy.has(row.id)) await visit(row);
        }
      },
      async pendingChanges() {
        const lease = capture();
        if (!lease.scope) return [];
        const rows = await (io.pending
          ? io.pending(lease.scope)
          : io.list(lease.scope));
        return lease.current()
          ? rows.filter((row) => row.pending && !busy.has(row.id))
          : [];
      },
      async acknowledge(id, sequence, version) {
        const lease = capture();
        if (!lease.scope) return;
        const result = await io.update(lease.scope, id, (previous) => {
          if (
            !lease.current() ||
            !previous ||
            previous.sequence !== sequence ||
            busy.has(id)
          )
            return undefined;
          return { ...previous, pending: false, version };
        });
        if (result && lease.current())
          rowsSnapshot = rowsSnapshot.map((row) =>
            row.id === id ? catalogRow(result) : row,
          );
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
        // Fresh metadata is an admission optimization, never a stored authority
        // result. Mutations still reread the row atomically in the write transaction.
        const inspect = io.getCatalog ?? io.get;
        if (inspect) {
          const revision = revisions.get(id);
          const previous = await inspect.call(io, lease.scope, id);
          if (
            !lease.current() ||
            !current() ||
            busy.has(id) ||
            revisions.get(id) !== revision
          )
            return false;
          if (previous?.pending) return false;
          if (
            previous?.version &&
            compareVersions(previous.version, version) >= 0
          )
            return true;
        }
        invalidate(id);
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
  indexedPackStorage,
  () => captureRuntimeOwner().current,
);
export function installedTheme(id: string) {
  const state = usePacks.getState();
  const pack = state.selectedTheme?.id === id ? state.selectedTheme : undefined;
  return pack?.kind === "theme" ? pack : undefined;
}
