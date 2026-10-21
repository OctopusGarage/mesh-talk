import type { PresenceMap } from "@/lib/types";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

const { getPresence } = vi.hoisted(() => ({ getPresence: vi.fn() }));
vi.mock("@/lib/api", () => ({ presence: { get: getPresence } }));

import { useAuth } from "./auth";
import {
  notifySyncInvalidation,
  setPushStateSupport,
} from "@/lib/syncInvalidation";
import {
  agePresence,
  hasReconnectingPresence,
  presenceStatus,
  presenceLabel,
  usePresence,
} from "./presence";

beforeEach(() => {
  vi.useFakeTimers();
  getPresence.mockReset();
  setPushStateSupport(null);
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
  const snapshot: PresenceMap = {
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
    // Expired watchdogs release frontend slots; the IPC adapter bounds native work.
    expect(getPresence.mock.calls.length).toBeGreaterThan(3);
    expect(getPresence.mock.calls.length).toBeLessThanOrEqual(9);
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

it("shows reconnecting after fresh evidence expires and offline at the fixed 60-second bound", () => {
  const snapshot: PresenceMap = {
    bob: {
      online: true,
      last_seen_secs: 0,
      reconnect_expires_in_secs: 60,
      devices: [
        {
          user_id: "bob-device",
          evidence: [
            { source: "routed", last_seen_secs: 0, expires_in_secs: 15 },
          ],
        },
      ],
    },
  };
  expect(presenceStatus(agePresence(snapshot, 14).bob)).toBe("online");
  const expired = agePresence(snapshot, 15).bob;
  expect(expired.online).toBe(false);
  expect(presenceStatus(expired)).toBe("reconnecting");
  expect(presenceStatus(agePresence(snapshot, 59).bob)).toBe("reconnecting");
  expect(presenceStatus(agePresence(snapshot, 60).bob)).toBe("offline");
  expect(presenceStatus(agePresence({ bob: expired }, 45).bob)).toBe("offline");
});

it("ends reconnecting immediately on an authoritative withdrawal and restores online on fresh evidence", async () => {
  getPresence.mockResolvedValue({
    bob: { online: false, last_seen_secs: 17, reconnect_expires_in_secs: 13 },
  });
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    getPresence.mockResolvedValue({});
    await vi.advanceTimersByTimeAsync(2000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("offline");
    getPresence.mockResolvedValue({
      bob: { online: true, last_seen_secs: 0, reconnect_expires_in_secs: 30 },
    });
    await vi.advanceTimersByTimeAsync(2000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("online");
  } finally {
    stop();
  }
});

it("never extends reconnecting on failed refreshes or counts it as a known-online group member", async () => {
  getPresence
    .mockResolvedValueOnce({
      bob: { online: false, last_seen_secs: 20, reconnect_expires_in_secs: 10 },
      group: {
        online: false,
        last_seen_secs: null,
        member_count: 1,
        online_count: 0,
        member_keys: ["bob"],
      },
    })
    .mockRejectedValue(new Error("network unavailable"));
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(6000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    expect(usePresence.getState().map.group.online_count).toBe(0);
    await vi.advanceTimersByTimeAsync(4000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("offline");
  } finally {
    stop();
  }
});

it("excludes self and groups from the reconnecting shell state", () => {
  const recovering = {
    online: false,
    last_seen_secs: 16,
    reconnect_expires_in_secs: 14,
  };
  expect(hasReconnectingPresence({ self: recovering }, "self")).toBe(false);
  expect(
    hasReconnectingPresence({ group: { ...recovering, member_keys: ["bob"] } }),
  ).toBe(false);
  expect(hasReconnectingPresence({ bob: recovering }, "self")).toBe(true);
});

it("expires reconnecting at its exact deadline between regular poll ticks", async () => {
  getPresence
    .mockResolvedValueOnce({
      bob: { online: false, last_seen_secs: 29, reconnect_expires_in_secs: 1 },
    })
    .mockRejectedValue(new Error("unavailable"));
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(1000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("offline");
  } finally {
    stop();
  }
});

it("does not add subsecond IPC latency to the reconnect deadline", async () => {
  let finish!: (snapshot: Record<string, unknown>) => void;
  getPresence.mockImplementation(
    () => new Promise((resolve) => (finish = resolve)),
  );
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(800);
    finish({
      bob: { online: false, last_seen_secs: 29, reconnect_expires_in_secs: 1 },
    });
    await vi.advanceTimersByTimeAsync(0);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(200);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("offline");
  } finally {
    stop();
  }
});

it("waits for five seconds of sustained fresh presence after a reconnect without delaying first discovery", async () => {
  getPresence.mockResolvedValue({
    bob: { online: false, last_seen_secs: 35, reconnect_expires_in_secs: 25 },
  });
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    getPresence.mockResolvedValue({
      bob: {
        online: true,
        last_seen_secs: 0,
        reconnect_expires_in_secs: 60,
        devices: [
          {
            user_id: "bob-device",
            evidence: [
              { source: "routed", last_seen_secs: 0, expires_in_secs: 15 },
            ],
          },
        ],
      },
      group: {
        online: false,
        last_seen_secs: null,
        member_keys: ["bob"],
        member_count: 1,
        online_count: 1,
      },
    });
    await vi.advanceTimersByTimeAsync(2000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    expect(usePresence.getState().map.group.online_count).toBe(0);
    await vi.advanceTimersByTimeAsync(4000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    await vi.advanceTimersByTimeAsync(2000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("online");
    expect(usePresence.getState().map.group.online_count).toBe(1);
  } finally {
    stop();
  }
});

it("does not count failed refreshes as a stable recovery and still honors withdrawal", async () => {
  const recovering = {
    bob: { online: false, last_seen_secs: 35, reconnect_expires_in_secs: 25 },
  };
  getPresence.mockResolvedValue(recovering);
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    getPresence.mockResolvedValue({
      bob: { online: true, last_seen_secs: 0, reconnect_expires_in_secs: 60 },
    });
    await vi.advanceTimersByTimeAsync(2000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    getPresence.mockRejectedValue(new Error("IPC unavailable"));
    await vi.advanceTimersByTimeAsync(6000);
    getPresence.mockResolvedValue({
      bob: { online: true, last_seen_secs: 0, reconnect_expires_in_secs: 60 },
    });
    await vi.advanceTimersByTimeAsync(2000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    getPresence.mockResolvedValue({});
    await vi.advanceTimersByTimeAsync(2000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("offline");
  } finally {
    stop();
  }
});

it("does not treat repeated polls of one old public observation as a stable recovery", async () => {
  getPresence.mockResolvedValue({
    bob: { online: false, last_seen_secs: 35, reconnect_expires_in_secs: 25 },
  });
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    const observedAt = performance.now() + 2000;
    getPresence.mockImplementation(async () => {
      const age = Math.max(0, (performance.now() - observedAt) / 1000);
      return {
        bob: {
          online: true,
          last_seen_secs: age,
          reconnect_expires_in_secs: 60 - age,
        },
      };
    });
    await vi.advanceTimersByTimeAsync(8000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("reconnecting");
    getPresence.mockResolvedValue({
      bob: { online: true, last_seen_secs: 0, reconnect_expires_in_secs: 60 },
    });
    await vi.advanceTimersByTimeAsync(2000);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("online");
  } finally {
    stop();
  }
});

it("keeps a latency-tolerant peer online with a weak label instead of re-entering recovery", async () => {
  getPresence.mockResolvedValue({
    bob: {
      online: true,
      connection_weak: true,
      last_seen_secs: 20,
      reconnect_expires_in_secs: 40,
      devices: [
        {
          user_id: "bob",
          evidence: [
            { source: "routed", last_seen_secs: 20, expires_in_secs: 40 },
          ],
        },
      ],
    },
  });
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(4000);
    expect(usePresence.getState().map.bob.online).toBe(true);
    expect(hasReconnectingPresence(usePresence.getState().map)).toBe(false);
    expect(
      presenceLabel(
        usePresence.getState().map.bob,
        ((key: string) => key) as never,
      ),
    ).toBe("presence.onlineWeak");
  } finally {
    stop();
  }
});

it("keeps weak quality steady until thirty seconds of healthy renewed observations", async () => {
  getPresence.mockResolvedValue({
    bob: {
      online: true,
      connection_weak: true,
      last_seen_secs: 35,
      reconnect_expires_in_secs: 25,
    },
  });
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    getPresence.mockResolvedValue({
      bob: {
        online: true,
        connection_weak: false,
        last_seen_secs: 0,
        reconnect_expires_in_secs: 60,
      },
    });
    await vi.advanceTimersByTimeAsync(28000);
    expect(usePresence.getState().map.bob.online).toBe(true);
    expect(usePresence.getState().map.bob.connection_weak).toBe(true);
    await vi.advanceTimersByTimeAsync(4000);
    expect(usePresence.getState().map.bob.connection_weak).toBe(false);
  } finally {
    stop();
  }
});

it("ages cross-network online tolerance independently of expired device evidence", () => {
  const snapshot = {
    bob: {
      online: true,
      connection_weak: false,
      last_seen_secs: 0,
      online_expires_in_secs: 120,
      reconnect_expires_in_secs: 120,
      devices: [
        {
          user_id: "bob-device",
          evidence: [
            {
              source: "routed" as const,
              last_seen_secs: 0,
              expires_in_secs: 15,
            },
          ],
        },
      ],
    },
    group: {
      online: false,
      last_seen_secs: null,
      member_keys: ["bob"],
      member_count: 1,
      online_count: 1,
    },
  };
  const tolerated = agePresence(snapshot, 119);
  expect(tolerated.bob.online).toBe(true);
  expect(tolerated.bob.connection_weak).toBe(true);
  expect(tolerated.bob.devices).toEqual([]);
  expect(tolerated.group.online_count).toBe(1);
  const expired = agePresence(snapshot, 120);
  expect(expired.bob.online).toBe(false);
  expect(expired.group.online_count).toBe(0);
});

it("expires cross-network tolerance exactly during IPC failure without extending the observation", async () => {
  getPresence
    .mockResolvedValueOnce({
      bob: {
        online: true,
        connection_weak: true,
        last_seen_secs: 119,
        online_expires_in_secs: 1,
        reconnect_expires_in_secs: 1,
        devices: [],
      },
    })
    .mockRejectedValue(new Error("IPC unavailable"));
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(usePresence.getState().map.bob.online).toBe(true);
    await vi.advanceTimersByTimeAsync(999);
    expect(usePresence.getState().map.bob.online).toBe(true);
    await vi.advanceTimersByTimeAsync(1);
    expect(usePresence.getState().map.bob.online).toBe(false);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("offline");
  } finally {
    stop();
  }
});

it("recovers after two presence IPCs remain unresolved past their deadlines", async () => {
  const releases: Array<(snapshot: Record<string, unknown>) => void> = [];
  getPresence
    .mockImplementationOnce(
      () => new Promise((resolve) => releases.push(resolve)),
    )
    .mockImplementationOnce(
      () => new Promise((resolve) => releases.push(resolve)),
    )
    .mockResolvedValue({ bob: { online: true, last_seen_secs: 0 } });
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(14000);
    expect(usePresence.getState().map.bob?.online).toBe(true);
  } finally {
    stop();
    releases.forEach((resolve) => resolve({}));
  }
});

it("retains one push invalidation while both presence reads are occupied", async () => {
  const releases: Array<(snapshot: Record<string, unknown>) => void> = [];
  getPresence
    .mockImplementationOnce(
      () => new Promise((resolve) => releases.push(resolve)),
    )
    .mockImplementationOnce(
      () => new Promise((resolve) => releases.push(resolve)),
    )
    .mockResolvedValue({ bob: { online: true, last_seen_secs: 0 } });
  setPushStateSupport("alice");
  const stop = usePresence.getState().start();
  try {
    notifySyncInvalidation({ owner: "alice", kind: "roster" });
    for (let i = 0; i < 8; i++)
      notifySyncInvalidation({ owner: "alice", kind: "roster" });
    expect(getPresence).toHaveBeenCalledTimes(2);
    releases[0]({ bob: { online: false, last_seen_secs: 30 } });
    await vi.advanceTimersByTimeAsync(0);
    expect(getPresence).toHaveBeenCalledTimes(3);
    expect(usePresence.getState().map.bob.online).toBe(true);
    releases[1]({ bob: { online: false, last_seen_secs: 30 } });
    await vi.advanceTimersByTimeAsync(0);
    expect(getPresence).toHaveBeenCalledTimes(3);
    expect(usePresence.getState().map.bob.online).toBe(true);
  } finally {
    stop();
    releases.forEach((resolve) => resolve({}));
  }
});

it("does not retry a failed presence push follow-up without another invalidation", async () => {
  const releases: Array<(snapshot: Record<string, unknown>) => void> = [];
  getPresence
    .mockImplementationOnce(
      () => new Promise((resolve) => releases.push(resolve)),
    )
    .mockImplementationOnce(
      () => new Promise((resolve) => releases.push(resolve)),
    )
    .mockRejectedValue(new Error("read unavailable"));
  setPushStateSupport("alice");
  const stop = usePresence.getState().start();
  try {
    notifySyncInvalidation({ owner: "alice", kind: "roster" });
    notifySyncInvalidation({ owner: "alice", kind: "roster" });
    releases[0]({});
    await vi.advanceTimersByTimeAsync(0);
    releases[1]({});
    await vi.advanceTimersByTimeAsync(5000);
    expect(getPresence).toHaveBeenCalledTimes(3);
  } finally {
    stop();
    releases.forEach((resolve) => resolve({}));
  }
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

it("fresh local observations clear a previous routed weak hint immediately", async () => {
  getPresence.mockResolvedValue({
    bob: {
      online: true,
      connection_weak: true,
      last_seen_secs: 20,
      online_expires_in_secs: 100,
      reconnect_expires_in_secs: 100,
      devices: [],
    },
  });
  const stop = usePresence.getState().start();
  try {
    await vi.advanceTimersByTimeAsync(0);
    expect(usePresence.getState().map.bob.connection_weak).toBe(true);
    getPresence.mockResolvedValue({
      bob: {
        online: true,
        connection_weak: false,
        last_seen_secs: 0,
        online_expires_in_secs: 120,
        reconnect_expires_in_secs: 120,
        devices: [
          {
            user_id: "bob-local",
            evidence: [
              { source: "local", last_seen_secs: 0, expires_in_secs: 15 },
            ],
          },
        ],
      },
    });
    await vi.advanceTimersByTimeAsync(2000);
    expect(usePresence.getState().map.bob.connection_weak).toBe(false);
    expect(presenceStatus(usePresence.getState().map.bob)).toBe("online");
  } finally {
    stop();
  }
});

it("a fresh local path outranks a weak routed hint but only while local evidence is fresh", () => {
  const snapshot: PresenceMap = {
    bob: {
      online: true,
      connection_weak: true,
      last_seen_secs: 0,
      online_expires_in_secs: 120,
      reconnect_expires_in_secs: 120,
      devices: [
        {
          user_id: "bob",
          evidence: [
            { source: "local", last_seen_secs: 0, expires_in_secs: 15 },
            { source: "routed", last_seen_secs: 20, expires_in_secs: 40 },
          ],
        },
      ],
    },
  };
  expect(agePresence(snapshot, 14).bob.connection_weak).toBe(false);
  expect(agePresence(snapshot, 15).bob.connection_weak).toBe(true);
});
