import { beforeEach, expect, it, vi } from "vitest";
import { createStore } from "zustand/vanilla";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import {
  submitGroupControlIntent,
  refreshGroupOperationIntents,
  GROUP_OPERATION_TICKET_CAP,
} from "./groupOperations";
import { emptyConversationCache } from "../conversationState";
import type { OutgoingIntentContext } from "../outgoingIntent";
import type { AccountGroupOperationStatus } from "@/lib/types";
import { useAuth } from "../auth";
const id = "ab".repeat(32);
const group = "cd".repeat(32);
function result(
  phase: AccountGroupOperationStatus["phase"],
): AccountGroupOperationStatus {
  return {
    id,
    account_id: "account",
    group,
    kind: "rename",
    phase,
    executions: [],
    committed_events: [],
    primary_event: null,
  };
}
function session() {
  return createStore<OutgoingIntentContext>(() => ({
    ...emptyConversationCache(),
    statusBusy: false,
    runEpoch: 0,
    identityEpoch: 0,
    active: null,
    myId: "device",
    myAccountId: "account",
    reload: vi.fn(async () => true),
    refreshStatuses: vi.fn(async () => {}),
    setError: vi.fn(),
  }));
}
beforeEach(() => {
  invoke.mockReset();
  useAuth.setState({
    user: { id: "host", username: "alice", display_name: "Alice" },
    generation: 0,
  });
});
it("stores only operation metadata for accepted administrative work", async () => {
  const s = session();
  invoke
    .mockResolvedValueOnce(id)
    .mockResolvedValueOnce(result({ kind: "accepted" }));
  expect(
    (
      await submitGroupControlIntent(s.setState, s.getState, group, {
        kind: "rename",
        name: "New name",
      })
    ).kind,
  ).toBe("pending");
  expect(invoke).toHaveBeenNthCalledWith(
    2,
    "owner_submit_account_group_control",
    {
      owner: "host",
      expectedAccount: "account",
      operationId: id,
      channelId: group,
      operation: { kind: "rename", name: "New name" },
    },
  );
  expect(s.getState().groupOperations?.[id].status?.phase.kind).toBe(
    "accepted",
  );
  expect(s.getState().messages).toEqual({});
  expect(s.getState().intents).toEqual({});
  expect(JSON.stringify(s.getState().groupOperations)).not.toContain(
    "New name",
  );
});
it("discovers native pending IDs without constructing message bubbles", async () => {
  const s = session();
  invoke.mockResolvedValueOnce([result({ kind: "waiting_for_executor" })]);
  await refreshGroupOperationIntents(s.setState, s.getState);
  expect(s.getState().groupOperations?.[id].status?.phase.kind).toBe(
    "waiting_for_executor",
  );
  expect(s.getState().messages).toEqual({});
});
it("rejects a new admission without evicting active metadata tickets", async () => {
  const s = session();
  const tickets = Object.fromEntries(
    Array.from({ length: GROUP_OPERATION_TICKET_CAP }, (_, index) => {
      const key = (index + 1).toString(16).padStart(64, "0");
      return [
        key,
        {
          id: key,
          account_id: "account",
          group,
          kind: "rename" as const,
          stage: "unconfirmed" as const,
          observation: 0,
        },
      ];
    }),
  );
  s.setState({ groupOperations: tickets });
  invoke.mockResolvedValueOnce(id);
  expect(
    (
      await submitGroupControlIntent(s.setState, s.getState, group, {
        kind: "rename",
        name: "Full",
      })
    ).kind,
  ).toBe("capacity");
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(s.getState().groupOperations).toEqual(tickets);
});

it("does not invoke a mutation if reactive reservation retires its session", async () => {
  const s = session();
  const stop = s.subscribe((state) => {
    if (state.groupOperations?.[id]?.stage === "submitting")
      useAuth.setState({ generation: 1 });
  });
  invoke
    .mockResolvedValueOnce(id)
    .mockResolvedValueOnce(result({ kind: "accepted" }));
  const outcome = await submitGroupControlIntent(
    s.setState,
    s.getState,
    group,
    { kind: "rename", name: "Stale" },
  );
  stop();
  expect(outcome.kind).toBe("stale");
  expect(invoke).toHaveBeenCalledTimes(1);
});
it("does not run terminal presentation effects after a reactive status write retires the session", async () => {
  const s = session();
  const terminal = vi.fn();
  s.setState({ onGroupOperationTerminal: terminal });
  const stop = s.subscribe((state) => {
    if (state.groupOperations?.[id]?.status?.phase.kind === "committed")
      useAuth.setState({ generation: 1 });
  });
  invoke
    .mockResolvedValueOnce(id)
    .mockResolvedValueOnce(result({ kind: "committed" }));
  const outcome = await submitGroupControlIntent(
    s.setState,
    s.getState,
    group,
    { kind: "rename", name: "Before retirement" },
  );
  stop();
  expect(outcome.kind).toBe("stale");
  expect(terminal).not.toHaveBeenCalled();
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
it("protects a terminal ticket until its original native invocation settles", async () => {
  const s = session();
  const reply = deferred<AccountGroupOperationStatus>();
  const entered = deferred<void>();
  const nextId = "ac".repeat(32);
  let minted = 0;
  invoke.mockImplementation(
    async (command: string, payload: Record<string, unknown>) => {
      if (command === "owner_new_account_group_operation_id")
        return ++minted === 1 ? id : nextId;
      if (command === "owner_submit_account_group_control") {
        if (payload.operationId === id) {
          entered.resolve();
          return reply.promise;
        }
        return { ...result({ kind: "accepted" }), id: nextId };
      }
      if (command === "owner_list_account_group_operations")
        return [result({ kind: "committed" })];
      return null;
    },
  );
  const original = submitGroupControlIntent(s.setState, s.getState, group, {
    kind: "rename",
    name: "Original",
  });
  await entered.promise;
  s.setState((state) => ({
    groupOperations: {
      ...state.groupOperations,
      ...Object.fromEntries(
        Array.from({ length: GROUP_OPERATION_TICKET_CAP - 1 }, (_, index) => {
          const key = (index + 1).toString(16).padStart(64, "0");
          return [
            key,
            {
              id: key,
              account_id: "account",
              group,
              kind: "rename" as const,
              stage: "unconfirmed" as const,
              observation: 0,
            },
          ];
        }),
      ),
    },
  }));
  await refreshGroupOperationIntents(s.setState, s.getState);
  expect(s.getState().groupOperations?.[id].status?.phase.kind).toBe(
    "committed",
  );
  expect(
    (
      await submitGroupControlIntent(s.setState, s.getState, group, {
        kind: "rename",
        name: "Next",
      })
    ).kind,
  ).toBe("capacity");
  expect(s.getState().groupOperations?.[id]).toBeDefined();
  reply.resolve(result({ kind: "committed" }));
  expect((await original).kind).toBe("committed");
  expect(
    (
      await submitGroupControlIntent(s.setState, s.getState, group, {
        kind: "rename",
        name: "Next",
      })
    ).kind,
  ).toBe("pending");
  expect(s.getState().groupOperations?.[id]).toBeUndefined();
  expect(Object.keys(s.getState().groupOperations ?? {})).toHaveLength(
    GROUP_OPERATION_TICKET_CAP,
  );
});
it("does not evict terminal metadata still referenced by a message intent", async () => {
  const s = session();
  const retainedId = "01".padStart(64, "0");
  const tickets = Object.fromEntries(
    Array.from({ length: GROUP_OPERATION_TICKET_CAP }, (_, index) => {
      const key = (index + 1).toString(16).padStart(64, "0");
      return [
        key,
        {
          id: key,
          account_id: "account",
          group,
          kind: "rename" as const,
          stage: "unconfirmed" as const,
          observation: 0,
        },
      ];
    }),
  );
  s.setState({
    groupOperations: {
      ...tickets,
      [retainedId]: {
        id: retainedId,
        account_id: "account",
        group,
        kind: "text",
        stage: "observed",
        observation: 1,
        status: {
          ...result({ kind: "committed" }),
          id: retainedId,
          kind: "text",
        },
      },
    },
    intents: {
      client: {
        operationId: retainedId,
        conversation: { kind: "channel", id: group, name: "Group" },
        payload: { kind: "text", text: "retained", replyTo: null },
        message: {
          id: "event",
          fromMe: true,
          who: "device",
          text: "retained",
          wallClock: 1,
          replyTo: null,
        },
      },
    },
  });
  invoke.mockResolvedValueOnce(id);
  expect(
    (
      await submitGroupControlIntent(s.setState, s.getState, group, {
        kind: "rename",
        name: "Full",
      })
    ).kind,
  ).toBe("capacity");
  expect(s.getState().groupOperations?.[retainedId]).toBeDefined();
  expect(s.getState().intents.client).toBeDefined();
});

it("does not resurrect a ticket when reporting a mismatched response retires the session", async () => {
  const s = session();
  let retired = false;
  const stop = s.subscribe((state) => {
    if (!retired && state.groupOperationIssue === "invalid_response") {
      retired = true;
      useAuth.setState({ generation: 1 });
      s.setState({ groupOperations: {} });
    }
  });
  invoke.mockResolvedValueOnce(id).mockResolvedValueOnce({
    ...result({ kind: "committed" }),
    group: "other-group",
  });
  const outcome = await submitGroupControlIntent(
    s.setState,
    s.getState,
    group,
    { kind: "rename", name: "Mismatch" },
  );
  stop();
  expect(outcome.kind).toBe("stale");
  expect(s.getState().groupOperations).toEqual({});
});
