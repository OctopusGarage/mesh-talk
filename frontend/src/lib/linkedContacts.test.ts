import { afterEach, beforeEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  policy: vi.fn(),
  preferences: vi.fn(),
  publish: vi.fn(),
  favorites: vi.fn(),
  hidden: vi.fn(),
  setFavorite: vi.fn(),
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
    setFavorite: mocks.setFavorite,
    setAlias: mocks.setAlias,
    setChannelHidden: mocks.setChannelHidden,
  },
  contactPolicy: { get: mocks.hidden, set: mocks.setHidden },
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
  expect(mocks.setFavorite).toHaveBeenCalledWith("owner", contactId, true);
  expect(mocks.setAlias).toHaveBeenCalledWith("owner", contactId, "Alice");
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
  await vi.waitFor(() => expect(mocks.setHidden).toHaveBeenCalledTimes(1));
  expect(mocks.setHidden).toHaveBeenCalledWith("owner", contactId, true, "Bob");
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
  );
  expect(mocks.publish).toHaveBeenCalledWith(
    "contacts",
    `hidden:${contactId}`,
    JSON.stringify("Bob"),
  );
  stop();
});
