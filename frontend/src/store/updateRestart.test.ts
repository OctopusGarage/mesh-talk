import { beforeEach, expect, it, vi } from "vitest";
const state = vi.hoisted(() => ({
  phase: "idle",
  transfers: {} as Record<string, { delivered?: boolean }>,
  save: vi.fn(async () => {}),
}));
vi.mock("./calls", () => ({
  useCalls: { getState: () => ({ phase: state.phase }) },
}));
vi.mock("./transfers", () => ({
  useTransfers: { getState: () => ({ transfers: state.transfers }) },
}));
vi.mock("@/lib/api/drafts", () => ({
  draftsApi: {
    load: async () => ({ session: "session", drafts: {} }),
    save: state.save,
  },
}));
import { useDrafts } from "./drafts";
import { prepareUpdateRestart } from "./updateRestart";
beforeEach(async () => {
  state.phase = "idle";
  state.transfers = {};
  state.save.mockReset().mockResolvedValue(undefined);
  await useDrafts.getState().start("alice", 1);
});
it("defers calls and unfinished transfers but allows completed delivery rows", async () => {
  state.phase = "connected";
  await expect(prepareUpdateRestart()).rejects.toThrow();
  expect(state.save).not.toHaveBeenCalled();
  state.phase = "idle";
  state.transfers = { file: {} };
  await expect(prepareUpdateRestart()).rejects.toThrow();
  state.transfers = { file: { delivered: true } };
  const handoff = await prepareUpdateRestart();
  expect(handoff).toBeTypeOf("function");
});
it("flushes drafts and rechecks work that started during persistence", async () => {
  useDrafts.getState().edit("alice", 1, "account:bob", "draft");
  state.save.mockImplementationOnce(async () => {
    state.phase = "incoming";
  });
  await expect(prepareUpdateRestart()).rejects.toThrow();
  state.phase = "idle";
  useDrafts.getState().edit("alice", 1, "account:bob", "new");
  state.save.mockRejectedValueOnce(new Error("disk full"));
  await expect(prepareUpdateRestart()).rejects.toThrow();
});
it("defers installation when drafts change while persistence is pending", async () => {
  useDrafts.getState().edit("alice", 1, "account:bob", "first");
  let finish!: () => void;
  state.save.mockImplementationOnce(
    () =>
      new Promise<void>((yes) => {
        finish = yes;
      }),
  );
  const prepared = prepareUpdateRestart();
  await Promise.resolve();
  useDrafts.getState().edit("alice", 1, "account:bob", "second");
  finish();
  await expect(prepared).rejects.toThrow();
  expect(useDrafts.getState().texts["account:bob"]).toBe("second");
  const handoff = await prepareUpdateRestart();
  handoff();
  expect(state.save).toHaveBeenLastCalledWith(
    "alice",
    "session",
    { "account:bob": "second" },
    {},
  );
});
it("rechecks drafts, session and active work synchronously at installer handoff", async () => {
  const handoff = await prepareUpdateRestart();
  useDrafts.getState().edit("alice", 1, "account:bob", "late edit");
  expect(handoff).toThrow();
  const next = await prepareUpdateRestart();
  state.phase = "incoming";
  expect(next).toThrow();
  state.phase = "idle";
  await useDrafts.getState().start("bob", 2);
  expect(next).toThrow();
});
