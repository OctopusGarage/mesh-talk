import { beforeEach, expect, it, vi } from "vitest";
import { createStore } from "zustand/vanilla";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import {
  reconcileMessages,
  beginSendIntent,
  refreshDeliveryStatuses,
  SEND_INTENT_CAP,
  type SendIntent,
  type OutgoingIntentContext,
} from "./outgoingIntent";
import {
  completeSendIntentState,
  emptyConversationCache,
} from "./conversationState";
import { useAuth } from "./auth";
import { useTransfers } from "./transfers";
import type { ChatMessage, Conversation } from "./conversationModel";

const row = (id: string | null, clientId?: string): ChatMessage => ({
  id,
  clientId,
  fromMe: true,
  who: "me",
  text: "same text",
  wallClock: 1,
  replyTo: null,
});

it("reconciles an accepted intent only with its exact event ID", () => {
  const first = row("event-a");
  const second = row("event-b");
  const pending = row("event-b", "client-b");
  const result = reconcileMessages(
    [first, second],
    [pending],
    [
      {
        conversation: { kind: "account", id: "peer", name: "Peer" },
        payload: { kind: "text", text: "same text", replyTo: null },
        message: pending,
      },
    ],
    [],
  );
  expect(result).toHaveLength(2);
  expect(result[0].clientId).toBeUndefined();
  expect(result[1].clientId).toBe("client-b");
});

// Exercise outgoing ownership without constructing the aggregate application store.
const conversation: Conversation = {
  kind: "account",
  id: "peer",
  name: "Peer",
};
const key = "account:peer";
function session() {
  return createStore<OutgoingIntentContext>(() => ({
    ...emptyConversationCache(),
    statusBusy: false,
    runEpoch: 0,
    identityEpoch: 0,
    active: conversation,
    myId: "local-device",
    myAccountId: "local-account",
    reload: vi.fn(async () => true),
    refreshStatuses: vi.fn(async () => {}),
    setError: vi.fn(),
  }));
}

beforeEach(() => {
  invoke.mockReset();
  invoke.mockResolvedValue([]);
  useAuth.setState({
    user: { id: "host", username: "alice", display_name: "Alice" },
    generation: 0,
  });
  useTransfers.getState().reset();
});

it("keeps durable acceptance when presentation hydration rejects", async () => {
  const store = session();
  invoke.mockResolvedValue("event-a");
  store.setState({
    reload: vi.fn(async () => {
      throw new Error("history unavailable");
    }),
  });
  expect(
    await beginSendIntent(store.setState, store.getState, conversation, {
      kind: "text",
      text: "hello",
      replyTo: null,
    }),
  ).toBe(true);
  const accepted = Object.values(store.getState().intents)[0].message;
  expect(accepted).toMatchObject({ id: "event-a", pending: false });
  expect(accepted.failed).not.toBe(true);
  expect(store.getState().messages[key]).toEqual([accepted]);
  expect(store.getState().setError).not.toHaveBeenCalled();
});

it("ignores acceptance completed after its owner generation retires", async () => {
  const store = session();
  let finish!: (id: string) => void;
  invoke.mockImplementation(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = beginSendIntent(
    store.setState,
    store.getState,
    conversation,
    { kind: "text", text: "hello", replyTo: null },
  );
  useAuth.setState({ generation: 1 });
  finish("late-acceptance");
  expect(await pending).toBe(false);
  expect(Object.values(store.getState().intents)[0].message).toMatchObject({
    id: null,
    pending: true,
  });
  expect(store.getState().reload).not.toHaveBeenCalled();
});

it("reports rejected file metadata through the supplied error capability", async () => {
  const store = session();
  invoke.mockRejectedValue(new Error("file manifest exceeds transport frame"));
  const pending = beginSendIntent(
    store.setState,
    store.getState,
    conversation,
    { kind: "file", path: "/tmp/report.txt", media: false },
  );
  const clientId = Object.keys(store.getState().intents)[0];
  useTransfers
    .getState()
    .applyUploadStatus(clientId, { done: 1, total: 100, delivered: false });
  expect(await pending).toBe(false);
  expect(store.getState().intents[clientId].message).toMatchObject({
    pending: false,
    failed: true,
  });
  expect(useTransfers.getState().transfers[clientId]).toBeUndefined();
  expect(store.getState().setError).toHaveBeenCalledWith(
    "This attachment has too much metadata to send. Try a shorter name or a smaller folder.",
  );
});

it("refuses excess intents through the supplied error capability before dispatch", () => {
  const store = session();
  const intent: SendIntent = {
    conversation,
    payload: { kind: "text", text: "same text", replyTo: null },
    message: row(null),
  };
  store.setState({
    intents: Object.fromEntries(
      Array.from({ length: SEND_INTENT_CAP }, (_, index) => [
        `c${index}`,
        intent,
      ]),
    ),
  });
  expect(
    beginSendIntent(
      store.setState,
      store.getState,
      conversation,
      intent.payload,
    ),
  ).toBeNull();
  expect(invoke).not.toHaveBeenCalled();
  expect(store.getState().setError).toHaveBeenCalledWith(
    "Too many pending messages. Resolve a failed send before sending more.",
  );
});

it("polls an accepted fallback without recursively polling through reload", async () => {
  const store = session();
  const message = row("event-a", "client-a");
  store.setState({
    intents: {
      "client-a": {
        conversation,
        payload: { kind: "text", text: "same text", replyTo: null },
        message,
        acceptanceHistoryOnly: true,
      },
    },
    reload: vi.fn(async () => {
      await refreshDeliveryStatuses(store.setState, store.getState);
      return true;
    }),
  });
  invoke.mockResolvedValue([{ id: "event-a", status: "delivered" }]);
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(store.getState().reload).toHaveBeenCalledTimes(1);
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(store.getState().intents["client-a"].message.delivery).toBe(
    "delivered",
  );
  expect(store.getState().statusBusy).toBe(false);
});

it("continues polling file approval and bytes after the file offer is delivered", async () => {
  const store = session();
  store.setState({
    messages: {
      [key]: [
        {
          ...row("file-offer"),
          delivery: "awaiting",
          file: {
            fileConv: "file",
            name: "photo.png",
            size: 10,
            mime: "image/png",
            media: true,
          },
        },
        { ...row("text"), delivery: "delivered" },
      ],
    },
  });
  invoke
    .mockResolvedValueOnce([
      {
        id: "file-offer",
        status: "delivered",
        file_progress: {
          done: 0,
          total: 10,
          delivered: false,
          consent: "pending",
        },
      },
    ])
    .mockResolvedValueOnce([
      {
        id: "file-offer",
        status: "delivered",
        file_progress: {
          done: 5,
          total: 10,
          delivered: false,
          consent: "accepted",
        },
      },
    ])
    .mockResolvedValueOnce([
      {
        id: "file-offer",
        status: "delivered",
        file_progress: {
          done: 10,
          total: 10,
          delivered: true,
          consent: "accepted",
        },
      },
    ]);
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(store.getState().messages[key][0].delivery).toBe("delivered");
  expect(useTransfers.getState().transfers["file-offer"]?.consent).toBe(
    "pending",
  );
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(useTransfers.getState().transfers["file-offer"]).toMatchObject({
    consent: "accepted",
    done: 5,
  });
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(useTransfers.getState().transfers["file-offer"]?.delivered).toBe(true);
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(invoke).toHaveBeenCalledTimes(3);
  for (const [, args] of invoke.mock.calls)
    expect(args.ids).toEqual(["file-offer"]);
});

it("keeps the original deadline on an optimistic expiring send", async () => {
  const store = session();
  invoke.mockReturnValue(new Promise(() => {}));
  const deadline = Date.now() + 300_000;
  void beginSendIntent(store.setState, store.getState, conversation, {
    kind: "text",
    text: "expires",
    replyTo: null,
    expiresAtMs: deadline,
  });
  expect(store.getState().messages[key][0]).toMatchObject({
    expiresAtMs: deadline,
  });
});

it("does not resurrect expired history or an expired outgoing overlay", () => {
  const expired = { ...row("expired"), expiresAtMs: Date.now() - 1 };
  const live = row("retained");
  expect(reconcileMessages([expired, live], [], [], [])).toEqual([
    expect.objectContaining({ id: "retained" }),
  ]);
  expect(
    reconcileMessages(
      [],
      [expired],
      [
        {
          conversation,
          payload: { kind: "text", text: "same text", replyTo: null },
          message: expired,
        },
      ],
      [],
    ),
  ).toEqual([]);
});

it("uses the session expiry clock when reconciling history after a wall-clock rollback", () => {
  const now = Date.now();
  const expired = { ...row("expired"), expiresAtMs: now + 1000 };
  expect(reconcileMessages([expired], [], [], [], now + 1001)).toEqual([]);
});

it("retires an expired completion after its conversation cache was evicted", () => {
  const conversation: Conversation = {
    kind: "account",
    id: "friend",
    name: "Friend",
  };
  const message = { ...row(null, "pending"), expiresAtMs: 1000 };
  const state = {
    ...emptyConversationCache(),
    intents: {
      pending: {
        conversation,
        message,
        payload: { kind: "text" as const, text: "same text", replyTo: null },
      },
    },
  };
  const result = completeSendIntentState(
    state,
    conversation,
    "pending",
    { ...message, id: "accepted" },
    1001,
  );
  expect(result.intents).toEqual({});
  expect(result.messages).toEqual({});
});

it("coalesces delivery invalidations received during a stale status read", async () => {
  const store = session();
  store.setState({
    messages: { [key]: [{ ...row("event-a"), delivery: "awaiting" }] },
  });
  let release!: (rows: unknown[]) => void;
  invoke
    .mockImplementationOnce(() => new Promise((resolve) => (release = resolve)))
    .mockResolvedValue([{ id: "event-a", status: "delivered" }]);
  const read = refreshDeliveryStatuses(store.setState, store.getState);
  for (let i = 0; i < 8; i++)
    await refreshDeliveryStatuses(
      store.setState,
      store.getState,
      undefined,
      true,
    );
  expect(invoke).toHaveBeenCalledTimes(1);
  release([{ id: "event-a", status: "awaiting" }]);
  await read;
  expect(invoke).toHaveBeenCalledTimes(2);
  expect(store.getState().messages[key][0].delivery).toBe("delivered");
  expect(store.getState().statusBusy).toBe(false);
});

it("drops queued delivery invalidations when the captured runtime changes", async () => {
  const store = session();
  store.setState({
    messages: { [key]: [{ ...row("event-a"), delivery: "awaiting" }] },
  });
  let release!: (rows: unknown[]) => void;
  invoke.mockImplementationOnce(
    () => new Promise((resolve) => (release = resolve)),
  );
  const read = refreshDeliveryStatuses(store.setState, store.getState);
  await refreshDeliveryStatuses(
    store.setState,
    store.getState,
    undefined,
    true,
  );
  store.setState({ runEpoch: 1, statusBusy: false });
  release([{ id: "event-a", status: "awaiting" }]);
  await read;
  expect(invoke).toHaveBeenCalledTimes(1);
});

it.each(["text", "sticker", "file"] as const)(
  "settles a channel %s by exact acceptance before network delivery finishes",
  async (kind) => {
    const store = session();
    const channel: Conversation = {
      kind: "channel",
      id: "group",
      name: "Group",
    };
    store.setState({ active: channel });
    invoke.mockImplementation(async (command) => {
      if (
        command === "owner_enqueue_channel_text" ||
        command === "owner_enqueue_channel_sticker"
      )
        return "accepted-event";
      if (command === "owner_enqueue_channel_file")
        return { id: "accepted-event", fileConv: "photo-bytes" };
      // The previous channel IPC waits for a stalled optional relay.
      return new Promise(() => {});
    });
    const payload =
      kind === "text"
        ? { kind, text: "kk", replyTo: null }
        : kind === "sticker"
          ? { kind, stickerId: "heart", fallback: "❤️" }
          : { kind, path: "/photo.png", media: true };
    const sending = beginSendIntent(
      store.setState,
      store.getState,
      channel,
      payload,
    );
    await vi.waitFor(
      () => {
        expect(store.getState().messages["channel:group"]).toEqual([
          expect.objectContaining({ id: "accepted-event", pending: false }),
        ]);
      },
      { timeout: 200 },
    );
    expect(await sending).toBe(true);
    const message = store.getState().messages["channel:group"][0];
    if (kind === "file") expect(message.file?.fileConv).toBe("photo-bytes");
    const reconciled = reconcileMessages(
      [{ ...message, clientId: undefined, replicas: 1 }],
      [message],
      Object.values(store.getState().intents),
      [],
    );
    expect(reconciled).toHaveLength(1);
    expect(reconciled[0]).toMatchObject({
      id: "accepted-event",
      pending: false,
      replicas: 1,
    });
  },
);

it("projects member and post-office custody separately for exact channel IDs", async () => {
  const store = session();
  const channel: Conversation = { kind: "channel", id: "group", name: "Group" };
  store.setState({
    active: channel,
    messages: { "channel:group": [{ ...row("event"), pending: false }] },
  });
  invoke.mockResolvedValue([
    { id: "event", replicas: 0, post_offices: 1 },
    { id: "unrelated", replicas: 2, post_offices: 9 },
  ]);
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(store.getState().messages["channel:group"]).toEqual([
    expect.objectContaining({
      id: "event",
      pending: false,
      replicas: 0,
      postOfficeCopies: 1,
    }),
  ]);
  expect(invoke).toHaveBeenCalledWith("channel_custody_statuses", {
    channelId: "group",
    ids: ["event"],
  });
});

it("samples local delivery facts monotonically and clears absent or confirmed observations", async () => {
  const store = session();
  store.setState({ messages: { [key]: [row("event-a")] } });
  const clock = vi.spyOn(performance, "now").mockReturnValue(200);
  invoke.mockResolvedValue([
    {
      id: "event-a",
      status: "awaiting",
      observation: { kind: "attempting", age_ms: 3000 },
    },
  ]);
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(store.getState().messages[key][0].deliveryObservation).toEqual({
    kind: "attempting",
    ageMs: 3000,
    sampledAt: 200,
  });
  const sampled = store.getState().messages[key][0];
  expect(
    reconcileMessages([row("event-a")], [sampled], [], [])[0]
      .deliveryObservation,
  ).toEqual(sampled.deliveryObservation);
  invoke.mockResolvedValue([{ id: "event-a", status: "awaiting" }]);
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(store.getState().messages[key][0].deliveryObservation).toBeUndefined();
  for (const observation of [
    { kind: "future", age_ms: 0 },
    { kind: "attempting", age_ms: -1 },
    { kind: "attempting", age_ms: Infinity },
  ]) {
    invoke.mockResolvedValue([
      { id: "event-a", status: "awaiting", observation },
    ]);
    await refreshDeliveryStatuses(store.setState, store.getState);
    expect(
      store.getState().messages[key][0].deliveryObservation,
    ).toBeUndefined();
  }
  invoke.mockResolvedValue([
    {
      id: "event-a",
      status: "delivered",
      observation: { kind: "attempting", age_ms: 0 },
    },
  ]);
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(store.getState().messages[key][0]).toMatchObject({
    delivery: "delivered",
  });
  expect(store.getState().messages[key][0].deliveryObservation).toBeUndefined();
  clock.mockRestore();
});

it("scopes observations to requested outgoing IDs and the captured conversation and runtime", async () => {
  const store = session();
  store.setState({
    messages: {
      [key]: [row("event-a"), { ...row("incoming"), fromMe: false }],
      "account:other": [row("event-a")],
    },
  });
  let release!: (rows: unknown[]) => void;
  invoke.mockImplementationOnce(
    () => new Promise((resolve) => (release = resolve)),
  );
  const read = refreshDeliveryStatuses(store.setState, store.getState);
  const rows = ["event-a", "incoming", "unrequested"].map((id) => ({
    id,
    status: "awaiting",
    observation: { kind: "retry_scheduled", age_ms: 0 },
  }));
  release(rows);
  await read;
  expect(store.getState().messages[key][0].deliveryObservation?.kind).toBe(
    "retry_scheduled",
  );
  expect(store.getState().messages[key][1].deliveryObservation).toBeUndefined();
  expect(
    store.getState().messages["account:other"][0].deliveryObservation,
  ).toBeUndefined();
  invoke.mockImplementationOnce(
    () => new Promise((resolve) => (release = resolve)),
  );
  const late = refreshDeliveryStatuses(store.setState, store.getState);
  store.setState({ runEpoch: 1 });
  release([
    {
      id: "event-a",
      status: "awaiting",
      observation: { kind: "attempting", age_ms: 0 },
    },
  ]);
  await late;
  expect(store.getState().messages[key][0].deliveryObservation?.kind).toBe(
    "retry_scheduled",
  );
});

it("clears evicted observations and never projects a personal conversation hint", async () => {
  const store = session();
  const observed = {
    ...row("event-a"),
    delivery: "awaiting" as const,
    deliveryObservation: {
      kind: "attempting" as const,
      ageMs: 0,
      sampledAt: 100,
    },
  };
  store.setState({ messages: { [key]: [observed] } });
  invoke.mockResolvedValue([]);
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(store.getState().messages[key][0].deliveryObservation).toBeUndefined();
  store.setState({
    active: { ...conversation, id: "local-account" },
    messages: { "account:local-account": [row("event-a")] },
  });
  invoke.mockResolvedValue([
    {
      id: "event-a",
      status: "awaiting",
      observation: { kind: "attempting", age_ms: 0 },
    },
  ]);
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(
    store.getState().messages["account:local-account"][0].deliveryObservation,
  ).toBeUndefined();
});

it("keeps a canonical confirmation ahead of a late awaiting hint and acceptance", async () => {
  const store = session();
  store.setState({ messages: { [key]: [row("event-a")] } });
  let release!: (rows: unknown[]) => void;
  invoke.mockImplementationOnce(
    () => new Promise((resolve) => (release = resolve)),
  );
  const read = refreshDeliveryStatuses(store.setState, store.getState);
  const confirmed = { ...row("event-a"), delivery: "delivered" as const };
  store.setState({ messages: { [key]: [confirmed] } });
  release([
    {
      id: "event-a",
      status: "awaiting",
      observation: { kind: "attempting", age_ms: 0 },
    },
  ]);
  await read;
  expect(store.getState().messages[key][0].delivery).toBe("delivered");
  expect(store.getState().messages[key][0].deliveryObservation).toBeUndefined();
  const stale = {
    ...row("event-a", "client"),
    delivery: "awaiting" as const,
    deliveryObservation: {
      kind: "attempting" as const,
      ageMs: 0,
      sampledAt: 0,
    },
  };
  const state = {
    ...emptyConversationCache(),
    messages: { [key]: [confirmed] },
    intents: {
      client: {
        conversation,
        message: stale,
        payload: { kind: "text" as const, text: "text", replyTo: null },
      },
    },
  };
  const completion = completeSendIntentState(
    state,
    conversation,
    "client",
    stale,
  );
  expect(completion.messages?.[key][0].delivery).toBe("delivered");
  expect(completion.messages?.[key][0].deliveryObservation).toBeUndefined();
});

it("clears a local observation when send completion itself carries confirmation", () => {
  const stale = {
    ...row("event-a", "client"),
    delivery: "awaiting" as const,
    deliveryObservation: {
      kind: "attempting" as const,
      ageMs: 0,
      sampledAt: 0,
    },
  };
  const state = {
    ...emptyConversationCache(),
    messages: { [key]: [stale] },
    intents: {
      client: {
        conversation,
        message: stale,
        payload: { kind: "text" as const, text: "text", replyTo: null },
      },
    },
  };
  const completion = completeSendIntentState(state, conversation, "client", {
    ...stale,
    delivery: "delivered",
  });
  expect(completion.messages?.[key][0].deliveryObservation).toBeUndefined();
});
