import type { ChannelInfo } from "@/lib/types";

type ChannelAuthority = Pick<
  ChannelInfo,
  | "owner"
  | "can_manage"
  | "shared_reference"
  | "account_access"
  | "previous_account"
>;

/** Account role is independent of signer availability. Legacy IPC only applies when account access is absent. */
export function canManageChannel(
  channel: ChannelAuthority | null | undefined,
  myDeviceId: string,
): boolean {
  if (!channel || channel.previous_account) return false;
  if (channel.account_access !== undefined)
    return channel.account_access?.role === "owner";
  if (channel.shared_reference) return false;
  if (channel.can_manage !== undefined) return channel.can_manage === true;
  return myDeviceId !== "" && channel.owner === myDeviceId;
}

type ChannelParticipation = Partial<
  Pick<
    ChannelInfo,
    | "account_access"
    | "shared_reference"
    | "pending_invite"
    | "left"
    | "dissolved"
    | "archived"
    | "previous_account"
  >
>;
export function canParticipateChannel(
  channel: ChannelParticipation | null | undefined,
): boolean {
  if (!channel || channel.previous_account) return false;
  if (channel.account_access !== undefined)
    return (
      channel.account_access?.role === "owner" ||
      channel.account_access?.role === "member"
    );
  return (
    !channel.shared_reference &&
    !channel.pending_invite &&
    !channel.left &&
    !channel.dissolved &&
    !channel.archived
  );
}
export function isChannelInvitation(
  channel: ChannelParticipation | null | undefined,
): boolean {
  if (!channel || channel.previous_account) return false;
  if (channel.account_access !== undefined)
    return channel.account_access?.role === "invited";
  return channel.pending_invite === true;
}
