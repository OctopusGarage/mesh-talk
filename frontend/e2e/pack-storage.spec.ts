import { test, expect } from "./tauri-mock";

// Real IndexedDB exercises upgrade/transaction semantics that memory adapters cannot.
test("version-two pack migration is payload-free after upgrade and preserves tombstones on failed writes", async ({
  page,
}) => {
  await page.goto("/");
  const result = await page.evaluate(async () => {
    const modulePath = "/src/store/packs.ts";
    const { createPacksStore, indexedPackStorage } = await import(
      /* @vite-ignore */ modulePath
    );
    const scope = { owner: "catalog-fixture", account: "account" };
    const name = "mesh-talk-customization-account:catalog-fixture:account";
    const pack = {
      format: 1,
      id: "fixture.theme",
      version: "1.0.0",
      name: "Fixture",
      kind: "theme",
      base: "dark",
      colors: {},
      wallpaper: "data:image/png;base64,AQID",
    };
    const open = (version?: number) =>
      new Promise<IDBDatabase>((resolve, reject) => {
        const request = indexedDB.open(name, version);
        request.onupgradeneeded = () => {
          const store = request.result.createObjectStore("packs", {
            keyPath: "id",
          });
          store.createIndex("pendingSync", "pendingSync");
        };
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
    const db = await open(2);
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction("packs", "readwrite");
      tx.objectStore("packs").put({
        id: pack.id,
        pack,
        archive: new Uint8Array([1, 2]),
        sequence: 3,
        pending: false,
        version: null,
      });
      tx.objectStore("packs").put({
        id: "removed.theme",
        pack: null,
        archive: null,
        sequence: 4,
        pending: true,
        pendingSync: 1,
        version: null,
      });
      tx.objectStore("packs").put({
        id: "z.pending",
        pack: { ...pack, id: "z.pending" },
        archive: new Uint8Array(256 * 1024),
        sequence: 5,
        pending: true,
        pendingSync: 1,
        version: null,
      });
      tx.oncomplete = () => resolve();
      tx.onabort = () => reject(tx.error);
    });
    db.close();
    let payloadEnumerations = 0;
    const originalGetAll = IDBObjectStore.prototype.getAll;
    IDBObjectStore.prototype.getAll = function (...args) {
      if (this.name === "packs") payloadEnumerations++;
      return originalGetAll.apply(this, args);
    };
    try {
      const store = createPacksStore(indexedPackStorage);
      store.getState().setScope(scope.owner, scope.account);
      await store.getState().load();
      const catalog = store.getState().packs;
      const pending = await store.getState().pendingChanges();
      const originalGet = IDBObjectStore.prototype.get;
      let pointReads = 0;
      let readsDuringVisitor = 0;
      const streamed: string[] = [];
      IDBObjectStore.prototype.get = function (...args) {
        if (this.name === "packs" && this.transaction.db.name === name)
          pointReads++;
        return originalGet.apply(this, args);
      };
      try {
        await store.getState().forEachPending(async (row: { id: string }) => {
          streamed.push(row.id);
          const before = pointReads;
          await Promise.resolve();
          readsDuringVisitor += pointReads - before;
        });
      } finally {
        IDBObjectStore.prototype.get = originalGet;
      }
      const originalPut = IDBObjectStore.prototype.put;
      IDBObjectStore.prototype.put = function (...args) {
        if (this.name === "catalog")
          throw new Error("injected catalog failure");
        return originalPut.apply(this, args);
      };
      let failed = false;
      try {
        await store.getState().remove(pack.id);
      } catch {
        failed = true;
      } finally {
        IDBObjectStore.prototype.put = originalPut;
      }
      const retained = await indexedPackStorage.get(scope, pack.id);
      const rows = await indexedPackStorage.catalog(scope);
      return {
        catalog,
        streamed,
        pointReads,
        readsDuringVisitor,
        pending: pending.map((row: { id: string }) => row.id),
        payloadEnumerations,
        failed,
        retainedSequence: retained.sequence,
        retainedWallpaper: retained.pack.wallpaper,
        catalogSequence: rows.find((row: { id: string }) => row.id === pack.id)
          .sequence,
        tombstone: rows.find(
          (row: { id: string }) => row.id === "removed.theme",
        ).pack,
      };
    } finally {
      IDBObjectStore.prototype.getAll = originalGetAll;
    }
  });
  expect(result.payloadEnumerations).toBe(0); // sparse pending reads use the index, never enumerate payload rows
  expect(result.catalog).toEqual([
    {
      format: 1,
      id: "fixture.theme",
      version: "1.0.0",
      name: "Fixture",
      kind: "theme",
      base: "dark",
      colors: {},
    },
    {
      format: 1,
      id: "z.pending",
      version: "1.0.0",
      name: "Fixture",
      kind: "theme",
      base: "dark",
      colors: {},
    },
  ]);
  expect(result.pending).toEqual(["removed.theme"]);
  expect(result.streamed).toEqual(["removed.theme", "z.pending"]);
  expect(result.pointReads).toBe(2);
  expect(result.readsDuringVisitor).toBe(0);
  expect(result.failed).toBe(true);
  expect(result.retainedSequence).toBe(3);
  expect(result.catalogSequence).toBe(3);
  expect(result.retainedWallpaper).toBe("data:image/png;base64,AQID");
  expect(result.tombstone).toBeNull();
});
