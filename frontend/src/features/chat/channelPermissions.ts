import type { ChannelInfo } from "@/lib/types";

type ChannelAuthority = Pick<
  ChannelInfo,
  "owner" | "can_manage" | "shared_reference"
>;

/** Native account authority wins; absent fields retain legacy device-owner IPC. */
export function canManageChannel(
  channel: ChannelAuthority | null | undefined,
  myDeviceId: string,
): boolean {
  if (!channel || channel.shared_reference) return false;
  if (channel.can_manage !== undefined) return channel.can_manage === true;
  return myDeviceId !== "" && channel.owner === myDeviceId;
}
