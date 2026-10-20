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
    myId: "me",
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
