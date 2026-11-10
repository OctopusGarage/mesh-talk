import { readWithDeadline } from "@/lib/readDeadline";
import type {
  AccountGroupControlRequest,
  AccountGroupOperationStatus,
  ChannelInfo,
  ChannelMemberInfo,
  ChannelMembersInfo,
  PeerInfo,
} from "@/lib/types";
import {
  isGroupHistoryCleanupPending,
  type GroupOperationOutcome,
} from "./accountGroupOperations";
import { errorMessage } from "@/lib/error";
import { convKey } from "./conversationState";
import type { Conversation } from "./conversationModel";

/** Account authority and durable acceptance live in core. This owner only refreshes projections after commitment. */
export interface ChannelActionState {
  active: Conversation | null;
  activeRequest: number;
  channels: ChannelInfo[];
  members: ChannelMemberInfo[];
  channelOwner: string;
  unread: Record<string, number>;
  peers: PeerInfo[];
  myAccountId: string;
}
interface ChannelActionContext {
  read(): ChannelActionState;
  write(
    patch:
      | Partial<ChannelActionState>
      | ((state: ChannelActionState) => Partial<ChannelActionState>),
  ): void;
  capture(): { current(): boolean };
  commands: {
    createChannel(name: string, memberAccountIds: string[]): Promise<string>;
    channelMembers(id: string): Promise<ChannelMembersInfo>;
  };
  submitControl(
    group: string,
    operation: AccountGroupControlRequest,
  ): Promise<GroupOperationOutcome>;
  refreshRoster(): Promise<void>;
  open(conversation: Conversation): Promise<void>;
  reportError(message: string): void;
  deleteCommittedHistory?(group: string): void;
}
export interface ChannelActions {
  createChannel(name: string, memberAccountIds: string[]): Promise<boolean>;
  addMember(memberId: string): Promise<GroupOperationOutcome>;
  removeMember(memberId: string): Promise<GroupOperationOutcome>;
  removeMemberAccount(accountId: string): Promise<GroupOperationOutcome>;
  transferChannelOwner(memberId: string): Promise<GroupOperationOutcome>;
  leaveChannel(deleteHistory?: boolean): Promise<GroupOperationOutcome>;
  acceptChannelInvite(): Promise<GroupOperationOutcome>;
  declineChannelInvite(): Promise<GroupOperationOutcome>;
  dissolveChannel(channelId: string): Promise<GroupOperationOutcome>;
  renameChannel(
    channelId: string,
    name: string,
  ): Promise<GroupOperationOutcome>;
  setChannelHistoryPolicy(
    channelId: string,
    policy: ChannelMembersInfo["history_policy"],
  ): Promise<GroupOperationOutcome>;
  reconcileCommitted(status: AccountGroupOperationStatus): Promise<void>;
}
export function createChannelActions(
  context: ChannelActionContext,
): ChannelActions {
  const { read, write, commands, refreshRoster, open, reportError } = context;
  function capture() {
    const lease = context.capture();
    const accountId = read().myAccountId;
    const current = () => lease.current() && read().myAccountId === accountId;
    return {
      current,
      set: (patch: Parameters<typeof write>[0]) => {
        if (current()) write(patch);
      },
    };
  }
  function unavailable(): GroupOperationOutcome {
    return {
      kind: "not_admitted",
      error: new Error(
        "Verified account binding unavailable. Update your linked device and refresh the member list.",
      ),
    };
  }
  // A small completion cache only deduplicates projection refreshes. Native tickets remain authoritative.
  const reconciled = new Map<
    string,
    { current(): boolean; work: Promise<void> }
  >();
  async function reconcileCommitted(
    status: AccountGroupOperationStatus,
  ): Promise<void> {
    const lease = capture();
    if (
      !lease.current() ||
      status.phase.kind !== "committed" ||
      isGroupHistoryCleanupPending(status) ||
      status.account_id !== read().myAccountId
    )
      return;
    const effect =
      status.kind === "leave_account" &&
      status.delete_history === true &&
      status.history_cleanup_complete === true
        ? "history"
        : "public";
    const key = `${status.account_id}:${status.id}:${effect}`;
    const known = reconciled.get(key);
    if (known?.current()) return known.work;
    for (const [id, entry] of reconciled)
      if (!entry.current()) reconciled.delete(id);
    if (reconciled.size >= 256)
      reconciled.delete(reconciled.keys().next().value!);
    let finish!: () => void;
    const entry = {
      ...lease,
      work: new Promise<void>((resolve) => {
        finish = resolve;
      }),
    };
    reconciled.set(key, entry);
    const group = status.group;
    const request = read().activeRequest;
    const current = () =>
      lease.current() &&
      read().activeRequest === request &&
      read().active?.kind === "channel" &&
      read().active?.id === group;
    try {
      if (
        status.kind === "leave_account" ||
        status.kind === "decline_invitation"
      ) {
        lease.set((state) => {
          const unread = { ...state.unread };
          delete unread[convKey({ kind: "channel", id: group, name: "" })];
          const leaving =
            state.active?.kind === "channel" && state.active.id === group;
          return {
            unread,
            ...(leaving ? { active: null, members: [], channelOwner: "" } : {}),
          };
        });
      } else if (status.kind === "dissolve") {
        lease.set((state) => ({
          channels: state.channels.map((channel) =>
            channel.channel_id === group
              ? { ...channel, dissolved: true }
              : channel,
          ),
        }));
      }
      if (
        status.kind === "leave_account" &&
        status.delete_history === true &&
        status.history_cleanup_complete === true &&
        lease.current()
      )
        context.deleteCommittedHistory?.(group);
      await readWithDeadline(refreshRoster);
      if (!lease.current()) return;
      if (status.kind === "rename" && current()) {
        const channel = read().channels.find(
          (channel) => channel.channel_id === group,
        );
        const active = read().active;
        if (channel && active)
          lease.set({ active: { ...active, name: channel.name } });
      }
      if (
        current() &&
        [
          "invite",
          "remove_account",
          "transfer_ownership",
          "accept_invitation",
          "history_policy",
        ].includes(status.kind)
      ) {
        const info = await readWithDeadline(() =>
          commands.channelMembers(group),
        );
        if (current())
          lease.set({ members: info.members, channelOwner: info.owner });
      }
    } catch (error) {
      if (reconciled.get(key) === entry) reconciled.delete(key);
      // A failed read cannot undo a durable committed operation; later ordinary refreshes retry it.
      if (lease.current())
        reportError(`Couldn't refresh channel: ${errorMessage(error)}`);
    } finally {
      finish();
    }
  }
  async function submit(
    group: string | undefined,
    operation: AccountGroupControlRequest,
  ): Promise<GroupOperationOutcome> {
    const lease = capture();
    if (!lease.current()) return { kind: "stale" };
    if (!group) return unavailable();
    const outcome = await context.submitControl(group, operation);
    if (!lease.current()) return { kind: "stale" };
    if (outcome.kind === "committed") await reconcileCommitted(outcome.status);
    return lease.current() ? outcome : { kind: "stale" };
  }
  function activeGroup() {
    const active = read().active;
    return active?.kind === "channel" ? active.id : undefined;
  }
  function memberAccount(device: string) {
    const member = read().members.find((member) => member.user_id === device);
    return (
      member?.account_id ||
      (member
        ? read().peers.find((peer) => peer.user_id === device)?.account_id
        : undefined)
    );
  }
  function target(
    kind: "invite" | "remove_account" | "transfer_ownership",
    account: string | null | undefined,
  ) {
    return account
      ? submit(activeGroup(), { kind, target_account_id: account })
      : Promise.resolve(unavailable());
  }
  return {
    async createChannel(name, memberAccountIds) {
      const lease = capture();
      if (!lease.current()) return false;
      const request = read().activeRequest + 1;
      lease.set({ activeRequest: request });
      const current = () => lease.current() && read().activeRequest === request;
      const id = await commands.createChannel(name, memberAccountIds);
      if (!current()) return false;
      await refreshRoster();
      if (!current()) return false;
      await open({ kind: "channel", id, name });
      return lease.current();
    },

    addMember: (device) =>
      target(
        "invite",
        read().peers.find((peer) => peer.user_id === device)?.account_id,
      ),
    removeMember: (device) => target("remove_account", memberAccount(device)),
    removeMemberAccount: (account) =>
      target(
        "remove_account",
        read().members.some(
          (member) =>
            member.account_id === account ||
            (!member.account_id && memberAccount(member.user_id) === account),
        )
          ? account
          : undefined,
      ),
    transferChannelOwner: (targetId) =>
      target(
        "transfer_ownership",
        read().members.some(
          (member) => memberAccount(member.user_id) === targetId,
        )
          ? targetId
          : memberAccount(targetId),
      ),
    leaveChannel: (deleteHistory) =>
      submit(activeGroup(), {
        kind: "leave_account",
        ...(deleteHistory === undefined
          ? {}
          : { delete_history: deleteHistory }),
      }),
    acceptChannelInvite: () =>
      submit(activeGroup(), { kind: "accept_invitation" }),
    declineChannelInvite: () =>
      submit(activeGroup(), { kind: "decline_invitation" }),
    dissolveChannel: (group) => submit(group, { kind: "dissolve" }),
    renameChannel: (group, name) => submit(group, { kind: "rename", name }),
    setChannelHistoryPolicy: (group, policy) =>
      submit(group, { kind: "set_history_policy", policy }),
    reconcileCommitted,
  };
}
