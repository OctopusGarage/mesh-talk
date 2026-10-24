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
  await store.install(archive());
  expect((await store.pendingChanges())[0].archive).toEqual(archive());
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
