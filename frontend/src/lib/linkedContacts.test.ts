import { afterEach, beforeEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  policy: vi.fn(),
  preferences: vi.fn(),
  publish: vi.fn(),
  favorites: vi.fn(),
  hidden: vi.fn(),
  setFavorite: vi.fn(),
  applyFavorites: vi.fn(),
  applyHidden: vi.fn(),
  setAlias: vi.fn(),
  setChannelHidden: vi.fn(),
  setHidden: vi.fn(),
  loadFavorites: vi.fn(),
  loadHidden: vi.fn(),
}));
vi.mock("@/lib/api/chat", () => ({
  chat: {
    getDeviceSyncPolicy: mocks.policy,
    getAccountPreferences: mocks.preferences,
    setAccountPreference: mocks.publish,
  },
}));
vi.mock("@/lib/api", () => ({
  favorites: {
    get: mocks.favorites,
    apply: mocks.applyFavorites,
    setFavorite: mocks.setFavorite,
    setAlias: mocks.setAlias,
    setChannelHidden: mocks.setChannelHidden,
  },
  contactPolicy: {
    get: mocks.hidden,
    set: mocks.setHidden,
    apply: mocks.applyHidden,
  },
}));
vi.mock("@/store/chat", () => ({
  useChat: { getState: () => ({ loadFavorites: mocks.loadFavorites }) },
}));
vi.mock("@/store/contactPolicy", () => ({
  useContactPolicy: { getState: () => ({ load: mocks.loadHidden }) },
}));

import { startLinkedContacts } from "./linkedContacts";

const contactId = "ab".repeat(16);

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("window", globalThis);
  for (const mock of Object.values(mocks)) mock.mockReset();
  mocks.policy.mockResolvedValue({ contacts: true });
  mocks.preferences.mockResolvedValue([
    {
      category: "contacts",
      key: `favorite:${contactId}`,
      value: JSON.stringify({
        pinned: true,
        custom_alias: "Alice",
        hidden: false,
      }),
      revision: 5,
      device_id: "cd".repeat(16),
    },
  ]);
  mocks.favorites.mockResolvedValue([]);
  mocks.hidden.mockResolvedValue({ contacts: [] });
  mocks.publish.mockResolvedValue({ revision: 6 });
  mocks.setFavorite.mockResolvedValue(undefined);
  mocks.setAlias.mockResolvedValue(undefined);
  mocks.setChannelHidden.mockResolvedValue(undefined);
  mocks.loadFavorites.mockResolvedValue(undefined);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it("applies a linked favorite only when this device enables contact sync", async () => {
  mocks.policy.mockResolvedValueOnce({ contacts: false });
  const stop = startLinkedContacts("owner");
  await vi.waitFor(() => expect(mocks.preferences).toHaveBeenCalledTimes(1));
  expect(mocks.setFavorite).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(10_000);
  expect(mocks.applyFavorites).toHaveBeenCalledWith(
    "owner",
    [{ id: contactId, pinned: true, custom_alias: "Alice", hidden: false }],
    { [contactId]: null },
  );
  expect(mocks.loadFavorites).toHaveBeenCalledTimes(1);
  expect(mocks.publish).not.toHaveBeenCalled();
  stop();
});

it("imports a signed-account hidden contact without accepting malformed keys", async () => {
  mocks.preferences.mockResolvedValue([
    {
      category: "contacts",
      key: `hidden:${contactId}`,
      value: JSON.stringify("Bob"),
      revision: 5,
      device_id: "cd".repeat(16),
    },
    {
      category: "contacts",
      key: "favorite:bad-id",
      value: JSON.stringify({ pinned: true }),
      revision: 5,
      device_id: "cd".repeat(16),
    },
  ]);
  const stop = startLinkedContacts("owner");
  await vi.waitFor(() => expect(mocks.applyHidden).toHaveBeenCalledTimes(1));
  expect(mocks.applyHidden).toHaveBeenCalledWith(
    "owner",
    [{ account: contactId, hidden: true, name: "Bob" }],
    { [contactId]: null },
  );
  expect(mocks.loadHidden).toHaveBeenCalledWith("owner");
  expect(mocks.setFavorite).not.toHaveBeenCalled();
  stop();
});

it("publishes local favorite and hidden-contact choices after opt-in", async () => {
  mocks.preferences.mockResolvedValue([]);
  mocks.favorites.mockResolvedValue([
    { id: contactId, pinned: true, custom_alias: "Alice", hidden: true },
  ]);
  mocks.hidden.mockResolvedValue({
    contacts: [{ account_id: contactId, name: "Bob" }],
  });
  const stop = startLinkedContacts("owner");
  await vi.waitFor(() => expect(mocks.publish).toHaveBeenCalledTimes(2));
  expect(mocks.publish).toHaveBeenCalledWith(
    "contacts",
    `favorite:${contactId}`,
    JSON.stringify({ pinned: true, custom_alias: "Alice", hidden: true }),
    "owner",
  );
  expect(mocks.publish).toHaveBeenCalledWith(
    "contacts",
    `hidden:${contactId}`,
    JSON.stringify("Bob"),
    "owner",
  );
  stop();
});

it("publishes a local contact edit immediately after its commit notification", async () => {
  mocks.preferences.mockResolvedValue([]);
  const stop = startLinkedContacts("owner");
  const { notifySyncInvalidation } = await import("./syncInvalidation");
  try {
    await vi.advanceTimersByTimeAsync(0);
    mocks.favorites.mockResolvedValue([
      { id: contactId, pinned: true, custom_alias: null },
    ]);
    notifySyncInvalidation({ owner: "owner", kind: "contacts" });
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.publish).toHaveBeenCalledWith(
      "contacts",
      `favorite:${contactId}`,
      JSON.stringify({ pinned: true, custom_alias: null, hidden: false }),
      "owner",
    );
  } finally {
    stop();
  }
});

it("reconciles a newer local edit when a deferred linked batch loses its commit precondition", async () => {
  let reject!: (error: Error) => void;
  mocks.applyFavorites.mockImplementationOnce(
    () =>
      new Promise((_, fail) => {
        reject = fail;
      }),
  );
  const stop = startLinkedContacts("owner");
  const { notifySyncInvalidation } = await import("./syncInvalidation");
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.applyFavorites).toHaveBeenCalledTimes(1);
    mocks.favorites.mockResolvedValue([
      { id: contactId, pinned: true, custom_alias: "New local", hidden: false },
    ]);
    notifySyncInvalidation({ owner: "owner", kind: "contacts" });
    reject(new Error("Local favorites changed during linked import"));
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.publish).toHaveBeenCalledWith(
      "contacts",
      `favorite:${contactId}`,
      JSON.stringify({
        pinned: true,
        custom_alias: "New local",
        hidden: false,
      }),
      "owner",
    );
  } finally {
    stop();
  }
});

it("does not turn unsupported remote contact preferences into local deletions", async () => {
  mocks.preferences.mockResolvedValue([
    {
      category: "contacts",
      key: "future-setting",
      value: "opaque",
      revision: 1,
      device_id: "a".repeat(32),
    },
  ]);
  const stop = startLinkedContacts("owner");
  try {
    await vi.advanceTimersByTimeAsync(20000);
    expect(mocks.publish).not.toHaveBeenCalled();
    expect(mocks.applyFavorites).not.toHaveBeenCalled();
    expect(mocks.applyHidden).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});

it("keeps locally deleted contacts deleted while importing other hidden preferences", async () => {
  const otherId = "ef".repeat(16);
  mocks.hidden.mockResolvedValue({ contacts: [], deleted: [contactId] });
  mocks.preferences.mockResolvedValue(
    [contactId, otherId].map((id) => ({
      category: "contacts",
      key: `hidden:${id}`,
      value: JSON.stringify("Peer"),
      revision: 5,
      device_id: "cd".repeat(16),
    })),
  );
  const stop = startLinkedContacts("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.applyHidden).toHaveBeenCalledWith(
      "owner",
      [{ account: otherId, hidden: true, name: "Peer" }],
      { [otherId]: null },
    );
    expect(mocks.publish).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});
