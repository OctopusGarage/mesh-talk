import { beforeEach, expect, it, vi } from "vitest";
const state = vi.hoisted(() => ({
  phase: "idle",
  transfers: {} as Record<string, { delivered?: boolean }>,
  error: null as string | null,
  flush: vi.fn(async () => {}),
}));
vi.mock("./calls", () => ({
  useCalls: { getState: () => ({ phase: state.phase }) },
}));
vi.mock("./transfers", () => ({
  useTransfers: { getState: () => ({ transfers: state.transfers }) },
}));
vi.mock("./drafts", () => ({
  useDrafts: { getState: () => ({ flush: state.flush, error: state.error }) },
}));
import { prepareUpdateRestart } from "./updateRestart";
beforeEach(() => {
  state.phase = "idle";
  state.transfers = {};
  state.error = null;
  vi.clearAllMocks();
});
it("defers calls and unfinished transfers but allows completed delivery rows", async () => {
  state.phase = "connected";
  await expect(prepareUpdateRestart()).rejects.toThrow();
  expect(state.flush).not.toHaveBeenCalled();
  state.phase = "idle";
  state.transfers = { file: {} };
  await expect(prepareUpdateRestart()).rejects.toThrow();
  state.transfers = { file: { delivered: true } };
  await expect(prepareUpdateRestart()).resolves.toBeUndefined();
});
it("flushes drafts and rechecks work that started during persistence", async () => {
  state.flush.mockImplementationOnce(async () => {
    state.phase = "incoming";
  });
  await expect(prepareUpdateRestart()).rejects.toThrow();
  state.phase = "idle";
  state.flush.mockImplementationOnce(async () => {
    state.error = "save";
  });
  await expect(prepareUpdateRestart()).rejects.toThrow();
});
