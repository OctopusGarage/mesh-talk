// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import type { GroupOperationOutcome } from "@/store/accountGroupOperations";
import type { GroupOperationTicket } from "@/store/accountGroupOperations";
const { state, auth } = vi.hoisted(() => ({
  state: {
    myAccountId: "account" as string | undefined,
    runEpoch: 1,
    identityEpoch: 1,
    groupOperations: {} as Record<string, GroupOperationTicket>,
  },
  auth: { user: { id: "local" }, generation: 1 },
}));
vi.mock("@/store/chat", () => ({
  useChat: (select: (s: typeof state) => unknown) => select(state),
}));
vi.mock("@/store/auth", () => ({
  useAuth: (select: (s: typeof auth) => unknown) => select(auth),
}));
import { useGroupControlOperation } from "./useGroupControlOperation";
(
  globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
).IS_REACT_ACT_ENVIRONMENT = true;
function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
async function mount() {
  const root = createRoot(document.createElement("div"));
  let operation!: ReturnType<typeof useGroupControlOperation>;
  function Harness() {
    operation = useGroupControlOperation("group");
    return null;
  }
  const render = () => act(async () => root.render(createElement(Harness)));
  await render();
  return {
    get: () => operation,
    render,
    stop: () => act(async () => root.unmount()),
  };
}
it("admits one click even before React renders busy state", async () => {
  const h = await mount();
  const waiting = deferred<GroupOperationOutcome>();
  const invoke = vi.fn(() => waiting.promise);
  try {
    await act(async () => {
      void h.get().run(invoke);
      void h.get().run(invoke);
    });
    expect(invoke).toHaveBeenCalledOnce();
    await act(async () =>
      waiting.resolve({ kind: "unconfirmed", id: "original" }),
    );
    expect(h.get().id).toBe("original");
    expect(h.get().busy).toBe(true);
  } finally {
    await h.stop();
  }
});
it("does not run completion after desktop session changes", async () => {
  const h = await mount();
  const waiting = deferred<GroupOperationOutcome>();
  const done = vi.fn();
  try {
    await act(async () => {
      void h.get().run(() => waiting.promise, done);
    });
    auth.generation++;
    await h.render();
    await act(async () =>
      waiting.resolve({ kind: "committed", id: "op" } as GroupOperationOutcome),
    );
    expect(done).not.toHaveBeenCalled();
    expect(h.get().outcome).toBeUndefined();
  } finally {
    await h.stop();
  }
});
it("pending and cancellation-requested never call completion, committed ticket does", async () => {
  const h = await mount();
  const done = vi.fn();
  const status = {
    id: "original",
    account_id: "account",
    group: "group",
    kind: "rename" as const,
    phase: { kind: "accepted" as const },
    executions: [],
    primary_event: null,
    committed_events: [],
  };
  try {
    await act(async () => {
      await h
        .get()
        .run(async () => ({ kind: "pending", id: "original", status }), done);
    });
    expect(done).not.toHaveBeenCalled();
    state.groupOperations.original = {
      ...status,
      stage: "observed",
      observation: 1,
      status: { ...status, phase: { kind: "cancellation_requested" } },
      cancelRequested: true,
    };
    await h.render();
    expect(h.get().busy).toBe(true);
    expect(done).not.toHaveBeenCalled();
    state.groupOperations.original.status!.phase = { kind: "committed" };
    await h.render();
    expect(done).toHaveBeenCalledOnce();
    await h.render();
    expect(done).toHaveBeenCalledOnce();
  } finally {
    state.groupOperations = {};
    await h.stop();
  }
});

it("renders before account initialization without a ticket or operation ID", async () => {
  const previous = state.myAccountId;
  state.myAccountId = undefined;
  let h: Awaited<ReturnType<typeof mount>> | undefined;
  try {
    h = await mount();
    expect(h.get().id).toBeUndefined();
    expect(h.get().outcome).toBeUndefined();
    expect(h.get().busy).toBe(false);
  } finally {
    state.myAccountId = previous;
    if (h) await h.stop();
  }
});
