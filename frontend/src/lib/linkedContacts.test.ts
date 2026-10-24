import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useAuth } from "@/store/auth";

const scope = vi.hoisted(() => ({ account: "source-crypto-account" }));

const mocks = vi.hoisted(() => ({
  policy: vi.fn(),
  preferences: vi.fn(),
  publish: vi.fn(),
  seed: vi.fn(),
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
    seedAccountPreference: mocks.seed,
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
  useChat: {
    getState: () => ({
      myAccountId: scope.account,
      loadFavorites: mocks.loadFavorites,
    }),
  },
}));
vi.mock("@/store/contactPolicy", () => ({
  useContactPolicy: { getState: () => ({ load: mocks.loadHidden }) },
}));

import {
  publishContactsBeforeLink,
  startLinkedContacts,
} from "./linkedContacts";

const contactId = "ab".repeat(16);

beforeEach(() => {
  scope.account = "source-crypto-account";
  useAuth.setState({
    user: { id: "owner", username: "Owner", display_name: "Owner" },
    generation: 1,
  });
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
  mocks.seed.mockResolvedValue({ revision: 6 });
  mocks.publish.mockResolvedValue({ revision: 6 });
  mocks.setFavorite.mockResolvedValue(undefined);
  mocks.setAlias.mockResolvedValue(undefined);
  mocks.setChannelHidden.mockResolvedValue(undefined);
  mocks.loadFavorites.mockResolvedValue(undefined);
  mocks.applyHidden.mockImplementation(
    async (
      owner: string,
      _account: string,
      records: Array<{ account: string; hidden: boolean; name: string }>,
    ) => ({
      owner,
      contacts: records
        .filter((record) => record.hidden)
        .map((record) => ({ account_id: record.account, name: record.name })),
      deleted: [],
    }),
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

it("durably publishes saved contacts before a pairing code can be issued", async () => {
  mocks.preferences.mockResolvedValue([]);
  mocks.favorites.mockResolvedValue([
    { id: contactId, pinned: true, custom_alias: "Alice", hidden: false },
  ]);
  mocks.hidden.mockResolvedValue({
    contacts: [{ account_id: contactId, name: "Bob" }],
  });
  let finish!: () => void;
  mocks.seed.mockImplementationOnce(
    () => new Promise<void>((resolve) => (finish = resolve)),
  );
  let completed = false;
  const seed = publishContactsBeforeLink("owner").then(() => {
    completed = true;
  });
  await vi.waitFor(() => expect(mocks.seed).toHaveBeenCalledTimes(1));
  expect(completed).toBe(false);
  finish();
  await seed;
  expect(mocks.seed).toHaveBeenCalledWith(
    "contacts",
    `favorite:${contactId}`,
    JSON.stringify({ pinned: true, custom_alias: "Alice", hidden: false }),
    "owner",
    "source-crypto-account",
  );
  expect(mocks.seed).toHaveBeenCalledWith(
    "contacts",
    `hidden:${contactId}`,
    JSON.stringify("Bob"),
    "owner",
    "source-crypto-account",
  );
});

it("preserves account tombstones while seeding other missing source choices", async () => {
  mocks.preferences.mockResolvedValue([
    { key: `favorite:${contactId}`, value: null, revision: 7 },
  ]);
  mocks.favorites.mockResolvedValue([
    { id: contactId, pinned: true, custom_alias: "Alice" },
  ]);
  mocks.hidden.mockResolvedValue({
    contacts: [{ account_id: contactId, name: "Bob" }],
  });
  await publishContactsBeforeLink("owner");
  expect(mocks.seed).not.toHaveBeenCalledWith(
    "contacts",
    `favorite:${contactId}`,
    expect.anything(),
    "owner",
    "source-crypto-account",
  );
  expect(mocks.seed).toHaveBeenCalledWith(
    "contacts",
    `hidden:${contactId}`,
    JSON.stringify("Bob"),
    "owner",
    "source-crypto-account",
  );
});

it("does not seed empty contact defaults or erase account contacts", async () => {
  await publishContactsBeforeLink("owner");
  expect(mocks.seed).not.toHaveBeenCalled();
});

it("rejects contact seeding when durable publication fails", async () => {
  mocks.preferences.mockResolvedValue([]);
  mocks.favorites.mockResolvedValue([
    { id: contactId, pinned: true, custom_alias: null },
  ]);
  mocks.seed.mockRejectedValue(new Error("storage unavailable"));
  await expect(publishContactsBeforeLink("owner")).rejects.toThrow(
    "storage unavailable",
  );
});

it("applies a linked favorite only when this device enables contact sync", async () => {
  mocks.policy.mockResolvedValueOnce({ contacts: false });
  const stop = startLinkedContacts("owner");
  await vi.waitFor(() => expect(mocks.preferences).toHaveBeenCalledTimes(1));
  expect(mocks.setFavorite).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(10_000);
  expect(mocks.applyFavorites).toHaveBeenCalledWith(
    "owner",
    "source-crypto-account",
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
    "source-crypto-account",
    [{ account: contactId, hidden: true, name: "Bob" }],
    { [contactId]: null },
  );
  expect(mocks.loadHidden).toHaveBeenCalledWith("owner");
  expect(mocks.setFavorite).not.toHaveBeenCalled();
  stop();
});

it("does not seed a joining device's saved aliases or hidden defaults before source records arrive", async () => {
  mocks.preferences.mockResolvedValue([]);
  mocks.favorites.mockResolvedValue([
    {
      id: contactId,
      pinned: true,
      custom_alias: "Previous device remark",
      hidden: false,
    },
  ]);
  mocks.hidden.mockResolvedValue({
    contacts: [{ account_id: contactId, name: "Previous name" }],
  });
  const stop = startLinkedContacts("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.publish).not.toHaveBeenCalled();
    mocks.preferences.mockResolvedValue([
      {
        key: `favorite:${contactId}`,
        value: JSON.stringify({
          pinned: true,
          custom_alias: "Ronaldo",
          hidden: false,
        }),
        revision: 5,
        device_id: "cd".repeat(16),
      },
    ]);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(mocks.applyFavorites).toHaveBeenCalledWith(
      "owner",
      "source-crypto-account",
      [{ id: contactId, pinned: true, custom_alias: "Ronaldo", hidden: false }],
      {
        [contactId]: {
          pinned: true,
          custom_alias: "Previous device remark",
          hidden: false,
        },
      },
    );
    expect(mocks.publish).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});

it("does not turn saved pre-opt-in defaults into edits, but publishes an actual later alias change", async () => {
  mocks.preferences.mockResolvedValue([]);
  mocks.policy.mockResolvedValue({ contacts: false });
  mocks.favorites.mockResolvedValue([
    { id: contactId, pinned: true, custom_alias: "Alice", hidden: false },
  ]);
  const stop = startLinkedContacts("owner");
  const { notifySyncInvalidation } = await import("./syncInvalidation");
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.publish).not.toHaveBeenCalled();
    mocks.policy.mockResolvedValue({ contacts: true });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(mocks.publish).not.toHaveBeenCalled();
    mocks.favorites.mockResolvedValue([
      { id: contactId, pinned: true, custom_alias: "Ronaldo", hidden: false },
    ]);
    notifySyncInvalidation({ owner: "owner", kind: "contacts" });
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.publish).toHaveBeenCalledExactlyOnceWith(
      "contacts",
      `favorite:${contactId}`,
      JSON.stringify({ pinned: true, custom_alias: "Ronaldo", hidden: false }),
      "owner",
      "source-crypto-account",
    );
  } finally {
    stop();
  }
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
      "source-crypto-account",
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
      "source-crypto-account",
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
      "source-crypto-account",
      [{ account: otherId, hidden: true, name: "Peer" }],
      { [otherId]: null },
    );
    expect(mocks.publish).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});

it("preserves an existing canonical remark during source preparation", async () => {
  mocks.favorites.mockResolvedValue([
    { id: contactId, pinned: false, custom_alias: "Ronaldo", hidden: false },
  ]);
  useAuth.setState({
    user: { id: "source-local-uuid", username: "Owner", display_name: "Owner" },
  });
  await publishContactsBeforeLink("source-local-uuid");
  expect(mocks.publish).not.toHaveBeenCalled();
  expect(mocks.seed).not.toHaveBeenCalled();
});

it("preserves an account tombstone or remote alias when no saved source favorite exists", async () => {
  mocks.preferences.mockResolvedValue([
    { key: `favorite:${contactId}`, value: null, revision: 7 },
    {
      key: `favorite:${"ef".repeat(16)}`,
      value: JSON.stringify({ pinned: false, custom_alias: "Remote remark" }),
      revision: 8,
    },
  ]);
  await publishContactsBeforeLink("owner");
  expect(mocks.publish).not.toHaveBeenCalled();
});

it("does not republish an unchanged source favorite with an older optional-field encoding", async () => {
  mocks.preferences.mockResolvedValue([
    {
      key: `favorite:${contactId}`,
      value: JSON.stringify({ pinned: true, custom_alias: "Ronaldo" }),
      revision: 7,
    },
  ]);
  mocks.favorites.mockResolvedValue([
    { id: contactId, pinned: true, custom_alias: "Ronaldo", hidden: false },
  ]);
  await publishContactsBeforeLink("owner");
  expect(mocks.publish).not.toHaveBeenCalled();
});

it.each([
  '"Ronaldo"',
  "[]",
  '{"pinned":"yes","custom_alias":null}',
  '{"custom_alias":42}',
])(
  "does not turn malformed remote favorite %s into an alias deletion",
  async (value) => {
    mocks.preferences.mockResolvedValue([
      {
        key: `favorite:${contactId}`,
        value,
        revision: 5,
        device_id: "cd".repeat(16),
      },
    ]);
    mocks.favorites.mockResolvedValue([
      { id: contactId, pinned: false, custom_alias: "Ronaldo", hidden: false },
    ]);
    const stop = startLinkedContacts("owner");
    try {
      await vi.advanceTimersByTimeAsync(0);
      expect(mocks.applyFavorites).not.toHaveBeenCalled();
      expect(mocks.publish).not.toHaveBeenCalled();
    } finally {
      stop();
    }
  },
);

it("imports valid Unicode remarks using the same character bound as native favorites", async () => {
  const alias = "😀".repeat(200);
  mocks.preferences.mockResolvedValue([
    {
      key: `favorite:${contactId}`,
      value: JSON.stringify({
        pinned: false,
        custom_alias: alias,
        hidden: false,
      }),
      revision: 5,
      device_id: "cd".repeat(16),
    },
  ]);
  const stop = startLinkedContacts("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.applyFavorites).toHaveBeenCalledWith(
      "owner",
      "source-crypto-account",
      [{ id: contactId, pinned: false, custom_alias: alias, hidden: false }],
      { [contactId]: null },
    );
  } finally {
    stop();
  }
});

it("applies an explicit remote favorite tombstone to clear a saved remark", async () => {
  mocks.preferences.mockResolvedValue([
    {
      key: `favorite:${contactId}`,
      value: null,
      revision: 5,
      device_id: "cd".repeat(16),
    },
  ]);
  mocks.favorites.mockResolvedValue([
    { id: contactId, pinned: false, custom_alias: "Ronaldo", hidden: false },
  ]);
  const stop = startLinkedContacts("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.applyFavorites).toHaveBeenCalledWith(
      "owner",
      "source-crypto-account",
      [{ id: contactId, pinned: false, custom_alias: null, hidden: false }],
      {
        [contactId]: { pinned: false, custom_alias: "Ronaldo", hidden: false },
      },
    );
    expect(mocks.publish).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});

it("retires a pending contact import when the cryptographic account changes under the same local owner", async () => {
  let finish!: (records: unknown[]) => void;
  mocks.preferences.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const stop = startLinkedContacts("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    scope.account = "replacement-crypto-account";
    finish([
      {
        key: `favorite:${contactId}`,
        value: JSON.stringify({
          pinned: false,
          custom_alias: "Old account remark",
        }),
        revision: 100,
        device_id: "cd".repeat(16),
      },
    ]);
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.applyFavorites).not.toHaveBeenCalled();
    expect(mocks.publish).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});

it("starts fresh revision admission after the same local owner adopts a different cryptographic account", async () => {
  const stopPrevious = startLinkedContacts("owner");
  await vi.advanceTimersByTimeAsync(0);
  stopPrevious();
  scope.account = "replacement-crypto-account";
  mocks.preferences.mockResolvedValue([
    {
      key: `favorite:${contactId}`,
      value: JSON.stringify({
        pinned: false,
        custom_alias: "Ronaldo",
        hidden: false,
      }),
      revision: 1,
      device_id: "cd".repeat(16),
    },
  ]);
  const stop = startLinkedContacts("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(mocks.applyFavorites).toHaveBeenLastCalledWith(
      "owner",
      "replacement-crypto-account",
      [
        {
          id: contactId,
          pinned: false,
          custom_alias: "Ronaldo",
          hidden: false,
        },
      ],
      { [contactId]: null },
    );
    expect(mocks.publish).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});

it("does not echo an imported legacy favorite encoding over a newer sibling remark", async () => {
  mocks.preferences.mockResolvedValue([
    {
      key: `favorite:${contactId}`,
      value: JSON.stringify({ pinned: true, custom_alias: "Ronaldo" }),
      revision: 5,
      device_id: "cd".repeat(16),
    },
  ]);
  const stop = startLinkedContacts("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    mocks.favorites.mockResolvedValue([
      { id: contactId, pinned: true, custom_alias: "Ronaldo", hidden: false },
    ]);
    mocks.preferences.mockResolvedValue([
      {
        key: `favorite:${contactId}`,
        value: JSON.stringify({
          pinned: true,
          custom_alias: "New remark",
          hidden: false,
        }),
        revision: 6,
        device_id: "cd".repeat(16),
      },
    ]);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(mocks.publish).not.toHaveBeenCalled();
    expect(mocks.applyFavorites).toHaveBeenLastCalledWith(
      "owner",
      "source-crypto-account",
      [
        {
          id: contactId,
          pinned: true,
          custom_alias: "New remark",
          hidden: false,
        },
      ],
      { [contactId]: { pinned: true, custom_alias: "Ronaldo", hidden: false } },
    );
  } finally {
    stop();
  }
});

it("does not echo a pruned imported empty record as a deletion over a new sibling alias", async () => {
  mocks.preferences.mockResolvedValue([
    {
      key: `favorite:${contactId}`,
      value: JSON.stringify({ pinned: false, custom_alias: null }),
      revision: 5,
      device_id: "cd".repeat(16),
    },
  ]);
  const stop = startLinkedContacts("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    mocks.preferences.mockResolvedValue([
      {
        key: `favorite:${contactId}`,
        value: JSON.stringify({
          pinned: false,
          custom_alias: "Ronaldo",
          hidden: false,
        }),
        revision: 6,
        device_id: "cd".repeat(16),
      },
    ]);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(mocks.publish).not.toHaveBeenCalled();
    expect(mocks.applyFavorites).toHaveBeenLastCalledWith(
      "owner",
      "source-crypto-account",
      [
        {
          id: contactId,
          pinned: false,
          custom_alias: "Ronaldo",
          hidden: false,
        },
      ],
      { [contactId]: null },
    );
  } finally {
    stop();
  }
});

it("uses the native committed hidden-name projection instead of echoing its noncanonical source encoding", async () => {
  mocks.preferences.mockResolvedValue([
    {
      key: `hidden:${contactId}`,
      value: JSON.stringify("  Ronaldo  "),
      revision: 5,
      device_id: "cd".repeat(16),
    },
  ]);
  mocks.applyHidden.mockResolvedValueOnce({
    owner: "owner",
    contacts: [{ account_id: contactId, name: "Ronaldo" }],
    deleted: [],
  });
  const stop = startLinkedContacts("owner");
  try {
    await vi.advanceTimersByTimeAsync(0);
    mocks.hidden.mockResolvedValue({
      contacts: [{ account_id: contactId, name: "Ronaldo" }],
    });
    mocks.preferences.mockResolvedValue([
      {
        key: `hidden:${contactId}`,
        value: JSON.stringify("New name"),
        revision: 6,
        device_id: "cd".repeat(16),
      },
    ]);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(mocks.publish).not.toHaveBeenCalled();
    expect(mocks.applyHidden).toHaveBeenLastCalledWith(
      "owner",
      "source-crypto-account",
      [{ account: contactId, hidden: true, name: "New name" }],
      { [contactId]: "Ronaldo" },
    );
  } finally {
    stop();
  }
});
