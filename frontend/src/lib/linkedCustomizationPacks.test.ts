import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { strToU8, zipSync } from "fflate";
import { useAuth } from "@/store/auth";
import type {
  AccountDocumentMeta,
  AccountDocumentKey,
  DocumentVersion,
} from "./api/accountDocuments";
const mocks = vi.hoisted(() => ({
  account: "crypto-account",
  policy: vi.fn(),
  legacy: vi.fn(),
  list: vi.fn(),
  meta: vi.fn(),
  read: vi.fn(),
  put: vi.fn(),
  remove: vi.fn(),
  rows: new Map<string, Map<string, unknown>>(),
  documents: new Map<
    string,
    { meta: AccountDocumentMeta; bytes: Uint8Array | null }
  >(),
}));
vi.mock("@/store/chat", () => ({
  useChat: { getState: () => ({ myAccountId: mocks.account }) },
}));
vi.mock("./api/chat", () => ({ chat: { getDeviceSyncPolicy: mocks.policy } }));
vi.mock("./packImages", () => ({ verifyPackImages: async () => {} }));
vi.mock("./api/accountDocuments", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api/accountDocuments")>()),
  accountDocuments: {
    list: mocks.list,
    meta: mocks.meta,
    read: mocks.read,
    put: mocks.put,
    remove: mocks.remove,
  },
}));
vi.mock("@/store/packs", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/store/packs")>();
  const rows = (scope: { owner: string; account: string }) => {
    const key = `${scope.owner}:${scope.account}`;
    if (!mocks.rows.has(key)) mocks.rows.set(key, new Map());
    return mocks.rows.get(key)! as Map<
      string,
      import("@/store/packs").StoredPack
    >;
  };
  return {
    ...actual,
    usePacks: actual.createPacksStore({
      publicPacks: async () => [],
      legacyPacks: mocks.legacy,
      list: async (scope) => [...rows(scope).values()],
      update: async (scope, id, mutate) => {
        const result = mutate(rows(scope).get(id));
        if (result) rows(scope).set(id, result);
        return result;
      },
    }),
  };
});
import { usePacks } from "@/store/packs";
import { parsePack } from "./pack";
import {
  startLinkedCustomizationPacks,
  publishCustomizationPacksBeforeLink,
} from "./linkedCustomizationPacks";
const key = (id = "test.library"): AccountDocumentKey => ({
  kind: "customization_pack",
  pack_id: id,
});
const archive = (id = "test.library", name = "Library") =>
  zipSync({
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
function record(
  id = "test.library",
  revision = 1,
  tombstone = false,
): AccountDocumentMeta {
  return {
    key: key(id),
    revision,
    writer_device: "a".repeat(32),
    content_hash: [revision],
    content_len: tombstone ? 0 : archive(id).length,
    tombstone,
  };
}
function documentId(documentKey: AccountDocumentKey) {
  if (documentKey.kind !== "customization_pack")
    throw new Error("bad test key");
  return documentKey.pack_id;
}
function matches(
  expected: DocumentVersion | null,
  actual: AccountDocumentMeta | undefined,
) {
  return actual ? expected?.revision === actual.revision : expected === null;
}
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("window", globalThis);
  useAuth.setState({
    user: { id: "owner", username: "owner", display_name: "Owner" },
    generation: 1,
  });
  mocks.account = "crypto-account";
  mocks.rows.clear();
  mocks.documents.clear();
  usePacks.getState().setScope(null, null);
  mocks.policy.mockReset().mockResolvedValue({ appearance: true });
  mocks.legacy.mockReset().mockResolvedValue([]);
  mocks.list.mockReset().mockImplementation(async () => ({
    items: [...mocks.documents.values()].map((row) => row.meta),
    next: null,
    digest: [],
  }));
  mocks.meta
    .mockReset()
    .mockImplementation(
      async (_owner, _account, k) =>
        mocks.documents.get(documentId(k))?.meta ?? null,
    );
  mocks.read
    .mockReset()
    .mockImplementation(
      async (_owner, _account, k) =>
        mocks.documents.get(documentId(k))!.bytes!.slice().buffer,
    );
  mocks.put
    .mockReset()
    .mockImplementation(async (_owner, _account, k, bytes, expected) => {
      const id = documentId(k);
      const previous = mocks.documents.get(id);
      if (!matches(expected, previous?.meta)) throw new Error("CAS conflict");
      const meta = record(id, (previous?.meta.revision ?? 0) + 1);
      mocks.documents.set(id, { meta, bytes });
      return meta;
    });
  mocks.remove
    .mockReset()
    .mockImplementation(async (_owner, _account, k, expected) => {
      const id = documentId(k);
      const previous = mocks.documents.get(id);
      if (!matches(expected, previous?.meta)) throw new Error("CAS conflict");
      const meta = record(id, (previous?.meta.revision ?? 0) + 1, true);
      mocks.documents.set(id, { meta, bytes: null });
      return meta;
    });
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});
it("imports validated assets and tombstones without republishing them", async () => {
  mocks.documents.set("test.library", { meta: record(), bytes: archive() });
  const stop = startLinkedCustomizationPacks("owner");
  try {
    await vi.waitFor(() =>
      expect(usePacks.getState().packs[0]?.id).toBe("test.library"),
    );
    expect(mocks.put).not.toHaveBeenCalled();
    expect(mocks.legacy).not.toHaveBeenCalled();
    mocks.documents.set("test.library", {
      meta: record("test.library", 2, true),
      bytes: null,
    });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(usePacks.getState().packs).toEqual([]);
    expect(mocks.remove).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});
it("retries durable pending archives after a CAS failure and binds every transfer to its account", async () => {
  mocks.put.mockRejectedValueOnce(new Error("CAS conflict"));
  const stop = startLinkedCustomizationPacks("owner");
  try {
    await vi.waitFor(() => expect(mocks.list).toHaveBeenCalled());
    await usePacks.getState().install(archive());
    await vi.waitFor(() => expect(mocks.put).toHaveBeenCalledTimes(1));
    expect(await usePacks.getState().pendingChanges()).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(10_000);
    await vi.waitFor(async () =>
      expect(await usePacks.getState().pendingChanges()).toHaveLength(0),
    );
    expect(mocks.put).toHaveBeenLastCalledWith(
      "owner",
      "crypto-account",
      key(),
      expect.any(Uint8Array),
      null,
    );
  } finally {
    stop();
  }
});
it("publishes a create-only deletion tombstone when a pack was removed before upload", async () => {
  const store = usePacks.getState();
  store.setScope("owner", "crypto-account");
  await store.remove("test.library");
  const stop = startLinkedCustomizationPacks("owner");
  try {
    await vi.waitFor(() =>
      expect(mocks.remove).toHaveBeenCalledWith(
        "owner",
        "crypto-account",
        key(),
        null,
      ),
    );
    expect(mocks.documents.get("test.library")?.meta.tombstone).toBe(true);
  } finally {
    stop();
  }
});
it("does not transfer optional libraries while appearance sync is disabled", async () => {
  mocks.policy.mockResolvedValue({ appearance: false });
  const stop = startLinkedCustomizationPacks("owner");
  try {
    await vi.waitFor(() => expect(mocks.policy).toHaveBeenCalled());
    expect(mocks.list).not.toHaveBeenCalled();
    expect(mocks.legacy).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});
it("attributes legacy libraries only during explicit source preparation and preserves canonical tombstones", async () => {
  const first = parsePack(archive());
  const deleted = parsePack(archive("deleted.library"));
  mocks.legacy.mockResolvedValue([first, deleted]);
  mocks.documents.set("deleted.library", {
    meta: record("deleted.library", 5, true),
    bytes: null,
  });
  await publishCustomizationPacksBeforeLink("owner");
  expect(mocks.put).toHaveBeenCalledTimes(1);
  expect(mocks.put).toHaveBeenCalledWith(
    "owner",
    "crypto-account",
    key(),
    expect.any(Uint8Array),
    null,
  );
  expect(mocks.documents.get("deleted.library")?.meta.tombstone).toBe(true);
  expect(mocks.legacy).toHaveBeenCalledTimes(1);
});
it("keeps a sibling asset that arrives between the source absence check and publication", async () => {
  mocks.legacy.mockResolvedValue([parsePack(archive())]);
  const remote = parsePack(archive("test.library", "Sibling"));
  mocks.put.mockImplementationOnce(async () => {
    mocks.documents.set("test.library", {
      meta: record("test.library", 7),
      bytes: archive("test.library", "Sibling"),
    });
    throw new Error("CAS conflict");
  });
  await publishCustomizationPacksBeforeLink("owner");
  expect(parsePack(mocks.documents.get("test.library")!.bytes!)).toEqual(
    remote,
  );
  expect(await usePacks.getState().pendingChanges()).toEqual([]);
});
it("ignores a delayed archive read after same-owner session replacement and clears its private view", async () => {
  mocks.documents.set("test.library", { meta: record(), bytes: archive() });
  let finish!: (bytes: ArrayBuffer) => void;
  mocks.read.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const stop = startLinkedCustomizationPacks("owner");
  await vi.waitFor(() => expect(finish).toBeDefined());
  useAuth.setState({ generation: 2 });
  finish(archive().slice().buffer as ArrayBuffer);
  await vi.advanceTimersByTimeAsync(0);
  stop();
  expect(usePacks.getState().packs).toEqual([]);
  expect(usePacks.getState().scope).toBeNull();
});

it("reports a local library storage failure after canonical source publication", async () => {
  mocks.legacy.mockResolvedValue([parsePack(archive())]);
  const blocked = new Map<string, import("@/store/packs").StoredPack>();
  blocked.set = () => {
    throw new Error("Library storage quota exceeded");
  };
  mocks.rows.set("owner:crypto-account", blocked);
  await expect(publishCustomizationPacksBeforeLink("owner")).rejects.toThrow(
    /quota/,
  );
  expect(mocks.documents.has("test.library")).toBe(true);
});
