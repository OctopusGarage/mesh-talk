import { expect, it, vi } from "vitest";
import {
  createChannelActions,
  type ChannelActionState,
} from "./channelActions";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

function session() {
  let generation = 0;
  let state: ChannelActionState = {
    active: { kind: "channel", id: "team", name: "Team" },
    activeRequest: 1,
    members: [{ user_id: "old", name: "Old" }],
    channelOwner: "old",
    channels: [
      { channel_id: "team", name: "Team", member_count: 1, owner: "old" },
    ],
    unread: { "channel:team": 3, "account:friend": 2 },
  };
  const commands = {
    createChannel: vi.fn(async (_name: string, _ids: string[]) => "new"),
    addChannelMember: vi.fn(async (_id: string, _member: string) => {}),
    removeChannelMember: vi.fn(async (_id: string, _member: string) => {}),
    removeChannelAccountMembers: vi.fn(
      async (_id: string, _account: string) => {},
    ),
    transferChannelOwner: vi.fn(async (_id: string, _member: string) => {}),
    channelMembers: vi.fn(async (_id: string) => ({
      history_policy: "off" as const,
      owner: "new-owner",
      members: [{ user_id: "new-owner", name: "New" }],
    })),
    leaveChannel: vi.fn(async (_id: string) => {}),
    acceptChannelInvite: vi.fn(async (_id: string) => {}),
    dissolveChannel: vi.fn(async (_id: string) => {}),
    renameChannel: vi.fn(async (_id: string, _name: string) => {}),
  };
  const refreshRoster = vi.fn(async () => {});
  const open = vi.fn(async () => {});
  const reportError = vi.fn();
  const actions = createChannelActions({
    read: () => state,
    write: (patch) => {
      state = {
        ...state,
        ...(typeof patch === "function" ? patch(state) : patch),
      };
    },
    capture: () => {
      const captured = generation;
      return { current: () => generation === captured };
    },
    commands,
    refreshRoster,
    open,
    reportError,
  });
  return {
    actions,
    commands,
    refreshRoster,
    open,
    reportError,
    read: () => state,
    patch: (patch: Partial<ChannelActionState>) => {
      state = { ...state, ...patch };
    },
    relogin: () => {
      generation++;
    },
  };
}

it.each([
  ["addMember", "addChannelMember"],
  ["removeMember", "removeChannelMember"],
  ["removeMemberAccount", "removeChannelAccountMembers"],
  ["transferChannelOwner", "transferChannelOwner"],
] as const)(
  "%s owns mutation and refreshed member projection",
  async (action, command) => {
    const s = session();
    expect(await s.actions[action]("member")).toBe(true);
    expect(s.commands[command]).toHaveBeenCalledExactlyOnceWith(
      "team",
      "member",
    );
    expect(s.read().channelOwner).toBe("new-owner");
    expect(s.read().members).toEqual([{ user_id: "new-owner", name: "New" }]);
    expect(s.refreshRoster).toHaveBeenCalledOnce();
  },
);

it("rejects member completion after same-owner relogin", async () => {
  const s = session();
  const command = deferred<void>();
  s.commands.addChannelMember.mockReturnValue(command.promise);
  const pending = s.actions.addMember("member");
  s.relogin();
  command.resolve();
  expect(await pending).toBe(false);
  expect(s.commands.channelMembers).not.toHaveBeenCalled();
  expect(s.read().channelOwner).toBe("old");
});

it("does not apply membership after navigation during its refresh", async () => {
  const s = session();
  const details = deferred<{
    history_policy: "off";
    owner: string;
    members: { user_id: string; name: string }[];
  }>();
  s.commands.channelMembers.mockReturnValue(details.promise);
  const pending = s.actions.removeMember("member");
  await vi.waitFor(() =>
    expect(s.commands.channelMembers).toHaveBeenCalledOnce(),
  );
  s.patch({
    activeRequest: s.read().activeRequest + 1,
    active: { kind: "account", id: "friend", name: "Friend" },
  });
  details.resolve({ history_policy: "off", owner: "stale", members: [] });
  expect(await pending).toBe(false);
  expect(s.read().channelOwner).toBe("old");
  expect(s.refreshRoster).not.toHaveBeenCalled();
});

it("suppresses old membership errors after another request", async () => {
  const s = session();
  const command = deferred<void>();
  s.commands.transferChannelOwner.mockReturnValue(command.promise);
  const pending = s.actions.transferChannelOwner("member");
  s.patch({ activeRequest: s.read().activeRequest + 1 });
  command.reject(new Error("late"));
  expect(await pending).toBe(false);
  expect(s.reportError).not.toHaveBeenCalled();
});

it("reports a current mutation refresh failure", async () => {
  const s = session();
  s.commands.channelMembers.mockRejectedValue(new Error("offline"));
  expect(await s.actions.addMember("member")).toBe(false);
  expect(s.reportError).toHaveBeenCalledExactlyOnceWith(
    "Couldn't add member: offline",
  );
});

it("acceptance stays successful when member detail refresh fails", async () => {
  const s = session();
  s.commands.channelMembers.mockRejectedValue(new Error("offline"));
  expect(await s.actions.acceptChannelInvite()).toBe(true);
  expect(s.refreshRoster).toHaveBeenCalledOnce();
  expect(s.reportError).not.toHaveBeenCalled();
});

it("leaving clears only the departed channel unread and active details", async () => {
  const s = session();
  expect(await s.actions.leaveChannel()).toBe(true);
  expect(s.read()).toMatchObject({
    active: null,
    members: [],
    channelOwner: "",
    unread: { "account:friend": 2 },
  });
});

it("leaving after navigation preserves the new conversation and member details", async () => {
  const s = session();
  const command = deferred<void>();
  s.commands.leaveChannel.mockReturnValue(command.promise);
  const pending = s.actions.leaveChannel();
  s.patch({
    active: { kind: "channel", id: "other", name: "Other" },
    channelOwner: "other-owner",
  });
  command.resolve();
  expect(await pending).toBe(true);
  expect(s.read()).toMatchObject({
    active: { id: "other" },
    channelOwner: "other-owner",
    unread: { "account:friend": 2 },
  });
});

it("dissolving preserves selection, history ownership and unread", async () => {
  const s = session();
  expect(await s.actions.dissolveChannel("team")).toBe(true);
  expect(s.read().channels[0].dissolved).toBe(true);
  expect(s.read().active?.id).toBe("team");
  expect(s.read().unread["channel:team"]).toBe(3);
});

it("does not open a created channel after navigation supersedes it", async () => {
  const s = session();
  const refresh = deferred<void>();
  s.refreshRoster.mockReturnValue(refresh.promise);
  const pending = s.actions.createChannel("New", []);
  await vi.waitFor(() => expect(s.refreshRoster).toHaveBeenCalledOnce());
  s.patch({ activeRequest: s.read().activeRequest + 1 });
  refresh.resolve();
  expect(await pending).toBe(false);
  expect(s.open).not.toHaveBeenCalled();
});

it("renames the header only if that channel is still active", async () => {
  const s = session();
  const command = deferred<void>();
  s.commands.renameChannel.mockReturnValue(command.promise);
  const pending = s.actions.renameChannel("team", "Renamed");
  s.patch({ active: { kind: "channel", id: "other", name: "Other" } });
  command.resolve();
  await pending;
  expect(s.read().active?.name).toBe("Other");
});

it("does not mutate membership for an account conversation", async () => {
  const s = session();
  s.patch({ active: { kind: "account", id: "friend", name: "Friend" } });
  expect(await s.actions.addMember("member")).toBe(false);
  expect(s.commands.addChannelMember).not.toHaveBeenCalled();
});

it.each([
  ["leaveChannel", "Couldn't leave channel"],
  ["acceptChannelInvite", "Couldn't accept invitation"],
  ["dissolveChannel", "Couldn't dissolve channel"],
  ["renameChannel", "Couldn't rename channel"],
] as const)(
  "%s reports its current command failure without changing state",
  async (command, prefix) => {
    const s = session();
    const before = structuredClone(s.read());
    s.commands[command].mockRejectedValueOnce(new Error("offline"));
    if (command === "renameChannel") {
      await s.actions.renameChannel("team", "Renamed");
    } else if (command === "dissolveChannel") {
      expect(await s.actions.dissolveChannel("team")).toBe(false);
    } else {
      expect(await s.actions[command]()).toBe(false);
    }
    expect(s.reportError).toHaveBeenCalledExactlyOnceWith(`${prefix}: offline`);
    expect(s.read()).toEqual(before);
    expect(s.refreshRoster).not.toHaveBeenCalled();
  },
);

it("opens a created channel only after refreshing the roster", async () => {
  const s = session();
  s.open.mockImplementationOnce(async () => {
    expect(s.refreshRoster).toHaveBeenCalledOnce();
  });
  expect(await s.actions.createChannel("New", ["member"])).toBe(true);
  expect(s.commands.createChannel).toHaveBeenCalledExactlyOnceWith("New", [
    "member",
  ]);
  expect(s.open).toHaveBeenCalledExactlyOnceWith({
    kind: "channel",
    id: "new",
    name: "New",
  });
});

it("renames the active channel after its roster refresh", async () => {
  const s = session();
  await s.actions.renameChannel("team", "Renamed");
  expect(s.commands.renameChannel).toHaveBeenCalledExactlyOnceWith(
    "team",
    "Renamed",
  );
  expect(s.refreshRoster).toHaveBeenCalledOnce();
  expect(s.read().active).toEqual({
    kind: "channel",
    id: "team",
    name: "Renamed",
  });
});

it("does not rename a new session after the roster refresh completes", async () => {
  const s = session();
  const refresh = deferred<void>();
  s.refreshRoster.mockReturnValueOnce(refresh.promise);
  const pending = s.actions.renameChannel("team", "Renamed");
  await vi.waitFor(() => expect(s.refreshRoster).toHaveBeenCalledOnce());
  s.relogin();
  refresh.resolve();
  await pending;
  expect(s.read().active?.name).toBe("Team");
  expect(s.reportError).not.toHaveBeenCalled();
});
