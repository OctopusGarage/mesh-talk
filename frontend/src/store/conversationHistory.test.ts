import { beforeEach, expect, it, vi } from "vitest";
import { createStore } from "zustand/vanilla";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import type { Conversation } from "./conversationModel";
import { useAuth } from "./auth";
import {
  emptyConversationHistory,
  type ConversationHistoryContext,
  reloadConversation,
  requestConversationReload,
  revealConversationIdentity,
  scanConversationHistory,
} from "./conversationHistory";

const conversation: Conversation = {
  kind: "account",
  id: "friend",
  name: "Friend",
};
const key = "account:friend";
const row = (id: string) => ({
  id,
  from_me: false,
  who: "Friend",
  text: id,
  wall_clock: 1,
});
function session() {
  return createStore<ConversationHistoryContext>(() => ({
    ...emptyConversationHistory(),
    runEpoch: 0,
    identityEpoch: 0,
    refreshStatuses: vi.fn(async () => {}),
  }));
}

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  useAuth.setState({
    user: { id: "host", username: "alice", display_name: "Alice" },
    generation: 0,
  });
});

it.each(["owner_account_history", "account_reactions"])(
  "releases the history spinner when %s stalls and retries without restarting",
  async (stalledCommand) => {
    vi.useFakeTimers();
    const pending: (() => void)[] = [];
    try {
      const store = session();
      store.setState({ active: conversation, cacheOrder: [key] });
      invoke.mockImplementation((command: string) =>
        command === stalledCommand
          ? new Promise((resolve) => pending.push(() => resolve([])))
          : Promise.resolve([]),
      );
      const loading = reloadConversation(store.setState, store.getState);
      expect(store.getState().loading).toBe(true);
      await vi.advanceTimersByTimeAsync(10_000);
      expect(store.getState().loading).toBe(false);
      expect(store.getState().historyError).toBe(key);
      expect(await loading).toBe(false);

      invoke.mockImplementation((command: string) =>
        Promise.resolve(
          command === "owner_account_history" ? [row("fresh")] : [],
        ),
      );
      expect(await reloadConversation(store.setState, store.getState)).toBe(
        true,
      );
      pending.forEach((finish) => finish());
      await vi.advanceTimersByTimeAsync(0);
      expect(store.getState().messages[key]?.[0]?.id).toBe("fresh");
      expect(store.getState().historyError).toBeNull();
    } finally {
      pending.forEach((finish) => finish());
      vi.useRealTimers();
    }
  },
);

it("establishes expanded coverage without unread archive, then discovers changed snapshots", async () => {
  const store = session();
  store.setState({ active: conversation, cacheOrder: [key] });
  const recent = Array.from({ length: 200 }, (_, index) =>
    row(`recent${index}`),
  );
  let rows = recent;
  invoke.mockImplementation((command: string) =>
    Promise.resolve(command === "owner_account_history" ? rows : []),
  );
  await reloadConversation(store.setState, store.getState);
  rows = [row("archive"), ...recent.slice(1), row("new")];
  await scanConversationHistory(
    store.setState,
    store.getState,
    [conversation],
    1,
  );
  expect(store.getState().unreadIds[key]).toEqual(["new"]);
  expect(store.getState().historyFullBaselines).toContain(key);
  rows = [row("older-import"), ...rows];
  await scanConversationHistory(
    store.setState,
    store.getState,
    [conversation],
    2,
  );
  expect(store.getState().unreadIds[key]).toEqual(["new", "older-import"]);
  expect(store.getState().historyRevision).toBe(2);
});

it("rotates failed snapshots behind later work and only advances a completed revision", async () => {
  const store = session();
  const conversations = Array.from(
    { length: 10 },
    (_, index): Conversation => ({
      kind: "account",
      id: `a${index}`,
      name: "",
    }),
  );
  let failed = false;
  invoke.mockImplementation((command: string, args: { account?: string }) => {
    if (
      command === "owner_account_history" &&
      args.account === "a0" &&
      !failed
    ) {
      failed = true;
      return Promise.reject(new Error("busy"));
    }
    return Promise.resolve([]);
  });
  await scanConversationHistory(
    store.setState,
    store.getState,
    conversations,
    7,
  );
  expect(invoke.mock.calls).toHaveLength(8);
  expect(store.getState().historyRevision).toBeNull();
  expect(store.getState().historyScanQueue.map((c) => c.id)).toEqual([
    "a8",
    "a9",
    "a0",
  ]);
  await scanConversationHistory(
    store.setState,
    store.getState,
    conversations,
    7,
  );
  expect(store.getState().historyScanQueue).toEqual([]);
  expect(store.getState().historyRevision).toBe(7);
});

it("rejects a reload completed after its cache is evicted", async () => {
  const store = session();
  store.setState({ active: conversation, cacheOrder: [key] });
  let finish!: (rows: unknown[]) => void;
  invoke.mockImplementation((command: string) =>
    command === "owner_account_history"
      ? new Promise((resolve) => {
          finish = resolve;
        })
      : Promise.resolve([]),
  );
  const pending = reloadConversation(store.setState, store.getState);
  store.setState({
    active: { ...conversation, id: "other" },
    cacheOrder: ["account:other"],
  });
  finish([row("evicted")]);
  expect(await pending).toBe(false);
  expect(store.getState().messages[key]).toBeUndefined();
  expect(store.getState().historyBaselines).not.toContain(key);
});

it("retires a superseded roster scan without leaving future scans busy", async () => {
  const store = session();
  let current = true;
  let finish!: (rows: unknown[]) => void;
  invoke.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = scanConversationHistory(
    store.setState,
    store.getState,
    [conversation],
    1,
    () => current,
  );
  current = false;
  finish([row("stale")]);
  await pending;
  expect(store.getState().ledger[key]).toBeUndefined();
  expect(store.getState().historyScanBusy).toBe(false);
  invoke.mockResolvedValue([]);
  await scanConversationHistory(
    store.setState,
    store.getState,
    [conversation],
    1,
  );
  expect(store.getState().historyRevision).toBe(1);
});

it.each(["resolve", "reject"] as const)(
  "retires a superseded active reload's loading slot on %s",
  async (outcome) => {
    const store = session();
    store.setState({ active: conversation, cacheOrder: [key] });
    let current = true;
    let finish!: (rows: unknown[]) => void;
    let fail!: (error: Error) => void;
    let calls = 0;
    invoke.mockImplementation((command: string) => {
      if (command !== "owner_account_history") return Promise.resolve([]);
      if (++calls === 1) return Promise.resolve([]);
      return new Promise((resolve, reject) => {
        finish = resolve;
        fail = reject;
      });
    });
    const pending = scanConversationHistory(
      store.setState,
      store.getState,
      [conversation],
      1,
      () => current,
    );
    await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
    expect(store.getState().loading).toBe(true);
    current = false;
    if (outcome === "resolve") finish([row("stale")]);
    else fail(new Error("stale failure"));
    await pending;
    expect(store.getState().loading).toBe(false);
    expect(store.getState().historyError).toBeNull();
    expect(store.getState().historyRevision).toBeNull();
    expect(store.getState().messages[key]).toBeUndefined();
  },
);

it("starts with independent history snapshots for each session", () => {
  const first = emptyConversationHistory();
  const second = emptyConversationHistory();
  expect(first).toMatchObject({
    active: null,
    loading: false,
    historyRevision: null,
    historyScanQueue: [],
    historyBaselines: [],
    historyFullBaselines: [],
    cacheOrder: [],
    messages: {},
    reactions: {},
    intents: {},
    deleted: {},
    historyRequests: {},
    historyLimits: {},
    unread: {},
    ledger: {},
  });
  first.historyBaselines.push(key);
  first.historyScanQueue.push(conversation);
  first.deleted[key] = ["deleted"];
  expect(second.historyBaselines).toEqual([]);
  expect(second.historyScanQueue).toEqual([]);
  expect(second.deleted).toEqual({});
});

it("commits recovered history with historical arrival metadata while preserving live arrivals", async () => {
  const { receiveIdentity, receptionPatch } = await import("./reception");
  const store = session();
  const channel: Conversation = { kind: "channel", id: "team", name: "Team" };
  const channelKey = "channel:team";
  store.setState({
    active: channel,
    cacheOrder: [channelKey],
    historyBaselines: [channelKey],
  });
  store.setState(
    receptionPatch(
      store.getState(),
      receiveIdentity(store.getState(), channelKey, {
        event_id: "live",
        arrival_kind: "live",
        received_at: 42,
      }),
    ),
  );
  const committed: Array<
    Array<{ id: string | null; arrivalKind?: string; receivedAt?: number }>
  > = [];
  const stop = store.subscribe((next, previous) => {
    if (next.messages[channelKey] !== previous.messages[channelKey])
      committed.push(next.messages[channelKey]);
  });
  invoke.mockImplementation((command: string) =>
    Promise.resolve(
      command === "channel_history" ? [row("recovered"), row("live")] : [],
    ),
  );
  try {
    await reloadConversation(store.setState, store.getState);
    expect(committed).toHaveLength(1);
    expect(committed[0]).toEqual([
      expect.objectContaining({ id: "recovered", arrivalKind: "history_sync" }),
      expect.objectContaining({
        id: "live",
        arrivalKind: "live",
        receivedAt: 42,
      }),
    ]);
    expect(store.getState().unreadIds[channelKey]).toEqual([
      "live",
      "recovered",
    ]);
  } finally {
    stop();
  }
});

it("refreshes exact context independently without replacing recent history or unread coverage", async () => {
  const store = session();
  store.setState({
    active: conversation,
    cacheOrder: [key],
    activeRequest: 1,
    searchTarget: {
      key,
      request: 1,
      reference: { kind: "account", target: "friend", message_id: "archive" },
      context: [],
    },
  });
  invoke.mockImplementation((command: string) =>
    Promise.resolve(
      command === "owner_search_context"
        ? {
            status: "found",
            reference: {
              kind: "account",
              target: "friend",
              message_id: "archive",
            },
            messages: [row("archive")],
          }
        : command === "owner_account_history"
          ? [row("recent")]
          : [],
    ),
  );
  await reloadConversation(store.setState, store.getState);
  expect(store.getState().messages[key].map((m) => m.id)).toEqual(["recent"]);
  const target = store.getState().searchTarget;
  expect(
    target && "reference" in target && target.context.map((m) => m.id),
  ).toEqual(["archive"]);
  expect(store.getState().ledger[key]).not.toHaveProperty("archive");
});

it("releases a stalled exact context read without losing recent history or accepting late context", async () => {
  vi.useFakeTimers();
  let finish: ((value: unknown) => void) | undefined;
  try {
    const store = session();
    const reference = {
      kind: "account" as const,
      target: "friend",
      message_id: "archive",
    };
    store.setState({
      active: conversation,
      cacheOrder: [key],
      searchTarget: { key, request: 1, reference, context: [] },
    });
    invoke.mockImplementation((command: string) =>
      command === "owner_search_context"
        ? new Promise((resolve) => {
            finish = resolve;
          })
        : Promise.resolve(
            command === "owner_account_history" ? [row("recent")] : [],
          ),
    );
    const loading = reloadConversation(store.setState, store.getState);
    await vi.advanceTimersByTimeAsync(10_001);
    await loading;
    expect(store.getState().loading).toBe(false);
    expect(store.getState().messages[key].map((message) => message.id)).toEqual(
      ["recent"],
    );
    finish?.({ status: "found", reference, messages: [row("archive")] });
    await vi.advanceTimersByTimeAsync(0);
    const target = store.getState().searchTarget;
    expect(target && "reference" in target && target.context).toEqual([]);
  } finally {
    finish?.({ status: "unavailable" });
    vi.useRealTimers();
  }
});

it("drops context with an identical ID from a different namespace", async () => {
  const store = session();
  store.setState({
    active: conversation,
    cacheOrder: [key],
    searchTarget: {
      key,
      request: 1,
      reference: { kind: "account", target: "friend", message_id: "same-id" },
      context: [],
    },
  });
  invoke.mockImplementation((command: string) =>
    Promise.resolve(
      command === "owner_search_context"
        ? {
            status: "found",
            reference: {
              kind: "legacy-device",
              target: "friend",
              message_id: "same-id",
            },
            messages: [row("same-id")],
          }
        : [],
    ),
  );
  await reloadConversation(store.setState, store.getState);
  const target = store.getState().searchTarget;
  expect(target && "reference" in target && target.context).toEqual([]);
});

it.each(["Alice", ""])(
  "classifies channel history mentions with display name %j while preserving unread identities",
  async (displayName) => {
    useAuth.setState({
      user: { id: "host", username: "alice", display_name: displayName },
    });
    const store = session();
    const channel: Conversation = { kind: "channel", id: "team", name: "Team" };
    const channelKey = "channel:team";
    store.setState({
      active: channel,
      cacheOrder: [channelKey],
      ledger: { [channelKey]: { mention: { seen: false, late: true } } },
      unreadIds: { [channelKey]: ["mention"] },
      unread: { [channelKey]: 1 },
    });
    invoke.mockImplementation((command: string) =>
      Promise.resolve(
        command === "channel_history"
          ? [
              { ...row("mention"), text: "@Alice please check" },
              { ...row("own"), text: "@Alice", from_me: true },
              { ...row("other"), text: "@Bob" },
              { ...row("archive"), text: "@Alice archived" },
            ]
          : [],
      ),
    );
    expect(await reloadConversation(store.setState, store.getState)).toBe(true);
    expect(store.getState().ledger[channelKey].mention).toMatchObject({
      mentioned: true,
      seen: false,
    });
    expect(store.getState().ledger[channelKey].own.mentioned).not.toBe(true);
    expect(store.getState().ledger[channelKey].other.mentioned).not.toBe(true);
    expect(store.getState().ledger[channelKey].archive.seen).toBe(true);
  },
);

it("coalesces a burst into one running automatic reload and one shared follow-up", async () => {
  const store = session();
  store.setState({ active: conversation, cacheOrder: [key] });
  const finishes: Array<(rows: unknown[]) => void> = [];
  invoke.mockImplementation((command: string) =>
    command === "owner_account_history"
      ? new Promise((resolve) => finishes.push(resolve))
      : Promise.resolve([]),
  );
  const first = requestConversationReload(store.setState, store.getState);
  const queued = requestConversationReload(store.setState, store.getState);
  for (let index = 0; index < 1000; index++) {
    expect(requestConversationReload(store.setState, store.getState)).toBe(
      queued,
    );
  }
  expect(finishes).toHaveLength(1);
  expect(invoke.mock.calls).toHaveLength(2);
  finishes[0]([row("first")]);
  expect(await first).toBe(true);
  await vi.waitFor(() => expect(finishes).toHaveLength(2));
  finishes[1]([row("latest")]);
  expect(await queued).toBe(true);
  expect(store.getState().messages[key].map((message) => message.id)).toEqual([
    "latest",
  ]);
  expect(invoke.mock.calls).toHaveLength(4);
});

it.each(["owner_account_history", "account_reactions", "owner_search_context"])(
  "keeps automatic reload admission occupied until timed-out %s settles",
  async (stalledCommand) => {
    vi.useFakeTimers();
    const finishes: Array<(value: unknown) => void> = [];
    try {
      const store = session();
      store.setState({
        active: conversation,
        cacheOrder: [key],
        searchTarget: {
          key,
          request: 1,
          reference: {
            kind: "account",
            target: "friend",
            message_id: "archive",
          },
          context: [],
        },
      });
      invoke.mockImplementation((command: string) =>
        command === stalledCommand
          ? new Promise((resolve) => finishes.push(resolve))
          : Promise.resolve(
              command === "owner_account_history" ? [row("retained")] : [],
            ),
      );
      const running = requestConversationReload(store.setState, store.getState);
      const queued = requestConversationReload(store.setState, store.getState);
      let queuedResult: boolean | undefined;
      void queued.then((loaded) => {
        queuedResult = loaded;
      });
      const timerCount = vi.getTimerCount();
      for (let index = 0; index < 1000; index++)
        expect(requestConversationReload(store.setState, store.getState)).toBe(
          queued,
        );
      expect(vi.getTimerCount()).toBe(timerCount);
      await vi.advanceTimersByTimeAsync(10_001);
      await running;
      expect(queuedResult).toBe(false);
      expect(store.getState().loading).toBe(false);
      if (stalledCommand !== "owner_search_context")
        expect(store.getState().historyError).toBe(key);
      for (let index = 0; index < 1000; index++)
        requestConversationReload(store.setState, store.getState);
      expect(invoke.mock.calls).toHaveLength(3);
      expect(vi.getTimerCount()).toBe(0);
      invoke.mockImplementation((command: string) =>
        Promise.resolve(
          command === "owner_account_history" ? [row("fresh")] : [],
        ),
      );
      finishes[0]([]);
      await vi.advanceTimersByTimeAsync(0);
      expect(await queued).toBe(false);
      expect(invoke.mock.calls).toHaveLength(6);
      expect(
        store.getState().messages[key].map((message) => message.id),
      ).toEqual(["fresh"]);
      expect(store.getState().historyError).toBeNull();
    } finally {
      finishes.forEach((finish) => finish([]));
      await vi.advanceTimersByTimeAsync(0);
      vi.useRealTimers();
    }
  },
);

it("waits for every automatic bundle read after an early rejection", async () => {
  const store = session();
  store.setState({
    active: conversation,
    cacheOrder: [key],
    searchTarget: {
      key,
      request: 1,
      reference: { kind: "account", target: "friend", message_id: "archive" },
      context: [],
    },
  });
  const finishes: Array<() => void> = [];
  invoke.mockImplementation((command: string) =>
    command === "owner_account_history"
      ? Promise.reject(new Error("history unavailable"))
      : new Promise((resolve) => finishes.push(() => resolve([]))),
  );
  try {
    const running = requestConversationReload(store.setState, store.getState);
    const queued = requestConversationReload(store.setState, store.getState);
    expect(await running).toBe(false);
    expect(store.getState().historyError).toBe(key);
    expect(invoke.mock.calls).toHaveLength(3);
    finishes[0]();
    await Promise.resolve();
    await Promise.resolve();
    expect(invoke.mock.calls).toHaveLength(3);
    invoke.mockResolvedValue([]);
    finishes[1]();
    expect(await queued).toBe(true);
    expect(invoke.mock.calls).toHaveLength(6);
  } finally {
    finishes.forEach((finish) => finish());
  }
});

it.each(["owner", "run", "identity", "caller"])(
  "drops queued automatic work after its %s lifetime retires",
  async (retirement) => {
    const store = session();
    store.setState({ active: conversation, cacheOrder: [key] });
    const finishes: Array<() => void> = [];
    let callerCurrent = true;
    invoke.mockImplementation((command: string) =>
      command === "owner_account_history"
        ? new Promise((resolve) => finishes.push(() => resolve([row("stale")])))
        : Promise.resolve([]),
    );
    const running = requestConversationReload(
      store.setState,
      store.getState,
      true,
      () => callerCurrent,
    );
    const queued = requestConversationReload(
      store.setState,
      store.getState,
      true,
      () => callerCurrent,
    );
    if (retirement === "owner") useAuth.setState({ generation: 1 });
    else if (retirement === "run") store.setState({ runEpoch: 1 });
    else if (retirement === "identity") store.setState({ identityEpoch: 1 });
    else callerCurrent = false;
    finishes.forEach((finish) => finish());
    expect(await running).toBe(false);
    expect(await queued).toBe(false);
    expect(invoke.mock.calls).toHaveLength(2);
    expect(store.getState().messages[key]).toBeUndefined();
  },
);

it("admits the replacement session only after old automatic native work settles", async () => {
  const store = session();
  store.setState({ active: conversation, cacheOrder: [key] });
  const finishes: Array<() => void> = [];
  invoke.mockImplementation((command: string) =>
    command === "owner_account_history"
      ? new Promise((resolve) => finishes.push(() => resolve([row("stale")])))
      : Promise.resolve([]),
  );
  try {
    const originalGeneration = useAuth.getState().generation;
    const oldSet: Parameters<typeof requestConversationReload>[0] = (
      partial,
    ) => {
      if (useAuth.getState().generation === originalGeneration)
        store.setState(partial);
    };
    const running = requestConversationReload(oldSet, store.getState);
    const oldQueued = requestConversationReload(oldSet, store.getState);
    useAuth.setState({ generation: 1 });
    invoke.mockImplementation((command: string) =>
      Promise.resolve(
        command === "owner_account_history" ? [row("replacement")] : [],
      ),
    );
    const replacement = requestConversationReload(
      store.setState,
      store.getState,
    );
    expect(await oldQueued).toBe(false);
    expect(invoke.mock.calls).toHaveLength(2);
    finishes[0]();
    expect(await running).toBe(false);
    expect(await replacement).toBe(true);
    expect(store.getState().messages[key].map((message) => message.id)).toEqual(
      ["replacement"],
    );
    expect(invoke.mock.calls).toHaveLength(4);
  } finally {
    finishes.forEach((finish) => finish());
  }
});

it("lets explicit reloads satisfy already queued automatic invalidations", async () => {
  const store = session();
  store.setState({ active: conversation, cacheOrder: [key] });
  let finish!: () => void;
  invoke.mockImplementation((command: string) =>
    command === "owner_account_history"
      ? new Promise((resolve) => {
          finish = () => resolve([row("stale")]);
        })
      : Promise.resolve([]),
  );
  const running = requestConversationReload(store.setState, store.getState);
  const queued = requestConversationReload(store.setState, store.getState);
  invoke.mockImplementation((command: string) =>
    Promise.resolve(
      command === "owner_account_history" ? [row("explicit")] : [],
    ),
  );
  expect(await reloadConversation(store.setState, store.getState)).toBe(true);
  expect(store.getState().loading).toBe(false);
  finish();
  expect(await running).toBe(false);
  expect(await queued).toBe(true);
  expect(invoke.mock.calls).toHaveLength(4);
  expect(store.getState().messages[key][0].id).toBe("explicit");
});

it("runs queued automatic history against the latest selected conversation", async () => {
  const store = session();
  store.setState({ active: conversation, cacheOrder: [key] });
  let finish!: () => void;
  invoke.mockImplementation((command: string) =>
    command === "owner_account_history"
      ? new Promise((resolve) => {
          finish = () => resolve([row("previous")]);
        })
      : Promise.resolve([]),
  );
  const running = requestConversationReload(store.setState, store.getState);
  const queued = requestConversationReload(store.setState, store.getState);
  const latestKey = "account:latest";
  store.setState({
    active: { ...conversation, id: "latest" },
    activeRequest: 1,
    cacheOrder: [latestKey],
  });
  invoke.mockImplementation((command: string) =>
    Promise.resolve(command === "owner_account_history" ? [row("latest")] : []),
  );
  finish();
  expect(await running).toBe(false);
  expect(await queued).toBe(true);
  expect(
    invoke.mock.calls
      .filter(([command]) => command === "owner_account_history")
      .map(([, args]) => args.account),
  ).toEqual(["friend", "latest"]);
  expect(store.getState().messages[key]).toBeUndefined();
  expect(store.getState().messages[latestKey][0].id).toBe("latest");
});

it("keeps a queued live invalidation when a coalesced roster request retires", async () => {
  const store = session();
  store.setState({ active: conversation, cacheOrder: [key] });
  let finish!: () => void;
  let rosterCurrent = true;
  invoke.mockImplementation((command: string) =>
    command === "owner_account_history"
      ? new Promise((resolve) => {
          finish = () => resolve([]);
        })
      : Promise.resolve([]),
  );
  const running = requestConversationReload(store.setState, store.getState);
  const live = requestConversationReload(store.setState, store.getState);
  const roster = requestConversationReload(
    store.setState,
    store.getState,
    true,
    () => rosterCurrent,
  );
  expect(roster).toBe(live);
  rosterCurrent = false;
  invoke.mockResolvedValue([]);
  finish();
  await running;
  expect(await live).toBe(true);
  expect(invoke.mock.calls).toHaveLength(4);
});

it("preserves archive expansion baselines while satisfying a live reload burst", async () => {
  const { receiveIdentity, receptionPatch } = await import("./reception");
  const store = session();
  store.setState({ active: conversation, cacheOrder: [key] });
  const recent = Array.from({ length: 200 }, (_, index) =>
    row(`recent-${index}`),
  );
  invoke.mockImplementation((command: string) =>
    Promise.resolve(command === "owner_account_history" ? recent : []),
  );
  await reloadConversation(store.setState, store.getState);
  store.setState(
    receptionPatch(
      store.getState(),
      receiveIdentity(store.getState(), key, {
        event_id: "live",
        arrival_kind: "live",
        received_at: 42,
      }),
    ),
  );
  let finish!: () => void;
  invoke.mockImplementation((command: string) =>
    command === "owner_account_history"
      ? new Promise((resolve) => {
          finish = () => resolve(recent);
        })
      : Promise.resolve([]),
  );
  const running = requestConversationReload(store.setState, store.getState);
  const queued = requestConversationReload(store.setState, store.getState);
  for (let index = 0; index < 100; index++)
    requestConversationReload(store.setState, store.getState);
  invoke.mockImplementation((command: string) =>
    Promise.resolve(
      command === "owner_account_history"
        ? [row("archive"), ...recent, row("live")]
        : [],
    ),
  );
  try {
    expect(
      await revealConversationIdentity(
        store.setState,
        store.getState,
        key,
        "archive",
      ),
    ).toBe(true);
    expect(await queued).toBe(true);
    expect(store.getState().historyLimits[key]).toBe(500);
    expect(store.getState().ledger[key].archive.seen).toBe(true);
    expect(store.getState().unreadIds[key]).toEqual(["live"]);
    expect(store.getState().ledger[key].live).toMatchObject({
      seen: false,
      arrivalKind: "live",
      receivedAt: 42,
    });
    expect(store.getState().loading).toBe(false);
  } finally {
    finish();
    await running;
  }
});

it("reconciles late file identity bindings in both history display and unread reception", async () => {
  const store = session();
  store.setState({
    active: conversation,
    cacheOrder: [key],
    ledger: {
      [key]: {
        first: { seen: false, late: false },
        second: { seen: false, late: true },
      },
    },
    unreadIds: { [key]: ["first", "second"] },
    unread: { [key]: 2 },
  });
  invoke.mockImplementation((command: string) =>
    Promise.resolve(
      command === "owner_account_history"
        ? [
            {
              ...row("canonical"),
              file_message_aliases: ["canonical", "first", "second"],
            },
          ]
        : [],
    ),
  );
  await reloadConversation(store.setState, store.getState);
  expect(store.getState().unreadIds[key]).toEqual(["canonical"]);
  expect(store.getState().unread[key]).toBe(1);
  expect(store.getState().messages[key][0].fileMessageAliases).toEqual([
    "canonical",
    "first",
    "second",
  ]);
});

it("removes confirmed expiry and recall from unread while retaining identities absent from a bounded history page", async () => {
  const { emptyReception } = await import("./reception");
  const store = session();
  store.setState({
    ...emptyReception({
      [key]: {
        expired: { seen: false, late: false },
        recalled: { seen: false, late: false },
        outside: { seen: false, late: true },
      },
    }),
    active: conversation,
    cacheOrder: [key],
    unread: { [key]: 3 },
  });
  invoke.mockImplementation((command: string) =>
    Promise.resolve(
      command === "owner_account_history"
        ? [
            { ...row("expired"), expires_at_ms: Date.now() - 1 },
            { ...row("recalled"), recalled: true },
          ]
        : [],
    ),
  );
  await reloadConversation(store.setState, store.getState);
  expect(store.getState().unreadIds[key]).toEqual(["outside"]);
  expect(store.getState().unread[key]).toBe(1);
  expect(store.getState().ledger[key].expired.seen).toBe(false);
  expect(store.getState().ledger[key].recalled.seen).toBe(false);
});
