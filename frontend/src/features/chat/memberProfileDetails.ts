import type { PeerInfo } from "@/lib/types";
import { rosterProjection } from "@/lib/rosterProjection";

export interface MemberProfileTarget {
  name: string;
  canonicalName?: string;
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
  const roster = rosterProjection(peers);
  const matches = target.accountId
    ? roster.devicesForAccount(target.accountId)
    : target.deviceId
      ? [roster.peerForDevice(target.deviceId)].filter(
          (peer): peer is PeerInfo => peer !== undefined,
        )
      : [];
  const devices = matches.map((peer) => ({
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
