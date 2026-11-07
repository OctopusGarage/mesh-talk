import { beforeEach, expect, it, vi } from "vitest";
import { strToU8, zipSync } from "fflate";
import {
  createPacksStore,
  type PackStorage,
  type PackScope,
  type StoredPack,
} from "./packs";
import type { DocumentVersion } from "@/lib/api/accountDocuments";
const verify = vi.hoisted(() => vi.fn());
vi.mock("@/lib/packImages", () => ({ verifyPackImages: verify }));
const version: DocumentVersion = {
  revision: 1,
  writer_device: "device",
  content_hash: [1],
};
function archive(id = "test.library", name = "Library") {
  return zipSync({
    "manifest.json": strToU8(
      JSON.stringify({
        format: 1,
        id,
        version: "1.0.0",
        name,
        kind: "theme",
        base: "dark",
        colors: { background: "0 0% 0%" },
      }),
    ),
  });
}
function memory() {
  const libraries = new Map<string, Map<string, StoredPack>>();
  const library = (scope: PackScope) => {
    const key = `${scope.owner}:${scope.account}`;
    if (!libraries.has(key)) libraries.set(key, new Map());
    return libraries.get(key)!;
  };
  const io: PackStorage = {
    publicPacks: async () => [],
    legacyPacks: async () => [],
    list: async (scope) => [...library(scope).values()],
    update: async (scope, id, mutate) => {
      const next = mutate(library(scope).get(id));
      if (next) library(scope).set(id, next);
      return next;
    },
  };
  return { io, libraries };
}
beforeEach(() => verify.mockReset().mockResolvedValue(undefined));
it("keeps installed archives and pending edits scoped across account changes and restart", async () => {
  const { io } = memory();
  const use = createPacksStore(io);
  const store = use.getState();
  store.setScope("owner", "first");
  const installedArchive = archive();
  await store.install(installedArchive);
  expect((await store.pendingChanges())[0].archive).toEqual(installedArchive);
  store.setScope("owner", "other");
  await store.load();
  expect(use.getState().packs).toEqual([]);
  const reopened = createPacksStore(io);
  reopened.getState().setScope("owner", "first");
  await reopened.getState().load();
  expect(reopened.getState().packs[0].id).toBe("test.library");
  expect(await reopened.getState().pendingChanges()).toHaveLength(1);
});
it("rejects remote overwrites while a local install or removal is pending", async () => {
  const { io } = memory();
  const use = createPacksStore(io);
  const store = use.getState();
  store.setScope("owner", "account");
  await store.install(archive());
  expect(
    await store.applyLinked(
      "test.library",
      archive("test.library", "Remote"),
      version,
      () => true,
    ),
  ).toBe(false);
  const pending = (await store.pendingChanges())[0];
  await store.acknowledge(pending.id, pending.sequence, version);
  expect(
    await store.applyLinked(
      "test.library",
      archive("test.library", "Remote"),
      { ...version, revision: 2 },
      () => true,
    ),
  ).toBe(true);
  expect(await store.pendingChanges()).toHaveLength(0);
  expect(use.getState().packs[0].name).toBe("Remote");
  await store.remove("test.library");
  expect(
    await store.applyLinked("test.library", archive(), version, () => true),
  ).toBe(false);
  expect(use.getState().packs).toEqual([]);
});
it("does not acknowledge a newer edit using an older upload result", async () => {
  const { io } = memory();
  const use = createPacksStore(io);
  const store = use.getState();
  store.setScope("owner", "account");
  await store.install(archive());
  const first = (await store.pendingChanges())[0];
  await store.remove(first.id);
  await store.acknowledge(first.id, first.sequence, version);
  expect((await store.pendingChanges())[0]).toMatchObject({
    pack: null,
    pending: true,
    sequence: first.sequence + 1,
  });
});
it("does not release a delayed image verification into a replacement account", async () => {
  const { io } = memory();
  const use = createPacksStore(io);
  const store = use.getState();
  store.setScope("owner", "first");
  let finish!: () => void;
  verify.mockImplementationOnce(
    () =>
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
  );
  const pending = store.install(archive());
  await vi.waitFor(() => expect(finish).toBeDefined());
  store.setScope("owner", "second");
  finish();
  await expect(pending).rejects.toThrow(/Account changed/);
  await store.load();
  expect(use.getState().packs).toEqual([]);
  expect(await store.pendingChanges()).toEqual([]);
});
it("applies remote deletion without echoing and rejects a mismatched archive ID", async () => {
  const { io } = memory();
  const use = createPacksStore(io);
  const store = use.getState();
  store.setScope("owner", "account");
  expect(
    await store.applyLinked("test.library", archive(), version, () => true),
  ).toBe(true);
  expect(
    await store.applyLinked(
      "test.library",
      null,
      { ...version, revision: 2 },
      () => true,
    ),
  ).toBe(true);
  expect(use.getState().packs).toEqual([]);
  expect(await store.pendingChanges()).toEqual([]);
  await expect(
    store.applyLinked("other.library", archive(), version, () => true),
  ).rejects.toThrow(/ID mismatch/);
});

it("clears private libraries on logout and keeps a new session's scoped namespace independent", async () => {
  const { io } = memory();
  const use = createPacksStore(io);
  const store = use.getState();
  store.setScope("owner", "account", "first");
  await store.install(archive());
  const first = use.getState().scope;
  store.setScope("owner", "account", "replacement");
  expect(use.getState().scope).not.toBe(first);
  store.setScope(null, null);
  expect(use.getState().packs).toEqual([]);
});
it("does not let an older decoded archive replace a newer imported revision", async () => {
  const { io } = memory();
  const use = createPacksStore(io);
  const store = use.getState();
  store.setScope("owner", "account");
  await store.applyLinked(
    "test.library",
    archive("test.library", "New"),
    { ...version, revision: 20 },
    () => true,
  );
  await store.applyLinked(
    "test.library",
    archive("test.library", "Old"),
    version,
    () => true,
  );
  expect(use.getState().packs[0].name).toBe("New");
});

it("uses the core hash tie breaker when imported version clocks and writers match", async () => {
  const { io } = memory();
  const use = createPacksStore(io);
  const store = use.getState();
  store.setScope("owner", "account");
  await store.applyLinked(
    "test.library",
    archive("test.library", "First"),
    version,
    () => true,
  );
  await store.applyLinked(
    "test.library",
    archive("test.library", "Hash winner"),
    { ...version, content_hash: [2] },
    () => true,
  );
  expect(use.getState().packs[0].name).toBe("Hash winner");
});

it("keeps bundled libraries public before login while clearing private account assets", async () => {
  const { io } = memory();
  const { parsePack } = await import("@/lib/pack");
  const bundled = parsePack(archive("public.theme", "Public"));
  io.publicPacks = async () => [bundled];
  const use = createPacksStore(io, () => () => false);
  await use.getState().load();
  expect(use.getState().packs).toEqual([bundled]);
  use.getState().setScope("owner", "account");
  expect(use.getState().packs).toEqual([bundled]);
  use.getState().setScope(null, null);
  expect(use.getState().packs).toEqual([bundled]);
});

it("restores the selected private theme before unrelated library rows finish loading", async () => {
  const { io } = memory();
  const { parsePack } = await import("@/lib/pack");
  const pack = parsePack(archive("private.theme"));
  const row: StoredPack = {
    id: pack.id,
    pack,
    archive: null,
    sequence: 0,
    pending: false,
    version: null,
  };
  let finish!: (rows: StoredPack[]) => void;
  io.list = () =>
    new Promise((resolve) => {
      finish = resolve;
    });
  io.get = vi.fn(async () => row);
  const use = createPacksStore(io);
  use.getState().setScope("owner", "account", "session");
  const loading = use.getState().load(pack.id);
  await vi.waitFor(() => expect(use.getState().selectedTheme).toEqual(pack));
  expect(io.get).toHaveBeenCalledWith(
    { owner: "owner", account: "account", session: "session" },
    pack.id,
  );
  expect(use.getState().loaded).toBe(false);
  expect(use.getState().preparedThemeId).toBe(pack.id);
  finish([row]);
  await loading;
  expect(use.getState().loaded).toBe(true);
});

it("does not restore a delayed selected theme after the verified account changes", async () => {
  const { io } = memory();
  const { parsePack } = await import("@/lib/pack");
  const pack = parsePack(archive("private.theme"));
  let finish!: (row: StoredPack) => void;
  io.get = () =>
    new Promise((resolve) => {
      finish = resolve;
    });
  const use = createPacksStore(io);
  use.getState().setScope("owner", "account");
  const loading = use.getState().load(pack.id);
  await vi.waitFor(() => expect(finish).toBeDefined());
  use.getState().setScope("owner", "replacement");
  finish({
    id: pack.id,
    pack,
    archive: null,
    sequence: 0,
    pending: false,
    version: null,
  });
  await loading;
  expect(use.getState().packs).toEqual([]);
  expect(use.getState().preparedThemeId).toBeNull();
});

it("surfaces selected-theme storage failure without declaring a missing selection", async () => {
  const { io } = memory();
  const { parsePack } = await import("@/lib/pack");
  const pack = parsePack(archive("private.theme"));
  io.get = async () => {
    throw new Error("read unavailable");
  };
  io.list = async () => [
    {
      id: pack.id,
      pack,
      archive: null,
      sequence: 0,
      pending: false,
      version: null,
    },
  ];
  const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
  try {
    const use = createPacksStore(io);
    use.getState().setScope("owner", "account");
    await expect(use.getState().load(pack.id)).rejects.toThrow(
      "read unavailable",
    );
    expect(use.getState().preparedThemeId).toBeNull();
    expect(use.getState().selectedTheme).toBeUndefined();
  } finally {
    warning.mockRestore();
  }
});

it.each(["bundled", "missing", "tombstone"] as const)(
  "settles a known %s selected-theme result before unrelated rows",
  async (kind) => {
    const { io } = memory();
    const { parsePack } = await import("@/lib/pack");
    const pack = parsePack(archive("selected.theme"));
    io.publicPacks = async () => (kind === "missing" ? [] : [pack]);
    io.get = async () =>
      kind === "tombstone"
        ? {
            id: pack.id,
            pack: null,
            archive: null,
            sequence: 0,
            pending: false,
            version: null,
          }
        : undefined;
    let finish!: (rows: StoredPack[]) => void;
    io.list = () =>
      new Promise((resolve) => {
        finish = resolve;
      });
    const use = createPacksStore(io);
    use.getState().setScope("owner", "account", "session");
    const loading = use.getState().load(pack.id);
    await vi.waitFor(() =>
      expect(use.getState().preparedThemeId).toBe(pack.id),
    );
    expect(use.getState().loaded).toBe(false);
    expect(use.getState().selectedTheme).toEqual(
      kind === "bundled" ? pack : undefined,
    );
    use.getState().setScope("owner", "account", "replacement-session");
    expect(use.getState().preparedThemeId).toBeNull();
    finish([]);
    await loading;
    expect(use.getState().preparedThemeId).toBeNull();
  },
);

it("reads authoritative pending rows without enumerating installed assets", async () => {
  const { io } = memory();
  const pending = vi.fn(async () => [] as StoredPack[]);
  const list = vi.spyOn(io, "list");
  const store = createPacksStore({ ...io, pending }).getState();
  store.setScope("owner", "account");
  expect(await store.pendingChanges()).toEqual([]);
  expect(pending).toHaveBeenCalledWith({
    owner: "owner",
    account: "account",
    session: undefined,
  });
  expect(list).not.toHaveBeenCalled();
});

it("surfaces pending lookup failures without falling back to full asset enumeration", async () => {
  const { io } = memory();
  const list = vi.spyOn(io, "list");
  const store = createPacksStore({
    ...io,
    pending: async () => {
      throw new Error("pending storage unavailable");
    },
  }).getState();
  store.setScope("owner", "account");
  await expect(store.pendingChanges()).rejects.toThrow(
    "pending storage unavailable",
  );
  expect(list).not.toHaveBeenCalled();
});

it("discards a pending lookup completed after the desktop session changes", async () => {
  const { io } = memory();
  let finish!: (rows: StoredPack[]) => void;
  const store = createPacksStore({
    ...io,
    pending: () =>
      new Promise<StoredPack[]>((resolve) => {
        finish = resolve;
      }),
  }).getState();
  store.setScope("owner", "account", "first");
  const read = store.pendingChanges();
  store.setScope("owner", "account", "next");
  finish([
    {
      id: "old",
      pack: null,
      archive: null,
      sequence: 1,
      pending: true,
      version: null,
    },
  ]);
  expect(await read).toEqual([]);
});

it("loads metadata without reading unrelated payloads and discards held reads after removal", async () => {
  const { parsePack } = await import("@/lib/pack");
  const pack = parsePack(archive());
  const { io } = memory();
  io.publicCatalog = vi.fn(async () => []);
  io.catalog = vi.fn(async () => [
    { id: pack.id, pack, sequence: 0, pending: false, version: null },
  ]);
  const list = vi.spyOn(io, "list");
  let finish!: (row: StoredPack) => void;
  io.get = vi.fn(
    () =>
      new Promise<StoredPack>((resolve) => {
        finish = resolve;
      }),
  );
  const use = createPacksStore(io);
  use.getState().setScope("owner", "account");
  await use.getState().load();
  expect(list).not.toHaveBeenCalled();
  expect(io.get).not.toHaveBeenCalled();
  const read = use.getState().readPack(pack.id);
  await vi.waitFor(() => expect(finish).toBeDefined());
  await use.getState().remove(pack.id);
  finish({
    id: pack.id,
    pack,
    archive: null,
    sequence: 0,
    pending: false,
    version: null,
  });
  expect(await read).toBeUndefined();
});

it("waits for bundled preparation before declaring a selected public theme missing", async () => {
  const { parsePack } = await import("@/lib/pack");
  const pack = parsePack(archive("public.theme"));
  const { io } = memory();
  let finish!: () => void;
  io.publicCatalog = () =>
    new Promise((resolve) => {
      finish = () => resolve([pack]);
    });
  io.publicGet = vi.fn(async () => pack);
  io.get = async () => undefined;
  io.catalog = async () => [];
  const use = createPacksStore(io);
  use.getState().setScope("owner", "account");
  const loading = use.getState().load(pack.id);
  await Promise.resolve();
  expect(use.getState().preparedThemeId).toBeNull();
  expect(io.publicGet).not.toHaveBeenCalled();
  finish();
  await loading;
  expect(use.getState().selectedTheme).toEqual(pack);
});

it("shares simultaneous reads and keeps the active theme on storage failure", async () => {
  const { parsePack } = await import("@/lib/pack");
  const pack = parsePack(archive());
  const { io } = memory();
  io.catalog = async () => [
    { id: pack.id, pack, sequence: 0, pending: false, version: null },
  ];
  io.get = vi.fn(async () => ({
    id: pack.id,
    pack,
    archive: null,
    sequence: 0,
    pending: false,
    version: null,
  }));
  const use = createPacksStore(io);
  use.getState().setScope("owner", "account");
  await use.getState().load();
  const [left, right] = await Promise.all([
    use.getState().readPack(pack.id),
    use.getState().readPack(pack.id),
  ]);
  expect(left).toBe(right);
  expect(io.get).toHaveBeenCalledOnce();
  await use.getState().prepareTheme(pack.id);
  io.get = async () => {
    throw new Error("storage unavailable");
  };
  await expect(use.getState().prepareTheme(pack.id)).rejects.toThrow(
    "storage unavailable",
  );
  expect(use.getState().selectedTheme).toBe(pack);
});

it("catalog projection owns no artwork URLs or archive bytes", async () => {
  const { packMetadata } = await import("./packs");
  const full = {
    format: 1 as const,
    id: "test.stickers",
    version: "1.0.0",
    name: "Stickers",
    kind: "sticker" as const,
    stickers: [
      {
        id: "one",
        label: "One",
        fallback: "x",
        url: "data:image/png;base64,AQID",
      },
    ],
  };
  expect(packMetadata(full)).toEqual({
    ...full,
    stickers: [{ id: "one", label: "One", fallback: "x" }],
  });
});

it("keeps the latest requested theme through an unrelated library reload and returns an exact readiness receipt", async () => {
  const { io } = memory();
  const use = createPacksStore(io);
  const store = use.getState();
  store.setScope("owner", "account");
  await store.install(archive("first.theme"));
  await store.install(archive("second.theme"));
  await store.prepareTheme("first.theme");
  let finish!: () => void;
  io.get = async (scope, id) => {
    const row = (await io.list(scope)).find((row) => row.id === id);
    if (id === "second.theme")
      await new Promise<void>((resolve) => {
        finish = resolve;
      });
    return row;
  };
  const selecting = store.prepareTheme("second.theme");
  await vi.waitFor(() => expect(finish).toBeDefined());
  const mutation = store.install(archive("unrelated.theme"));
  await vi.waitFor(() =>
    expect(
      use.getState().packs.some((pack) => pack.id === "unrelated.theme"),
    ).toBe(true),
  );
  finish();
  expect(await selecting).toBe(true);
  await mutation;
  expect(use.getState().selectedTheme?.id).toBe("second.theme");
  expect(use.getState().preparedThemeId).toBe("second.theme");
});

it("bounds unique queued reads with a retryable capacity failure and recovers permits", async () => {
  const { parsePack } = await import("@/lib/pack");
  const pack = parsePack(archive());
  const { io } = memory();
  io.catalog = async () =>
    Array.from({ length: 67 }, (_, index) => ({
      id: `pack.${index}`,
      pack: { ...pack, id: `pack.${index}` },
      sequence: 0,
      pending: false,
      version: null,
    }));
  const finishes: Array<() => void> = [];
  io.get = vi.fn(async (_scope, id) => {
    await new Promise<void>((resolve) => finishes.push(resolve));
    return {
      id,
      pack: { ...pack, id },
      archive: null,
      sequence: 0,
      pending: false,
      version: null,
    };
  });
  const use = createPacksStore(io);
  use.getState().setScope("owner", "account");
  await use.getState().load();
  const reads = Array.from({ length: 66 }, (_, index) =>
    use.getState().readPack(`pack.${index}`),
  );
  let rejected = false;
  const overflow = use
    .getState()
    .readPack("pack.66")
    .catch((error) => {
      rejected = /capacity/i.test(String(error));
    });
  for (let turn = 0; turn < 20; turn++) await Promise.resolve();
  expect(io.get).toHaveBeenCalledTimes(2);
  expect(rejected).toBe(true);
  for (let turn = 0; turn < 200; turn++) {
    finishes.splice(0).forEach((finish) => finish());
    await Promise.resolve();
  }
  await Promise.all([...reads, overflow]);
  const recovered = use.getState().readPack("pack.66");
  await vi.waitFor(() => expect(finishes.length).toBe(1));
  finishes.shift()!();
  expect((await recovered)?.id).toBe("pack.66");
});

it("publishes pending payloads one at a time from metadata and refreshes acknowledged version authority", async () => {
  const { io } = memory();
  const list = io.list;
  io.catalog = async (scope) =>
    (await list(scope)).map(({ archive: _archive, ...row }) => row);
  io.get = vi.fn(async (scope, id) =>
    (await list(scope)).find((row) => row.id === id),
  );
  io.pending = vi.fn(async (scope) => list(scope));
  const use = createPacksStore(io);
  const store = use.getState();
  store.setScope("owner", "account");
  await store.install(archive("first.theme"));
  await store.install(archive("later.theme"));
  vi.mocked(io.get).mockClear();
  const visited: string[] = [];
  await store.forEachPending(async (row) => {
    visited.push(row.id);
    expect(io.get).toHaveBeenCalledTimes(visited.length);
    if (row.id === "later.theme")
      await store.acknowledge(row.id, row.sequence, version);
  });
  expect(visited).toEqual(["first.theme", "later.theme"]);
  expect(io.pending).not.toHaveBeenCalled();
  expect(store.linkedVersion("later.theme")).toEqual(version);
  expect(store.linkedVersion("first.theme")).toBeNull();
});

it("retires queued view demand without dropping a shared live reader or releasing active work early", async () => {
  const { parsePack } = await import("@/lib/pack");
  const pack = parsePack(archive());
  const { io } = memory();
  io.catalog = async () =>
    Array.from({ length: 4 }, (_, index) => ({
      id: `pack.${index}`,
      pack: { ...pack, id: `pack.${index}` },
      sequence: 0,
      pending: false,
      version: null,
    }));
  const finishes = new Map<string, () => void>();
  io.get = vi.fn(async (_scope, id) => {
    await new Promise<void>((resolve) => finishes.set(id, resolve));
    return {
      id,
      pack: { ...pack, id },
      archive: null,
      sequence: 0,
      pending: false,
      version: null,
    };
  });
  const use = createPacksStore(io);
  use.getState().setScope("owner", "account");
  await use.getState().load();
  let active = true;
  let open = true;
  const first = use.getState().readPack("pack.0", () => active);
  const second = use.getState().readPack("pack.1");
  const retired = use.getState().readPack("pack.2", () => open);
  const sharedRetired = use.getState().readPack("pack.3", () => open);
  const live = use.getState().readPack("pack.3", () => true);
  open = false;
  active = false;
  for (let turn = 0; turn < 20; turn++) await Promise.resolve();
  expect(io.get).toHaveBeenCalledTimes(2); // retiring the observer did not release actual work
  finishes.get("pack.0")!();
  await vi.waitFor(() => expect(finishes.has("pack.3")).toBe(true));
  expect(finishes.has("pack.2")).toBe(false);
  expect(io.get).toHaveBeenCalledTimes(3);
  finishes.get("pack.1")!();
  finishes.get("pack.3")!();
  expect(await first).toBeUndefined();
  expect(await retired).toBeUndefined();
  expect(await sharedRetired).toBeUndefined();
  expect((await live)?.id).toBe("pack.3");
  await second;
});
