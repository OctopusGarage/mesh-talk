import { describe, it, expect, vi, beforeEach } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

// Capture the event handlers start() registers, so we can drive inbound events.
const { captured } = vi.hoisted(() => ({
  captured: { current: null as null | Record<string, (e: unknown) => void> },
}));
vi.mock("@/lib/events", () => ({
  subscribeNodeEvents: (h: Record<string, (e: unknown) => void>) => {
    captured.current = h;
    return () => {};
  },
}));

import { useChat, convKey } from "./chat";
import { SEND_INTENT_CAP } from "./outgoingIntent";
import { useAuth } from "./auth";

it("old roster identity failure cannot undo newer readiness", async () => {
  let reject!: (e: Error) => void;
  let count = 0;
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity") {
      if (++count === 1)
        return new Promise((_, r) => {
          reject = r;
        });
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    }
    return Promise.resolve([]);
  });
  const old = useChat.getState().refreshRoster();
  await useChat.getState().refreshRoster();
  reject(new Error("late identity failure"));
  await old;
  expect(useChat.getState().ready).toBe(true);
});

it("keeps unchanged discovery snapshots referentially stable", async () => {
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    if (command === "list_peers")
      return Promise.resolve([
        {
          user_id: "peer",
          account_id: "account",
          name: "Alice",
          addr: "192.168.1.2:47474",
          post_office: false,
        },
      ]);
    if (command === "list_accounts")
      return Promise.resolve([
        { account_id: "account", device_count: 1, names: ["Alice"] },
      ]);
    return Promise.resolve([]);
  });
  await useChat.getState().refreshRoster();
  const first = useChat.getState();
  await useChat.getState().refreshRoster();
  const second = useChat.getState();
  expect(second.peers).toBe(first.peers);
  expect(second.accounts).toBe(first.accounts);
  expect(second.channels).toBe(first.channels);
});

it("refreshes channel member snapshots for group avatars", async () => {
  const members = [{ user_id: "peer", name: "Alice", account_id: "account" }];
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    if (command === "list_channels")
      return Promise.resolve([
        { channel_id: "team", name: "Team", owner: "me", member_count: 1 },
      ]);
    if (command === "channel_members")
      return Promise.resolve({ owner: "me", members });
    return Promise.resolve([]);
  });

  await useChat.getState().refreshRoster();
  expect(useChat.getState().channelMembersById.team).toEqual(members);
  const first = useChat.getState().channelMembersById;
  await useChat.getState().refreshRoster();
  expect(useChat.getState().channelMembersById).toBe(first);
});

it("shows a newly discovered peer on the next two-second roster poll", async () => {
  vi.useFakeTimers();
  let stop: (() => void) | undefined;
  try {
    let discovered = false;
    invoke.mockImplementation((command: string) => {
      if (command === "owner_node_identity")
        return Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        });
      if (command === "list_peers")
        return Promise.resolve(
          discovered
            ? [
                {
                  user_id: "peer",
                  account_id: "account",
                  name: "Alice",
                  addr: "192.168.1.2:47474",
                  post_office: false,
                },
              ]
            : [],
        );
      return Promise.resolve([]);
    });
    stop = useChat.getState().start();
    await vi.advanceTimersByTimeAsync(0);
    expect(useChat.getState().peers).toEqual([]);
    discovered = true;
    await vi.advanceTimersByTimeAsync(2000);
    expect(useChat.getState().peers).toHaveLength(1);
  } finally {
    stop?.();
    vi.useRealTimers();
  }
});

it("keeps a failed startup visible instead of starting another silent poll", async () => {
  vi.useFakeTimers();
  let stop: (() => void) | undefined;
  try {
    invoke.mockImplementation((command: string) =>
      command === "owner_node_identity"
        ? Promise.reject(new Error("node unavailable"))
        : Promise.resolve([]),
    );
    stop = useChat.getState().start();
    await vi.advanceTimersByTimeAsync(30_100);
    expect(useChat.getState().bootFailed).toBe(true);
    const attempts = invoke.mock.calls.filter(
      ([command]) => command === "owner_node_identity",
    ).length;

    await vi.advanceTimersByTimeAsync(2_500);
    expect(useChat.getState().bootFailed).toBe(true);
    expect(
      invoke.mock.calls.filter(
        ([command]) => command === "owner_node_identity",
      ),
    ).toHaveLength(attempts);
  } finally {
    stop?.();
    vi.useRealTimers();
  }
});

it.each(["text", "sticker", "file"] as const)(
  "deleted early history ID cannot return through delayed %s enqueue completion",
  async (kind) => {
    let finish!: (id: unknown) => void;
    let historyCalls = 0;
    invoke.mockImplementation((command: string) => {
      if (command === `owner_enqueue_${kind}`)
        return new Promise((r) => {
          finish = r;
        });
      if (command === "owner_account_history") {
        if (++historyCalls === 1)
          return Promise.resolve([
            { id: "E", from_me: true, text: "same", who: "me", wall_clock: 1 },
          ]);
        return Promise.reject(new Error("hydration unavailable"));
      }
      return Promise.resolve([]);
    });
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
    });
    const sending =
      kind === "text"
        ? useChat.getState().send("same", null)
        : kind === "sticker"
          ? useChat.getState().sendSticker("sticker", "same")
          : useChat.getState().sendFile("/tmp/same", false);
    await useChat.getState().reload();
    await useChat.getState().deleteMessage("E");
    finish(kind === "file" ? { id: "E", fileConv: "file" } : "E");
    await sending;
    expect(
      useChat.getState().messages["account:target"].some((m) => m.id === "E"),
    ).toBe(false);
    expect(useChat.getState().intents).toEqual({});
  },
);

const reset = () =>
  useChat.setState({
    runEpoch: 0,
    identityEpoch: 0,
    bootBusy: false,
    bootRequest: 0,
    intents: {},
    deleted: {},
    statusBusy: false,
    historyRequests: {},
    activeRequest: 0,
    rosterRequest: 0,
    favoritesRequest: 0,
    loadingRequest: 0,
    loading: false,
    error: null,
    ready: true,
    myId: "me",
    myAccountId: "myacct",
    peers: [],
    accounts: [],
    channels: [],
    active: null,
    messages: {},
    reactions: {},
    unread: {},
    members: [],
    incomingFiles: [],
    cacheOrder: [],
  });

for (const kind of ["text", "sticker", "file"] as const) {
  for (const pressure of ["cache", "scopes", "ids"] as const) {
    it(`keeps ${kind} deletion protection across ${pressure} capacity pressure`, async () => {
      const finishes: Array<(id: unknown) => void> = [];
      const histories: Record<string, unknown[]> = {};
      invoke.mockImplementation(
        (command: string, args: { account?: string; target?: string }) => {
          if (command === `owner_enqueue_${kind}`)
            return new Promise((resolve) => {
              finishes.push(resolve);
            });
          if (command === "owner_account_history")
            return Promise.resolve(histories[args.account!] ?? []);
          return Promise.resolve([]);
        },
      );
      const pending: Promise<void>[] = [];
      const scopes = pressure === "scopes" ? 51 : 1;
      for (let i = 0; i < scopes; i++) {
        const account = `protected-${i}`;
        await useChat
          .getState()
          .open({ kind: "account", id: account, name: account });
        pending.push(
          kind === "text"
            ? useChat.getState().send("same", null)
            : kind === "sticker"
              ? useChat.getState().sendSticker("sticker", "same")
              : useChat.getState().sendFile("/tmp/same", false),
        );
        histories[account] = [
          {
            id: `E-${i}`,
            from_me: true,
            text: "same",
            who: "me",
            wall_clock: 1,
          },
        ];
        await useChat.getState().reload();
        histories[account] = [];
        await useChat.getState().deleteMessage(`E-${i}`);
      }
      if (pressure === "cache")
        for (let i = 0; i < 51; i++)
          await useChat
            .getState()
            .open({ kind: "account", id: `other-${i}`, name: "Other" });
      if (pressure === "ids")
        for (let i = 0; i < 257; i++)
          await useChat.getState().deleteMessage(`other-${i}`);
      expect(
        Object.keys(useChat.getState().messages).length,
      ).toBeLessThanOrEqual(50);
      finishes.forEach((finish, i) =>
        finish(kind === "file" ? { id: `E-${i}`, fileConv: "file" } : `E-${i}`),
      );
      await Promise.all(pending);
      for (let i = 0; i < scopes; i++) {
        await useChat
          .getState()
          .open({ kind: "account", id: `protected-${i}`, name: "Protected" });
        expect(
          useChat
            .getState()
            .messages[`account:protected-${i}`].some((m) => m.id === `E-${i}`),
        ).toBe(false);
      }
      expect(useChat.getState().intents).toEqual({});
    });
  }
}

it("capacity fallback retains accepted ID through failed history and recovers on the owned ticker without reenqueue", async () => {
  vi.useFakeTimers();
  let stop: (() => void) | undefined;
  try {
    let finish!: (id: string) => void;
    let history: unknown[] = [];
    let unavailable = false;
    invoke.mockImplementation((command: string) => {
      if (command === "owner_node_identity")
        return Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        });
      if (command === "owner_enqueue_text")
        return new Promise((resolve) => {
          finish = resolve;
        });
      if (command === "owner_account_history")
        return unavailable
          ? Promise.reject(new Error("history temporarily unavailable"))
          : Promise.resolve(history);
      return Promise.resolve([]);
    });
    stop = useChat.getState().start();
    await vi.advanceTimersByTimeAsync(0);
    expect(useChat.getState().ready).toBe(true);
    await useChat
      .getState()
      .open({ kind: "account", id: "target", name: "Target" });
    const sending = useChat.getState().send("legitimate", null);
    const clientId = Object.keys(useChat.getState().intents)[0];
    for (let i = 0; i < 257; i++)
      await useChat.getState().deleteMessage(`unrelated-${i}`);
    unavailable = true;
    finish("accepted");
    await sending;
    expect(useChat.getState().intents[clientId].message.id).toBe("accepted");
    expect(useChat.getState().intents[clientId].deletedIds).toHaveLength(256);
    expect(useChat.getState().messages["account:target"]).toEqual([]);
    await useChat.getState().retry(clientId);
    expect(
      invoke.mock.calls.filter(([command]) => command === "owner_enqueue_text"),
    ).toHaveLength(1);
    unavailable = false;
    history = [
      {
        id: "accepted",
        from_me: true,
        text: "legitimate",
        who: "me",
        wall_clock: 1,
      },
    ];
    await vi.advanceTimersByTimeAsync(4000);
    expect(useChat.getState().messages["account:target"][0]).toMatchObject({
      id: "accepted",
      clientId,
    });
    expect(useChat.getState().intents).toEqual({});
    expect(
      invoke.mock.calls.filter(([command]) => command === "owner_enqueue_text"),
    ).toHaveLength(1);
  } finally {
    stop?.();
    vi.useRealTimers();
  }
});

it("deletion after protection overflow settles only after successful empty owner history", async () => {
  let finish!: (id: string) => void;
  let unavailable = false;
  invoke.mockImplementation((command: string) => {
    if (command === "owner_enqueue_text")
      return new Promise((resolve) => {
        finish = resolve;
      });
    if (command === "owner_account_history" && unavailable)
      return Promise.reject(new Error("history unavailable"));
    return Promise.resolve([]);
  });
  await useChat
    .getState()
    .open({ kind: "account", id: "target", name: "Target" });
  const sending = useChat.getState().send("same", null);
  const clientId = Object.keys(useChat.getState().intents)[0];
  for (let i = 0; i < 257; i++)
    await useChat.getState().deleteMessage(`other-${i}`);
  await useChat.getState().deleteMessage("E");
  expect(useChat.getState().intents[clientId].deletedIds?.includes("E")).toBe(
    false,
  );
  // Evict the ordinary scope tombstone; the capacity fallback must stand alone.
  for (let i = 0; i < 51; i++)
    await useChat
      .getState()
      .open({ kind: "account", id: `other-scope-${i}`, name: "Other" });
  unavailable = true;
  finish("E");
  await sending;
  await useChat
    .getState()
    .open({ kind: "account", id: "target", name: "Target" });
  expect(useChat.getState().intents[clientId].message.id).toBe("E");
  expect(useChat.getState().messages["account:target"] ?? []).toEqual([]);
  unavailable = false;
  await useChat.getState().refreshStatuses();
  expect(useChat.getState().intents).toEqual({});
  expect(useChat.getState().messages["account:target"]).toEqual([]);
  expect(
    invoke.mock.calls.filter(([command]) => command === "owner_enqueue_text"),
  ).toHaveLength(1);
});

it("preacceptance failure and retry retain exact deletion protection outside the LRU", async () => {
  let reject!: (e: Error) => void;
  let attempts = 0;
  invoke.mockImplementation((command: string) => {
    if (command === "owner_enqueue_text")
      return ++attempts === 1
        ? new Promise((_, r) => {
            reject = r;
          })
        : Promise.resolve("E");
    return Promise.resolve([]);
  });
  await useChat
    .getState()
    .open({ kind: "account", id: "target", name: "Target" });
  const sending = useChat.getState().send("same", null);
  const clientId = Object.keys(useChat.getState().intents)[0];
  await useChat.getState().deleteMessage("E");
  reject(new Error("not accepted"));
  await sending;
  expect(useChat.getState().intents[clientId].message.failed).toBe(true);
  expect(useChat.getState().intents[clientId].deletedIds).toEqual(["E"]);
  for (let i = 0; i < 51; i++)
    await useChat
      .getState()
      .open({ kind: "account", id: `other-${i}`, name: "Other" });
  await useChat
    .getState()
    .open({ kind: "account", id: "target", name: "Target" });
  await useChat.getState().retry(clientId);
  expect(useChat.getState().messages["account:target"]).toEqual([]);
  expect(useChat.getState().intents).toEqual({});
  expect(attempts).toBe(2);
});

beforeEach(() => {
  invoke.mockReset();
  useAuth.setState({
    user: { id: "host", username: "alice", display_name: "Alice" },
    generation: 0,
    operation: 0,
    loading: false,
    error: null,
  });
  reset();
});

describe("convKey", () => {
  it("namespaces by kind + id", () => {
    expect(convKey({ kind: "account", id: "a1", name: "x" })).toBe(
      "account:a1",
    );
    expect(convKey({ kind: "channel", id: "c1", name: "y" })).toBe(
      "channel:c1",
    );
  });
});

describe("open", () => {
  it("latest channel navigation owns member results", async () => {
    const finish: Record<string, (info: unknown) => void> = {};
    invoke.mockImplementation((cmd: string, args: unknown) =>
      cmd === "channel_members"
        ? new Promise((resolve) => {
            finish[(args as { channelId: string }).channelId] = resolve;
          })
        : Promise.resolve([]),
    );
    const first = useChat
      .getState()
      .open({ kind: "channel", id: "first", name: "First" });
    await vi.waitFor(() => expect(finish.first).toBeDefined());
    const second = useChat
      .getState()
      .open({ kind: "channel", id: "second", name: "Second" });
    await vi.waitFor(() => expect(finish.second).toBeDefined());
    finish.second({
      owner: "second-owner",
      members: [{ user_id: "second-member", name: "Second" }],
    });
    await second;
    finish.first({
      owner: "first-owner",
      members: [{ user_id: "first-member", name: "First" }],
    });
    await first;
    expect(useChat.getState().channelOwner).toBe("second-owner");
    expect(useChat.getState().members[0].user_id).toBe("second-member");
  });

  it("does not apply an earlier history response over a later reload", async () => {
    const finishes: Array<(items: unknown[]) => void> = [];
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_account_history" || cmd === "owner_account_history")
        return new Promise((resolve) => {
          finishes.push(resolve);
        });
      return Promise.resolve([]);
    });
    useChat.setState({ active: { kind: "account", id: "a1", name: "A" } });
    const first = useChat.getState().reload();
    const second = useChat.getState().reload();
    finishes[1]([
      { id: "new", from_me: false, text: "new", who: "peer", wall_clock: 2 },
    ]);
    await second;
    finishes[0]([
      { id: "old", from_me: false, text: "old", who: "peer", wall_clock: 1 },
    ]);
    await first;
    expect(useChat.getState().messages["account:a1"][0].id).toBe("new");
  });

  it("loads history + reactions, clears unread, sets active", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_account_history")
        return Promise.resolve([
          {
            id: "e1",
            from_me: false,
            who: "alice",
            text: "hello",
            wall_clock: 1000,
            reply_to: null,
          },
        ]);
      if (cmd === "account_reactions") return Promise.resolve([]);
      return Promise.resolve(undefined);
    });
    useChat.setState({ unread: { "account:a1": 3 } });
    await useChat.getState().open({ kind: "account", id: "a1", name: "A" });

    const s = useChat.getState();
    expect(s.active).toEqual({ kind: "account", id: "a1", name: "A" });
    expect(s.messages["account:a1"]).toHaveLength(1);
    expect(s.messages["account:a1"][0].text).toBe("hello");
    expect(s.unread["account:a1"]).toBe(0);
  });
});

describe("conversation cache LRU", () => {
  it("evicts the least-recently-opened caches beyond the cap, never the active one", async () => {
    // Every open() resolves empty history/reactions; the cache entry is the (empty) array.
    invoke.mockResolvedValue([]);
    // Open 60 distinct conversations (cap is 50).
    for (let i = 0; i < 60; i++) {
      await useChat
        .getState()
        .open({ kind: "account", id: `a${i}`, name: "x" });
    }
    const s = useChat.getState();
    // At most 50 cached message arrays remain.
    expect(Object.keys(s.messages).length).toBeLessThanOrEqual(50);
    expect(Object.keys(s.reactions).length).toBeLessThanOrEqual(50);
    expect(s.cacheOrder.length).toBeLessThanOrEqual(50);
    // The most-recently-opened (active) is retained; the oldest are evicted.
    expect(s.messages["account:a59"]).toBeDefined();
    expect(s.messages["account:a0"]).toBeUndefined();
    expect(s.messages["account:a9"]).toBeUndefined();
    // The active conversation key matches the last opened.
    expect(convKey(s.active!)).toBe("account:a59");
  });

  it("repopulates an evicted conversation when reopened", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_account_history")
        return Promise.resolve([
          {
            id: "e1",
            from_me: false,
            who: "alice",
            text: "back",
            wall_clock: 1,
            reply_to: null,
          },
        ]);
      return Promise.resolve([]);
    });
    for (let i = 0; i < 60; i++) {
      await useChat
        .getState()
        .open({ kind: "account", id: `a${i}`, name: "x" });
    }
    expect(useChat.getState().messages["account:a0"]).toBeUndefined();
    await useChat.getState().open({ kind: "account", id: "a0", name: "x" });
    expect(useChat.getState().messages["account:a0"]).toHaveLength(1);
    expect(useChat.getState().messages["account:a0"][0].text).toBe("back");
  });

  it("keeps unread counts while evicting cached conversation state", async () => {
    invoke.mockResolvedValue([]);
    await useChat.getState().open({ kind: "account", id: "a0", name: "x" });
    useChat.setState({ unread: { "account:a0": 4 } });
    for (let i = 1; i < 60; i++) {
      await useChat
        .getState()
        .open({ kind: "account", id: `a${i}`, name: "x" });
    }
    const state = useChat.getState();
    expect(state.messages["account:a0"]).toBeUndefined();
    expect(state.unread["account:a0"]).toBe(4);
  });
});

describe("send", () => {
  it("refuses a full optimistic queue synchronously so the composer can retain its draft", () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_enqueue_text"
        ? new Promise(() => {})
        : Promise.resolve([]),
    );
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
      ready: true,
    });
    expect(useChat.getState().admitText("first", null)).toBe(true);
    const existing = Object.values(useChat.getState().intents)[0];
    const full = Object.fromEntries(
      Array.from({ length: SEND_INTENT_CAP }, (_, i) => [
        `full-${i}`,
        existing,
      ]),
    );
    useChat.setState({ intents: full });
    expect(useChat.getState().admitText("keep this draft", null)).toBe(false);
    expect(useChat.getState().messages["account:target"]).toHaveLength(1);
    expect(
      invoke.mock.calls.filter(([cmd]) => cmd === "owner_enqueue_text"),
    ).toHaveLength(1);
  });

  it("file retry uses its captured path and navigation cannot retarget completion", async () => {
    let failed = true;
    let finish!: (result: { id: string; fileConv: string }) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_file")
        return failed
          ? Promise.reject("disk full")
          : new Promise((resolve) => {
              finish = resolve;
            });
      return Promise.resolve([]);
    });
    useChat.setState({
      active: { kind: "account", id: "original", name: "Original" },
    });
    await useChat.getState().sendFile("C:\\files\\original.bin", false);
    const client = useChat.getState().messages["account:original"][0].clientId!;
    failed = false;
    const retry = useChat.getState().retry(client);
    await useChat
      .getState()
      .open({ kind: "account", id: "other", name: "Other" });
    finish({ id: "file-original", fileConv: "file-conv" });
    await retry;
    expect(useChat.getState().messages["account:other"]).toEqual([]);
    expect(useChat.getState().messages["account:original"][0]).toMatchObject({
      id: "file-original",
      clientId: client,
      file: { name: "original.bin", fileConv: "file-conv" },
    });
    expect(
      invoke.mock.calls
        .filter(([cmd]) => cmd === "owner_enqueue_file")
        .every(
          ([, args]) =>
            (args as { account: string; path: string }).account ===
              "original" &&
            (args as { path: string }).path === "C:\\files\\original.bin",
        ),
    ).toBe(true);
  });

  it("old status finally cannot clear a new run's serial poll ownership", async () => {
    const finishes: Array<(rows: unknown[]) => void> = [];
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_node_identity")
        return Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        });
      if (cmd === "owner_delivery_statuses")
        return new Promise((resolve) => {
          finishes.push(resolve);
        });
      return Promise.resolve([]);
    });
    const row = {
      id: "event",
      fromMe: true,
      who: "me",
      text: "text",
      wallClock: 1,
      replyTo: null,
    };
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
      messages: { "account:target": [row] },
    });
    const old = useChat.getState().refreshStatuses();
    useAuth.setState((s) => ({ generation: s.generation + 1 }));
    const stop = useChat.getState().start();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
      messages: { "account:target": [row] },
    });
    const current = useChat.getState().refreshStatuses();
    finishes[0]([{ id: "event", status: "delivered" }]);
    await old;
    expect(useChat.getState().statusBusy).toBe(true);
    expect(
      useChat.getState().messages["account:target"][0].delivery,
    ).toBeUndefined();
    finishes[1]([{ id: "event", status: "awaiting" }]);
    await current;
    expect(useChat.getState().statusBusy).toBe(false);
    expect(useChat.getState().messages["account:target"][0].delivery).toBe(
      "awaiting",
    );
    stop();
  });

  it("group void completion removes only the exact matching local placeholder", async () => {
    const finish: Array<() => void> = [];
    invoke.mockImplementation((cmd: string) =>
      cmd === "send_channel_message"
        ? new Promise<void>((resolve) => {
            finish.push(resolve);
          })
        : Promise.resolve([]),
    );
    useChat.setState({
      active: { kind: "channel", id: "group", name: "Group" },
    });
    const first = useChat.getState().send("same", null);
    const second = useChat.getState().send("same", null);
    const remaining = useChat.getState().messages["channel:group"][1].clientId;
    finish[0]();
    await first;
    expect(useChat.getState().messages["channel:group"]).toHaveLength(1);
    expect(useChat.getState().messages["channel:group"][0]).toMatchObject({
      clientId: remaining,
      pending: true,
    });
    finish[1]();
    await second;
    expect(useChat.getState().messages["channel:group"]).toEqual([]);
  });

  it("sticker retry retains its typed payload and local identity before acceptance only", async () => {
    let reject = true;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_sticker")
        return reject
          ? Promise.reject({
              kind: "invalid-input",
              message: "transport frame too large",
            })
          : Promise.resolve("sticker-id");
      if (cmd === "owner_delivery_statuses")
        return Promise.resolve([{ id: "sticker-id", status: "awaiting" }]);
      return Promise.resolve([]);
    });
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
    });
    await useChat.getState().sendSticker("sticker-pack:item", "🙂");
    const failed = useChat.getState().messages["account:target"][0];
    expect(failed.failReason).toBe("invalid-input"); // Not keyword transport -> relay.
    reject = false;
    await useChat.getState().retry(failed.clientId!);
    expect(useChat.getState().messages["account:target"][0]).toMatchObject({
      clientId: failed.clientId,
      id: "sticker-id",
      sticker: "sticker-pack:item",
      delivery: "awaiting",
    });
    expect(
      invoke.mock.calls
        .filter(([cmd]) => cmd === "owner_enqueue_sticker")
        .every(
          ([, args]) =>
            (args as { stickerId: string }).stickerId === "sticker-pack:item",
        ),
    ).toBe(true);
  });

  it("clear prevents a later accepted-ID completion from resurrecting its placeholder", async () => {
    let finish!: (id: string) => void;
    let history: unknown[] = [];
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_text")
        return new Promise((resolve) => {
          finish = resolve;
        });
      if (cmd === "owner_account_history") return Promise.resolve(history);
      return Promise.resolve([]);
    });
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
    });
    const sending = useChat.getState().send("cleared", null);
    await useChat.getState().clearConversation();
    history = [
      {
        id: "late-id",
        from_me: true,
        who: "me",
        text: "cleared",
        wall_clock: 1,
      },
    ];
    finish("late-id");
    await sending;
    await useChat.getState().reload();
    expect(useChat.getState().messages["account:target"]).toEqual([]);
    expect(useChat.getState().intents).toEqual({});
  });

  it("own-account sparse absence never invents a clock and group legacy void retires only its own intent", async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_enqueue_text"
        ? Promise.resolve("own-id")
        : Promise.resolve([]),
    );
    useChat.setState({
      active: { kind: "account", id: "myacct", name: "Own" },
    });
    await useChat.getState().send("self copy", null);
    expect(
      useChat.getState().messages["account:myacct"][0].delivery,
    ).toBeUndefined();
    await useChat
      .getState()
      .open({ kind: "channel", id: "group", name: "Group" });
    await useChat.getState().sendSticker("sticker", "🙂");
    expect(useChat.getState().messages["channel:group"]).toEqual([]);
    expect(Object.values(useChat.getState().intents)).toHaveLength(1); // Own accepted intent, not group placeholder.
  });

  it("keeps two same-content sends distinct across early history and reversed ID completions", async () => {
    const finishes: Array<(id: string) => void> = [];
    let history: unknown[] = [];
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_text")
        return new Promise((resolve) => {
          finishes.push(resolve);
        });
      if (cmd === "owner_account_history") return Promise.resolve(history);
      if (cmd === "owner_delivery_statuses")
        return Promise.resolve([
          { id: "second", status: "delivered" },
          { id: "first", status: "awaiting" },
        ]);
      return Promise.resolve([]);
    });
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
    });
    const first = useChat.getState().send("same", null);
    const second = useChat.getState().send("same", null);
    const clients = useChat
      .getState()
      .messages["account:target"].map((m) => m.clientId);
    history = [
      { id: "second", from_me: true, text: "same", who: "me", wall_clock: 2 },
      { id: "first", from_me: true, text: "same", who: "me", wall_clock: 1 },
    ];
    await useChat.getState().reload();
    expect(useChat.getState().messages["account:target"]).toHaveLength(4); // Unknown IDs cannot be guessed by content.
    finishes[1]("second");
    await second;
    finishes[0]("first");
    await first;
    const rows = useChat.getState().messages["account:target"];
    expect(rows).toHaveLength(2);
    expect(rows.find((m) => m.id === "first")?.clientId).toBe(clients[0]);
    expect(rows.find((m) => m.id === "second")?.clientId).toBe(clients[1]);
    expect(rows.find((m) => m.id === "second")?.delivery).toBe("delivered");
  });

  it("accepted file metadata is unknown until exact-ID history, and accepted sends cannot retry", async () => {
    let history: unknown[] = [];
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_file")
        return Promise.resolve({ id: "file-id", fileConv: "fc" });
      if (cmd === "owner_account_history") return Promise.resolve(history);
      if (cmd === "owner_delivery_statuses")
        return Promise.resolve([{ id: "file-id", status: "delivered" }]);
      return Promise.resolve([]);
    });
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
    });
    await useChat.getState().sendFile("/tmp/photo.png", true);
    const pendingMetadata = useChat.getState().messages["account:target"][0];
    expect(pendingMetadata).toMatchObject({
      id: "file-id",
      metadataPending: true,
      pending: false,
      delivery: "delivered",
      file: { name: "photo.png", fileConv: "fc", size: 0, mime: "" },
    });
    await useChat.getState().retry(pendingMetadata.clientId!);
    expect(
      invoke.mock.calls.filter(([cmd]) => cmd === "owner_enqueue_file"),
    ).toHaveLength(1);
    history = [
      {
        id: "file-id",
        from_me: true,
        who: "me",
        text: "",
        wall_clock: 1,
        file: {
          name: "authoritative.png",
          size: 1234,
          mime: "image/png",
          file_conv: "fc",
          media: true,
        },
      },
    ];
    await useChat.getState().reload();
    expect(useChat.getState().messages["account:target"][0]).toMatchObject({
      clientId: pendingMetadata.clientId,
      metadataPending: false,
      file: { size: 1234, name: "authoritative.png" },
    });
  });

  it("preserves failed intents through LRU eviction without late completion growing the cache", async () => {
    let rejectSend!: (error: unknown) => void;
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_enqueue_text"
        ? new Promise((_, reject) => {
            rejectSend = reject;
          })
        : Promise.resolve([]),
    );
    const original = {
      kind: "account" as const,
      id: "original",
      name: "Original",
    };
    await useChat.getState().open(original);
    const sending = useChat.getState().send("pending", null);
    for (let i = 0; i < 60; i++)
      await useChat
        .getState()
        .open({ kind: "account", id: `other-${i}`, name: "Other" });
    rejectSend("offline");
    await sending;
    expect(useChat.getState().messages["account:original"]).toBeUndefined();
    expect(Object.keys(useChat.getState().messages)).toHaveLength(50);
    await useChat.getState().open(original);
    expect(useChat.getState().messages["account:original"][0]).toMatchObject({
      failed: true,
      text: "pending",
    });
  });

  it("hydrates serial <=256 batches and keeps Delivered monotonic against old/missing projection", async () => {
    const batches: string[][] = [];
    let delivered = true;
    invoke.mockImplementation((cmd: string, args: unknown) => {
      if (cmd === "owner_delivery_statuses") {
        const ids = (args as { ids: string[] }).ids;
        batches.push(ids);
        return Promise.resolve(
          delivered ? ids.map((id) => ({ id, status: "delivered" })) : [],
        );
      }
      return Promise.resolve([]);
    });
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
      messages: {
        "account:target": Array.from({ length: 500 }, (_, n) => ({
          id: `e-${n}`,
          fromMe: true,
          who: "me",
          text: "",
          replyTo: null,
          wallClock: n,
        })),
      },
    });
    await useChat.getState().refreshStatuses();
    expect(batches.map((b) => b.length)).toEqual([256, 244]);
    delivered = false;
    await useChat.getState().refreshStatuses();
    expect(
      useChat
        .getState()
        .messages["account:target"].every((m) => m.delivery === "delivered"),
    ).toBe(true);
  });

  it("keeps client identity and stable accepted event ID, then trusts only exact authoritative status", async () => {
    let finishSend!: (id: string) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_text" || cmd === "owner_enqueue_text")
        return new Promise((resolve) => {
          finishSend = resolve;
        });
      return Promise.resolve([]);
    });
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
    });
    const sending = useChat.getState().send("same content", null);
    const clientId = useChat.getState().messages["account:target"][0].clientId;
    finishSend("accepted-id");
    await sending;
    const accepted = useChat.getState().messages["account:target"][0];
    expect(accepted.id).toBe("accepted-id");
    expect(accepted.clientId).toBe(clientId);
    expect(accepted.pending).toBe(false);
    expect(accepted.delivery).toBeUndefined(); // Sparse absent projection is untracked, not an invented clock.
    expect(
      invoke.mock.calls.some(([cmd]) => cmd === "owner_enqueue_text"),
    ).toBe(true);
  });

  it("cannot resurrect an old-owner failed send after a different owner takes over", async () => {
    let rejectSend!: (error: unknown) => void;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_text" || cmd === "owner_enqueue_text")
        return new Promise((_, reject) => {
          rejectSend = reject;
        });
      return Promise.resolve([]);
    });
    useAuth.setState({
      user: { id: "a", username: "alice", display_name: "Alice" },
    });
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
    });
    const sending = useChat.getState().send("old owner", null);
    useAuth.setState({
      user: { id: "b", username: "bob", display_name: "Bob" },
    });
    useChat.setState({ messages: {}, active: null });
    rejectSend("transport failed");
    await sending;
    expect(useChat.getState().messages).toEqual({});
    expect(useChat.getState().error).toBeNull();
  });

  it("sends then reloads so the message gets its real id", async () => {
    let history: unknown[] = [];
    invoke.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "owner_enqueue_text") {
        const a = args as { text: string };
        history = [
          {
            id: "e1",
            from_me: true,
            who: "me",
            text: a.text,
            wall_clock: 1,
            reply_to: null,
          },
        ];
        return Promise.resolve("e1");
      }
      if (cmd === "owner_account_history") return Promise.resolve(history);
      if (cmd === "account_reactions") return Promise.resolve([]);
      return Promise.resolve(undefined);
    });
    useChat.setState({ active: { kind: "account", id: "a1", name: "A" } });
    await useChat.getState().send("hi", null);

    expect(invoke).toHaveBeenCalledWith("owner_enqueue_text", {
      owner: "host",
      account: "a1",
      text: "hi",
      replyTo: null,
    });
    const msgs = useChat.getState().messages["account:a1"];
    expect(msgs).toHaveLength(1);
    expect(msgs[0]).toMatchObject({ id: "e1", text: "hi", fromMe: true });
    expect(msgs[0].pending).toBeFalsy();
  });

  it("ignores an empty message", async () => {
    useChat.setState({ active: { kind: "account", id: "a1", name: "A" } });
    await useChat.getState().send("   ", null);
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("toggleReaction", () => {
  // For an ACCOUNT conversation, reaction `who` is keyed by ACCOUNT id (myAccountId),
  // NOT the device id — the self-check must use myAccountId or the reaction can never be
  // toggled off (regression: account reactions were compared against the device myId).
  beforeEach(() => {
    invoke.mockResolvedValue([]);
    useChat.setState({
      active: { kind: "account", id: "a1", name: "A" },
      reactions: {
        "account:a1": [{ target: "t1", emoji: "👍", who: ["myacct"] }],
      },
    });
  });
  it("removes my own account reaction (who keyed by account id)", async () => {
    await useChat.getState().toggleReaction("t1", "👍");
    expect(invoke).toHaveBeenCalledWith("react_account", {
      account: "a1",
      target: "t1",
      emoji: "👍",
      remove: true,
    });
  });
  it("adds a reaction I have not made", async () => {
    await useChat.getState().toggleReaction("t1", "🎉");
    expect(invoke).toHaveBeenCalledWith("react_account", {
      account: "a1",
      target: "t1",
      emoji: "🎉",
      remove: false,
    });
  });
  it("matches the DEVICE id for channel conversations", async () => {
    useChat.setState({
      active: { kind: "channel", id: "c1", name: "C" },
      reactions: { "channel:c1": [{ target: "t1", emoji: "👍", who: ["me"] }] },
    });
    await useChat.getState().toggleReaction("t1", "👍");
    expect(invoke).toHaveBeenCalledWith("react_channel", {
      channelId: "c1",
      target: "t1",
      emoji: "👍",
      remove: true,
    });
  });
});

describe("incoming events", () => {
  it("atomic owner readiness precedes every legacy roster/profile read", async () => {
    let finish!: (identity: unknown) => void;
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_node_identity"
        ? new Promise((resolve) => {
            finish = resolve;
          })
        : Promise.resolve([]),
    );
    const stop = useChat.getState().start();
    expect(invoke.mock.calls.map(([cmd]) => cmd)).toEqual([
      "owner_node_identity",
    ]);
    useAuth.setState({
      user: { id: "new-owner", username: "bob", display_name: "Bob" },
      generation: 1,
    });
    finish({
      owner: "host",
      device_id: "old-device",
      account_id: "old-account",
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(useChat.getState().ready).toBe(false);
    expect(
      invoke.mock.calls.some(
        ([cmd]) =>
          cmd === "list_peers" ||
          cmd === "peer_avatars" ||
          cmd === "get_favorites",
      ),
    ).toBe(false);
    stop();
  });

  it("same-host runtime account replacement resets cached receipt state and stale work", async () => {
    let account = "myacct";
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_node_identity"
        ? Promise.resolve({
            owner: "host",
            device_id: "me",
            account_id: account,
          })
        : Promise.resolve([]),
    );
    const stop = useChat.getState().start();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    useChat.setState({
      messages: {
        "account:target": [
          {
            id: "old",
            fromMe: true,
            who: "me",
            text: "old",
            wallClock: 1,
            replyTo: null,
            delivery: "delivered",
          },
        ],
      },
    });
    account = "replacement-account";
    await useChat.getState().refreshRoster();
    await vi.waitFor(() =>
      expect(useChat.getState().myAccountId).toBe(account),
    );
    expect(useChat.getState().messages).toEqual({});
    expect(useChat.getState().identityEpoch).toBe(1);
    expect(useChat.getState().intents).toEqual({});
    stop();
  });

  it("drops captured old-run events and old cleanup cannot stop a newer run", async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_node_identity"
        ? Promise.resolve({
            owner: "host",
            device_id: "me",
            account_id: "myacct",
          })
        : Promise.resolve([]),
    );
    const oldStop = useChat.getState().start();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    const oldEvents = captured.current!;
    useAuth.setState((s) => ({ generation: s.generation + 1 })); // Same UUID, new session.
    const newStop = useChat.getState().start();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    const run = useChat.getState().runEpoch;
    oldEvents.onFile({
      from: "stranger",
      file_conv: "old",
      name: "old",
      size: 1,
      conv: "old",
      media: false,
    });
    oldStop();
    expect(useChat.getState().incomingFiles).toEqual([]);
    expect(useChat.getState().runEpoch).toBe(run);
    expect(useChat.getState().ready).toBe(true);
    newStop();
  });

  const peer = {
    user_id: "dev1",
    account_id: "acctA",
    name: "Alice",
    addr: "1.2.3.4:7000",
    post_office: false,
  };

  async function boot() {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "owner_node_identity":
          return Promise.resolve({
            owner: "host",
            device_id: "me",
            account_id: "myacct",
          });
        case "list_peers":
          return Promise.resolve([peer]);
        default:
          return Promise.resolve([]);
      }
    });
    const stop = useChat.getState().start();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    return stop;
  }

  it("re-publishes our persisted own avatar on boot (so peers pull it after restart)", async () => {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "owner_node_identity":
          return Promise.resolve({
            owner: "host",
            device_id: "me",
            account_id: "myacct",
          });
        case "get_avatars":
          // a previously-set own avatar, persisted across the restart
          return Promise.resolve({ myacct: "data:image/png;base64,AAAA" });
        default:
          return Promise.resolve([]);
      }
    });
    const stop = useChat.getState().start();
    await vi.waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("publish_avatar", {
        avatar: "data:image/png;base64,AAAA",
      }),
    );
    stop();
  });

  it("routes a DM to the sender's ACCOUNT conversation and bumps unread", async () => {
    const stop = await boot();
    captured.current!.onDm({
      from: "dev1",
      from_name: "Alice",
      text: "hi",
      reply_to: null,
    });
    expect(useChat.getState().unread["account:acctA"]).toBe(1);
    stop();
  });

  it("does not bump unread for a DM from an undiscovered peer", async () => {
    const stop = await boot();
    captured.current!.onDm({
      from: "ghost",
      from_name: "?",
      text: "hi",
      reply_to: null,
    });
    expect(useChat.getState().unread["account:ghost"]).toBeUndefined();
    stop();
  });

  it("routes a channel message to its channel conversation", async () => {
    const stop = await boot();
    captured.current!.onChannelMessage({
      channel_id: "c1",
      channel_name: "general",
      from: "dev1",
      text: "yo",
      reply_to: null,
    });
    expect(useChat.getState().unread["channel:c1"]).toBe(1);
    stop();
  });

  it("de-dupes received files by file_conv", async () => {
    const stop = await boot();
    const f = {
      conv: "x",
      from: "dev1",
      name: "a.pdf",
      size: 10,
      mime: "application/pdf",
      file_conv: "fc1",
    };
    captured.current!.onFile(f);
    captured.current!.onFile(f);
    expect(useChat.getState().incomingFiles).toHaveLength(1);
    expect(useChat.getState().incomingFiles[0].fromName).toBe("Alice");
    stop();
  });
});

describe("send/action failures", () => {
  const acct = { kind: "account" as const, id: "a1", name: "A" };

  it("keeps the optimistic bubble (marked failed) when the send rejects", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_text")
        return Promise.reject({ kind: "service", message: "node down" });
      return Promise.resolve([]);
    });
    useChat.setState({ active: acct });
    await useChat.getState().send("hi", null);

    const msgs = useChat.getState().messages[convKey(acct)] ?? [];
    expect(msgs).toHaveLength(1);
    expect(msgs[0].text).toBe("hi");
    expect(msgs[0].failed).toBe(true);
    expect(msgs[0].pending).toBe(false);
    // "node down" maps to the relay-unreachable coarse reason.
    expect(msgs[0].failReason).toBe("relay-unreachable");
  });

  it("retry re-sends a failed bubble and clears the failed state on success", async () => {
    let history: unknown[] = [];
    let fail = true;
    invoke.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "owner_enqueue_text") {
        if (fail)
          return Promise.reject({ kind: "service", message: "no peer" });
        const a = args as { text: string };
        history = [
          {
            id: "e1",
            from_me: true,
            who: "me",
            text: a.text,
            wall_clock: 1,
            reply_to: null,
          },
        ];
        return Promise.resolve("e1");
      }
      if (cmd === "owner_account_history") return Promise.resolve(history);
      if (cmd === "account_reactions") return Promise.resolve([]);
      return Promise.resolve([]);
    });
    useChat.setState({ active: acct });
    await useChat.getState().send("hi", null);

    const failed = useChat.getState().messages[convKey(acct)][0];
    expect(failed.failed).toBe(true);
    expect(failed.failReason).toBe("peer-unknown");
    const clientId = failed.clientId!;
    expect(clientId).toBeTruthy();

    // The send now succeeds; retry should re-dispatch and reconcile from the log.
    fail = false;
    await useChat.getState().retry(clientId);

    const msgs = useChat.getState().messages[convKey(acct)];
    expect(msgs).toHaveLength(1);
    expect(msgs[0]).toMatchObject({ id: "e1", text: "hi", fromMe: true });
    expect(msgs[0].failed).toBeFalsy();
    expect(msgs[0].pending).toBeFalsy();
  });

  it("retry keeps the bubble failed (new reason) when the resend also fails", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_text")
        return Promise.reject({ kind: "service", message: "decrypt error" });
      return Promise.resolve([]);
    });
    useChat.setState({ active: acct });
    await useChat.getState().send("hi", null);
    const clientId = useChat.getState().messages[convKey(acct)][0].clientId!;
    await useChat.getState().retry(clientId);

    const msgs = useChat.getState().messages[convKey(acct)];
    expect(msgs).toHaveLength(1);
    expect(msgs[0].failed).toBe(true);
    expect(msgs[0].failReason).toBe("crypto");
  });

  it("sets a structured error message when sendFile rejects", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_file")
        return Promise.reject({ kind: "service", message: "disk full" });
      return Promise.resolve([]);
    });
    useChat.setState({ active: acct, error: null });
    await useChat.getState().sendFile("/tmp/x", false);
    expect(useChat.getState().error).toContain("disk full");
  });

  it("sets a structured error message when toggleReaction rejects", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "react_account")
        return Promise.reject({ kind: "service", message: "no peer" });
      return Promise.resolve([]);
    });
    useChat.setState({ active: acct, error: null });
    await useChat.getState().toggleReaction("e1", "👍");
    expect(useChat.getState().error).toContain("no peer");
  });
});
