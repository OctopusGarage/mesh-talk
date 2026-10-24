import type {
  ChannelInfo,
  ChannelMemberInfo,
  ChannelMembersInfo,
} from "@/lib/types";
import { errorMessage } from "@/lib/error";
import { convKey } from "./conversationState";
import type { Conversation } from "./conversationModel";

/** Channel coordination owns only its projection and navigation effects. Signed
 * membership authority remains in core; session authority is supplied by the host store. */
export interface ChannelActionState {
  active: Conversation | null;
  activeRequest: number;
  channels: ChannelInfo[];
  members: ChannelMemberInfo[];
  channelOwner: string;
  unread: Record<string, number>;
}

interface ChannelCommands {
  createChannel(name: string, memberIds: string[]): Promise<string>;
  addChannelMember(channelId: string, memberId: string): Promise<void>;
  removeChannelMember(channelId: string, memberId: string): Promise<void>;
  removeChannelAccountMembers(
    channelId: string,
    accountId: string,
  ): Promise<void>;
  transferChannelOwner(channelId: string, memberId: string): Promise<void>;
  channelMembers(channelId: string): Promise<ChannelMembersInfo>;
  leaveChannel(channelId: string): Promise<void>;
  acceptChannelInvite(channelId: string): Promise<void>;
  dissolveChannel(channelId: string): Promise<void>;
  renameChannel(channelId: string, name: string): Promise<void>;
}

interface ChannelActionContext {
  read(): ChannelActionState;
  write(
    patch:
      | Partial<ChannelActionState>
      | ((state: ChannelActionState) => Partial<ChannelActionState>),
  ): void;
  capture(): { current(): boolean };
  commands: ChannelCommands;
  refreshRoster(): Promise<void>;
  open(conversation: Conversation): Promise<void>;
  reportError(message: string): void;
}

export interface ChannelActions {
  createChannel(name: string, memberIds: string[]): Promise<boolean>;
  addMember(memberId: string): Promise<boolean>;
  removeMember(memberId: string): Promise<boolean>;
  removeMemberAccount(accountId: string): Promise<boolean>;
  transferChannelOwner(memberId: string): Promise<boolean>;
  leaveChannel(): Promise<boolean>;
  acceptChannelInvite(): Promise<boolean>;
  dissolveChannel(channelId: string): Promise<boolean>;
  renameChannel(channelId: string, name: string): Promise<void>;
}

export function createChannelActions(
  context: ChannelActionContext,
): ChannelActions {
  const { read, write, commands, refreshRoster, open, reportError } = context;

  function capture() {
    const lease = context.capture();
    return {
      current: lease.current,
      set: (patch: Parameters<typeof write>[0]) => {
        if (lease.current()) write(patch);
      },
      error: (prefix: string, error: unknown) => {
        if (lease.current()) reportError(`${prefix}: ${errorMessage(error)}`);
      },
    };
  }

  async function mutateMember(
    memberId: string,
    mutate: (channelId: string, memberId: string) => Promise<void>,
    errorPrefix: string,
  ) {
    const lease = capture();
    const conversation = read().active;
    if (!lease.current() || conversation?.kind !== "channel") return false;
    const request = read().activeRequest + 1;
    lease.set({ activeRequest: request });
    const current = () => lease.current() && read().activeRequest === request;
    try {
      await mutate(conversation.id, memberId);
      if (!current()) return false;
      const info = await commands.channelMembers(conversation.id);
      if (!current()) return false;
      lease.set({ members: info.members, channelOwner: info.owner });
      void refreshRoster();
      return true;
    } catch (error) {
      if (current()) lease.error(errorPrefix, error);
      return false;
    }
  }

  return {
    async createChannel(name, memberIds) {
      const lease = capture();
      if (!lease.current()) return false;
      const request = read().activeRequest + 1;
      lease.set({ activeRequest: request });
      const current = () => lease.current() && read().activeRequest === request;
      const id = await commands.createChannel(name, memberIds);
      if (!current()) return false;
      await refreshRoster();
      if (!current()) return false;
      await open({ kind: "channel", id, name });
      return lease.current();
    },

    addMember: (memberId) =>
      mutateMember(memberId, commands.addChannelMember, "Couldn't add member"),
    removeMember: (memberId) =>
      mutateMember(
        memberId,
        commands.removeChannelMember,
        "Couldn't remove member",
      ),
    removeMemberAccount: (accountId) =>
      mutateMember(
        accountId,
        commands.removeChannelAccountMembers,
        "Couldn't remove member",
      ),
    transferChannelOwner: (memberId) =>
      mutateMember(
        memberId,
        commands.transferChannelOwner,
        "Couldn't transfer channel ownership",
      ),

    async leaveChannel() {
      const lease = capture();
      const conversation = read().active;
      if (!lease.current() || conversation?.kind !== "channel") return false;
      try {
        await commands.leaveChannel(conversation.id);
        if (!lease.current()) return false;
        lease.set((state) => {
          const unread = { ...state.unread };
          delete unread[convKey(conversation)];
          const leavingActive =
            state.active?.kind === "channel" &&
            state.active.id === conversation.id;
          return {
            active: leavingActive ? null : state.active,
            members: leavingActive ? [] : state.members,
            channelOwner: leavingActive ? "" : state.channelOwner,
            unread,
          };
        });
        await refreshRoster();
        return lease.current();
      } catch (error) {
        lease.error("Couldn't leave channel", error);
        return false;
      }
    },

    async acceptChannelInvite() {
      const lease = capture();
      const conversation = read().active;
      if (!lease.current() || conversation?.kind !== "channel") return false;
      const request = read().activeRequest;
      const current = () =>
        lease.current() &&
        read().activeRequest === request &&
        read().active?.id === conversation.id;
      try {
        await commands.acceptChannelInvite(conversation.id);
        if (!lease.current()) return false;
        await refreshRoster();
        if (current()) {
          try {
            const info = await commands.channelMembers(conversation.id);
            if (current())
              lease.set({ members: info.members, channelOwner: info.owner });
          } catch {
            // Acceptance succeeded; opening the member list can retry its details.
          }
        }
        return lease.current();
      } catch (error) {
        lease.error("Couldn't accept invitation", error);
        return false;
      }
    },

    async dissolveChannel(channelId) {
      const lease = capture();
      if (!lease.current()) return false;
      try {
        await commands.dissolveChannel(channelId);
        if (!lease.current()) return false;
        lease.set((state) => ({
          channels: state.channels.map((channel) =>
            channel.channel_id === channelId
              ? { ...channel, dissolved: true }
              : channel,
          ),
        }));
        await refreshRoster();
        return lease.current();
      } catch (error) {
        lease.error("Couldn't dissolve channel", error);
        return false;
      }
    },

    async renameChannel(channelId, name) {
      const lease = capture();
      if (!lease.current()) return;
      try {
        await commands.renameChannel(channelId, name);
        if (!lease.current()) return;
        await refreshRoster();
        if (!lease.current()) return;
        const active = read().active;
        if (active?.kind === "channel" && active.id === channelId)
          lease.set({ active: { ...active, name } });
      } catch (error) {
        lease.error("Couldn't rename channel", error);
      }
    },
  };
}
