import { beforeEach, expect, it, vi } from "vitest";
import { chat } from "@/lib/api";
import {
  refreshFileAvailability,
  resetFileAvailability,
  invalidateFileAvailability,
  useFileAvailability,
  markUnavailableFileError,
} from "./fileAvailability";

vi.mock("@/lib/api", () => ({ chat: { fileStatuses: vi.fn() } }));

it("recognizes a native tagged error when a file expires during save", () => {
  expect(
    markUnavailableFileError("expired", {
      kind: "internal",
      message: "file error: unknown file",
    }),
  ).toBe(true);
  expect(useFileAvailability.getState().statuses.expired.unavailable).toBe(
    true,
  );
  expect(
    markUnavailableFileError("writable", { kind: "io", message: "disk full" }),
  ).toBe(false);
});

it("keeps an incomplete transfer pending instead of permanently unavailable", () => {
  useFileAvailability
    .getState()
    .merge([{ file_conv: "resuming", done: 4, total: 4, ready: true }]);
  expect(
    markUnavailableFileError("resuming", {
      kind: "internal",
      message: "file incomplete: 2/4 chunks",
    }),
  ).toBe(true);
  const status = useFileAvailability.getState().statuses.resuming;
  expect(status.ready).toBe(false);
  expect(status.unavailable).not.toBe(true);
  expect(status.total).toBeGreaterThan(0);
});

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

it("revalidates watched ready files when their manifest expires", async () => {
  vi.useFakeTimers();
  vi.mocked(chat.fileStatuses)
    .mockResolvedValueOnce([
      { file_conv: "expiring", done: 1, total: 1, ready: true },
    ])
    .mockResolvedValue([
      { file_conv: "expiring", done: 0, total: 0, ready: false },
    ]);
  const { watchFileAvailability } = await import("./fileAvailability");
  const stop = watchFileAvailability("expiring");
  await vi.advanceTimersByTimeAsync(0);
  expect(useFileAvailability.getState().statuses.expiring.ready).toBe(true);
  await vi.advanceTimersByTimeAsync(750);
  expect(useFileAvailability.getState().statuses.expiring.ready).toBe(false);
  expect(useFileAvailability.getState().statuses.expiring.unavailable).toBe(
    true,
  );
  stop();
  vi.useRealTimers();
});
