import { beforeEach, expect, it, vi } from "vitest";
import { createStore } from "zustand/vanilla";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import {
  reconcileMessages,
  beginSendIntent,
  dispatchIntent,
  settleGroupSendIntents,
  refreshGroupOperationIntents,
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
import type { AccountGroupOperationStatus } from "@/lib/types";
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

it("settles a channel File only from committed canonical metadata before network delivery", async () => {
  const store = session();
  store.setState({ active: groupConversation });
  invoke.mockImplementation(async (command) => {
    if (command === "owner_new_account_group_operation_id") return operationId;
    if (command === "owner_submit_account_group_file")
      return groupStatus({ kind: "committed" }, "file");
    return new Promise(() => {});
  });
  store.setState({
    reload: vi.fn(async () => {
      store.setState({
        messages: {
          "channel:group": [
            {
              ...row("public-event"),
              pending: false,
              metadataPending: false,
              file: {
                name: "photo.png",
                size: 10,
                mime: "image/png",
                media: true,
                fileConv: "photo-bytes",
              },
            },
          ],
        },
      });
      return true;
    }),
  });
  expect(
    await beginSendIntent(store.setState, store.getState, groupConversation, {
      kind: "file",
      path: "/photo.png",
      media: true,
    }),
  ).toBe(true);
  expect(store.getState().messages["channel:group"]).toEqual([
    expect.objectContaining({
      id: "public-event",
      pending: false,
      file: expect.objectContaining({ fileConv: "photo-bytes" }),
    }),
  ]);
});

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

it("retires stale copy counts only for requested own channel messages omitted by status", async () => {
  const store = session();
  const channel: Conversation = { kind: "channel", id: "group", name: "Group" };
  const incoming = { ...row("incoming"), fromMe: false, replicas: 2 };
  const other = { ...row("other"), replicas: 1, postOfficeCopies: 1 };
  store.setState({
    active: channel,
    messages: {
      "channel:group": [
        { ...row("event"), replicas: 2, postOfficeCopies: 1 },
        incoming,
      ],
      "channel:other": [other],
    },
  });
  invoke.mockResolvedValue([]);
  await refreshDeliveryStatuses(store.setState, store.getState);
  expect(store.getState().messages["channel:group"][0]).toMatchObject({
    id: "event",
    replicas: undefined,
    postOfficeCopies: undefined,
  });
  expect(store.getState().messages["channel:group"][1]).toEqual(incoming);
  expect(store.getState().messages["channel:other"]).toEqual([other]);
});

it("does not retire counts for channel messages added after a status request", async () => {
  const store = session();
  const channel: Conversation = { kind: "channel", id: "group", name: "Group" };
  const requested = { ...row("requested"), replicas: 1 };
  const addedLater = {
    ...row("added-later"),
    replicas: 2,
    postOfficeCopies: 1,
  };
  store.setState({
    active: channel,
    messages: { "channel:group": [requested] },
  });
  let release!: (rows: unknown[]) => void;
  invoke.mockImplementation(
    () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  );
  const refresh = refreshDeliveryStatuses(store.setState, store.getState);
  store.setState({ messages: { "channel:group": [requested, addedLater] } });
  release([]);
  await refresh;
  expect(
    store.getState().messages["channel:group"][0].replicas,
  ).toBeUndefined();
  expect(store.getState().messages["channel:group"][1]).toEqual(addedLater);
});

it("does not clear channel confirmation after its owner runtime retires", async () => {
  const store = session();
  const channel: Conversation = { kind: "channel", id: "group", name: "Group" };
  const message = { ...row("event"), replicas: 1, postOfficeCopies: 1 };
  store.setState({ active: channel, messages: { "channel:group": [message] } });
  let release!: (rows: unknown[]) => void;
  invoke.mockImplementation(
    () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  );
  const refresh = refreshDeliveryStatuses(store.setState, store.getState);
  store.setState({ runEpoch: 1, statusBusy: false });
  release([]);
  await refresh;
  expect(store.getState().messages["channel:group"]).toEqual([message]);
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

it("projects only recognized recovery reasons from exact local observations", async () => {
  const store = session();
  store.setState({ messages: { [key]: [row("event-a")] } });
  for (const reason of [
    "post_office_disabled",
    "no_known_route",
    "attempt_timed_out",
    "future_reason",
  ]) {
    invoke.mockResolvedValue([
      {
        id: "event-a",
        status: "awaiting",
        observation: { kind: "retry_scheduled", age_ms: 0, reason },
      },
    ]);
    await refreshDeliveryStatuses(store.setState, store.getState);
    expect(store.getState().messages[key][0].deliveryObservation?.reason).toBe(
      reason === "future_reason" ? undefined : reason,
    );
  }
});

const groupConversation: Conversation = {
  kind: "channel",
  id: "group",
  name: "Group",
};
const operationId = "ab".repeat(32);
function groupStatus(
  phase: AccountGroupOperationStatus["phase"],
  kind: "text" | "sticker" | "file" = "text",
): AccountGroupOperationStatus {
  return {
    id: operationId,
    account_id: "local-account",
    group: "group",
    kind,
    phase,
    executions:
      phase.kind === "committed"
        ? [
            {
              device_id: "source-device",
              phase,
              committed_events: ["public-event"],
            },
          ]
        : [],
    committed_events: phase.kind === "committed" ? ["public-event"] : [],
    primary_event: phase.kind === "committed" ? "public-event" : null,
  };
}
it.each(["text", "sticker"] as const)(
  "retains %s admission as pending and links the original operation before mutation",
  async (kind) => {
    const store = session();
    store.setState({ active: groupConversation });
    const onAccepted = vi.fn();
    invoke.mockImplementation(
      async (command: string, args: Record<string, unknown>) => {
        if (command === "owner_new_account_group_operation_id")
          return operationId;
        expect(command).toBe(`owner_submit_account_group_${kind}`);
        expect(Object.values(store.getState().intents)[0].operationId).toBe(
          operationId,
        );
        expect(args).toMatchObject({
          owner: "host",
          expectedAccount: "local-account",
          operationId,
          channelId: "group",
        });
        return groupStatus({ kind: "accepted" }, kind);
      },
    );
    const payload =
      kind === "text"
        ? { kind, text: "hello", replyTo: null }
        : { kind, stickerId: "heart", fallback: "❤️" };
    expect(
      await beginSendIntent(
        store.setState,
        store.getState,
        groupConversation,
        payload,
        { onAccepted },
      ),
    ).toBe(true);
    expect(Object.values(store.getState().intents)[0].message).toMatchObject({
      id: null,
      pending: true,
    });
    expect(onAccepted).toHaveBeenCalledTimes(1);
    expect(store.getState().reload).not.toHaveBeenCalled();
    expect(JSON.stringify(store.getState().groupOperations)).not.toContain(
      "hello",
    );
  },
);
it("retries uncertain channel work with its original operation ID and original body", async () => {
  const store = session();
  store.setState({ active: groupConversation });
  invoke.mockImplementation(async (command: string) => {
    if (command === "owner_new_account_group_operation_id") return operationId;
    throw new Error("timeoutUnconfirmed");
  });
  expect(
    await beginSendIntent(store.setState, store.getState, groupConversation, {
      kind: "text",
      text: "original",
      replyTo: null,
    }),
  ).toBe(true);
  const intent = Object.values(store.getState().intents)[0];
  expect(intent).toMatchObject({
    operationId,
    message: { id: null, pending: true },
  });
  invoke.mockClear();
  invoke.mockResolvedValue(groupStatus({ kind: "accepted" }));
  expect(await dispatchIntent(store.setState, store.getState, intent)).toBe(
    true,
  );
  expect(invoke).toHaveBeenCalledExactlyOnceWith(
    "owner_submit_account_group_text",
    expect.objectContaining({ operationId, text: "original" }),
  );
});
it("settles a polled committed group operation once and preserves canonical source authorship", async () => {
  const store = session();
  store.setState({
    active: groupConversation,
    onGroupOperationTerminal: (status) =>
      settleGroupSendIntents(store.setState, store.getState, status),
  });
  invoke
    .mockResolvedValueOnce(operationId)
    .mockResolvedValueOnce(groupStatus({ kind: "accepted" }));
  await beginSendIntent(store.setState, store.getState, groupConversation, {
    kind: "text",
    text: "original",
    replyTo: null,
  });
  const canonical = {
    ...row("public-event"),
    who: "actual-source",
    text: "canonical",
    pending: false,
  };
  store.setState((state) => ({
    messages: {
      "channel:group": [...state.messages["channel:group"], canonical],
    },
  }));
  invoke.mockImplementation(async (command) =>
    command === "owner_list_account_group_operations"
      ? []
      : groupStatus({ kind: "committed" }),
  );
  await refreshGroupOperationIntents(store.setState, store.getState);
  expect(store.getState().messages["channel:group"]).toEqual([
    expect.objectContaining({
      id: "public-event",
      who: "actual-source",
      text: "canonical",
      pending: false,
    }),
  ]);
  expect(Object.values(store.getState().intents)[0].message.id).toBe(
    "public-event",
  );
  expect(
    store.getState().messages["channel:group"][0].delivery,
  ).toBeUndefined();
});
it("never binds a pending group receipt, operation ID, or committed result without a primary event", () => {
  const store = session();
  const pending = row(null, "client");
  store.setState({
    intents: {
      client: {
        conversation: groupConversation,
        payload: { kind: "text", text: "original", replyTo: null },
        message: { ...pending, pending: true },
        operationId,
      },
    },
    messages: { "channel:group": [{ ...pending, pending: true }] },
  });
  settleGroupSendIntents(
    store.setState,
    store.getState,
    groupStatus({ kind: "accepted" }),
  );
  settleGroupSendIntents(store.setState, store.getState, {
    ...groupStatus({ kind: "committed" }),
    primary_event: null,
  });
  expect(store.getState().intents.client.message).toMatchObject({
    id: null,
    pending: true,
  });
});

it("acknowledges a recovered uncertain group send exactly once on public commitment", async () => {
  const store = session();
  const onAccepted = vi.fn();
  store.setState({
    active: groupConversation,
    onGroupOperationTerminal: (status) =>
      settleGroupSendIntents(store.setState, store.getState, status),
  });
  invoke
    .mockResolvedValueOnce(operationId)
    .mockRejectedValueOnce(new Error("timeoutUnconfirmed"));
  await beginSendIntent(
    store.setState,
    store.getState,
    groupConversation,
    { kind: "text", text: "hello", replyTo: null },
    { onAccepted },
  );
  expect(onAccepted).not.toHaveBeenCalled();
  invoke.mockImplementation(async (command) =>
    command === "owner_list_account_group_operations"
      ? []
      : groupStatus({ kind: "committed" }),
  );
  await refreshGroupOperationIntents(store.setState, store.getState);
  await refreshGroupOperationIntents(store.setState, store.getState);
  expect(onAccepted).toHaveBeenCalledTimes(1);
  expect(store.getState().messages["channel:group"]).toEqual([
    expect.objectContaining({
      id: "public-event",
      who: "source-device",
      pending: false,
    }),
  ]);
});
it("keeps an accepted group operation pending when its draft callback throws", async () => {
  const store = session();
  invoke
    .mockResolvedValueOnce(operationId)
    .mockResolvedValueOnce(groupStatus({ kind: "accepted" }));
  expect(
    await beginSendIntent(
      store.setState,
      store.getState,
      groupConversation,
      { kind: "text", text: "hello", replyTo: null },
      {
        onAccepted: () => {
          throw new Error("UI effect failed");
        },
      },
    ),
  ).toBe(true);
  expect(Object.values(store.getState().intents)[0].message).toMatchObject({
    id: null,
    pending: true,
  });
});
it("keeps a rejected protected group draft with its composer when failure retention is disabled", async () => {
  const store = session();
  const onAccepted = vi.fn();
  invoke
    .mockResolvedValueOnce(operationId)
    .mockResolvedValueOnce(
      groupStatus({ kind: "rejected", reason: "not_authorized" }),
    );
  expect(
    await beginSendIntent(
      store.setState,
      store.getState,
      groupConversation,
      { kind: "text", text: "protected", replyTo: null },
      { onAccepted, retainFailure: false },
    ),
  ).toBe(false);
  expect(store.getState().intents).toEqual({});
  expect(store.getState().messages["channel:group"]).toEqual([]);
  expect(onAccepted).not.toHaveBeenCalled();
});
it("does not submit a group body after reservation or payload linkage retires its owner", async () => {
  for (const retireAt of ["ticket", "intent"] as const) {
    useAuth.setState({ generation: 0 });
    const store = session();
    const stop = store.subscribe((state) => {
      if (
        retireAt === "ticket"
          ? !!state.groupOperations?.[operationId]
          : Object.values(state.intents).some((intent) => !!intent.operationId)
      )
        useAuth.setState({ generation: 1 });
    });
    invoke.mockReset();
    invoke.mockResolvedValueOnce(operationId);
    expect(
      await beginSendIntent(store.setState, store.getState, groupConversation, {
        kind: "text",
        text: "hello",
        replyTo: null,
      }),
    ).toBe(false);
    expect(invoke).toHaveBeenCalledTimes(1);
    stop();
  }
});
it("keeps the existing coordinator usable after a runtime restart", async () => {
  const store = session();
  invoke
    .mockResolvedValueOnce(operationId)
    .mockResolvedValueOnce(groupStatus({ kind: "accepted" }));
  await beginSendIntent(store.setState, store.getState, groupConversation, {
    kind: "text",
    text: "first",
    replyTo: null,
  });
  store.setState({ runEpoch: 1 });
  const nextId = "cd".repeat(32);
  invoke.mockResolvedValueOnce(nextId).mockResolvedValueOnce({
    ...groupStatus({ kind: "accepted" }),
    id: nextId,
  });
  await beginSendIntent(store.setState, store.getState, groupConversation, {
    kind: "text",
    text: "second",
    replyTo: null,
  });
  expect(store.getState().groupOperations?.[nextId]?.status?.phase.kind).toBe(
    "accepted",
  );
});
it("settles an inactive group through its existing cache and does not repopulate an evicted cache", async () => {
  for (const cached of [true, false]) {
    const store = session();
    invoke
      .mockResolvedValueOnce(operationId)
      .mockResolvedValueOnce(groupStatus({ kind: "accepted" }));
    await beginSendIntent(store.setState, store.getState, groupConversation, {
      kind: "text",
      text: "hello",
      replyTo: null,
    });
    if (!cached) store.setState({ messages: {} });
    settleGroupSendIntents(
      store.setState,
      store.getState,
      groupStatus({ kind: "committed" }),
    );
    expect(Object.values(store.getState().intents)[0].message).toMatchObject({
      id: "public-event",
      who: "source-device",
      pending: false,
    });
    if (cached)
      expect(store.getState().messages["channel:group"]).toHaveLength(1);
    else expect(store.getState().messages).toEqual({});
  }
});
it("does not apply a late group mutation after account identity changes", async () => {
  const store = session();
  let finish!: (status: AccountGroupOperationStatus) => void;
  const entered = new Promise<void>((resolve) => {
    invoke.mockImplementation(async (command) => {
      if (command === "owner_new_account_group_operation_id")
        return operationId;
      resolve();
      return new Promise((done) => {
        finish = done;
      });
    });
  });
  const pending = beginSendIntent(
    store.setState,
    store.getState,
    groupConversation,
    { kind: "text", text: "hello", replyTo: null },
  );
  await entered;
  store.setState({ myAccountId: "other-account", identityEpoch: 1 });
  finish(groupStatus({ kind: "committed" }));
  expect(await pending).toBe(false);
  expect(Object.values(store.getState().intents)[0].message.id).toBeNull();
});

it("binds a polled public group event even if draft cleanup throws", async () => {
  const store = session();
  store.setState({
    onGroupOperationTerminal: (status) =>
      settleGroupSendIntents(store.setState, store.getState, status),
  });
  invoke
    .mockResolvedValueOnce(operationId)
    .mockRejectedValueOnce(new Error("timeoutUnconfirmed"));
  expect(
    await beginSendIntent(
      store.setState,
      store.getState,
      groupConversation,
      { kind: "text", text: "hello", replyTo: null },
      {
        onAccepted: () => {
          throw new Error("draft cleanup failed");
        },
      },
    ),
  ).toBe(true);
  invoke.mockImplementation(async (command) =>
    command === "owner_list_account_group_operations"
      ? []
      : groupStatus({ kind: "committed" }),
  );
  await refreshGroupOperationIntents(store.setState, store.getState);
  expect(Object.values(store.getState().intents)[0].message).toMatchObject({
    id: "public-event",
    who: "source-device",
    pending: false,
  });
});

it.each(["account", "channel"] as const)(
  "does not dispatch an %s body under an owner retired during optimistic insertion",
  async (kind) => {
    const store = session();
    const c = { kind, id: "target", name: "Target" };
    let retired = false;
    const stop = store.subscribe((state) => {
      if (retired || !Object.keys(state.intents).length) return;
      retired = true;
      useAuth.setState({
        user: { id: "replacement-owner", username: "bob", display_name: "Bob" },
        generation: 1,
      });
    });
    try {
      const admission = beginSendIntent(store.setState, store.getState, c, {
        kind: "text",
        text: "old private body",
        replyTo: null,
      });
      await Promise.resolve();
      expect(invoke).not.toHaveBeenCalled();
      expect(admission).toBeNull();
      expect(store.getState().intents).toEqual({});
      expect(store.getState().messages[`${kind}:target`]).toEqual([]);
    } finally {
      stop();
    }
  },
);
it("does not mint a group operation when account replacement occurs during optimistic insertion", async () => {
  const store = session();
  let retired = false;
  const stop = store.subscribe((state) => {
    if (retired || !Object.keys(state.intents).length) return;
    retired = true;
    // Account equality is checked explicitly, even before its epoch projection catches up.
    store.setState({ myAccountId: "replacement-account" });
  });
  try {
    const admission = beginSendIntent(
      store.setState,
      store.getState,
      groupConversation,
      { kind: "text", text: "old body", replyTo: null },
    );
    await Promise.resolve();
    expect(invoke).not.toHaveBeenCalled();
    expect(admission).toBeNull();
    expect(store.getState().intents).toEqual({});
  } finally {
    stop();
  }
});
it("does not remove a replacement session's intent that reuses the optimistic key", () => {
  const store = session();
  let replacement: SendIntent | undefined;
  const stop = store.subscribe((state) => {
    if (replacement) return;
    const [clientId, original] = Object.entries(state.intents)[0] ?? [];
    if (!clientId || !original) return;
    useAuth.setState({ generation: 1 });
    replacement = {
      ...original,
      payload: { kind: "text", text: "new session body", replyTo: null },
      message: { ...original.message, text: "new session body" },
    };
    store.setState({
      intents: { [clientId]: replacement },
      messages: { "channel:group": [replacement.message] },
    });
  });
  try {
    expect(
      beginSendIntent(store.setState, store.getState, groupConversation, {
        kind: "text",
        text: "retired body",
        replyTo: null,
      }),
    ).toBeNull();
    expect(Object.values(store.getState().intents)).toEqual([replacement]);
    expect(store.getState().messages["channel:group"]).toEqual([
      replacement!.message,
    ]);
    expect(invoke).not.toHaveBeenCalled();
  } finally {
    stop();
  }
});

it("retains typed group File admission and retries its original path and ID", async () => {
  const store = session();
  store.setState({ active: groupConversation });
  invoke.mockImplementation(async (command: string) => {
    if (command === "owner_new_account_group_operation_id") return operationId;
    throw new Error("lost response");
  });
  await beginSendIntent(store.setState, store.getState, groupConversation, {
    kind: "file",
    path: "/tmp/original.png",
    media: true,
    expiresAtMs: 9000000000000,
    protection: "authenticate",
  });
  const intent = Object.values(store.getState().intents)[0];
  expect(intent.operationId).toBe(operationId);
  expect(intent.message).toMatchObject({
    pending: true,
    metadataPending: true,
    id: null,
  });
  invoke.mockClear();
  invoke.mockResolvedValue(groupStatus({ kind: "accepted" }, "file"));
  await dispatchIntent(store.setState, store.getState, intent);
  expect(invoke).toHaveBeenCalledExactlyOnceWith(
    "owner_submit_account_group_file",
    expect.objectContaining({
      owner: "host",
      expectedAccount: "local-account",
      operationId,
      channelId: "group",
      path: "/tmp/original.png",
      media: true,
      progressKey: intent.message.clientId,
      expiresAtMs: 9000000000000,
      protection: "authenticate",
    }),
  );
  settleGroupSendIntents(
    store.setState,
    store.getState,
    groupStatus({ kind: "committed" }, "file"),
  );
  expect(Object.values(store.getState().intents)).toHaveLength(1);
  const canonical = {
    ...row("public-event"),
    metadataPending: false,
    file: {
      name: "original.png",
      size: 10,
      mime: "image/png",
      media: true,
      fileConv: "real-file",
    },
  };
  store.setState((state) => ({
    messages: {
      ...state.messages,
      "channel:group": [...state.messages["channel:group"], canonical],
    },
  }));
  settleGroupSendIntents(
    store.setState,
    store.getState,
    groupStatus({ kind: "committed" }, "file"),
  );
  expect(Object.values(store.getState().intents)[0].message).toMatchObject({
    id: "public-event",
    pending: false,
    metadataPending: false,
  });
  expect(store.getState().messages["channel:group"][0].file?.fileConv).toBe(
    "real-file",
  );
});
