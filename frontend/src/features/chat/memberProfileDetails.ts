import type { PeerInfo, PresenceEvidence, PresenceInfo } from "@/lib/types";
import { rosterProjection } from "@/lib/rosterProjection";

export interface MemberDiscoverySource {
  source: PresenceEvidence["source"];
  postOffice?: PresenceEvidence["post_office"];
}

/** Show the closest identified visible introducer per device, never a guessed origin. */
export function memberDiscoverySources(
  presence: PresenceInfo,
): MemberDiscoverySource[] {
  const sources = new Map<string, MemberDiscoverySource>();
  for (const device of presence.devices ?? []) {
    const fresh = device.evidence.filter((entry) => entry.expires_in_secs > 0);
    const offices = fresh
      .filter(
        (entry) =>
          entry.source === "directory" &&
          entry.post_office?.user_id &&
          entry.post_office.name &&
          Number.isInteger(entry.origin_hops) &&
          entry.origin_hops! >= 0 &&
          entry.origin_hops! <= 16,
      )
      .sort(
        (a, b) =>
          a.origin_hops! - b.origin_hops! ||
          a.post_office!.user_id!.localeCompare(b.post_office!.user_id!),
      );
    const selected = offices[0];
    for (const evidence of fresh) {
      const postOffice =
        evidence.source === "directory" ? selected?.post_office : undefined;
      const key = JSON.stringify([
        evidence.source,
        postOffice?.user_id ?? null,
      ]);
      if (!sources.has(key))
        sources.set(key, {
          source: evidence.source,
          ...(postOffice ? { postOffice } : {}),
        });
    }
  }
  return [...sources.values()];
}

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
