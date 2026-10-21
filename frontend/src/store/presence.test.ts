import { afterEach, beforeEach, expect, it, vi } from "vitest";

const { getPresence } = vi.hoisted(() => ({ getPresence: vi.fn() }));
vi.mock("@/lib/api", () => ({ presence: { get: getPresence } }));

import { useAuth } from "./auth";
import { agePresence, presenceStatus, usePresence } from "./presence";

beforeEach(() => {
  vi.useFakeTimers();
  getPresence.mockReset();
  usePresence.setState({ map: {} });
  useAuth.setState({
    user: { id: "alice", username: "alice", display_name: "Alice" },
    generation: 1,
  });
});
afterEach(() => vi.useRealTimers());

it("refreshes online status within two seconds of discovery", async () => {
  getPresence
    .mockResolvedValueOnce({ bob: { online: false, last_seen_secs: 31 } })
    .mockResolvedValueOnce({ bob: { online: true, last_seen_secs: 0 } });
  const stop = usePresence.getState().start();
  try {
    await vi.waitFor(() =>
      expect(usePresence.getState().map.bob).toBeDefined(),
    );
    await vi.advanceTimersByTimeAsync(2000);
    expect(usePresence.getState().map.bob.online).toBe(true);
  } finally {
    stop();
  }
});

it("keeps unchanged records stable and retains missing peers only as offline history", async () => {
  getPresence
    .mockResolvedValueOnce({ a: { online: true, last_seen_secs: 0 } })
    .mockResolvedValueOnce({
      a: { online: true, last_seen_secs: 0 },
      b: { online: false, last_seen_secs: 10 },
    })
    .mockResolvedValueOnce({ a: { online: true, last_seen_secs: 0 } });
  const stop = usePresence.getState().start();
  try {
    await vi.waitFor(() => expect(usePresence.getState().map.a).toBeDefined());
    const first = usePresence.getState().map.a;
    await vi.advanceTimersByTimeAsync(2000);
    expect(usePresence.getState().map.a).toBe(first);
    expect(usePresence.getState().map.b).toBeDefined();
    await vi.advanceTimersByTimeAsync(2000);
    expect(usePresence.getState().map.a).toBe(first);
    expect(usePresence.getState().map.b).toMatchObject({
      online: false,
      last_seen_secs: 12,
      devices: [],
    });
  } finally {
    stop();
  }
});

it("ignores a late poll after the account changes", async () => {
  let finish!: (snapshot: Record<string, unknown>) => void;
  getPresence.mockReturnValue(new Promise((resolve) => (finish = resolve)));
  const stop = usePresence.getState().start();
  useAuth.setState({ generation: 2 });
  finish({ a: { online: true, last_seen_secs: 0 } });
  await Promise.resolve();
  expect(usePresence.getState().map).toEqual({});
  stop();
});

it("does not let an older poll overwrite a newer presence snapshot", async () => {
  const pending: Array<(snapshot: Record<string, unknown>) => void> = [];
  getPresence.mockImplementation(
    () => new Promise((resolve) => pending.push(resolve)),
  );
  const stop = usePresence.getState().start();
  try {
    expect(pending).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(2000);
    expect(pending).toHaveLength(2);
    pending[1]({ bob: { online: true, last_seen_secs: 0 } });
    await Promise.resolve();
    pending[0]({ bob: { online: false, last_seen_secs: 60 } });
    await Promise.resolve();
    expect(usePresence.getState().map.bob.online).toBe(true);
  } finally {
    stop();
  }
});

it("keeps recent history secondary to the offline primary state", () => {
  expect(presenceStatus({ online: true, last_seen_secs: null })).toBe("online");
  expect(presenceStatus({ online: false, last_seen_secs: 299 })).toBe(
    "offline",
  );
  expect(presenceStatus({ online: false, last_seen_secs: 300 })).toBe(
    "offline",
  );
  expect(presenceStatus({ online: false, last_seen_secs: null })).toBe(
    "offline",
  );
});

it("expires each source during failed refreshes, updates group counts, and recovers normally", async () => {
  const snapshot = {
    bob: {
      online: true,
      last_seen_secs: 0,
      devices: [
        {
          user_id: "b",
          evidence: [
            { source: "directory", last_seen_secs: 40, expires_in_secs: 4 },
            { source: "routed", last_seen_secs: 0, expires_in_secs: 8 },
          ],
        },
      ],
    },
    group: {
      online: false,
      last_seen_secs: null,
      member_count: 1,
      online_count: 1,
      member_keys: ["bob"],
    },
  };
  getPresence
    .mockResolvedValueOnce(snapshot)
    .mockRejectedValue(new Error("IPC offline"));
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(4000);
    expect(usePresence.getState().unavailable).toBe(true);
    expect(usePresence.getState().failure).toEqual({
      kind: "request",
      message: "IPC offline",
    });
    expect(usePresence.getState().map.bob.online).toBe(true);
    expect(usePresence.getState().map.bob.devices?.[0].evidence).toHaveLength(
      1,
    );
    await vi.advanceTimersByTimeAsync(4000);
    expect(usePresence.getState().map.bob.online).toBe(false);
    expect(usePresence.getState().map.bob.last_seen_secs).toBe(8);
    expect(usePresence.getState().map.group.online_count).toBe(0);
    getPresence.mockResolvedValue(snapshot);
    await vi.advanceTimersByTimeAsync(2000);
    expect(usePresence.getState().unavailable).toBe(false);
    expect(usePresence.getState().failure).toBeNull();
    expect(usePresence.getState().map.bob.online).toBe(true);
    expect(usePresence.getState().map.group.online_count).toBe(1);
  } finally {
    stop();
  }
});

it("does not treat a failed or malformed refresh as a successful empty snapshot", async () => {
  getPresence
    .mockResolvedValueOnce({ bob: { online: true, last_seen_secs: 2 } })
    .mockResolvedValueOnce(null);
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(2000);
    expect(usePresence.getState().unavailable).toBe(true);
    expect(usePresence.getState().failure).toEqual({ kind: "invalid" });
    expect(usePresence.getState().map.bob.online).toBe(true);
    expect(usePresence.getState().map.bob.last_seen_secs).toBe(4);
  } finally {
    stop();
  }
});

it("invalidates in-flight work on clear, stop and replacement polling sessions", async () => {
  const pending: Array<(snapshot: Record<string, unknown>) => void> = [];
  getPresence.mockImplementation(
    () => new Promise((resolve) => pending.push(resolve)),
  );
  const stopOld = usePresence.getState().start();
  usePresence.getState().clear();
  const stopNew = usePresence.getState().start();
  pending[1]({ bob: { online: true, last_seen_secs: 0 } });
  await Promise.resolve();
  pending[0]({ other: { online: true, last_seen_secs: 0 } });
  await Promise.resolve();
  expect(Object.keys(usePresence.getState().map)).toEqual(["bob"]);
  stopOld();
  stopNew();
});

it("bounds stalled overlapping polls and marks timeout without freezing evidence", async () => {
  getPresence
    .mockResolvedValueOnce({ bob: { online: true, last_seen_secs: 0 } })
    .mockImplementation(() => new Promise(() => {}));
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(32000);
    expect(getPresence).toHaveBeenCalledTimes(3);
    expect(usePresence.getState().unavailable).toBe(true);
    expect(usePresence.getState().failure).toEqual({ kind: "timeout" });
    expect(usePresence.getState().map.bob.online).toBe(false);
  } finally {
    stop();
  }
});

it("keeps a newer failure authoritative over an older successful response", async () => {
  const pending: Array<{
    resolve: (value: unknown) => void;
    reject: (error: Error) => void;
  }> = [];
  getPresence.mockImplementation(
    () => new Promise((resolve, reject) => pending.push({ resolve, reject })),
  );
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(2000);
    pending[1].reject(new Error("failed"));
    await Promise.resolve();
    pending[0].resolve({ bob: { online: true, last_seen_secs: 0 } });
    await Promise.resolve();
    expect(usePresence.getState().unavailable).toBe(true);
    expect(usePresence.getState().failure).toEqual({
      kind: "request",
      message: "failed",
    });
    expect(usePresence.getState().map).toEqual({});
  } finally {
    stop();
  }
});

it("expires compatibility snapshots within the twenty-second logout budget", () => {
  expect(
    agePresence({ bob: { online: true, last_seen_secs: 0 } }, 20).bob.online,
  ).toBe(false);
});

it.each([
  [new Error("snapshot read failed"), "snapshot read failed"],
  [
    { kind: "internal", message: "discovery unavailable" },
    "discovery unavailable",
  ],
  ["IPC disconnected", "IPC disconnected"],
])(
  "retains the actual refresh rejection for diagnostics: %s",
  async (error, message) => {
    getPresence.mockRejectedValue(error);
    const stop = usePresence.getState().start();
    try {
      await vi.advanceTimersByTimeAsync(0);
      expect(usePresence.getState().failure).toEqual({
        kind: "request",
        message,
      });
    } finally {
      stop();
    }
    expect(usePresence.getState().failure).toBeNull();
  },
);

it("clears diagnostic failures on replacement and ignores a retired owner's rejection", async () => {
  let rejectOld!: (error: Error) => void;
  getPresence
    .mockRejectedValueOnce(new Error("discovery stopped"))
    .mockImplementationOnce(
      () =>
        new Promise((_, reject) => {
          rejectOld = reject;
        }),
    )
    .mockResolvedValue({});
  const stopOld = usePresence.getState().start();
  await vi.advanceTimersByTimeAsync(2000);
  expect(usePresence.getState().failure).not.toBeNull();
  useAuth.setState({ generation: 2 });
  const stopNew = usePresence.getState().start();
  try {
    expect(usePresence.getState().failure).toBeNull();
    rejectOld(new Error("retired failure"));
    await vi.advanceTimersByTimeAsync(0);
    expect(usePresence.getState().failure).toBeNull();
    expect(usePresence.getState().unavailable).toBe(false);
  } finally {
    stopOld();
    stopNew();
  }
});
