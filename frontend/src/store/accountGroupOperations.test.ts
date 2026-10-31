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
it("bounds ID allocation without submitting an ID returned after the read deadline", async () => {
  vi.useFakeTimers();
  const f = fixture();
  const mint = deferred<string>();
  const invoke = vi.fn(async () => status({ kind: "accepted" }));
  f.commands.mint.mockReturnValueOnce(mint.promise);
  const completed = vi.fn();
  const submitting = f.coordinator
    .submit({ group, kind: "rename", invoke })
    .then(completed);
  try {
    await vi.advanceTimersByTimeAsync(10_001);
    expect(completed).toHaveBeenCalledWith(
      expect.objectContaining({ kind: "not_admitted" }),
    );
    mint.resolve(id);
    await submitting;
    expect(invoke).not.toHaveBeenCalled();
    expect(f.tickets.size).toBe(0);
  } finally {
    mint.resolve(id);
    await submitting;
    vi.useRealTimers();
  }
});

it.each(["committed", "rejected"] as const)(
  "returns an observed %s result while the original invocation remains outstanding",
  async (phase) => {
    vi.useFakeTimers();
    const f = fixture();
    const reply = deferred<AccountGroupOperationStatus>();
    const invoke = vi.fn(() => reply.promise);
    const completed = vi.fn();
    const submitting = f.coordinator
      .submit({ group, kind: "rename", invoke })
      .then(completed);
    try {
      await vi.advanceTimersByTimeAsync(0);
      f.commands.status.mockResolvedValue(
        status(
          phase === "committed"
            ? { kind: "committed" }
            : { kind: "rejected", reason: "not_authorized" },
        ),
      );
      await f.coordinator.refresh();
      await vi.advanceTimersByTimeAsync(10_001);
      expect(completed).toHaveBeenCalledWith(
        expect.objectContaining({ kind: phase, id }),
      );
      expect(f.coordinator.isInvoking(account, id)).toBe(true);
      expect((await f.coordinator.resume(id, invoke)).kind).toBe(phase);
      expect(invoke).toHaveBeenCalledOnce();
    } finally {
      reply.resolve(status({ kind: "accepted" }));
      await submitting;
      await vi.advanceTimersByTimeAsync(0);
      vi.useRealTimers();
    }
    expect(f.coordinator.isInvoking(account, id)).toBe(false);
    expect(f.tickets.get(id)?.status?.phase.kind).toBe(phase);
  },
);

it("exposes a stalled invocation as unconfirmed without permitting a second native submission", async () => {
  vi.useFakeTimers();
  const f = fixture();
  const reply = deferred<AccountGroupOperationStatus>();
  const invoke = vi.fn(() => reply.promise);
  const completed = vi.fn();
  const submitting = f.coordinator
    .submit({ group, kind: "rename", invoke })
    .then(completed);
  try {
    await vi.advanceTimersByTimeAsync(10_001);
    expect(completed).toHaveBeenCalledWith({ kind: "unconfirmed", id });
    expect(f.tickets.get(id)?.stage).toBe("unconfirmed");
    expect(f.coordinator.isInvoking(account, id)).toBe(true);
    expect((await f.coordinator.resume(id, invoke)).kind).toBe("unconfirmed");
    expect(invoke).toHaveBeenCalledOnce();
  } finally {
    reply.resolve(status({ kind: "committed" }));
    await submitting;
    await vi.advanceTimersByTimeAsync(0);
    vi.useRealTimers();
  }
  expect(f.coordinator.isInvoking(account, id)).toBe(false);
  expect(f.tickets.get(id)?.status?.phase.kind).toBe("committed");
});
it("publishes a definite late admission failure after the observer has returned", async () => {
  vi.useFakeTimers();
  const f = fixture();
  let reject!: (error: unknown) => void;
  const reply = new Promise<AccountGroupOperationStatus>((_resolve, fail) => {
    reject = fail;
  });
  const notAdmitted = vi.fn();
  const submitted = f.coordinator.submit({
    group,
    kind: "rename",
    invoke: () => reply,
    onNotAdmitted: notAdmitted,
  });
  const error = { kind: "authorization", message: "app locked" };
  try {
    await vi.advanceTimersByTimeAsync(10_001);
    expect(await submitted).toEqual({ kind: "unconfirmed", id });
    reject(error);
    await vi.advanceTimersByTimeAsync(0);
    expect(f.tickets.get(id)?.admissionFailure).toEqual({ error });
    expect(notAdmitted).toHaveBeenCalledWith(error);
    const retry = vi.fn();
    expect(await f.coordinator.resume(id, retry)).toEqual({
      kind: "not_admitted",
      id,
      error,
    });
    expect(retry).not.toHaveBeenCalled();
    await f.coordinator.refresh();
    expect(f.commands.status).not.toHaveBeenCalled();
    expect(f.coordinator.isInvoking(account, id)).toBe(false);
  } finally {
    reject(error);
    await vi.advanceTimersByTimeAsync(0);
    vi.useRealTimers();
  }
});
it.each(["committed", "rejected"] as const)(
  "applies a late %s result after observation timeout only to its original session",
  async (phase) => {
    vi.useFakeTimers();
    const f = fixture();
    const original = deferred<AccountGroupOperationStatus>();
    const result =
      phase === "committed"
        ? status({ kind: "committed" })
        : status({ kind: "rejected", reason: "not_authorized" });
    const submitted = f.coordinator.submit({
      group,
      kind: "rename",
      invoke: () => original.promise,
    });
    try {
      await vi.advanceTimersByTimeAsync(10_001);
      expect(await submitted).toEqual({ kind: "unconfirmed", id });
      original.resolve(result);
      await vi.advanceTimersByTimeAsync(0);
      expect(f.tickets.get(id)?.status).toEqual(result);
      expect(f.terminal).toHaveBeenCalledWith(result);

      const retired = fixture();
      const late = deferred<AccountGroupOperationStatus>();
      const old = retired.coordinator.submit({
        group,
        kind: "rename",
        invoke: () => late.promise,
      });
      await vi.advanceTimersByTimeAsync(10_001);
      expect(await old).toEqual({ kind: "unconfirmed", id });
      retired.retire();
      retired.tickets.clear();
      late.resolve(result);
      await vi.advanceTimersByTimeAsync(0);
      expect(retired.tickets.size).toBe(0);
      expect(retired.terminal).not.toHaveBeenCalled();
      expect(retired.coordinator.isInvoking(account, id)).toBe(false);
    } finally {
      original.resolve(result);
      await vi.advanceTimersByTimeAsync(0);
      vi.useRealTimers();
    }
  },
);
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
it("rebinds an unresolved Leave without minting or submitting another operation", async () => {
  const f = fixture();
  const original: GroupOperationTicket = {
    id,
    account_id: account,
    group,
    kind: "leave_account",
    stage: "observed",
    observation: 1,
    cancelRequested: true,
    status: {
      ...status({ kind: "cancellation_requested" }),
      kind: "leave_account",
    },
  };
  f.tickets.set(id, original);
  const invoke = vi.fn();
  const result = await f.coordinator.submit({
    group,
    kind: "leave_account",
    invoke,
  });
  expect(result).toEqual({ kind: "pending", id, status: original.status });
  expect(f.commands.mint).not.toHaveBeenCalled();
  expect(invoke).not.toHaveBeenCalled();
  expect(f.tickets.get(id)).toEqual(original);
});
it("retains the original unknown Leave ID when admission cannot be confirmed", async () => {
  const f = fixture();
  f.tickets.set(id, {
    id,
    account_id: account,
    group,
    kind: "leave_account",
    stage: "unconfirmed",
    observation: 0,
  });
  const invoke = vi.fn();
  expect(
    await f.coordinator.submit({ group, kind: "leave_account", invoke }),
  ).toEqual({ kind: "unconfirmed", id });
  expect(f.commands.mint).not.toHaveBeenCalled();
  expect(invoke).not.toHaveBeenCalled();
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
it("does not leave an unsupported legacy Leave permanently unconfirmed", async () => {
  const f = fixture();
  const error = {
    kind: "invalid-input",
    message:
      "group account-membership format update required for this operation",
  };
  const invoke = vi.fn(async () => {
    throw error;
  });
  expect(
    await f.coordinator.submit({ group, kind: "leave_account", invoke }),
  ).toEqual({ kind: "not_admitted", id, error });
  expect(f.tickets.size).toBe(0);
  await f.coordinator.refresh();
  expect(f.commands.status).not.toHaveBeenCalled();
  expect(f.commands.cancel).not.toHaveBeenCalled();
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
it("keeps observing status after a cancellation RPC outlives the observation deadline", async () => {
  vi.useFakeTimers();
  const f = fixture();
  const cancellation = deferred<AccountGroupOperationStatus>();
  try {
    await f.coordinator.submit({
      group,
      kind: "rename",
      invoke: async () => status({ kind: "accepted" }),
    });
    f.tickets.get(id)!.cancelRequested = true;
    f.commands.cancel.mockReturnValueOnce(cancellation.promise);
    const refreshing = f.coordinator.refresh();
    await vi.advanceTimersByTimeAsync(10_000);
    // A stalled cancellation must not own the shared observation lane forever.
    f.commands.status.mockResolvedValueOnce(status({ kind: "committed" }));
    await f.coordinator.refresh();
    expect(f.commands.list).toHaveBeenCalledTimes(2);
    expect(f.tickets.get(id)?.status?.phase.kind).toBe("committed");
    expect(f.commands.cancel).toHaveBeenCalledTimes(1);
    cancellation.resolve(status({ kind: "cancellation_requested" }));
    await refreshing;
    expect(f.tickets.get(id)?.status?.phase.kind).toBe("committed");
    expect(f.terminal).toHaveBeenCalledTimes(1);
  } finally {
    cancellation.resolve(status({ kind: "cancellation_requested" }));
    await Promise.resolve();
    vi.useRealTimers();
  }
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
  const entered = deferred<void>();
  const submission = f.coordinator.submit({
    group,
    kind: "rename",
    invoke: () => {
      entered.resolve();
      return pending.promise;
    },
  });
  await entered.promise;
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

it("keeps a committed leave pending until its exact private cleanup completes", async () => {
  const f = fixture();
  const pending = {
    ...status({ kind: "committed" }),
    kind: "leave_account" as const,
    delete_history: true,
    history_cleanup_complete: false,
  };
  const invoke = vi.fn(async () => pending);
  expect(
    (
      await f.coordinator.submit({
        group,
        kind: "leave_account",
        deleteHistory: true,
        invoke,
      })
    ).kind,
  ).toBe("pending");
  expect(f.terminal).not.toHaveBeenCalled();
  await f.coordinator.cancel(id);
  expect(f.commands.cancel).not.toHaveBeenCalled();
  f.commands.status.mockResolvedValue({
    ...pending,
    history_cleanup_complete: true,
  });
  await f.coordinator.refresh();
  expect(f.commands.status).toHaveBeenCalledWith("local-owner", account, id);
  expect(f.terminal).toHaveBeenCalledExactlyOnceWith({
    ...pending,
    history_cleanup_complete: true,
  });
  expect(invoke).toHaveBeenCalledOnce();
});

it("recovers and polls an original committed leave whose private cleanup is unknown", async () => {
  const f = fixture();
  const pending = {
    ...status({ kind: "committed" }),
    kind: "leave_account" as const,
    delete_history: true,
  };
  f.commands.list.mockResolvedValueOnce([pending]);
  await f.coordinator.refresh();
  expect(f.tickets.get(id)?.status).toEqual(pending);
  expect(f.terminal).not.toHaveBeenCalled();
  f.commands.status.mockResolvedValue({
    ...pending,
    history_cleanup_complete: true,
  });
  await f.coordinator.refresh();
  expect(f.commands.mint).not.toHaveBeenCalled();
  expect(f.commands.status).toHaveBeenCalledWith("local-owner", account, id);
  expect(f.terminal).toHaveBeenCalledOnce();
});

it("preserves a requested cleanup requirement when host observations omit its metadata", async () => {
  const f = fixture();
  const committed = {
    ...status({ kind: "committed" }),
    kind: "leave_account" as const,
  };
  const invoke = vi.fn(async () => committed);
  expect(
    (
      await f.coordinator.submit({
        group,
        kind: "leave_account",
        deleteHistory: true,
        invoke,
      })
    ).kind,
  ).toBe("pending");
  f.commands.status.mockResolvedValue(committed);
  await f.coordinator.refresh();
  expect(f.terminal).not.toHaveBeenCalled();
  expect(f.tickets.get(id)?.status?.delete_history).toBe(true);
  f.commands.status.mockResolvedValue({
    ...committed,
    history_cleanup_complete: true,
  });
  await f.coordinator.refresh();
  expect(f.terminal).toHaveBeenCalledOnce();
});

it("adopts delayed private cleanup metadata without changing the committed Leave", async () => {
  const f = fixture();
  const committed = {
    ...status({ kind: "committed" }),
    kind: "leave_account" as const,
  };
  await f.coordinator.submit({
    group,
    kind: "leave_account",
    invoke: async () => committed,
  });
  const pending = {
    ...committed,
    delete_history: true,
    history_cleanup_complete: false,
  };
  f.commands.list.mockResolvedValueOnce([pending]);
  await f.coordinator.refresh();
  expect(f.tickets.get(id)?.status).toEqual(pending);
  expect((await f.coordinator.cancel(id)).kind).toBe("pending");
  expect(f.commands.cancel).not.toHaveBeenCalled();
  const complete = { ...pending, history_cleanup_complete: true };
  f.commands.status.mockResolvedValue(complete);
  await f.coordinator.refresh();
  expect(f.tickets.get(id)?.status).toEqual(complete);
  expect(f.terminal).toHaveBeenLastCalledWith(complete);
  await f.coordinator.refresh();
  expect(f.terminal).toHaveBeenCalledTimes(2);
});

it("cannot turn an explicitly kept history into a later deletion", async () => {
  const f = fixture();
  const committed = {
    ...status({ kind: "committed" }),
    kind: "leave_account" as const,
  };
  await f.coordinator.submit({
    group,
    kind: "leave_account",
    deleteHistory: false,
    invoke: async () => committed,
  });
  f.commands.list.mockResolvedValueOnce([
    {
      ...committed,
      delete_history: true,
      history_cleanup_complete: true,
    },
  ]);
  await f.coordinator.refresh();
  expect(f.tickets.get(id)?.status?.delete_history).toBe(false);
  expect(f.report).toHaveBeenCalledWith("conflicting_terminal");
  expect(f.terminal).toHaveBeenCalledOnce();
});
