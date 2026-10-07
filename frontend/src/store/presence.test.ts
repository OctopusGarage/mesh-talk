import { afterEach, beforeEach, expect, it, vi } from "vitest";

const { getPresence } = vi.hoisted(() => ({ getPresence: vi.fn() }));
vi.mock("@/lib/api", () => ({ presence: { get: getPresence } }));

import { useAuth } from "./auth";
import { presenceStatus, usePresence } from "./presence";

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

it("keeps unchanged records stable and removes peers absent from the next poll", async () => {
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
    expect(usePresence.getState().map).toEqual({ a: first });
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

it("distinguishes online, recent, and offline at the TTL boundary", () => {
  expect(presenceStatus({ online: true, last_seen_secs: null })).toBe("online");
  expect(presenceStatus({ online: false, last_seen_secs: 299 })).toBe("recent");
  expect(presenceStatus({ online: false, last_seen_secs: 300 })).toBe(
    "offline",
  );
  expect(presenceStatus({ online: false, last_seen_secs: null })).toBe(
    "offline",
  );
});
