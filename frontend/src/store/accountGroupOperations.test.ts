import { expect, it, vi } from "vitest";
import type { AccountGroupOperationStatus } from "@/lib/types";
import {
  createGroupOperationCoordinator,
  type GroupOperationTicket,
} from "./accountGroupOperations";

const id = "ab".repeat(32);
const group = "cd".repeat(32);
const account = "ef".repeat(16);
function status(
  phase: AccountGroupOperationStatus["phase"],
): AccountGroupOperationStatus {
  return {
    id,
    account_id: account,
    group,
    kind: "rename",
    phase,
    executions: [],
    committed_events: phase.kind === "committed" ? ["12".repeat(32)] : [],
    primary_event: null,
  };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function fixture(capacity = 256) {
  let generation = 0;
  const tickets = new Map<string, GroupOperationTicket>();
  const terminal = vi.fn();
  const report = vi.fn();
  const commands = {
    mint: vi.fn(async () => id),
    status: vi.fn(
      async (
        _owner: string,
        _account: string,
        _id: string,
      ): Promise<AccountGroupOperationStatus | null> => null,
    ),
    list: vi.fn(
      async (
        _owner: string,
        _account: string,
        _query: { pendingOnly: boolean; after?: string; limit: number },
      ): Promise<AccountGroupOperationStatus[]> => [],
    ),
    cancel: vi.fn(async () => status({ kind: "cancellation_requested" })),
  };
  const coordinator = createGroupOperationCoordinator({
    capture: () => {
      const captured = generation;
      return {
        owner: "local-owner",
        accountId: account,
        scope: `scope-${captured}`,
        current: () => captured === generation,
      };
    },
    get: (key) => tickets.get(key),
    tickets: () => [...tickets.values()],
    reserve: (ticket) => {
      if (tickets.has(ticket.id) || tickets.size >= capacity) return false;
      tickets.set(ticket.id, ticket);
      return true;
    },
    update: (ticket) => {
      tickets.set(ticket.id, ticket);
    },
    release: (key) => {
      tickets.delete(key);
    },
    terminal,
    report,
    commands,
  });
  return {
    coordinator,
    tickets,
    terminal,
    report,
    commands,
    retire: () => {
      generation++;
    },
  };
}

it("retains accepted pending work without claiming a committed effect", async () => {
  const f = fixture();
  const invoke = vi.fn(async () => status({ kind: "accepted" }));
  const result = await f.coordinator.submit({ group, kind: "rename", invoke });
  expect(result.kind).toBe("pending");
  expect(invoke).toHaveBeenCalledWith("local-owner", account, id);
  expect(f.tickets.get(id)?.status?.committed_events).toEqual([]);
  expect(f.terminal).not.toHaveBeenCalled();
});
it("retains the same minted ID after uncertain IPC and resumes that intent", async () => {
  const f = fixture();
  const invoke = vi
    .fn()
    .mockRejectedValueOnce(new Error("IPC disconnected"))
    .mockResolvedValueOnce(status({ kind: "committed" }));
  expect(
    (await f.coordinator.submit({ group, kind: "rename", invoke })).kind,
  ).toBe("unconfirmed");
  expect(f.tickets.get(id)?.stage).toBe("unconfirmed");
  expect((await f.coordinator.resume(id, invoke)).kind).toBe("committed");
  expect(f.commands.mint).toHaveBeenCalledTimes(1);
  expect(invoke.mock.calls.map((args) => args[2])).toEqual([id, id]);
  expect(f.terminal).toHaveBeenCalledTimes(1);
});
it("rejects before mutation when local tracking capacity is exhausted", async () => {
  const f = fixture(0);
  const invoke = vi.fn(async () => status({ kind: "accepted" }));
  expect(
    (await f.coordinator.submit({ group, kind: "rename", invoke })).kind,
  ).toBe("capacity");
  expect(invoke).not.toHaveBeenCalled();
});
it("does not submit an ID minted for a retired session", async () => {
  const f = fixture();
  const mint = deferred<string>();
  f.commands.mint.mockReturnValueOnce(mint.promise);
  const invoke = vi.fn(async () => status({ kind: "accepted" }));
  const pending = f.coordinator.submit({ group, kind: "rename", invoke });
  f.retire();
  mint.resolve(id);
  expect((await pending).kind).toBe("stale");
  expect(invoke).not.toHaveBeenCalled();
  expect(f.tickets.size).toBe(0);
});
it("clears only definite pre-admission errors", async () => {
  const f = fixture();
  const invoke = vi.fn(async () => {
    throw { kind: "authorization", message: "app locked" };
  });
  expect(
    (await f.coordinator.submit({ group, kind: "rename", invoke })).kind,
  ).toBe("not_admitted");
  expect(f.tickets.size).toBe(0);
});
it("never applies a result for another account or group", async () => {
  const f = fixture();
  const invoke = vi.fn(async () => ({
    ...status({ kind: "committed" }),
    group: "wrong-group",
  }));
  expect(
    (await f.coordinator.submit({ group, kind: "rename", invoke })).kind,
  ).toBe("unconfirmed");
  expect(f.terminal).not.toHaveBeenCalled();
  expect(f.report).toHaveBeenCalledWith("invalid_response");
});
it("keeps cancellation requested separate from cancellation confirmed", async () => {
  const f = fixture();
  await f.coordinator.submit({
    group,
    kind: "rename",
    invoke: async () => status({ kind: "accepted" }),
  });
  expect((await f.coordinator.cancel(id)).kind).toBe("pending");
  expect(f.tickets.get(id)?.status?.phase.kind).toBe("cancellation_requested");
  expect(f.terminal).not.toHaveBeenCalled();
});
it("preserves a committed result against a late cancellation response", async () => {
  const f = fixture();
  await f.coordinator.submit({
    group,
    kind: "rename",
    invoke: async () => status({ kind: "accepted" }),
  });
  const cancel = deferred<AccountGroupOperationStatus>();
  f.commands.cancel.mockReturnValueOnce(cancel.promise);
  const pending = f.coordinator.cancel(id);
  f.commands.status.mockResolvedValueOnce(status({ kind: "committed" }));
  await f.coordinator.refresh();
  cancel.resolve(status({ kind: "cancellation_requested" }));
  expect((await pending).kind).toBe("committed");
  expect(f.tickets.get(id)?.status?.phase.kind).toBe("committed");
  expect(f.terminal).toHaveBeenCalledTimes(1);
});
it("discovers pending work after relaunch without retrieving its body", async () => {
  const f = fixture();
  f.commands.list.mockResolvedValueOnce([
    status({ kind: "waiting_for_executor" }),
  ]);
  await f.coordinator.refresh();
  expect(f.tickets.get(id)?.status?.phase.kind).toBe("waiting_for_executor");
  expect(f.commands.list).toHaveBeenCalledWith("local-owner", account, {
    pendingOnly: true,
    after: undefined,
    limit: 100,
  });
});
it("does not release observations into a replacement session", async () => {
  const f = fixture();
  const page = deferred<AccountGroupOperationStatus[]>();
  f.commands.list.mockReturnValueOnce(page.promise);
  const pending = f.coordinator.refresh();
  f.retire();
  page.resolve([status({ kind: "accepted" })]);
  await pending;
  expect(f.tickets.size).toBe(0);
  expect(f.terminal).not.toHaveBeenCalled();
});

it("rotates status reads beyond the first waiting batch", async () => {
  const f = fixture();
  const ids = Array.from({ length: 9 }, (_, index) =>
    (index + 1).toString(16).padStart(64, "0"),
  );
  for (const key of ids)
    f.tickets.set(key, {
      id: key,
      account_id: account,
      group,
      kind: "rename",
      stage: "unconfirmed",
      observation: 0,
    });
  await f.coordinator.refresh();
  await f.coordinator.refresh();
  expect(new Set(f.commands.status.mock.calls.map((args) => args[2]))).toEqual(
    new Set(ids),
  );
});
it("keeps accepted work tracked when cancellation cannot be confirmed", async () => {
  const f = fixture();
  await f.coordinator.submit({
    group,
    kind: "rename",
    invoke: async () => status({ kind: "accepted" }),
  });
  f.commands.cancel.mockRejectedValueOnce({
    kind: "invalid-input",
    message: "not yet known",
  });
  expect((await f.coordinator.cancel(id)).kind).toBe("unconfirmed");
  expect(f.tickets.get(id)?.status?.phase.kind).toBe("accepted");
  expect(f.terminal).not.toHaveBeenCalled();
});
it("does not turn an already committed effect into failure when presentation refresh throws", async () => {
  const f = fixture();
  f.terminal.mockImplementationOnce(() => {
    throw new Error("refresh failed");
  });
  expect(
    (
      await f.coordinator.submit({
        group,
        kind: "rename",
        invoke: async () => status({ kind: "committed" }),
      })
    ).kind,
  ).toBe("committed");
  expect(f.tickets.get(id)?.status?.phase.kind).toBe("committed");
  expect(f.report).toHaveBeenCalledWith("terminal_effect_failed");
});
it("serializes refresh within a session and releases it after the pending read settles", async () => {
  const f = fixture();
  const page = deferred<AccountGroupOperationStatus[]>();
  f.commands.list.mockReturnValueOnce(page.promise);
  const pending = f.coordinator.refresh();
  await f.coordinator.refresh();
  expect(f.commands.list).toHaveBeenCalledTimes(1);
  page.resolve([]);
  await pending;
  await f.coordinator.refresh();
  expect(f.commands.list).toHaveBeenCalledTimes(2);
});
it("does not let a retired observation lane release the replacement lane", async () => {
  const f = fixture();
  const old = deferred<AccountGroupOperationStatus[]>();
  const current = deferred<AccountGroupOperationStatus[]>();
  f.commands.list
    .mockReturnValueOnce(old.promise)
    .mockReturnValueOnce(current.promise);
  const retired = f.coordinator.refresh();
  f.retire();
  const replacement = f.coordinator.refresh();
  old.resolve([status({ kind: "accepted" })]);
  await retired;
  await f.coordinator.refresh();
  expect(f.commands.list).toHaveBeenCalledTimes(2);
  expect(f.tickets.size).toBe(0);
  current.resolve([status({ kind: "accepted" })]);
  await replacement;
  expect(f.tickets.get(id)?.status?.phase.kind).toBe("accepted");
});

it("compares terminal rejection meaning independently of object property order", async () => {
  const f = fixture();
  await f.coordinator.submit({
    group,
    kind: "rename",
    invoke: async () => status({ kind: "rejected", reason: "not_authorized" }),
  });
  f.commands.list.mockResolvedValueOnce([
    status({ reason: "not_authorized", kind: "rejected" }),
  ]);
  await f.coordinator.refresh();
  expect(f.report).not.toHaveBeenCalledWith("conflicting_terminal");
  expect(f.terminal).toHaveBeenCalledTimes(1);
});

it("preserves earlier admission uncertainty when a retry is refused before admission", async () => {
  const f = fixture();
  const invoke = vi
    .fn()
    .mockRejectedValueOnce(new Error("lost reply"))
    .mockRejectedValueOnce({ kind: "authorization", message: "app locked" });
  expect(
    (await f.coordinator.submit({ group, kind: "rename", invoke })).kind,
  ).toBe("unconfirmed");
  expect((await f.coordinator.resume(id, invoke)).kind).toBe("unconfirmed");
  expect(f.tickets.get(id)?.stage).toBe("unconfirmed");
  expect(f.commands.mint).toHaveBeenCalledTimes(1);
});
it("does not let a competing retry remove an outstanding original invocation", async () => {
  const f = fixture();
  const reply = deferred<AccountGroupOperationStatus>();
  const entered = deferred<void>();
  const invoke = vi.fn(() => {
    entered.resolve();
    return reply.promise;
  });
  const original = f.coordinator.submit({ group, kind: "rename", invoke });
  await entered.promise;
  const retry = vi.fn(async () => {
    throw { kind: "authorization", message: "app locked" };
  });
  expect((await f.coordinator.resume(id, retry)).kind).toBe("unconfirmed");
  expect(retry).not.toHaveBeenCalled();
  reply.resolve(status({ kind: "committed" }));
  expect((await original).kind).toBe("committed");
  expect(f.tickets.get(id)?.status?.phase.kind).toBe("committed");
});

it("links reserved message ownership before the first mutation and stops if linking fails", async () => {
  const f = fixture();
  let linked: string | undefined;
  const invoke = vi.fn(async () => {
    expect(linked).toBe(id);
    return status({ kind: "accepted" });
  });
  await f.coordinator.submit({
    group,
    kind: "rename",
    invoke,
    onReserved: (operationId) => {
      expect(f.tickets.has(operationId)).toBe(true);
      linked = operationId;
      return true;
    },
  });
  expect(invoke).toHaveBeenCalledTimes(1);
  const blocked = fixture();
  expect(
    (
      await blocked.coordinator.submit({
        group,
        kind: "rename",
        invoke,
        onReserved: () => false,
      })
    ).kind,
  ).toBe("stale");
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(blocked.tickets.size).toBe(0);
});

it("reissues cancellation once a long submission admits and again during refresh", async () => {
  const f = fixture();
  const pending = deferred<AccountGroupOperationStatus>();
  const submission = f.coordinator.submit({
    group,
    kind: "rename",
    invoke: () => pending.promise,
  });
  await Promise.resolve();
  f.commands.cancel.mockRejectedValueOnce(new Error("not admitted yet"));
  expect((await f.coordinator.cancel(id)).kind).toBe("unconfirmed");
  pending.resolve(status({ kind: "accepted" }));
  await submission;
  expect(f.commands.cancel).toHaveBeenCalledTimes(2);
  expect(f.tickets.get(id)?.status?.phase.kind).toBe("cancellation_requested");
  await f.coordinator.refresh();
  expect(f.commands.cancel).toHaveBeenCalledTimes(3);
  expect(f.commands.cancel).toHaveBeenLastCalledWith(
    "local-owner",
    account,
    id,
  );
  expect(f.terminal).not.toHaveBeenCalled();
});
