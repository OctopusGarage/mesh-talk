import { expect, it, vi } from "vitest";
import {
  createChannelRefresh,
  type ChannelRefreshState,
} from "./channelRefresh";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}
function session() {
  let generation = 0;
  let now = 0;
  let state: ChannelRefreshState = {
    active: { kind: "channel", id: "team", name: "Old" },
    activeRequest: 1,
    channels: [],
    members: [],
    channelOwner: "",
    channelMembersById: {},
  };
  const commands = {
    listChannels: vi.fn(async () => [
      { channel_id: "team", name: "Team", owner: "alice", member_count: 1 },
      { channel_id: "other", name: "Other", owner: "bob", member_count: 1 },
    ]),
    channelMembers: vi.fn(async (id: string) => ({
      history_policy: "off" as const,
      owner: id === "team" ? "alice" : "bob",
      members: [{ user_id: id === "team" ? "alice" : "bob", name: id }],
    })),
  };
  const refresh = createChannelRefresh({
    read: () => state,
    write: (patch) => {
      state = { ...state, ...patch(state) };
    },
    capture: () => {
      const captured = generation;
      return { current: () => generation === captured };
    },
    commands,
    now: () => now,
  });
  return {
    refresh,
    commands,
    read: () => state,
    patch: (patch: Partial<ChannelRefreshState>) => {
      state = { ...state, ...patch };
    },
    relogin: () => {
      generation++;
    },
    advance: (ms: number) => {
      now += ms;
    },
  };
}

it("updates the open group and avatar cache together without polling unchanged groups", async () => {
  const s = session();
  await s.refresh.ifDue();
  expect(s.read().active?.name).toBe("Team");
  expect(s.read().channelOwner).toBe("alice");
  expect(s.read().members).toBe(s.read().channelMembersById.team);
  const members = s.read().members;
  await s.refresh.ifDue();
  expect(s.commands.listChannels).toHaveBeenCalledOnce();
  expect(s.commands.channelMembers).toHaveBeenCalledTimes(2);
  expect(s.read().members).toBe(members);
  s.advance(30_000);
  await s.refresh.ifDue();
  expect(s.commands.listChannels).toHaveBeenCalledTimes(2);
});

it("coalesces bursts and refreshes only the invalidated group's members", async () => {
  const s = session();
  await s.refresh.ifDue();
  s.commands.channelMembers.mockClear();
  s.commands.listChannels.mockClear();
  await Promise.all(
    Array.from({ length: 20 }, () => s.refresh.request("team")),
  );
  expect(s.commands.listChannels).toHaveBeenCalledOnce();
  expect(s.commands.channelMembers).toHaveBeenCalledExactlyOnceWith("team");
});

it("retains one follow-up when a group changes during an active refresh", async () => {
  const s = session();
  await s.refresh.ifDue();
  const pending =
    deferred<Awaited<ReturnType<typeof s.commands.channelMembers>>>();
  s.commands.channelMembers.mockReturnValueOnce(pending.promise);
  const first = s.refresh.request("team");
  await vi.waitFor(() =>
    expect(s.commands.channelMembers).toHaveBeenCalledTimes(3),
  );
  const followups = Array.from({ length: 20 }, () => s.refresh.request("team"));
  pending.resolve({ history_policy: "off", owner: "alice", members: [] });
  await Promise.all([first, ...followups]);
  expect(s.commands.channelMembers).toHaveBeenCalledTimes(4);
  expect(s.read().members).toEqual([{ user_id: "alice", name: "team" }]);
});

it("preserves known members on transient failure and retries at fallback", async () => {
  const s = session();
  await s.refresh.ifDue();
  const members = s.read().members;
  s.commands.channelMembers.mockRejectedValueOnce(new Error("offline"));
  await s.refresh.request("team");
  expect(s.read().members).toBe(members);
  expect(s.read().channelMembersById.team).toBe(members);
  s.advance(30_000);
  await s.refresh.ifDue();
  expect(s.commands.channelMembers).toHaveBeenCalledTimes(5);
});

it("bounds member query concurrency during a full fallback", async () => {
  const s = session();
  const pending =
    deferred<Awaited<ReturnType<typeof s.commands.channelMembers>>>();
  s.commands.listChannels.mockResolvedValue(
    Array.from({ length: 20 }, (_, i) => ({
      channel_id: String(i),
      name: String(i),
      owner: "alice",
      member_count: 1,
    })),
  );
  s.commands.channelMembers.mockReturnValue(pending.promise);
  const work = s.refresh.request();
  await vi.waitFor(() =>
    expect(s.commands.channelMembers).toHaveBeenCalledTimes(4),
  );
  pending.resolve({ history_policy: "off", owner: "alice", members: [] });
  await work;
  expect(s.commands.channelMembers).toHaveBeenCalledTimes(20);
});

it("rejects old-session results while allowing a new session to refresh", async () => {
  const s = session();
  const old = deferred<Awaited<ReturnType<typeof s.commands.listChannels>>>();
  s.commands.listChannels.mockReturnValueOnce(old.promise);
  const work = s.refresh.request();
  await vi.waitFor(() =>
    expect(s.commands.listChannels).toHaveBeenCalledOnce(),
  );
  s.relogin();
  await s.refresh.ifDue();
  old.resolve([
    { channel_id: "stale", name: "Stale", owner: "old", member_count: 0 },
  ]);
  await work;
  expect(s.read().channels.map((c) => c.channel_id)).toEqual(["team", "other"]);
});

it("does not overwrite a newly opened group's member snapshot", async () => {
  const s = session();
  await s.refresh.ifDue();
  s.commands.channelMembers.mockClear();
  const pending =
    deferred<Awaited<ReturnType<typeof s.commands.channelMembers>>>();
  s.commands.channelMembers.mockReturnValueOnce(pending.promise);
  const work = s.refresh.request("team");
  await vi.waitFor(() =>
    expect(s.commands.channelMembers).toHaveBeenCalledOnce(),
  );
  s.patch({
    active: { kind: "channel", id: "other", name: "Other" },
    activeRequest: 2,
    members: [{ user_id: "new", name: "New" }],
    channelOwner: "new",
  });
  pending.resolve({ history_policy: "off", owner: "old", members: [] });
  await work;
  expect(s.read().members).toEqual([{ user_id: "new", name: "New" }]);
  expect(s.read().channelOwner).toBe("new");
});

it("repairs missed group invalidations when fallback becomes due during a targeted refresh", async () => {
  const s = session();
  await s.refresh.ifDue();
  s.advance(30_000);
  s.commands.channelMembers.mockClear();
  const pending =
    deferred<Awaited<ReturnType<typeof s.commands.channelMembers>>>();
  s.commands.channelMembers.mockReturnValueOnce(pending.promise);
  const targeted = s.refresh.request("team");
  await vi.waitFor(() =>
    expect(s.commands.channelMembers).toHaveBeenCalledOnce(),
  );
  const fallback = s.refresh.ifDue();
  pending.resolve({ history_policy: "off", owner: "alice", members: [] });
  await Promise.all([targeted, fallback]);
  expect(s.commands.channelMembers).toHaveBeenCalledWith("other");
});

it("publishes healthy members and recovers after a member read never settles", async () => {
  vi.useFakeTimers();
  const s = session();
  let finish!: (value: unknown) => void;
  s.commands.channelMembers.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve as typeof finish;
      }),
  );
  const pending = s.refresh.request();
  try {
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(50);
    expect(s.read().channelMembersById.other).toBeDefined();
    await vi.advanceTimersByTimeAsync(9950);
    await pending;
    s.advance(30000);
    await s.refresh.ifDue();
    expect(s.read().channelMembersById.team).toBeDefined();
  } finally {
    finish({ owner: "obsolete", members: [] });
    vi.useRealTimers();
  }
});

it("cannot republish an older list after personal deletion invalidates the lane", async () => {
  const s = session();
  const old = deferred<Awaited<ReturnType<typeof s.commands.listChannels>>>();
  const fresh = deferred<Awaited<ReturnType<typeof s.commands.listChannels>>>();
  s.commands.listChannels
    .mockReturnValueOnce(old.promise)
    .mockReturnValueOnce(fresh.promise);
  const first = s.refresh.request();
  await vi.waitFor(() =>
    expect(s.commands.listChannels).toHaveBeenCalledOnce(),
  );
  const invalidated = s.refresh.invalidate();
  old.resolve([
    {
      channel_id: "deleted",
      name: "Must not return",
      owner: "alice",
      member_count: 0,
    },
  ]);
  await vi.waitFor(() =>
    expect(s.commands.listChannels).toHaveBeenCalledTimes(2),
  );
  expect(s.read().channels).toEqual([]);
  fresh.resolve([]);
  await first;
  await invalidated;
  expect(s.read().channels).toEqual([]);
});
