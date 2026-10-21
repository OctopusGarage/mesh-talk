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
  await vi.advanceTimersByTimeAsync(30000);
  expect(useFileAvailability.getState().statuses.expiring.ready).toBe(false);
  expect(useFileAvailability.getState().statuses.expiring.unavailable).toBe(
    true,
  );
  stop();
  vi.useRealTimers();
});

it("push updates watched files while unchanged snapshots preserve references", async () => {
  vi.useFakeTimers();
  const { notifySyncInvalidation, setPushStateSupport } =
    await import("@/lib/syncInvalidation");
  const { watchFileAvailability } = await import("./fileAvailability");
  const status = { file_conv: "push", done: 1, total: 2, ready: false };
  vi.mocked(chat.fileStatuses).mockResolvedValue([status]);
  setPushStateSupport("host");
  const stop = watchFileAvailability("push");
  try {
    await vi.advanceTimersByTimeAsync(0);
    const initial = useFileAvailability.getState().statuses;
    await vi.advanceTimersByTimeAsync(2000);
    expect(chat.fileStatuses).toHaveBeenCalledTimes(1);
    notifySyncInvalidation({ owner: "host", kind: "files" });
    await vi.advanceTimersByTimeAsync(0);
    expect(chat.fileStatuses).toHaveBeenCalledTimes(2);
    expect(useFileAvailability.getState().statuses).toBe(initial);
    vi.mocked(chat.fileStatuses).mockResolvedValue([
      { ...status, done: 2, ready: true },
    ]);
    notifySyncInvalidation({ owner: "host", kind: "files" });
    await vi.advanceTimersByTimeAsync(0);
    expect(useFileAvailability.getState().statuses.push.ready).toBe(true);
  } finally {
    stop();
    setPushStateSupport(null);
    vi.useRealTimers();
  }
});

it("coalesces pushes and newly watched files during an active read", async () => {
  vi.useFakeTimers();
  const { notifySyncInvalidation, setPushStateSupport } =
    await import("@/lib/syncInvalidation");
  const { watchFileAvailability } = await import("./fileAvailability");
  let finish!: (statuses: []) => void;
  vi.mocked(chat.fileStatuses)
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue([
      { file_conv: "second", done: 1, total: 1, ready: true },
    ]);
  setPushStateSupport("host");
  const stopFirst = watchFileAvailability("first");
  const stopSecond = watchFileAvailability("second");
  try {
    for (let i = 0; i < 10; i++)
      notifySyncInvalidation({ owner: "host", kind: "files" });
    expect(chat.fileStatuses).toHaveBeenCalledTimes(1);
    finish([]);
    await vi.advanceTimersByTimeAsync(0);
    expect(chat.fileStatuses).toHaveBeenCalledTimes(2);
    expect(chat.fileStatuses).toHaveBeenLastCalledWith(["first", "second"]);
    expect(useFileAvailability.getState().statuses.second.ready).toBe(true);
  } finally {
    stopFirst();
    stopSecond();
    setPushStateSupport(null);
    vi.useRealTimers();
  }
});

it("restarts watched reads after a generation reset during an active read", async () => {
  vi.useFakeTimers();
  const { setPushStateSupport } = await import("@/lib/syncInvalidation");
  const { watchFileAvailability } = await import("./fileAvailability");
  let finish!: (statuses: []) => void;
  vi.mocked(chat.fileStatuses)
    .mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    )
    .mockResolvedValue([
      { file_conv: "current", done: 1, total: 1, ready: true },
    ]);
  setPushStateSupport("host");
  const stop = watchFileAvailability("current");
  try {
    resetFileAvailability();
    finish([]);
    await vi.advanceTimersByTimeAsync(0);
    expect(chat.fileStatuses).toHaveBeenCalledTimes(2);
    expect(useFileAvailability.getState().statuses.current.ready).toBe(true);
  } finally {
    stop();
    setPushStateSupport(null);
    vi.useRealTimers();
  }
});
