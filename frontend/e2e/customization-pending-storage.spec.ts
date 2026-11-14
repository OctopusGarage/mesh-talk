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
    // Pending payload reads are intentionally limited to one row. Inspect index
    // keys separately to prove every legacy edit migrated without cloning assets.
    const migratedIds = await new Promise<IDBValidKey[]>((resolve, reject) => {
      const request = indexedDB.open(name);
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        const db = request.result;
        const tx = db.transaction("packs", "readonly");
        const keys = tx.objectStore("packs").index("pendingSync").getAllKeys(1);
        tx.oncomplete = () => {
          db.close();
          resolve(keys.result);
        };
        tx.onabort = () => {
          db.close();
          reject(tx.error);
        };
      };
    });
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
      migrated: migratedIds.sort(),
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
    stale: 1,
    failed: 1,
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

test("idle library reads reuse one account connection until logout", async ({
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
    const modulePath = "/src/store/packs.ts";
    const { createPacksStore, indexedPackStorage } = await import(
      /* @vite-ignore */ modulePath
    );
    const scope = {
      owner: "idle-connections",
      account: "account",
      session: "first",
    };
    const name = "mesh-talk-customization-account:idle-connections:account";
    const open = indexedDB.open.bind(indexedDB);
    const close = IDBDatabase.prototype.close;
    let opens = 0;
    let closes = 0;
    indexedDB.open = (...args) => {
      if (args[0] === name) opens++;
      return open(...args);
    };
    IDBDatabase.prototype.close = function () {
      if (this.name === name) closes++;
      return close.call(this);
    };
    const use = createPacksStore(indexedPackStorage);
    try {
      use.getState().setScope(scope.owner, scope.account, scope.session);
      for (let i = 0; i < 3; i++) await indexedPackStorage.get(scope, "absent");
      const settled = { opens, closes };
      use.getState().setScope(null, null);
      await Promise.resolve();
      return { settled, retired: { opens, closes } };
    } finally {
      use.getState().setScope(null, null);
      indexedDB.open = open;
      IDBDatabase.prototype.close = close;
    }
  });
  expect(result).toEqual({
    settled: { opens: 1, closes: 0 },
    retired: { opens: 1, closes: 1 },
  });
});

test("idle library version confirmation uses no readwrite transaction or row put", async ({
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
    const modulePath = "/src/store/packs.ts";
    const { createPacksStore, indexedPackStorage } = await import(
      /* @vite-ignore */ modulePath
    );
    const use = createPacksStore(indexedPackStorage);
    const store = use.getState();
    store.setScope("idle-confirmation", "account", "first");
    const version = { revision: 1, writer_device: "device", content_hash: [1] };
    await store.applyLinked("idle.tombstone", null, version, () => true);
    const transaction = IDBDatabase.prototype.transaction;
    const put = IDBObjectStore.prototype.put;
    let writes = 0;
    let puts = 0;
    IDBDatabase.prototype.transaction = function (...args) {
      if (this.name.includes("idle-confirmation") && args[1] === "readwrite")
        writes++;
      return transaction.apply(this, args);
    };
    IDBObjectStore.prototype.put = function (...args) {
      if (this.transaction.db.name.includes("idle-confirmation")) puts++;
      return put.apply(this, args);
    };
    try {
      const retained = await store.applyLinked(
        "idle.tombstone",
        null,
        version,
        () => true,
      );
      const quiet = { retained, writes, puts };
      const applied = await store.applyLinked(
        "idle.tombstone",
        null,
        { ...version, revision: 2 },
        () => true,
      );
      return { quiet, changed: { applied, writes, puts } };
    } finally {
      store.setScope(null, null);
      IDBDatabase.prototype.transaction = transaction;
      IDBObjectStore.prototype.put = put;
    }
  });
  expect(result).toEqual({
    quiet: { retained: true, writes: 0, puts: 0 },
    changed: { applied: true, writes: 1, puts: 2 },
  });
});

test("library connections retire on versionchange and never mask database deletion", async ({
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
    const modulePath = "/src/store/packs.ts";
    const { createPacksStore, indexedPackStorage } = await import(
      /* @vite-ignore */ modulePath
    );
    const scope = {
      owner: "versionchange",
      account: "account",
      session: "one",
    };
    const use = createPacksStore(indexedPackStorage);
    const store = use.getState();
    store.setScope(scope.owner, scope.account, scope.session);
    const version = { revision: 1, writer_device: "device", content_hash: [1] };
    await store.applyLinked("gone", null, version, () => true);
    const name = "mesh-talk-customization-account:versionchange:account";
    await new Promise<void>((resolve, reject) => {
      const request = indexedDB.deleteDatabase(name);
      request.onsuccess = () => resolve();
      request.onerror = () => reject(request.error);
      request.onblocked = () =>
        reject(new Error("Owned connection blocked deletion"));
    });
    const absent = await indexedPackStorage.get(scope, "gone");
    // The same version must be written again after the actual database vanished.
    const restored = await store.applyLinked("gone", null, version, () => true);
    const row = await indexedPackStorage.get(scope, "gone");
    store.setScope(null, null);
    return {
      absent: absent === undefined,
      restored,
      version: row.version.revision,
    };
  });
  expect(result).toEqual({ absent: true, restored: true, version: 1 });
});

for (const sameAccount of [false, true]) {
  test(`a retired blocked database open never replaces the current ${sameAccount ? "session" : "account"} connection`, async ({
    page,
  }) => {
    await page.route("**/pending-storage", (route) =>
      route.fulfill({
        contentType: "text/html",
        body: "<!doctype html><body></body>",
      }),
    );
    await page.goto("/pending-storage");
    const result = await page.evaluate(async (sameAccount) => {
      const firstName = "mesh-talk-customization-account:stale-open:first";
      const secondName = sameAccount
        ? firstName
        : "mesh-talk-customization-account:stale-open:second";
      const blocker = await new Promise<IDBDatabase>((resolve, reject) => {
        const request = indexedDB.open(firstName, 1);
        request.onupgradeneeded = () =>
          request.result.createObjectStore("packs", { keyPath: "id" });
        request.onsuccess = () => resolve(request.result);
        request.onerror = () => reject(request.error);
      });
      const modulePath = "/src/store/packs.ts";
      const { createPacksStore, indexedPackStorage } = await import(
        /* @vite-ignore */ modulePath
      );
      const use = createPacksStore(indexedPackStorage);
      let blocked!: () => void;
      const pending = new Promise<void>((resolve) => {
        blocked = resolve;
      });
      const open = indexedDB.open.bind(indexedDB);
      let currentOpens = 0;
      let retired: IDBDatabase | undefined;
      indexedDB.open = (...args) => {
        const request = open(...args);
        if (args[0] === secondName) currentOpens++;
        if (args[0] === firstName) {
          request.addEventListener("blocked", () => blocked());
          request.addEventListener("success", () => {
            retired ??= request.result;
          });
        }
        return request;
      };
      try {
        const first = {
          owner: "stale-open",
          account: "first",
          session: "first",
        };
        use.getState().setScope(first.owner, first.account, first.session);
        const stale = indexedPackStorage.get(first, "absent");
        await pending;
        const second = {
          ...first,
          account: sameAccount ? "first" : "second",
          session: "second",
        };
        use.getState().setScope(second.owner, second.account, second.session);
        const current = indexedPackStorage.get(second, "absent");
        blocker.close();
        await Promise.all([stale, current]);
        await indexedPackStorage.get(second, "absent");
        let retiredClosed = false;
        try {
          retired!.transaction("packs", "readonly");
        } catch {
          retiredClosed = true;
        }
        return { currentOpens, retiredClosed };
      } finally {
        blocker.close();
        use.getState().setScope(null, null);
        indexedDB.open = open;
      }
    }, sameAccount);
    expect(result).toEqual({
      currentOpens: sameAccount ? 2 : 1,
      retiredClosed: true,
    });
  });
}

test("library connection retention stays bounded across sessions and legacy reads", async ({
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
    await new Promise<void>((resolve, reject) => {
      const request = indexedDB.open("mesh-talk-customization", 1);
      request.onupgradeneeded = () =>
        request.result.createObjectStore("packs", { keyPath: "id" });
      request.onsuccess = () => {
        request.result.close();
        resolve();
      };
      request.onerror = () => reject(request.error);
    });
    const modulePath = "/src/store/packs.ts";
    const { createPacksStore, indexedPackStorage } = await import(
      /* @vite-ignore */ modulePath
    );
    const use = createPacksStore(indexedPackStorage);
    const live = new Set<IDBDatabase>();
    const open = indexedDB.open.bind(indexedDB);
    const close = IDBDatabase.prototype.close;
    let legacyCloses = 0;
    indexedDB.open = (...args) => {
      const request = open(...args);
      request.addEventListener("success", () => live.add(request.result));
      return request;
    };
    IDBDatabase.prototype.close = function () {
      live.delete(this);
      if (this.name === "mesh-talk-customization") legacyCloses++;
      return close.call(this);
    };
    try {
      const counts: number[] = [];
      for (let turn = 0; turn < 4; turn++) {
        const scope = {
          owner: "bounded-connections",
          account: String(turn % 2),
          session: String(turn),
        };
        use.getState().setScope(scope.owner, scope.account, scope.session);
        await indexedPackStorage.get(scope, "absent");
        await indexedPackStorage.publicGet("absent");
        await indexedPackStorage.legacyPacks();
        await indexedPackStorage.legacyPacks();
        counts.push(live.size);
      }
      use.getState().setScope(null, null);
      return { counts, retired: live.size, legacyCloses };
    } finally {
      use.getState().setScope(null, null);
      indexedDB.open = open;
      IDBDatabase.prototype.close = close;
    }
  });
  expect(result).toEqual({ counts: [2, 2, 2, 2], retired: 0, legacyCloses: 8 });
});

test("a failed library connection open can retry after the incompatible database is removed", async ({
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
    const name = "mesh-talk-customization-account:failed-open:account";
    await new Promise<void>((resolve, reject) => {
      const request = indexedDB.open(name, 4);
      request.onsuccess = () => {
        request.result.close();
        resolve();
      };
      request.onerror = () => reject(request.error);
    });
    const modulePath = "/src/store/packs.ts";
    const { createPacksStore, indexedPackStorage } = await import(
      /* @vite-ignore */ modulePath
    );
    const use = createPacksStore(indexedPackStorage);
    const scope = { owner: "failed-open", account: "account", session: "one" };
    use.getState().setScope(scope.owner, scope.account, scope.session);
    let failed = false;
    try {
      await indexedPackStorage.get(scope, "absent");
    } catch (error) {
      failed = error instanceof DOMException && error.name === "VersionError";
    }
    await new Promise<void>((resolve, reject) => {
      const request = indexedDB.deleteDatabase(name);
      request.onsuccess = () => resolve();
      request.onerror = () => reject(request.error);
    });
    const recovered = await indexedPackStorage.get(scope, "absent");
    use.getState().setScope(null, null);
    return { failed, recovered: recovered === undefined };
  });
  expect(result).toEqual({ failed: true, recovered: true });
});
