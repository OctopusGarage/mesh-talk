import { expect, test } from "@playwright/test";

test("private pack pending index upgrades durable edits and avoids acknowledged assets", async ({
  page,
}) => {
  // A blank same-origin document exercises the production store without mounting
  // the application or starting unrelated background synchronization.
  await page.route("**/pending-storage", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: "<!doctype html><body></body>",
    }),
  );
  await page.goto("/pending-storage");
  const result = await page.evaluate(async () => {
    const name = "mesh-talk-customization-account:pending-test:account";
    await new Promise<void>((resolve, reject) => {
      const request = indexedDB.open(name, 1);
      request.onupgradeneeded = () => {
        const store = request.result.createObjectStore("packs", {
          keyPath: "id",
        });
        for (const [id, pending] of [
          ["asset", false],
          ["edit", true],
          ["deleted", true],
        ] as const) {
          store.put({
            id,
            pending,
            sequence: 3,
            pack: null,
            archive: id === "deleted" ? null : new Uint8Array(1024 * 1024),
            version: null,
          });
        }
      };
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        request.result.close();
        resolve();
      };
    });
    const reads: string[] = [];
    const original = IDBObjectStore.prototype.getAll;
    IDBObjectStore.prototype.getAll = function (...args) {
      reads.push(this.name);
      return original.apply(this, args);
    };
    const modulePath = "/src/store/packs.ts";
    const { usePacks } = await import(/* @vite-ignore */ modulePath);
    const authPath = "/src/store/auth.ts";
    const { useAuth } = await import(/* @vite-ignore */ authPath);
    useAuth.setState({ user: { id: "pending-test" } });
    const store = usePacks.getState();
    store.setScope("pending-test", "account", "session");
    const migrated = await store.pendingChanges();
    const version = { revision: 1, writer_device: "device", content_hash: [1] };
    await store.acknowledge("edit", 2, version);
    const stale = await store.pendingChanges();
    const put = IDBObjectStore.prototype.put;
    IDBObjectStore.prototype.put = function () {
      throw new Error("write failed");
    };
    let rejected = false;
    try {
      await store.acknowledge("edit", 3, version);
    } catch {
      rejected = true;
    }
    IDBObjectStore.prototype.put = put;
    const failed = await store.pendingChanges();
    await store.acknowledge("edit", 3, version);
    await store.acknowledge("deleted", 3, version);
    const empty = await store.pendingChanges();
    store.setScope("pending-test", "different", "next-session");
    const other = await store.pendingChanges();
    store.setScope("pending-test", "account", "reopened");
    const reopened = await store.pendingChanges();
    IDBObjectStore.prototype.getAll = original;
    return {
      migrated: migrated.map((row: { id: string }) => row.id).sort(),
      tombstone: migrated.find((row: { id: string }) => row.id === "deleted")
        ?.archive,
      stale: stale.length,
      failed: failed.length,
      rejected,
      empty: empty.length,
      other: other.length,
      reopened: reopened.length,
      reads,
    };
  });
  expect(result).toEqual({
    migrated: ["deleted", "edit"],
    tombstone: null,
    stale: 2,
    failed: 2,
    rejected: true,
    empty: 0,
    other: 0,
    reopened: 0,
    reads: [],
  });
});

test("new pending tombstones remain durable alongside public and legacy storage", async ({
  page,
}) => {
  await page.route("**/pending-storage", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: "<!doctype html><body></body>",
    }),
  );
  await page.goto("/pending-storage");
  const result = await page.evaluate(async () => {
    for (const [name, id] of [
      ["mesh-talk-customization-bundled-v2", "public.fixture"],
      ["mesh-talk-customization", "legacy.fixture"],
    ]) {
      await new Promise<void>((resolve, reject) => {
        const request = indexedDB.open(name, 1);
        request.onupgradeneeded = () =>
          request.result
            .createObjectStore("packs", { keyPath: "id" })
            .put({ id, name: id, kind: "theme" });
        request.onsuccess = () => {
          request.result.close();
          resolve();
        };
        request.onerror = () => reject(request.error);
      });
    }
    const modulePath = "/src/store/packs.ts";
    const authPath = "/src/store/auth.ts";
    const { useAuth } = await import(/* @vite-ignore */ authPath);
    useAuth.setState({ user: { id: "pending-test" } });
    const { usePacks } = await import(/* @vite-ignore */ modulePath);
    const store = usePacks.getState();
    store.setScope("pending-test", "account");
    await store.remove("new.tombstone");
    const pending = await store.pendingChanges();
    const legacy = await store.legacyPacks();
    store.setScope(null, null);
    store.setScope("pending-test", "account");
    const restored = await store.pendingChanges();
    return {
      pending: pending.map((row: { id: string }) => row.id),
      restored: restored.length,
      publicPreserved: usePacks
        .getState()
        .packs.some((pack: { id: string }) => pack.id === "public.fixture"),
      legacy: legacy.map((pack: { id: string }) => pack.id),
    };
  });
  expect(result).toEqual({
    pending: ["new.tombstone"],
    restored: 1,
    publicPreserved: true,
    legacy: ["legacy.fixture"],
  });
});
