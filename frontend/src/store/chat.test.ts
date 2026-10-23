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
import { emptyReception } from "./reception";
import { SEND_INTENT_CAP } from "./outgoingIntent";
import { useAuth } from "./auth";
import { useTransfers } from "./transfers";
import { useAvatars } from "./avatars";

it("coalesces roster invalidations and preserves readiness after an older failure", async () => {
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
  const next = useChat.getState().refreshRoster();
  reject(new Error("late identity failure"));
  await Promise.all([old, next]);
  expect(useChat.getState().ready).toBe(true);
});

it("refreshes imported history when the runtime projection changes", async () => {
  let revision = 1;
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: revision,
      });
    if (command === "owner_account_history")
      return Promise.resolve([
        {
          id: "synced",
          from_me: true,
          text: "from my other device",
          who: "me",
          wall_clock: 1,
        },
      ]);
    return Promise.resolve([]);
  });
  useChat.setState({
    active: { kind: "account", id: "friend", name: "Friend" },
  });
  await useChat.getState().refreshRoster();
  revision = 2;
  await useChat.getState().refreshRoster();
  expect(useChat.getState().messages["account:friend"]?.[0]?.id).toBe("synced");
  const calls = invoke.mock.calls.filter(
    ([name]) => name === "owner_account_history",
  ).length;
  await useChat.getState().refreshRoster();
  expect(
    invoke.mock.calls.filter(([name]) => name === "owner_account_history"),
  ).toHaveLength(calls);
});

it("retries an imported-history refresh after history is temporarily unavailable", async () => {
  let historyCalls = 0;
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: 7,
      });
    if (command === "owner_account_history") {
      if (++historyCalls === 1)
        return Promise.reject(new Error("temporarily unavailable"));
      return Promise.resolve([
        { id: "synced", from_me: true, text: "note", who: "me", wall_clock: 1 },
      ]);
    }
    return Promise.resolve([]);
  });
  useChat.setState({
    active: { kind: "account", id: "friend", name: "Friend" },
  });
  await useChat.getState().refreshRoster();
  await useChat.getState().refreshRoster();
  expect(useChat.getState().messages["account:friend"]?.[0]?.id).toBe("synced");
  expect(historyCalls).toBe(2);
});

it("ignores an old owner's history revision response", async () => {
  let finish!: (value: unknown) => void;
  invoke.mockImplementation((command: string) =>
    command === "owner_node_identity"
      ? new Promise((resolve) => {
          finish = resolve;
        })
      : Promise.resolve([]),
  );
  useChat.setState({
    active: { kind: "account", id: "friend", name: "Friend" },
  });
  const refreshing = useChat.getState().refreshRoster();
  useAuth.setState((state) => ({ generation: state.generation + 1 }));
  finish({
    owner: "host",
    device_id: "me",
    account_id: "myacct",
    history_revision: 9,
  });
  await refreshing;
  expect(useChat.getState().historyRevision).toBeNull();
  expect(
    invoke.mock.calls.some(([command]) => command === "owner_account_history"),
  ).toBe(false);
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

it("reports a failed member removal to the dialog", async () => {
  invoke.mockImplementation((command: string) =>
    command === "remove_channel_member"
      ? Promise.reject(new Error("offline"))
      : Promise.resolve([]),
  );
  useChat.setState({
    active: { kind: "channel", id: "team", name: "Team" },
  });
  expect(await useChat.getState().removeMember("peer")).toBe(false);
  expect(useChat.getState().error).toContain("offline");
});

it("updates the open channel owner after transfer", async () => {
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    if (command === "channel_members")
      return Promise.resolve({
        owner: "peer",
        members: [{ user_id: "peer", name: "Alice" }],
      });
    return Promise.resolve([]);
  });
  useChat.setState({
    active: { kind: "channel", id: "team", name: "Team" },
    channelOwner: "me",
  });
  expect(await useChat.getState().transferChannelOwner("peer")).toBe(true);
  expect(useChat.getState().channelOwner).toBe("peer");
});

it("closes the active channel after a successful leave", async () => {
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    return Promise.resolve([]);
  });
  useChat.setState({
    active: { kind: "channel", id: "team", name: "Team" },
    unread: { "channel:team": 2 },
  });
  expect(await useChat.getState().leaveChannel()).toBe(true);
  expect(useChat.getState().active).toBeNull();
  expect(useChat.getState().unread["channel:team"]).toBeUndefined();
});

it("does not clear another channel's members when a leave finishes late", async () => {
  let finishLeave!: () => void;
  invoke.mockImplementation((command: string) => {
    if (command === "leave_channel")
      return new Promise<void>((resolve) => {
        finishLeave = resolve;
      });
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    return Promise.resolve([]);
  });
  useChat.setState({
    active: { kind: "channel", id: "first", name: "First" },
    members: [{ user_id: "first-member", name: "First member" }],
    channelOwner: "first-owner",
    unread: { "channel:first": 2 },
  });
  const leaving = useChat.getState().leaveChannel();
  useChat.setState({
    active: { kind: "channel", id: "second", name: "Second" },
    members: [{ user_id: "second-member", name: "Second member" }],
    channelOwner: "second-owner",
  });
  finishLeave();
  expect(await leaving).toBe(true);
  expect(useChat.getState().active?.id).toBe("second");
  expect(useChat.getState().members[0]?.user_id).toBe("second-member");
  expect(useChat.getState().channelOwner).toBe("second-owner");
  expect(useChat.getState().unread["channel:first"]).toBeUndefined();
});

it("refreshes a pending invitation after acceptance", async () => {
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    if (command === "list_channels")
      return Promise.resolve([
        {
          channel_id: "team",
          name: "Team",
          member_count: 2,
          owner: "peer",
          pending_invite: false,
        },
      ]);
    if (command === "channel_members")
      return Promise.resolve({
        owner: "peer",
        members: [{ user_id: "me", name: "tester", pending: false }],
      });
    return Promise.resolve([]);
  });
  useChat.setState({
    active: { kind: "channel", id: "team", name: "Team" },
    channels: [
      {
        channel_id: "team",
        name: "Team",
        member_count: 2,
        owner: "peer",
        pending_invite: true,
      },
    ],
    members: [{ user_id: "me", name: "tester", pending: true }],
    channelOwner: "peer",
  });
  expect(await useChat.getState().acceptChannelInvite()).toBe(true);
  expect(invoke).toHaveBeenCalledWith("accept_channel_invite", {
    channelId: "team",
  });
  expect(useChat.getState().channels[0]?.pending_invite).toBe(false);
  expect(useChat.getState().members[0]?.pending).toBe(false);
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

it("bounds stalled startup reads to thirty seconds and allows retry without restart", async () => {
  vi.useFakeTimers();
  const pending: (() => void)[] = [];
  let stop: (() => void) | undefined;
  try {
    let available = false;
    invoke.mockImplementation((command: string) => {
      if (command === "owner_node_identity") {
        const identity = {
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        };
        return available
          ? Promise.resolve(identity)
          : new Promise((resolve) => pending.push(() => resolve(identity)));
      }
      return Promise.resolve([]);
    });
    stop = useChat.getState().start();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(useChat.getState().bootFailed).toBe(true);
    expect(useChat.getState().bootBusy).toBe(false);
    available = true;
    pending.forEach((finish) => finish());
    await vi.advanceTimersByTimeAsync(0);
    expect(useChat.getState().ready).toBe(false);
    useChat.getState().retryBoot();
    await vi.advanceTimersByTimeAsync(0);
    expect(useChat.getState().ready).toBe(true);
    expect(useChat.getState().bootFailed).toBe(false);
  } finally {
    stop?.();
    pending.forEach((finish) => finish());
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
    ...emptyReception(),
    historyScanBusy: false,
    historyScanQueue: [],
    historyScanRevision: null,
    historyBaselines: [],
    historyFullBaselines: [],
    runEpoch: useChat.getState().runEpoch + 1,
    identityEpoch: 0,
    historyRevision: null,
    bootBusy: false,
    bootRequest: 0,
    intents: {},
    deleted: {},
    statusBusy: false,
    historyRequests: {},
    historyLimits: {},
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
    channelMembersById: {},
    channelOwner: "",
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
  useAvatars.getState().setOwner(null);
  useAvatars.getState().setOwner("host");
  useTransfers.getState().reset();
  useAuth.setState({
    user: { id: "host", username: "alice", display_name: "Alice" },
    generation: 0,
    operation: 0,
    loading: false,
    error: null,
  });
  reset();
});

it("dissolves only the selected channel and refreshes its roster", async () => {
  useChat.setState({
    channels: [
      { channel_id: "team", name: "Team", owner: "me", member_count: 2 },
      { channel_id: "other", name: "Other", owner: "me", member_count: 2 },
    ],
  });
  invoke.mockImplementation((command: string) => {
    if (command === "list_channels")
      return Promise.resolve([
        {
          channel_id: "team",
          name: "Team",
          owner: "me",
          member_count: 2,
          dissolved: true,
        },
        { channel_id: "other", name: "Other", owner: "me", member_count: 2 },
      ]);
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    return Promise.resolve([]);
  });

  expect(await useChat.getState().dissolveChannel("team")).toBe(true);
  expect(invoke).toHaveBeenCalledWith("dissolve_channel", {
    channelId: "team",
  });
  expect(
    useChat.getState().channels.map((channel) => channel.dissolved),
  ).toEqual([true, undefined]);
  expect(invoke).toHaveBeenCalledWith("list_channels");
});

it("keeps a channel writable when dissolution is rejected", async () => {
  useChat.setState({
    channels: [
      { channel_id: "team", name: "Team", owner: "me", member_count: 2 },
    ],
  });
  invoke.mockRejectedValue(new Error("owner only"));

  expect(await useChat.getState().dissolveChannel("team")).toBe(false);
  expect(useChat.getState().channels[0].dissolved).toBeUndefined();
  expect(useChat.getState().error).toContain("owner only");
});

it("does not send dissolution without a signed-in owner", async () => {
  useAuth.setState({ user: null });

  expect(await useChat.getState().dissolveChannel("team")).toBe(false);
  expect(invoke).not.toHaveBeenCalled();
});

it("does not update a replacement account after dissolution completes", async () => {
  let finish!: () => void;
  invoke.mockImplementation((command: string) =>
    command === "dissolve_channel"
      ? new Promise<void>((resolve) => {
          finish = resolve;
        })
      : Promise.resolve([]),
  );
  useChat.setState({
    channels: [
      { channel_id: "team", name: "Team", owner: "me", member_count: 2 },
    ],
  });

  const pending = useChat.getState().dissolveChannel("team");
  useAuth.setState({
    user: { id: "replacement", username: "bob", display_name: "Bob" },
  });
  finish();

  expect(await pending).toBe(false);
  expect(useChat.getState().channels[0].dissolved).toBeUndefined();
  expect(invoke).not.toHaveBeenCalledWith("list_channels");
});

it("persists contact pins and aliases under the signed-in owner", async () => {
  invoke.mockImplementation((command: string) =>
    command === "get_favorites" ? Promise.resolve([]) : Promise.resolve(),
  );
  await useChat.getState().togglePinned("peer", true);
  expect(invoke).toHaveBeenCalledWith("set_favorite", {
    owner: "host",
    id: "peer",
    pinned: true,
  });
  await useChat.getState().setAlias("peer", " Friend ");
  expect(invoke).toHaveBeenCalledWith("set_alias", {
    owner: "host",
    id: "peer",
    alias: "Friend",
  });
  await useChat.getState().setChannelHidden("team", true);
  expect(invoke).toHaveBeenCalledWith("set_channel_hidden", {
    owner: "host",
    id: "team",
    hidden: true,
  });
});

it("projects recipient-confirmed upload progress onto only the matching outgoing file", async () => {
  let delivered = false;
  invoke.mockImplementation((cmd: string) => {
    if (cmd === "owner_delivery_statuses")
      return Promise.resolve([
        {
          id: "file-id",
          status: delivered ? "delivered" : "awaiting",
          file_progress: { done: 3, total: 8, delivered },
        },
      ]);
    return Promise.resolve([]);
  });
  useChat.setState({
    active: { kind: "account", id: "target", name: "Target" },
    messages: {
      "account:target": [
        {
          id: "file-id",
          fromMe: true,
          who: "me",
          text: "",
          replyTo: null,
          wallClock: 1,
          file: {
            name: "large.bin",
            fileConv: "fc",
            size: 8,
            mime: "application/octet-stream",
            media: false,
          },
        },
        {
          id: "other-id",
          fromMe: true,
          who: "me",
          text: "",
          replyTo: null,
          wallClock: 2,
        },
      ],
    },
  });
  await useChat.getState().refreshStatuses();
  expect(useTransfers.getState().transfers["file-id"]).toMatchObject({
    direction: "send",
    done: 3,
    total: 8,
  });
  expect(useTransfers.getState().transfers["other-id"]).toBeUndefined();
  delivered = true;
  await useChat.getState().refreshStatuses();
  expect(useTransfers.getState().transfers["file-id"]?.delivered).toBe(true);
});

it("projects channel custody without treating member copies as delivery", async () => {
  invoke.mockImplementation((cmd: string, args: unknown) => {
    if (cmd === "channel_custody_statuses") {
      expect(args).toEqual({ channelId: "team", ids: ["own-id"] });
      return Promise.resolve([{ id: "own-id", replicas: 2 }]);
    }
    return Promise.resolve([]);
  });
  useChat.setState({
    active: { kind: "channel", id: "team", name: "Team" },
    messages: {
      "channel:team": [
        {
          id: "own-id",
          fromMe: true,
          who: "me",
          text: "hello",
          replyTo: null,
          wallClock: 1,
        },
        {
          id: "other-id",
          fromMe: false,
          who: "peer",
          text: "hi",
          replyTo: null,
          wallClock: 2,
        },
      ],
    },
  });
  await useChat.getState().refreshStatuses();
  const [own, incoming] = useChat.getState().messages["channel:team"];
  expect(own.replicas).toBe(2);
  expect(own.delivery).toBeUndefined();
  expect(incoming.replicas).toBeUndefined();
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
    vi.stubGlobal("document", {
      visibilityState: "visible",
      hasFocus: () => true,
    });
    try {
      await useChat.getState().open({ kind: "account", id: "a1", name: "A" });
      const s = useChat.getState();
      expect(s.active).toEqual({ kind: "account", id: "a1", name: "A" });
      expect(s.messages["account:a1"]).toHaveLength(1);
      expect(s.messages["account:a1"][0].text).toBe("hello");
      expect(s.unread["account:a1"]).toBe(0);
    } finally {
      vi.unstubAllGlobals();
    }
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

  it("channel acceptance settles only the exact matching local placeholder", async () => {
    const finish: Array<(id: string) => void> = [];
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_enqueue_channel_text"
        ? new Promise<string>((resolve) => {
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
    finish[0]("event-a");
    await first;
    expect(useChat.getState().messages["channel:group"]).toHaveLength(2);
    expect(useChat.getState().messages["channel:group"][0]).toMatchObject({
      id: "event-a",
      pending: false,
    });
    expect(useChat.getState().messages["channel:group"][1]).toMatchObject({
      clientId: remaining,
      pending: true,
    });
    finish[1]("event-b");
    await second;
    expect(
      useChat.getState().messages["channel:group"].map((m) => m.id),
    ).toEqual(["event-a", "event-b"]);
    expect(
      useChat.getState().messages["channel:group"].every((m) => !m.pending),
    ).toBe(true);
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

  it("reports a failed clear so its confirmation remains open", async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === "clear_conversation"
        ? Promise.reject(new Error("disk full"))
        : Promise.resolve([]),
    );
    useChat.setState({
      active: { kind: "channel", id: "group", name: "Group" },
    });
    expect(await useChat.getState().clearConversation()).toBe(false);
    expect(useChat.getState().error).toContain("disk full");
  });

  it("own-account sparse absence never invents a clock and channel acceptance retains its exact ID", async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_enqueue_text"
        ? Promise.resolve("own-id")
        : cmd === "owner_enqueue_channel_sticker"
          ? Promise.resolve("channel-sticker")
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
    expect(useChat.getState().messages["channel:group"][0]).toMatchObject({
      id: "channel-sticker",
      pending: false,
    });
    expect(Object.values(useChat.getState().intents)).toHaveLength(2);
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

  it("keeps exact-ID file metadata when history arrives before enqueue completion", async () => {
    let accept!: (result: { id: string; fileConv: string }) => void;
    let historyAvailable = true;
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_file")
        return new Promise((resolve) => {
          accept = resolve;
        });
      if (cmd === "owner_account_history")
        return historyAvailable
          ? Promise.resolve([
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
            ])
          : Promise.reject(new Error("history unavailable"));
      return Promise.resolve([]);
    });
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
    });
    const sending = useChat.getState().sendFile("/tmp/photo.png", true);
    await useChat.getState().reload();
    historyAvailable = false;
    accept({ id: "file-id", fileConv: "fc" });
    await sending;
    expect(useChat.getState().messages["account:target"]).toEqual([
      expect.objectContaining({
        id: "file-id",
        metadataPending: false,
        file: expect.objectContaining({
          name: "authoritative.png",
          size: 1234,
        }),
      }),
    ]);
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

  it("keeps a late accepted send in its intent after cache eviction and restores it on reopen", async () => {
    let accept!: (id: string) => void;
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_enqueue_text"
        ? new Promise((resolve) => {
            accept = resolve;
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
    const clientId = Object.keys(useChat.getState().intents)[0];
    for (let i = 0; i < 60; i++)
      await useChat
        .getState()
        .open({ kind: "account", id: `other-${i}`, name: "Other" });
    accept("accepted-id");
    await sending;
    expect(useChat.getState().messages["account:original"]).toBeUndefined();
    expect(Object.keys(useChat.getState().messages)).toHaveLength(50);
    expect(useChat.getState().intents[clientId].message).toMatchObject({
      id: "accepted-id",
      pending: false,
    });
    await useChat.getState().open(original);
    expect(useChat.getState().messages["account:original"][0]).toMatchObject({
      id: "accepted-id",
      clientId,
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
      active: { kind: "account", id: "target", name: "Previous peer" },
      unread: { "account:target": 3 },
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
    expect(useChat.getState().active).toBeNull();
    expect(useChat.getState().unread).toEqual({});
    expect(useChat.getState().identityEpoch).toBe(1);
    expect(useChat.getState().intents).toEqual({});
    stop();
  });

  it("does not carry unread state through a failed identity lookup into another account", async () => {
    let account = "myacct";
    let failNext = false;
    invoke.mockImplementation((cmd: string) => {
      if (cmd !== "owner_node_identity") return Promise.resolve([]);
      if (failNext) {
        failNext = false;
        return Promise.reject(new Error("node restarting"));
      }
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: account,
      });
    });
    const stop = useChat.getState().start();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    useChat.setState({
      active: { kind: "account", id: "old-peer", name: "Old peer" },
      unread: { "account:old-peer": 3 },
    });
    failNext = true;
    await useChat.getState().refreshRoster();
    expect(useChat.getState().ready).toBe(false);
    account = "replacement-account";
    useChat.getState().retryBoot();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    expect(useChat.getState().unread).toEqual({});
    expect(useChat.getState().active).toBeNull();
    expect(useChat.getState().identityEpoch).toBe(1);
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
      is_channel: false,
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

  it("counts one logical DM once when duplicate delivery callbacks arrive", async () => {
    const stop = await boot();
    const event = {
      from: "dev1",
      from_name: "Alice",
      text: "one message",
      reply_to: null,
      message_id: "logical-dm",
      from_account: "acctA",
    };
    captured.current!.onDm(event);
    captured.current!.onDm(event);
    expect(useChat.getState().unread["account:acctA"]).toBe(1);
    stop();
  });

  it("does not count a live DM again when the same logical history row is scanned", async () => {
    const stop = await boot();
    useChat.setState({
      ledger: { "account:acctA": {} },
      historyBaselines: ["account:acctA"],
      historyFullBaselines: ["account:acctA"],
    });
    captured.current!.onDm({
      from: "dev1",
      from_name: "Alice",
      text: "one message",
      reply_to: null,
      message_id: "logical-dm",
      from_account: "acctA",
    });
    expect(useChat.getState().unread["account:acctA"]).toBe(1);
    invoke.mockImplementation((command: string) => {
      if (command === "owner_node_identity")
        return Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
          history_revision: 2,
        });
      if (command === "list_peers") return Promise.resolve([peer]);
      if (command === "list_accounts")
        return Promise.resolve([
          { account_id: "acctA", name: "Alice", devices: ["dev1"] },
        ]);
      if (command === "owner_account_history")
        return Promise.resolve([
          {
            id: "logical-dm",
            from_me: false,
            text: "one message",
            who: "acctA",
            wall_clock: 1,
          },
        ]);
      return Promise.resolve([]);
    });
    await useChat.getState().refreshRoster();
    expect(useChat.getState().unread["account:acctA"]).toBe(1);
    stop();
  });

  it("counts two distinct logical messages even when their text is identical", async () => {
    const stop = await boot();
    for (const id of ["first-message", "second-message"])
      captured.current!.onDm({
        from: "dev1",
        from_name: "Alice",
        text: "same text",
        reply_to: null,
        message_id: id,
        from_account: "acctA",
      });
    expect(useChat.getState().unread["account:acctA"]).toBe(2);
    stop();
  });

  it("indexes an identified DM before discovery without adding a second pending unread count", async () => {
    const stop = await boot();
    const event = {
      from: "not-yet-discovered",
      from_name: "Alice",
      text: "one message",
      reply_to: null,
      message_id: "early-message",
      from_account: "acctA",
    };
    captured.current!.onDm(event);
    captured.current!.onDm(event);
    expect(useChat.getState().unread["account:acctA"]).toBe(1);
    expect(useChat.getState().pendingUnknownUnread).toEqual({});
    stop();
  });

  it("does not count existing unread messages again when the same node recovers", async () => {
    const stop = await boot();
    useChat.setState({
      unread: { "account:acctA": 2 },
      ready: false,
      bootBusy: false,
    });
    useChat.getState().retryBoot();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    expect(useChat.getState().unread["account:acctA"]).toBe(2);
    stop();
  });

  it("keeps messages and files in the open chat unread while unfocused", async () => {
    const stop = await boot();
    const view = { visibilityState: "visible", hasFocus: () => false };
    vi.stubGlobal("document", view);
    try {
      useChat.setState({
        active: { kind: "account", id: "acctA", name: "Alice" },
      });
      captured.current!.onDm({
        from: "dev1",
        from_name: "Alice",
        text: "hi",
        reply_to: null,
      });
      captured.current!.onFile({
        conv: "acctA",
        is_channel: false,
        from: "dev1",
        name: "a.pdf",
        size: 10,
        mime: "application/pdf",
        file_conv: "fc-background",
      });
      expect(useChat.getState().unread["account:acctA"]).toBe(2);
      await useChat.getState().open({
        kind: "account",
        id: "acctA",
        name: "Alice",
      });
      expect(useChat.getState().unread["account:acctA"]).toBe(2);
      useChat.getState().markActiveRead();
      expect(useChat.getState().unread["account:acctA"]).toBe(2);
      view.hasFocus = () => true;
      useChat.getState().markActiveRead();
      expect(useChat.getState().unread["account:acctA"]).toBe(0);
    } finally {
      vi.unstubAllGlobals();
      stop();
    }
  });

  it("counts a DM after its sender appears in the peer roster", async () => {
    const stop = await boot();
    captured.current!.onDm({
      from: "ghost",
      from_name: "?",
      text: "hi",
      reply_to: null,
    });
    expect(useChat.getState().unread["account:ghost"]).toBeUndefined();
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_node_identity")
        return Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        });
      if (cmd === "list_peers")
        return Promise.resolve([
          peer,
          {
            user_id: "ghost",
            account_id: "new-account",
            name: "New contact",
            addr: "1.2.3.4:7001",
            post_office: false,
          },
        ]);
      return Promise.resolve([]);
    });
    await useChat.getState().refreshRoster();
    expect(useChat.getState().unread["account:new-account"]).toBe(1);
    stop();
  });

  it("counts a DM file after its sender appears in the peer roster", async () => {
    const stop = await boot();
    captured.current!.onFile({
      conv: "new-account",
      is_channel: false,
      from: "ghost-file",
      name: "report.pdf",
      size: 10,
      mime: "application/pdf",
      file_conv: "fc-early-file",
      media: false,
    });
    expect(useChat.getState().pendingUnknownUnread["ghost-file"]).toBe(1);
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_node_identity")
        return Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        });
      if (cmd === "list_peers")
        return Promise.resolve([
          peer,
          {
            user_id: "ghost-file",
            account_id: "new-account",
            name: "New contact",
            addr: "1.2.3.4:7001",
            post_office: false,
          },
        ]);
      return Promise.resolve([]);
    });
    await useChat.getState().refreshRoster();
    expect(useChat.getState().unread["account:new-account"]).toBe(1);
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

  it("routes a new channel file by its explicit scope before roster refresh", async () => {
    const stop = await boot();
    captured.current!.onFile({
      conv: "new-channel",
      is_channel: true,
      from: "dev1",
      name: "notes.pdf",
      size: 10,
      mime: "application/pdf",
      file_conv: "fc-new-channel",
      media: false,
    });
    expect(useChat.getState().unread["channel:new-channel"]).toBe(1);
    expect(useChat.getState().unread["account:acctA"]).toBeUndefined();
    stop();
  });

  it("de-dupes received files by file_conv", async () => {
    const stop = await boot();
    const f = {
      conv: "x",
      is_channel: false,
      from: "dev1",
      name: "a.pdf",
      size: 10,
      mime: "application/pdf",
      file_conv: "fc1",
    };
    captured.current!.onFile(f);
    captured.current!.onFile(f);
    expect(useChat.getState().incomingFiles).toHaveLength(1);
    expect(useChat.getState().unread["account:acctA"]).toBe(1);
    expect(useChat.getState().incomingFiles[0].fromName).toBe("Alice");
    stop();
  });

  it("discards a file callback arriving after its deadline", async () => {
    const stop = await boot();
    try {
      captured.current!.onFile({
        conv: "x",
        is_channel: false,
        from: "dev1",
        name: "late.pdf",
        size: 10,
        mime: "application/pdf",
        file_conv: "late-file",
        expires_at_ms: Date.now() - 1,
      });
      expect(useChat.getState().incomingFiles).toEqual([]);
    } finally {
      stop();
    }
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

describe("offline contact deletion", () => {
  beforeEach(() => {
    useChat.setState({
      ready: true,
      accounts: [{ account_id: "contact", device_count: 0, names: [] }],
      active: { kind: "account", id: "contact", name: "Contact" },
      unread: { "account:contact": 3, "account:other": 2 },
      messages: {
        "account:contact": [
          {
            id: "old",
            fromMe: true,
            who: "me",
            text: "private history",
            wallClock: 1,
            replyTo: null,
          },
        ],
      },
      error: null,
    });
    invoke.mockResolvedValue({ owner: "host", contacts: [] });
  });
  it("removes the contact and closes its conversation after persistence", async () => {
    expect(await useChat.getState().deleteContact("contact")).toBe(true);
    expect(invoke).toHaveBeenCalledWith("delete_contact", {
      owner: "host",
      account: "contact",
    });
    expect(useChat.getState().accounts).toEqual([]);
    expect(useChat.getState().active).toBeNull();
    expect(useChat.getState().unread["account:contact"]).toBeUndefined();
    expect(useChat.getState().unread["account:other"]).toBe(2);
    expect(useChat.getState().messages["account:contact"]).toEqual([]);
  });
  it("keeps the contact when persistence fails or it comes online", async () => {
    invoke.mockRejectedValue(new Error("Contact is online"));
    expect(await useChat.getState().deleteContact("contact")).toBe(false);
    expect(useChat.getState().accounts).toHaveLength(1);
    expect(useChat.getState().active?.id).toBe("contact");
  });
  it("does not apply a deletion completion to a later session", async () => {
    let complete!: (value: unknown) => void;
    invoke.mockImplementation(
      () =>
        new Promise((resolve) => {
          complete = resolve;
        }),
    );
    const request = useChat.getState().deleteContact("contact");
    useAuth.setState({ generation: useAuth.getState().generation + 1 });
    complete({ owner: "host", contacts: [] });
    expect(await request).toBe(false);
    expect(useChat.getState().accounts).toHaveLength(1);
  });
});

describe("externally accepted messages", () => {
  for (const kind of ["account", "channel"] as const) {
    it(`reloads the active ${kind} history without unread changes`, async () => {
      const history = [
        {
          id: "external-1",
          from_me: true,
          who: "you",
          text: "Alert from agent",
          wall_clock: 1,
          reply_to: null,
        },
      ];
      invoke.mockImplementation((command: string) => {
        if (command === "owner_node_identity")
          return Promise.resolve({
            owner: "host",
            account_id: "myacct",
            device_id: "me",
          });
        if (
          command ===
          (kind === "account" ? "owner_account_history" : "channel_history")
        )
          return Promise.resolve(history);
        return Promise.resolve([]);
      });
      const stop = useChat.getState().start();
      try {
        await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
        useChat.setState({
          active: { kind, id: "target", name: "Target" },
          unread: { "account:other": 3 },
        });
        captured.current!.onIntegrationMessageAccepted({
          owner: "host",
          account_id: "myacct",
          destination: { kind, id: "target" },
        });
        await vi.waitFor(() =>
          expect(useChat.getState().messages[`${kind}:target`]?.[0]?.text).toBe(
            "Alert from agent",
          ),
        );
        expect(useChat.getState().unread).toEqual({
          "account:other": 3,
          [`${kind}:target`]: 0,
        });
      } finally {
        stop();
      }
    });
  }

  it("ignores unrelated, unready, and retired-session integration events", async () => {
    invoke.mockImplementation((command: string) =>
      command === "owner_node_identity"
        ? Promise.resolve({
            owner: "host",
            account_id: "myacct",
            device_id: "me",
          })
        : Promise.resolve([]),
    );
    const stop = useChat.getState().start();
    try {
      await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
      useChat.setState({
        active: { kind: "account", id: "target", name: "Target" },
      });
      invoke.mockClear();
      const accepted = {
        owner: "host",
        account_id: "myacct",
        destination: { kind: "account", id: "target" },
      };
      const handler = captured.current!.onIntegrationMessageAccepted;
      handler({ ...accepted, owner: "other-owner" });
      handler({ ...accepted, account_id: "other-account" });
      handler({ ...accepted, destination: { kind: "account", id: "other" } });
      handler({ ...accepted, destination: { kind: "channel", id: "target" } });
      useChat.setState({ ready: false });
      handler(accepted);
      useChat.setState({ ready: true });
      useAuth.setState({ generation: useAuth.getState().generation + 1 });
      handler(accepted);
      useAuth.setState({ user: null });
      handler(accepted);
      expect(invoke).not.toHaveBeenCalled();
    } finally {
      stop();
    }
  });
});

describe("durable text admission", () => {
  beforeEach(() =>
    useChat.setState({
      active: { kind: "account", id: "target", name: "Target" },
      ready: true,
    }),
  );
  it("returns false when final authorization rejects after preflight", async () => {
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_enqueue_text"
        ? Promise.reject(new Error("authorization changed"))
        : Promise.resolve([]),
    );
    expect(
      await useChat.getState().admitTextAndWait("Keep this draft.", null),
    ).toBe(false);
    const messages = useChat.getState().messages["account:target"];
    expect(messages).toHaveLength(1);
    expect(messages[0]).toMatchObject({
      pending: false,
      failed: true,
      text: "Keep this draft.",
    });
    expect(
      invoke.mock.calls.filter(([cmd]) => cmd === "owner_enqueue_text"),
    ).toHaveLength(1);
  });
  it("returns true once an ID is accepted even when history hydration fails", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "owner_enqueue_text") return Promise.resolve("durable-id");
      if (cmd === "owner_account_history")
        return Promise.reject(new Error("history unavailable"));
      return Promise.resolve([]);
    });
    expect(
      await useChat.getState().admitTextAndWait("Accepted message.", null),
    ).toBe(true);
    expect(useChat.getState().messages["account:target"][0]).toMatchObject({
      id: "durable-id",
      pending: false,
    });
  });
  it("does not report admission to a replacement owner", async () => {
    let finish!: (id: string) => void;
    invoke.mockImplementation((cmd: string) =>
      cmd === "owner_enqueue_text"
        ? new Promise((resolve) => {
            finish = resolve;
          })
        : Promise.resolve([]),
    );
    const admission = useChat.getState().admitTextAndWait("Owned draft.", null);
    useAuth.setState({ generation: useAuth.getState().generation + 1 });
    finish("old-owner-id");
    expect(await admission).toBe(false);
  });
});

it("durable text admission settles before optional history hydration", async () => {
  useChat.setState({
    active: { kind: "account", id: "target", name: "Target" },
    ready: true,
  });
  invoke.mockImplementation((cmd: string) => {
    if (cmd === "owner_enqueue_text")
      return Promise.resolve("accepted-immediately");
    if (cmd === "owner_account_history") return new Promise(() => {});
    return Promise.resolve([]);
  });
  let admitted: boolean | undefined;
  void useChat
    .getState()
    .admitTextAndWait("Accepted without waiting for history.", null)
    .then((value) => {
      admitted = value;
    });
  await vi.waitFor(() => expect(admitted).toBe(true), { timeout: 100 });
});

it("deduplicates identified group arrivals and only marks actually viewed rows read", async () => {
  invoke.mockImplementation((command: string) =>
    command === "owner_node_identity"
      ? Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        })
      : Promise.resolve([]),
  );
  const stop = useChat.getState().start();
  await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
  vi.stubGlobal("document", {
    visibilityState: "visible",
    hasFocus: () => true,
  });
  const event = {
    channel_id: "team",
    channel_name: "Team",
    from: "peer",
    text: "late",
    reply_to: null,
    event_id: "old",
    arrival_kind: "catch_up",
  };
  captured.current!.onChannelMessage(event);
  captured.current!.onChannelMessage(event);
  expect(useChat.getState().unread["channel:team"]).toBe(1);
  await useChat.getState().open({ kind: "channel", id: "team", name: "Team" });
  expect(useChat.getState().unread["channel:team"]).toBe(1);
  useChat.getState().markMessagesViewed("channel:team", ["other"]);
  expect(useChat.getState().unread["channel:team"]).toBe(1);
  useChat.setState({
    messages: {
      "channel:team": [
        {
          id: "old",
          fromMe: false,
          who: "peer",
          text: "late",
          wallClock: 1,
          replyTo: null,
        },
      ],
    },
  });
  useChat.getState().markMessagesViewed("channel:team", ["old"]);
  expect(useChat.getState().unread["channel:team"]).toBe(0);
  captured.current!.onChannelMessage(event);
  expect(useChat.getState().unread["channel:team"]).toBe(0);
  vi.unstubAllGlobals();
  stop();
});

it("baselines inactive history then discovers only new external sibling imports", async () => {
  let revision = 1;
  let imported = false;
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: revision,
      });
    if (command === "list_channels")
      return Promise.resolve([
        { channel_id: "team", name: "Team", owner: "me", member_count: 2 },
      ]);
    if (command === "channel_members")
      return Promise.resolve({ owner: "me", members: [] });
    if (command === "channel_history")
      return Promise.resolve([
        {
          id: "existing",
          from_me: false,
          who: "peer",
          text: "old",
          wall_clock: 100,
        },
        ...(imported
          ? [
              {
                id: "late",
                from_me: false,
                who: "peer",
                text: "older",
                wall_clock: 1,
              },
              {
                id: "mine",
                from_me: true,
                who: "me",
                text: "mine",
                wall_clock: 2,
              },
            ]
          : []),
      ]);
    return Promise.resolve([]);
  });
  await useChat.getState().refreshRoster();
  expect(useChat.getState().unread["channel:team"] ?? 0).toBe(0);
  revision++;
  imported = true;
  await useChat.getState().refreshRoster();
  expect(useChat.getState().unreadIds["channel:team"]).toEqual(["late"]);
  expect(useChat.getState().lateUnreadIds["channel:team"]).toEqual(["late"]);
  await useChat.getState().refreshRoster();
  expect(useChat.getState().unread["channel:team"]).toBe(1);
});

it("bounds import scan work per poll and visits later conversations", async () => {
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: 1,
      });
    if (command === "list_accounts")
      return Promise.resolve(
        Array.from({ length: 20 }, (_, i) => ({
          account_id: `a${i}`,
          names: [],
          device_count: 1,
        })),
      );
    return Promise.resolve([]);
  });
  await useChat.getState().refreshRoster();
  expect(
    invoke.mock.calls.filter(
      ([command]) => command === "owner_account_history",
    ),
  ).toHaveLength(8);
  await useChat.getState().refreshRoster();
  await useChat.getState().refreshRoster();
  expect(
    invoke.mock.calls.filter(
      ([command]) => command === "owner_account_history",
    ),
  ).toHaveLength(20);
  await useChat.getState().refreshRoster();
  expect(
    invoke.mock.calls.filter(
      ([command]) => command === "owner_account_history",
    ),
  ).toHaveLength(20);
});

it("continues later import scans after a failed snapshot and retries the failure", async () => {
  let failed = false;
  invoke.mockImplementation((command: string, args?: { account?: string }) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: 1,
      });
    if (command === "list_accounts")
      return Promise.resolve(
        Array.from({ length: 10 }, (_, i) => ({
          account_id: `a${i}`,
          names: [],
          device_count: 1,
        })),
      );
    if (
      command === "owner_account_history" &&
      args?.account === "a0" &&
      !failed
    ) {
      failed = true;
      return Promise.reject(new Error("busy"));
    }
    return Promise.resolve([]);
  });
  await useChat.getState().refreshRoster();
  expect(useChat.getState().historyRevision).toBeNull();
  await useChat.getState().refreshRoster();
  expect(useChat.getState().historyRevision).toBe(1);
  expect(useChat.getState().historyScanQueue).toEqual([]);
  expect(
    invoke.mock.calls.filter(
      ([command, args]) =>
        command === "owner_account_history" && args.account === "a0",
    ),
  ).toHaveLength(2);
});

it("does not restart initial baselines after the bounded identity ledger evicts conversations", async () => {
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: 1,
      });
    if (command === "list_accounts")
      return Promise.resolve(
        Array.from({ length: 70 }, (_, i) => ({
          account_id: `a${i}`,
          names: [],
          device_count: 1,
        })),
      );
    return Promise.resolve([]);
  });
  for (let round = 0; round < 10; round++)
    await useChat.getState().refreshRoster();
  expect(
    invoke.mock.calls.filter(
      ([command]) => command === "owner_account_history",
    ),
  ).toHaveLength(70);
  expect(useChat.getState().historyBaselines).toHaveLength(70);
  expect(Object.keys(useChat.getState().ledger)).toHaveLength(64);
});

it("rejects a snapshot completed after the account owner changed", async () => {
  let finish!: (rows: unknown[]) => void;
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: 1,
      });
    if (command === "list_accounts")
      return Promise.resolve([
        { account_id: "friend", names: [], device_count: 1 },
      ]);
    if (command === "owner_account_history")
      return new Promise((resolve) => {
        finish = resolve;
      });
    return Promise.resolve([]);
  });
  const pending = useChat.getState().refreshRoster();
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  useAuth.setState((state) => ({ generation: state.generation + 1 }));
  finish([{ id: "stale", from_me: false }]);
  await pending;
  expect(useChat.getState().ledger["account:friend"]).toBeUndefined();
});

it("reveals an unread identity outside the normal 200-row history and reports pruned IDs", async () => {
  invoke.mockImplementation((command: string, args?: { limit?: number }) =>
    command === "owner_account_history"
      ? Promise.resolve(
          Array.from({ length: args?.limit ?? 200 }, (_, i) => ({
            id: `row-${i}`,
            from_me: false,
            who: "peer",
            text: "line",
            wall_clock: i,
          })),
        )
      : Promise.resolve([]),
  );
  await useChat
    .getState()
    .open({ kind: "account", id: "friend", name: "Friend" });
  expect(useChat.getState().messages["account:friend"]).toHaveLength(200);
  expect(
    await useChat.getState().revealUnread("account:friend", "row-300"),
  ).toBe(true);
  expect(useChat.getState().messages["account:friend"]).toHaveLength(500);
  await useChat.getState().reload();
  expect(useChat.getState().messages["account:friend"]).toHaveLength(500);
  expect(
    await useChat.getState().revealUnread("account:friend", "pruned"),
  ).toBe(false);
});

it("clears only legacy unread on focus while retaining identified late unread", () => {
  vi.stubGlobal("document", {
    visibilityState: "visible",
    hasFocus: () => true,
  });
  useChat.setState({
    active: { kind: "channel", id: "team", name: "Team" },
    unread: { "channel:team": 2 },
    ledger: { "channel:team": { late: { seen: false, late: true } } },
    unreadIds: { "channel:team": ["late"] },
    lateUnreadIds: { "channel:team": ["late"] },
  });
  useChat.getState().markLegacyActiveRead();
  expect(useChat.getState().unread["channel:team"]).toBe(1);
  expect(useChat.getState().unreadIds["channel:team"]).toEqual(["late"]);
  vi.unstubAllGlobals();
});

it("tracks identified recovered group file cards without replaying unread or own-device trays", async () => {
  invoke.mockImplementation((command: string) =>
    command === "owner_node_identity"
      ? Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        })
      : Promise.resolve([]),
  );
  const stop = useChat.getState().start();
  await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
  const event = {
    conv: "team",
    is_channel: true,
    from: "peer",
    name: "file.pdf",
    size: 10,
    mime: "application/pdf",
    file_conv: "file1",
    media: false,
    event_id: "file-event",
    received_at: 42,
    arrival_kind: "catch_up",
    batch_id: "file:b",
    batch_complete: false,
  };
  captured.current!.onFile(event);
  captured.current!.onFile({ ...event, batch_complete: true });
  expect(useChat.getState().unreadIds["channel:team"]).toEqual(["file-event"]);
  expect(useChat.getState().lateUnreadIds["channel:team"]).toEqual([
    "file-event",
  ]);
  expect(useChat.getState().reception["channel:team"]).toMatchObject({
    phase: "idle",
    received: 1,
  });
  expect(useChat.getState().incomingFiles).toHaveLength(1);
  captured.current!.onFile({
    ...event,
    event_id: "own-file",
    file_conv: "file2",
    own_device: true,
  });
  expect(useChat.getState().unread["channel:team"]).toBe(1);
  expect(useChat.getState().incomingFiles).toHaveLength(1);
  stop();
});

it("discovers imported offscreen rows when reopening a baselined conversation before its queued scan", async () => {
  let imported = false;
  invoke.mockImplementation((command: string) =>
    command === "channel_history"
      ? Promise.resolve([
          {
            id: "known",
            from_me: false,
            who: "peer",
            text: "known",
            wall_clock: 100,
          },
          ...(imported
            ? [
                {
                  id: "late-import",
                  from_me: false,
                  who: "peer",
                  text: "older",
                  wall_clock: 1,
                },
              ]
            : []),
        ])
      : Promise.resolve([]),
  );
  const conversation = { kind: "channel" as const, id: "team", name: "Team" };
  await useChat.getState().open(conversation);
  expect(useChat.getState().unread["channel:team"] ?? 0).toBe(0);
  imported = true;
  await useChat.getState().open(conversation);
  expect(useChat.getState().unreadIds["channel:team"]).toEqual(["late-import"]);
  expect(useChat.getState().lateUnreadIds["channel:team"]).toEqual([
    "late-import",
  ]);
});

it("does not let a permanently failing history snapshot block a newer revision in another conversation", async () => {
  let revision = 1;
  let imported = false;
  invoke.mockImplementation((command: string, args?: { account?: string }) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: revision,
      });
    if (command === "list_accounts")
      return Promise.resolve(
        ["a", "b"].map((account_id) => ({
          account_id,
          names: [],
          device_count: 1,
        })),
      );
    if (command === "owner_account_history")
      return args?.account === "a"
        ? Promise.reject(new Error("offline"))
        : Promise.resolve(imported ? [{ id: "late-b", from_me: false }] : []);
    return Promise.resolve([]);
  });
  await useChat.getState().refreshRoster();
  revision++;
  imported = true;
  await useChat.getState().refreshRoster();
  expect(useChat.getState().unreadIds["account:b"]).toEqual(["late-b"]);
});

it("clears withdrawn host recovery evidence and retains checked-peer time", async () => {
  let recovery: { phase: string; checked_at?: number } | null = {
    phase: "waiting_for_peer",
  };
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    if (command === "list_channels")
      return Promise.resolve([
        {
          channel_id: "team",
          name: "Team",
          owner: "me",
          member_count: 1,
          recovery,
        },
      ]);
    if (command === "channel_members")
      return Promise.resolve({ owner: "me", members: [] });
    return Promise.resolve([]);
  });
  await useChat.getState().refreshRoster();
  expect(useChat.getState().reception["channel:team"].phase).toBe("waiting");
  recovery = null;
  await useChat.getState().refreshChannels("team");
  await useChat.getState().refreshRoster();
  expect(useChat.getState().reception["channel:team"].phase).toBe("idle");
  recovery = { phase: "peer_checked", checked_at: 42 };
  await useChat.getState().refreshChannels("team");
  await useChat.getState().refreshRoster();
  expect(useChat.getState().reception["channel:team"].checkedAt).toBe(42);
});

it("preserves overflow unread during automatic legacy focus clearing and clears it only explicitly", () => {
  vi.stubGlobal("document", {
    visibilityState: "visible",
    hasFocus: () => true,
  });
  useChat.setState({
    active: { kind: "channel", id: "team", name: "Team" },
    unread: { "channel:team": 1026 },
    overflowUnread: { "channel:team": 1025 },
  });
  useChat.getState().markLegacyActiveRead();
  expect(useChat.getState().unread["channel:team"]).toBe(1025);
  expect(useChat.getState().overflowUnread["channel:team"]).toBe(1025);
  useChat.getState().markActiveRead();
  expect(useChat.getState().unread["channel:team"]).toBe(0);
  expect(useChat.getState().overflowUnread["channel:team"] ?? 0).toBe(0);
  vi.unstubAllGlobals();
});

it("discovers messages accepted while locked from its persisted identity baseline", async () => {
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
  });
  let imported = false;
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: 1,
      });
    if (command === "list_channels")
      return Promise.resolve([
        { channel_id: "team", name: "Team", owner: "me", member_count: 2 },
      ]);
    if (command === "channel_members")
      return Promise.resolve({ owner: "me", members: [] });
    if (command === "channel_history")
      return Promise.resolve([
        { id: "before-lock", from_me: false },
        ...(imported ? [{ id: "while-locked", from_me: false }] : []),
      ]);
    return Promise.resolve([]);
  });
  useChat.setState({ unreadOwner: "host" });
  await useChat.getState().open({ kind: "channel", id: "team", name: "Team" });
  useChat.getState().resetForLock();
  imported = true;
  const stop = useChat.getState().start();
  await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
  await vi.waitFor(() =>
    expect(useChat.getState().unreadIds["channel:team"]).toEqual([
      "while-locked",
    ]),
  );
  stop();
  vi.unstubAllGlobals();
});

it("quietly establishes full history coverage after opening a partial presentation snapshot", async () => {
  const archive = Array.from({ length: 320 }, (_, i) => ({
    id: `old-${i}`,
    from_me: false,
    who: "peer",
    text: "old",
    wall_clock: i,
  }));
  invoke.mockImplementation((command: string, args?: { limit?: number }) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: 1,
      });
    if (command === "list_channels")
      return Promise.resolve([
        { channel_id: "team", name: "Team", owner: "me", member_count: 2 },
      ]);
    if (command === "channel_members")
      return Promise.resolve({ owner: "me", members: [] });
    if (command === "channel_history")
      return Promise.resolve(archive.slice(-(args?.limit ?? 200)));
    return Promise.resolve([]);
  });
  await useChat.getState().open({ kind: "channel", id: "team", name: "Team" });
  expect(useChat.getState().messages["channel:team"]).toHaveLength(200);
  await useChat.getState().refreshRoster();
  expect(useChat.getState().unread["channel:team"] ?? 0).toBe(0);
  expect(useChat.getState().unreadIds["channel:team"]).toEqual([]);
});

it("does not reload or resurrect a cleared conversation from terminal-only reception metadata", async () => {
  invoke.mockImplementation((command: string) =>
    command === "owner_node_identity"
      ? Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        })
      : Promise.resolve([]),
  );
  const stop = useChat.getState().start();
  await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
  useChat.setState({
    active: { kind: "channel", id: "team", name: "Team" },
    reception: {
      "channel:team": {
        phase: "catching_up",
        received: 3,
        batchId: "b",
        processing: true,
      },
    },
  });
  await useChat.getState().clearConversation();
  const calls = invoke.mock.calls.length;
  captured.current!.onChannelMessage({
    channel_id: "team",
    channel_name: "Team",
    from: "me",
    text: "",
    reply_to: null,
    event_id: "erased",
    own_device: true,
    arrival_kind: "catch_up",
    batch_id: "b",
    batch_complete: true,
    reception_only: true,
  });
  expect(invoke.mock.calls).toHaveLength(calls);
  expect(useChat.getState().ledger["channel:team"]?.erased).toBeUndefined();
  expect(useChat.getState().unreadIds["channel:team"] ?? []).toEqual([]);
  expect(useChat.getState().reception["channel:team"]).toMatchObject({
    phase: "idle",
    received: 3,
  });
  const oldHandler = captured.current!.onChannelMessage;
  stop();
  useChat.getState().resetForLock();
  oldHandler({
    channel_id: "team",
    event_id: "old-owner",
    batch_id: "b",
    batch_complete: true,
    reception_only: true,
  });
  expect(useChat.getState().ledger).toEqual({});
  expect(useChat.getState().reception).toEqual({});
});

for (const kind of ["channel", "file"] as const)
  for (const persisted of [false, true]) {
    it(`preserves ${kind} identities ${persisted ? "with a disk baseline" : "on first initialization"} received during deferred boot through baseline, focus and reopen`, async () => {
      const storage = new Map<string, string>();
      vi.stubGlobal("localStorage", {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => storage.set(key, value),
      });
      vi.stubGlobal("document", {
        visibilityState: "visible",
        hasFocus: () => true,
      });
      const { saveReceptionLedger, loadReceptionLedger, saveUnread } =
        await import("@/lib/unreadStorage");
      const key = "channel:team";
      if (persisted) {
        saveReceptionLedger("host", "myacct", {
          [key]: {
            "disk-read": { seen: true, late: false },
            "disk-unread": { seen: false, late: true },
          },
        });
        saveUnread("host", "myacct", { [key]: 3 });
      }
      let finish!: (value: unknown) => void;
      let identityCalls = 0;
      const identity = {
        owner: "host",
        device_id: "me",
        account_id: "myacct",
        history_revision: 1,
      };
      const ids = [
        ...(persisted ? ["disk-read", "disk-unread"] : []),
        "during-boot",
        "own-device",
        "own-history",
      ];
      const expected = persisted
        ? ["disk-unread", "during-boot"]
        : ["during-boot"];
      invoke.mockImplementation((command: string) => {
        if (command === "owner_node_identity")
          return ++identityCalls === 1
            ? new Promise((resolve) => {
                finish = resolve;
              })
            : Promise.resolve(identity);
        if (command === "list_channels")
          return Promise.resolve([
            {
              channel_id: "team",
              name: "Team",
              owner: "peer",
              member_count: 2,
            },
          ]);
        if (command === "channel_history")
          return Promise.resolve(
            ids.map((id) => ({
              id,
              from_me: id.startsWith("own-"),
              who: "peer",
              text: id,
              wall_clock: 1,
            })),
          );
        if (command === "channel_members")
          return Promise.resolve({ owner: "peer", members: [] });
        return Promise.resolve([]);
      });
      let stop = useChat.getState().start();
      try {
        expect(useChat.getState().ready).toBe(false);
        for (const id of ids) {
          const event = {
            from: id === "own-history" ? "me" : "peer",
            event_id: id,
            own_device: id === "own-device",
            arrival_kind: "catch_up",
            channel_id: "team",
            channel_name: "Team",
            is_channel: true,
            conv: "team",
            file_conv: `file-${id}`,
            name: id,
            size: 1,
            mime: "text/plain",
            media: false,
            text: id,
          };
          captured.current![kind === "channel" ? "onChannelMessage" : "onFile"](
            event,
          );
        }
        finish(identity);
        await vi.waitFor(() =>
          expect(useChat.getState().historyFullBaselines).toContain(key),
        );
        expect(new Set(useChat.getState().unreadIds[key])).toEqual(
          new Set(expected),
        );
        expect(useChat.getState().unread[key]).toBe(persisted ? 4 : 1);
        await useChat
          .getState()
          .open({ kind: "channel", id: "team", name: "Team" });
        useChat.getState().markLegacyActiveRead();
        useChat.getState().markMessagesViewed(key, ["disk-read"]);
        expect(useChat.getState().unread[key]).toBe(expected.length);
        expect(
          loadReceptionLedger("host", "myacct")[key]["during-boot"].seen,
        ).toBe(false);
        expect(loadReceptionLedger("other", "myacct")).toEqual({});
        stop();
        useChat.getState().resetForLock();
        stop = useChat.getState().start();
        await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
        expect(new Set(useChat.getState().unreadIds[key])).toEqual(
          new Set(expected),
        );
      } finally {
        stop();
        vi.unstubAllGlobals();
      }
    });
  }

for (const replacementOwner of ["other", "host"]) {
  it(`does not hydrate deferred boot events across ${replacementOwner === "host" ? "a new generation" : "an owner change"}`, async () => {
    let finish!: (value: unknown) => void;
    let calls = 0;
    invoke.mockImplementation((command: string, args?: { owner?: string }) =>
      command === "owner_node_identity"
        ? ++calls === 1
          ? new Promise((resolve) => {
              finish = resolve;
            })
          : Promise.resolve({
              owner: args?.owner,
              device_id: "new-device",
              account_id: "new-account",
            })
        : Promise.resolve([]),
    );
    const stopOld = useChat.getState().start();
    const oldHandler = captured.current!.onChannelMessage;
    oldHandler({
      channel_id: "team",
      event_id: "old-boot",
      from: "peer",
      text: "old",
    });
    stopOld();
    useAuth.setState({
      user: { id: replacementOwner, username: "new", display_name: "New" },
      generation: 1,
    });
    const stopNew = useChat.getState().start();
    try {
      finish({ owner: "host", device_id: "me", account_id: "myacct" });
      oldHandler({
        channel_id: "team",
        event_id: "late-old-boot",
        from: "peer",
        text: "old",
      });
      await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
      expect(useChat.getState().unreadOwner).toBe(replacementOwner);
      expect(useChat.getState().myAccountId).toBe("new-account");
      expect(useChat.getState().ledger).toEqual({});
      expect(useChat.getState().unread).toEqual({});
    } finally {
      stopNew();
    }
  });
}

for (const kind of ["channel", "file"] as const) {
  it(`preserves ${kind} identities received during the replacement account's second identity lookup`, async () => {
    let replacement = false;
    let replacementCalls = 0;
    let finish!: (value: unknown) => void;
    const oldIdentity = {
      owner: "host",
      device_id: "me",
      account_id: "myacct",
    };
    const newIdentity = {
      ...oldIdentity,
      device_id: "new-device",
      account_id: "new-account",
    };
    invoke.mockImplementation((command: string) => {
      if (command === "owner_node_identity") {
        if (!replacement) return Promise.resolve(oldIdentity);
        return ++replacementCalls === 2
          ? new Promise((resolve) => {
              finish = resolve;
            })
          : Promise.resolve(newIdentity);
      }
      if (command === "list_channels")
        return Promise.resolve([
          { channel_id: "team", name: "Team", owner: "peer", member_count: 2 },
        ]);
      if (command === "channel_history")
        return Promise.resolve(
          replacement
            ? [
                {
                  id: "new-arrival",
                  from_me: false,
                  who: "peer",
                  text: "new",
                  wall_clock: 1,
                },
              ]
            : [],
        );
      if (command === "channel_members")
        return Promise.resolve({ owner: "peer", members: [] });
      return Promise.resolve([]);
    });
    const stop = useChat.getState().start();
    try {
      await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
      captured.current!.onChannelMessage({
        channel_id: "team",
        event_id: "old-account-arrival",
        from: "peer",
        text: "old",
      });
      replacement = true;
      await useChat.getState().refreshRoster();
      expect(replacementCalls).toBe(2);
      expect(useChat.getState().ready).toBe(false);
      captured.current![kind === "channel" ? "onChannelMessage" : "onFile"]({
        channel_id: "team",
        event_id: "new-arrival",
        from: "peer",
        text: "new",
        is_channel: true,
        conv: "team",
        file_conv: "new-file",
        name: "new.txt",
        size: 1,
        mime: "text/plain",
        media: false,
      });
      finish(newIdentity);
      await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
      await useChat
        .getState()
        .open({ kind: "channel", id: "team", name: "Team" });
      useChat.getState().markLegacyActiveRead();
      expect(useChat.getState().unreadIds["channel:team"]).toEqual([
        "new-arrival",
      ]);
      expect(useChat.getState().unread["channel:team"]).toBe(1);
      expect(
        useChat.getState().ledger["channel:team"]?.["old-account-arrival"],
      ).toBeUndefined();
      expect(useChat.getState().myAccountId).toBe("new-account");
      expect(useChat.getState().identityEpoch).toBe(1);
    } finally {
      stop();
    }
  });
}

it("removes expired sender and recipient bubbles from cached conversations while idle", async () => {
  vi.useFakeTimers();
  const stop = useChat.getState().start();
  try {
    const now = Date.now();
    const message = {
      id: "expiring",
      fromMe: true,
      who: "me",
      text: "secret",
      wallClock: now,
      replyTo: null,
      expiresAtMs: now + 1000,
    };
    useChat.setState({
      incomingFiles: [
        {
          fromName: "Friend",
          name: "permanent.txt",
          size: 1,
          mime: "text/plain",
          fileConv: "permanent",
          media: false,
        },
        {
          fromName: "Friend",
          name: "future.txt",
          size: 1,
          mime: "text/plain",
          fileConv: "future",
          media: false,
          expiresAtMs: now + 60_000,
        },
        {
          fromName: "Friend",
          name: "temporary.txt",
          size: 1,
          mime: "text/plain",
          fileConv: "file",
          media: false,
          expiresAtMs: now + 1000,
        },
      ],
      messages: {
        "account:friend": [
          message,
          { ...message, id: "retained", expiresAtMs: null },
        ],
        "channel:group": [{ ...message, id: "incoming", fromMe: false }],
      },
      reactions: {
        "account:friend": [
          { target: "expiring", emoji: "👍", who: ["friend"] },
          { target: "retained", emoji: "👍", who: ["friend"] },
        ],
      },
    });
    await vi.advanceTimersByTimeAsync(1001);
    expect(
      useChat.getState().messages["account:friend"].map((m) => m.id),
    ).toEqual(["retained"]);
    expect(useChat.getState().messages["channel:group"]).toEqual([]);
    expect(
      useChat.getState().incomingFiles.map((file) => file.fileConv),
    ).toEqual(["permanent", "future"]);
    expect(
      useChat
        .getState()
        .reactions["account:friend"].map((reaction) => reaction.target),
    ).toEqual(["retained"]);
  } finally {
    stop();
    vi.useRealTimers();
  }
});

it("expires idle cached content after elapsed time even if the wall clock rolls backward", async () => {
  vi.useFakeTimers();
  const wall = Date.now();
  let elapsed = 0;
  const monotonic = vi
    .spyOn(performance, "now")
    .mockImplementation(() => elapsed);
  const stop = useChat.getState().start();
  try {
    useChat.setState({
      messages: {
        "account:friend": [
          {
            id: "rollback",
            fromMe: false,
            who: "peer",
            text: "temporary",
            wallClock: wall,
            replyTo: null,
            expiresAtMs: wall + 1000,
          },
        ],
      },
    });
    vi.setSystemTime(wall - 60_000);
    elapsed = 1001;
    await vi.advanceTimersByTimeAsync(1001);
    expect(useChat.getState().messages["account:friend"]).toEqual([]);
    expect(useChat.getState().expiryNow).toBeGreaterThanOrEqual(wall + 1000);
  } finally {
    stop();
    monotonic.mockRestore();
    vi.useRealTimers();
  }
});

it("expires cached plaintext on focus and removes lifecycle listeners on stop", () => {
  const browser = new EventTarget();
  const page = new EventTarget();
  const removeFocus = vi.spyOn(browser, "removeEventListener");
  const removeVisibility = vi.spyOn(page, "removeEventListener");
  vi.stubGlobal("window", browser);
  vi.stubGlobal("document", page);
  const stop = useChat.getState().start();
  try {
    useChat.setState({
      messages: {
        "account:friend": [
          {
            id: "suspended",
            fromMe: false,
            who: "Friend",
            text: "temporary",
            wallClock: 1,
            replyTo: null,
            expiresAtMs: Date.now() - 1,
          },
        ],
      },
    });
    browser.dispatchEvent(new Event("focus"));
    expect(useChat.getState().messages["account:friend"]).toEqual([]);
    page.dispatchEvent(new Event("visibilitychange"));
  } finally {
    stop();
    expect(removeFocus).toHaveBeenCalledWith("focus", expect.any(Function));
    expect(removeVisibility).toHaveBeenCalledWith(
      "visibilitychange",
      expect.any(Function),
    );
    vi.unstubAllGlobals();
  }
});

it("pushes group metadata into the open view and ignores foreign runtime identities", async () => {
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    if (command === "list_channels")
      return Promise.resolve([
        {
          channel_id: "team",
          name: "Renamed",
          owner: "new-owner",
          member_count: 1,
        },
      ]);
    if (command === "channel_members")
      return Promise.resolve({
        owner: "new-owner",
        members: [
          { user_id: "new-owner", name: "New", replica_enabled: false },
        ],
      });
    return Promise.resolve([]);
  });
  const stop = useChat.getState().start();
  try {
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    await useChat.getState().refreshRoster();
    useChat.setState({
      active: { kind: "channel", id: "team", name: "Old" },
      members: [],
      channelOwner: "old",
    });
    invoke.mockClear();
    const event = {
      owner: "host",
      account_id: "myacct",
      device_id: "me",
      channel_id: "team",
    };
    captured.current!.onChannelChanged({ ...event, device_id: "retired" });
    await Promise.resolve();
    expect(invoke).not.toHaveBeenCalled();
    captured.current!.onChannelChanged(event);
    await vi.waitFor(() =>
      expect(useChat.getState().channelOwner).toBe("new-owner"),
    );
    expect(useChat.getState().active?.name).toBe("Renamed");
    expect(useChat.getState().members[0]?.replica_enabled).toBe(false);
    expect(invoke.mock.calls.some(([name]) => name === "list_peers")).toBe(
      false,
    );
  } finally {
    stop();
  }
});

it("does not refetch group members on each unchanged roster tick", async () => {
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
      return Promise.resolve({ owner: "me", members: [] });
    return Promise.resolve([]);
  });
  await useChat.getState().refreshRoster();
  invoke.mockClear();
  await useChat.getState().refreshRoster();
  expect(
    invoke.mock.calls.some(
      ([name]) => name === "channel_members" || name === "list_channels",
    ),
  ).toBe(false);
});

it.each([false, true])(
  "does not replace pushed member details with an older open request (unchanged members: %s)",
  async (unchangedMembers) => {
    let finish!: (value: unknown) => void;
    let first = true;
    const members = [{ user_id: "new", name: "New" }];
    if (unchangedMembers)
      useChat.setState({
        channelOwner: "old",
        channelMembersById: { team: members },
      });
    invoke.mockImplementation((command: string) => {
      if (command === "owner_node_identity")
        return Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        });
      if (command === "list_channels")
        return Promise.resolve([
          { channel_id: "team", name: "Team", owner: "new", member_count: 1 },
        ]);
      if (command === "channel_members") {
        if (first) {
          first = false;
          return new Promise((resolve) => {
            finish = resolve;
          });
        }
        return Promise.resolve({
          owner: "new",
          members: [{ user_id: "new", name: "New" }],
        });
      }
      return Promise.resolve([]);
    });
    const opening = useChat
      .getState()
      .open({ kind: "channel", id: "team", name: "Team" });
    await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
    await useChat.getState().refreshChannels("team");
    finish({
      owner: "old",
      members: unchangedMembers ? members : [{ user_id: "old", name: "Old" }],
    });
    await opening;
    expect(useChat.getState().channelOwner).toBe("new");
    expect(useChat.getState().members).toEqual([
      { user_id: "new", name: "New" },
    ]);
  },
);

it("keeps newly opened member details when an earlier fallback only updates the avatar cache", async () => {
  const finishes: Array<(value: unknown) => void> = [];
  invoke.mockImplementation((command: string) => {
    if (command === "list_channels")
      return Promise.resolve([
        { channel_id: "team", name: "Team", owner: "new", member_count: 1 },
      ]);
    if (command === "channel_members")
      return new Promise((resolve) => {
        finishes.push(resolve);
      });
    return Promise.resolve([]);
  });
  const fallback = useChat.getState().refreshChannels();
  await vi.waitFor(() => expect(finishes).toHaveLength(1));
  const opening = useChat
    .getState()
    .open({ kind: "channel", id: "team", name: "Team" });
  await vi.waitFor(() => expect(finishes).toHaveLength(2));
  finishes[0]({
    owner: "older",
    members: [{ user_id: "older", name: "Older" }],
  });
  await fallback;
  finishes[1]({ owner: "new", members: [{ user_id: "new", name: "New" }] });
  await opening;
  expect(useChat.getState().channelOwner).toBe("new");
  expect(useChat.getState().members).toEqual([{ user_id: "new", name: "New" }]);
});

it("publishes slow roster snapshots without overlapping periodic identity requests", async () => {
  vi.useFakeTimers();
  let slow = false;
  const identity = { owner: "host", device_id: "me", account_id: "myacct" };
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return slow
        ? new Promise((resolve) => setTimeout(() => resolve(identity), 3000))
        : Promise.resolve(identity);
    if (command === "list_peers")
      return Promise.resolve(slow ? [{ user_id: "fresh", name: "Fresh" }] : []);
    return Promise.resolve([]);
  });
  const stop = useChat.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    invoke.mockClear();
    slow = true;
    await vi.advanceTimersByTimeAsync(6000);
    expect(
      useChat.getState().peers.some((peer) => peer.user_id === "fresh"),
    ).toBe(true);
    expect(
      invoke.mock.calls.filter(
        ([command]) => command === "owner_node_identity",
      ),
    ).toHaveLength(2);
  } finally {
    stop();
    vi.useRealTimers();
  }
});

it("accepts scoped native state invalidations and ignores replaced devices", async () => {
  invoke.mockImplementation((command: string) =>
    command === "owner_node_identity"
      ? Promise.resolve({
          owner: "host",
          device_id: "me",
          account_id: "myacct",
        })
      : Promise.resolve([]),
  );
  const stop = useChat.getState().start();
  try {
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    await useChat.getState().refreshRoster();
    invoke.mockClear();
    const event = {
      owner: "host",
      account_id: "myacct",
      device_id: "old",
      kind: "roster",
    };
    captured.current!.onStateChanged(event);
    await Promise.resolve();
    expect(invoke).not.toHaveBeenCalled();
    captured.current!.onStateChanged({ ...event, device_id: "me" });
    await vi.waitFor(() =>
      expect(
        invoke.mock.calls.some(([command]) => command === "list_peers"),
      ).toBe(true),
    );
  } finally {
    stop();
  }
});

it("ignores an old roster response before negotiating push capability", async () => {
  const { setPushStateSupport, supportsPushState } =
    await import("@/lib/syncInvalidation");
  let finish!: (value: unknown[]) => void;
  invoke.mockImplementation((command: string) => {
    if (command === "owner_node_identity")
      return Promise.resolve({
        owner: "host",
        device_id: "me",
        account_id: "myacct",
      });
    if (command === "list_peers")
      return new Promise((resolve) => {
        finish = resolve;
      });
    return Promise.resolve([]);
  });
  const refreshing = useChat.getState().refreshRoster();
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  useAuth.setState((state) => ({ generation: state.generation + 1 }));
  setPushStateSupport("replacement");
  try {
    finish([]);
    await refreshing;
    expect(supportsPushState("replacement")).toBe(true);
  } finally {
    setPushStateSupport(null);
  }
});

it.each(["Alice", ""])(
  "tracks external channel mentions with display name %j independently of navigation",
  async (displayName) => {
    useAuth.setState({
      user: { id: "host", username: "alice", display_name: displayName },
    });
    invoke.mockImplementation((command: string) =>
      command === "owner_node_identity"
        ? Promise.resolve({
            owner: "host",
            device_id: "me",
            account_id: "myacct",
          })
        : Promise.resolve([]),
    );
    const stop = useChat.getState().start();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    const event = {
      channel_id: "team",
      channel_name: "Team",
      from: "peer",
      text: "@Alice hello",
      reply_to: null,
      event_id: "mention",
    };
    try {
      captured.current!.onChannelMessage(event);
      captured.current!.onChannelMessage({
        ...event,
        event_id: "ordinary",
        text: "latest message",
      });
      captured.current!.onChannelMessage({
        ...event,
        event_id: "other",
        text: "@Bob hello",
      });
      captured.current!.onChannelMessage({
        ...event,
        event_id: "own",
        own_device: true,
      });
      const entries = useChat.getState().ledger["channel:team"];
      expect(entries.mention).toMatchObject({ mentioned: true, seen: false });
      expect(entries.ordinary.mentioned).not.toBe(true);
      expect(entries.other.mentioned).not.toBe(true);
      expect(entries.own.mentioned).not.toBe(true);
      await useChat
        .getState()
        .open({ kind: "channel", id: "team", name: "Team" });
      expect(useChat.getState().ledger["channel:team"].mention.seen).toBe(
        false,
      );
    } finally {
      stop();
    }
  },
);
