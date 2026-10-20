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
