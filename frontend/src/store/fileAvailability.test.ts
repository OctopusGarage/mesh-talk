import { beforeEach, expect, it, vi } from "vitest";
import { chat } from "@/lib/api";
import {
  refreshFileAvailability,
  resetFileAvailability,
  invalidateFileAvailability,
  useFileAvailability,
} from "./fileAvailability";

vi.mock("@/lib/api", () => ({ chat: { fileStatuses: vi.fn() } }));

beforeEach(() => {
  vi.resetAllMocks();
  resetFileAvailability();
});

it("keeps received files unavailable until the node confirms complete bytes", async () => {
  vi.mocked(chat.fileStatuses)
    .mockResolvedValueOnce([
      { file_conv: "a", done: 2, total: 8, ready: false },
    ])
    .mockResolvedValueOnce([
      { file_conv: "a", done: 8, total: 8, ready: true },
    ]);

  await refreshFileAvailability(["a"]);
  expect(useFileAvailability.getState().statuses.a).toEqual({
    file_conv: "a",
    done: 2,
    total: 8,
    ready: false,
  });
  await refreshFileAvailability(["a"]);
  expect(useFileAvailability.getState().statuses.a.ready).toBe(true);
});

it("discards a late status reply after the runtime identity changes", async () => {
  let finish!: (
    value: { file_conv: string; done: number; total: number; ready: boolean }[],
  ) => void;
  vi.mocked(chat.fileStatuses).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = refreshFileAvailability(["old-account-file"]);
  resetFileAvailability();
  finish([{ file_conv: "old-account-file", done: 1, total: 1, ready: true }]);
  await pending;
  expect(useFileAvailability.getState().statuses).toEqual({});
});

it("discards a late ready reply after the saved path is evicted", async () => {
  let finish!: (
    value: { file_conv: string; done: number; total: number; ready: boolean }[],
  ) => void;
  vi.mocked(chat.fileStatuses).mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = refreshFileAvailability(["evicted-file"]);
  invalidateFileAvailability(["evicted-file"]);
  finish([{ file_conv: "evicted-file", done: 1, total: 1, ready: true }]);
  await pending;
  expect(
    useFileAvailability.getState().statuses["evicted-file"],
  ).toBeUndefined();
});
