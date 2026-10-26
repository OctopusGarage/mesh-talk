import type { ChannelMemberInfo, PeerInfo } from "@/lib/types";
import type { RosterProjection } from "@/lib/rosterProjection";

/** A person is an account; native channel metadata owns management authority. */
export interface ChannelMemberAccount {
  key: string;
  accountId: string | null;
  devices: ChannelMemberInfo[];
  representative: ChannelMemberInfo;
}

export function channelMemberAccounts(
  members: readonly ChannelMemberInfo[],
  roster: RosterProjection,
  local: { deviceId: string; accountId: string; ownerId: string },
): ChannelMemberAccount[] {
  const groups = new Map<string, ChannelMemberAccount>();
  for (const member of members) {
    const accountId =
      roster.accountForMember(member) ||
      (member.user_id === local.deviceId ? local.accountId || null : null);
    const key = accountId ? `account:${accountId}` : `device:${member.user_id}`;
    const previous = groups.get(key);
    if (previous) previous.devices.push(member);
    else
      groups.set(key, {
        key,
        accountId,
        devices: [member],
        representative: member,
      });
  }
  for (const group of groups.values()) {
    // Stable across roster ordering, with the exact signed owner preferred so
    // profile inspection and existing action selectors keep their device target.
    group.devices.sort((a, b) => a.user_id.localeCompare(b.user_id));
    group.representative =
      group.devices.find((device) => device.user_id === local.ownerId) ??
      group.devices.find((device) => device.user_id === local.deviceId) ??
      group.devices[0];
  }
  // Account ordering must not depend on which device supplied the roster.
  return [...groups.values()].sort((a, b) => a.key.localeCompare(b.key));
}

/** Invite one exact device per new account; existing accounts expose devices separately. */
export function channelInviteAccounts(peers: readonly PeerInfo[]): PeerInfo[] {
  const groups = new Map<string, PeerInfo>();
  for (const peer of peers) {
    const key = peer.account_id
      ? `account:${peer.account_id}`
      : `device:${peer.user_id}`;
    const previous = groups.get(key);
    if (!previous || peer.user_id.localeCompare(previous.user_id) < 0)
      groups.set(key, peer);
  }
  return [...groups.values()];
}
