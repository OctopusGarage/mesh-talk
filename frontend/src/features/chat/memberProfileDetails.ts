import type { PeerInfo } from "@/lib/types";

export interface MemberProfileTarget {
  name: string;
  accountId?: string | null;
  deviceId?: string | null;
  isSelf?: boolean;
}

export interface MemberProfileDevice {
  id: string;
  endpoint: string | null;
  postOffice: boolean;
}

export function memberProfileDevices(
  target: Pick<MemberProfileTarget, "accountId" | "deviceId">,
  peers: PeerInfo[],
): MemberProfileDevice[] {
  const devices = peers
    .filter(
      (peer) =>
        (target.accountId && peer.account_id === target.accountId) ||
        (!target.accountId &&
          target.deviceId &&
          peer.user_id === target.deviceId),
    )
    .map((peer) => ({
      id: peer.user_id,
      endpoint: peer.addr || null,
      postOffice: peer.post_office,
    }));
  if (
    target.deviceId &&
    !devices.some((device) => device.id === target.deviceId)
  )
    devices.push({
      id: target.deviceId,
      endpoint: null,
      postOffice: false,
    });
  return devices;
}
