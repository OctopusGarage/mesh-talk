import { expect, it, vi } from "vitest";
import {
  createChannelActions,
  type ChannelActionState,
} from "./channelActions";
import type {
  AccountGroupOperationStatus,
  AccountGroupControlRequest,
} from "@/lib/types";
import type { GroupOperationOutcome } from "./accountGroupOperations";
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
function status(
  kind: AccountGroupOperationStatus["kind"] = "rename",
  phase: AccountGroupOperationStatus["phase"]["kind"] = "committed",
): AccountGroupOperationStatus {
  return {
    id: "op",
    account_id: "me",
    group: "team",
    kind,
    phase: { kind: phase } as AccountGroupOperationStatus["phase"],
    executions: [],
    committed_events: [],
    primary_event: null,
  };
}
function session() {
  let generation = 0;
  let state: ChannelActionState = {
    active: { kind: "channel", id: "team", name: "Team" },
    activeRequest: 1,
    members: [
      { user_id: "device", account_id: "member-account", name: "Member" },
    ],
    channelOwner: "old",
    channels: [
      { channel_id: "team", name: "Team", member_count: 1, owner: "old" },
    ],
    unread: { "channel:team": 3, "account:friend": 2 },
    peers: [
      {
        user_id: "invite-device",
        account_id: "invite-account",
        name: "Invite",
      },
    ] as ChannelActionState["peers"],
    myAccountId: "me",
  };
  const submitControl = vi.fn(
    async (
      _group: string,
      request: AccountGroupControlRequest,
    ): Promise<GroupOperationOutcome> => ({
      kind: "committed",
      id: "op",
      status: status(
        request.kind === "set_history_policy"
          ? "history_policy"
          : request.kind === "react"
            ? "reaction"
            : request.kind,
      ),
    }),
  );
  const commands = {
    createChannel: vi.fn(async () => "new"),
    channelMembers: vi.fn(async () => ({
      history_policy: "off" as const,
      owner: "new-owner",
      members: [{ user_id: "new-owner", name: "New" }],
    })),
  };
  const refreshRoster = vi.fn(async () => {});
  const open = vi.fn(async () => {});
  const reportError = vi.fn();
  const deleteCommittedHistory = vi.fn();
  const actions = createChannelActions({
    read: () => state,
    write: (patch) => {
      state = {
        ...state,
        ...(typeof patch === "function" ? patch(state) : patch),
      };
    },
    capture: () => {
      const current = generation;
      return { current: () => generation === current };
    },
    commands,
    submitControl,
    refreshRoster,
    open,
    reportError,
    deleteCommittedHistory,
  });
  return {
    actions,
    deleteCommittedHistory,
    submitControl,
    commands,
    refreshRoster,
    open,
    reportError,
    read: () => state,
    patch: (patch: Partial<ChannelActionState>) => {
      state = { ...state, ...patch };
    },
    relogin: () => generation++,
  };
}
it.each([
  [
    "addMember",
    "invite-device",
    { kind: "invite", target_account_id: "invite-account" },
  ],
  [
    "removeMember",
    "device",
    { kind: "remove_account", target_account_id: "member-account" },
  ],
  [
    "removeMemberAccount",
    "member-account",
    { kind: "remove_account", target_account_id: "member-account" },
  ],
  [
    "transferChannelOwner",
    "device",
    { kind: "transfer_ownership", target_account_id: "member-account" },
  ],
] as const)(
  "%s resolves verified account and submits typed control",
  async (action, target, request) => {
    const s = session();
    expect((await s.actions[action](target)).kind).toBe("committed");
    expect(s.submitControl).toHaveBeenCalledExactlyOnceWith("team", request);
    expect(s.read().channelOwner).toBe("new-owner");
  },
);
it.each([
  "pending",
  "unconfirmed",
  "rejected",
  "cancelled",
  "capacity",
  "not_admitted",
] as const)("%s never changes projection or navigation", async (kind) => {
  const s = session();
  const before = structuredClone(s.read());
  s.submitControl.mockResolvedValue({
    kind,
    id: "op",
    status: status("leave_account", "accepted"),
    error: "no",
  } as GroupOperationOutcome);
  expect((await s.actions.leaveChannel()).kind).toBe(kind);
  expect(s.read()).toEqual(before);
  expect(s.refreshRoster).not.toHaveBeenCalled();
});
it("unknown target account fails before admission", async () => {
  const s = session();
  expect((await s.actions.transferChannelOwner("unknown")).kind).toBe(
    "not_admitted",
  );
  expect(s.submitControl).not.toHaveBeenCalled();
});
it("modern member account wins over current peer binding", async () => {
  const s = session();
  s.patch({
    peers: [
      { user_id: "device", account_id: "other" },
    ] as ChannelActionState["peers"],
  });
  await s.actions.removeMember("device");
  expect(s.submitControl).toHaveBeenCalledWith("team", {
    kind: "remove_account",
    target_account_id: "member-account",
  });
});
it("unknown direct account is not an invented grant", async () => {
  const s = session();
  expect((await s.actions.removeMemberAccount("invented")).kind).toBe(
    "not_admitted",
  );
  expect(s.submitControl).not.toHaveBeenCalled();
});
it("same-owner relogin suppresses completion", async () => {
  const s = session();
  const wait = deferred<GroupOperationOutcome>();
  s.submitControl.mockReturnValue(wait.promise);
  const pending = s.actions.leaveChannel();
  s.relogin();
  wait.resolve({
    kind: "committed",
    id: "op",
    status: status("leave_account"),
  });
  expect((await pending).kind).toBe("stale");
  expect(s.read().active?.id).toBe("team");
  expect(s.refreshRoster).not.toHaveBeenCalled();
});
it("committed leave preserves newly selected conversation", async () => {
  const s = session();
  const wait = deferred<GroupOperationOutcome>();
  s.submitControl.mockReturnValue(wait.promise);
  const pending = s.actions.leaveChannel();
  s.patch({
    active: { kind: "channel", id: "other", name: "Other" },
    channelOwner: "other",
  });
  wait.resolve({
    kind: "committed",
    id: "op",
    status: status("leave_account"),
  });
  expect((await pending).kind).toBe("committed");
  expect(s.read().active?.id).toBe("other");
  expect(s.read().channelOwner).toBe("other");
  expect(s.read().unread).toEqual({ "account:friend": 2 });
});
it("pending rename does not change active name", async () => {
  const s = session();
  s.submitControl.mockResolvedValue({
    kind: "pending",
    id: "op",
    status: status("rename", "accepted"),
  });
  expect((await s.actions.renameChannel("team", "Renamed")).kind).toBe(
    "pending",
  );
  expect(s.read().active?.name).toBe("Team");
});
it("terminal refresh failure cannot downgrade committed acceptance", async () => {
  const s = session();
  s.commands.channelMembers.mockRejectedValue(new Error("offline"));
  expect((await s.actions.acceptChannelInvite()).kind).toBe("committed");
});
it("reconciles rediscovered terminal only after commitment", async () => {
  const s = session();
  await s.actions.reconcileCommitted(status("dissolve", "accepted"));
  expect(s.refreshRoster).not.toHaveBeenCalled();
  await s.actions.reconcileCommitted(status("dissolve"));
  expect(s.read().channels[0].dissolved).toBe(true);
});
it("rejects terminal for another account", async () => {
  const s = session();
  await s.actions.reconcileCommitted({
    ...status("leave_account"),
    account_id: "other",
  });
  expect(s.read().active?.id).toBe("team");
});
it("late member refresh does not overwrite navigation", async () => {
  const s = session();
  const wait =
    deferred<Awaited<ReturnType<typeof s.commands.channelMembers>>>();
  s.commands.channelMembers.mockReturnValue(wait.promise);
  const pending = s.actions.removeMember("device");
  await vi.waitFor(() =>
    expect(s.commands.channelMembers).toHaveBeenCalledOnce(),
  );
  s.patch({
    activeRequest: 2,
    active: { kind: "account", id: "friend", name: "Friend" },
  });
  wait.resolve({ history_policy: "off", owner: "late", members: [] });
  await pending;
  expect(s.read().channelOwner).toBe("old");
});
it("creation stays explicit local owner flow", async () => {
  const s = session();
  expect(await s.actions.createChannel("New", ["device"])).toBe(true);
  expect(s.commands.createChannel).toHaveBeenCalledWith("New", ["device"]);
  expect(s.submitControl).not.toHaveBeenCalled();
});
it("retries failed terminal projection reads on the next reconciliation", async () => {
  const s = session();
  s.commands.channelMembers.mockRejectedValueOnce(new Error("offline"));
  await s.actions.reconcileCommitted(status("remove_account"));
  expect(s.read().channelOwner).toBe("old");
  await s.actions.reconcileCommitted(status("remove_account"));
  expect(s.read().channelOwner).toBe("new-owner");
  expect(s.commands.channelMembers).toHaveBeenCalledTimes(2);
});
it("refreshes active membership metadata after history policy commitment", async () => {
  const s = session();
  await s.actions.reconcileCommitted(status("history_policy"));
  expect(s.commands.channelMembers).toHaveBeenCalledWith("team");
});

it("shares an in-flight committed projection refresh with the submitting action", async () => {
  const s = session();
  const details =
    deferred<Awaited<ReturnType<typeof s.commands.channelMembers>>>();
  s.commands.channelMembers.mockReturnValue(details.promise);
  const terminal = s.actions.reconcileCommitted(status("transfer_ownership"));
  await vi.waitFor(() =>
    expect(s.commands.channelMembers).toHaveBeenCalledOnce(),
  );
  let completed = false;
  const submit = s.actions.transferChannelOwner("device").then((outcome) => {
    completed = true;
    return outcome;
  });
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
  expect(completed).toBe(false);
  details.resolve({ history_policy: "off", owner: "new-owner", members: [] });
  await terminal;
  expect((await submit).kind).toBe("committed");
  expect(s.read().channelOwner).toBe("new-owner");
});

it("refreshes an inactive committed group without renaming the selected conversation", async () => {
  const s = session();
  s.patch({ active: { kind: "channel", id: "other", name: "Other" } });
  s.refreshRoster.mockImplementationOnce(async () =>
    s.patch({
      channels: [
        {
          channel_id: "team",
          name: "Renamed by source",
          owner: "old",
          member_count: 1,
        },
      ],
    }),
  );
  await s.actions.reconcileCommitted(status("rename"));
  expect(s.read().channels[0].name).toBe("Renamed by source");
  expect(s.read().active).toMatchObject({ id: "other", name: "Other" });
  expect(s.commands.channelMembers).not.toHaveBeenCalled();
});
it("transfers to a verified member account without requiring its representative device ID", async () => {
  const s = session();
  expect((await s.actions.transferChannelOwner("member-account")).kind).toBe(
    "committed",
  );
  expect(s.submitControl).toHaveBeenCalledExactlyOnceWith("team", {
    kind: "transfer_ownership",
    target_account_id: "member-account",
  });
});

it("retains the leave cleanup choice without deleting before commitment", async () => {
  const s = session();
  const pending = {
    ...status("leave_account", "accepted"),
    delete_history: true,
  };
  s.submitControl.mockResolvedValue({
    kind: "pending",
    id: "op",
    status: pending,
  });
  await s.actions.leaveChannel(true);
  expect(s.submitControl).toHaveBeenCalledWith("team", {
    kind: "leave_account",
    delete_history: true,
  });
  expect(s.deleteCommittedHistory).not.toHaveBeenCalled();
  expect(s.read().active?.id).toBe("team");
  await s.actions.reconcileCommitted({
    ...pending,
    phase: { kind: "committed" },
    history_cleanup_complete: true,
  });
  expect(s.deleteCommittedHistory).toHaveBeenCalledExactlyOnceWith("team");
});

it("retains group history when committed leave has no deletion choice", async () => {
  const s = session();
  await s.actions.leaveChannel(false);
  expect(s.submitControl).toHaveBeenCalledWith("team", {
    kind: "leave_account",
    delete_history: false,
  });
  expect(s.deleteCommittedHistory).not.toHaveBeenCalled();
});

it("does not purge a publicly committed leave until private cleanup is complete", async () => {
  const s = session();
  const pending = {
    ...status("leave_account"),
    delete_history: true,
    history_cleanup_complete: false,
  };
  await s.actions.reconcileCommitted(pending);
  expect(s.deleteCommittedHistory).not.toHaveBeenCalled();
  expect(s.read().active?.id).toBe("team");
  await s.actions.reconcileCommitted({
    ...pending,
    history_cleanup_complete: true,
  });
  expect(s.deleteCommittedHistory).toHaveBeenCalledExactlyOnceWith("team");
});

it("reconciles delayed cleanup completion after an earlier public Leave observation", async () => {
  const s = session();
  const committed = status("leave_account");
  await s.actions.reconcileCommitted(committed);
  expect(s.deleteCommittedHistory).not.toHaveBeenCalled();
  const completed = {
    ...committed,
    delete_history: true,
    history_cleanup_complete: true,
  };
  await s.actions.reconcileCommitted(completed);
  await s.actions.reconcileCommitted(completed);
  expect(s.deleteCommittedHistory).toHaveBeenCalledExactlyOnceWith("team");
});
