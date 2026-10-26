import type { PeerInfo, PresenceEvidence, PresenceInfo } from "@/lib/types";
import { rosterProjection } from "@/lib/rosterProjection";

export interface MemberDiscoverySource {
  source: PresenceEvidence["source"];
  postOffice?: PresenceEvidence["post_office"];
}

/** Deduplicate people/devices without losing independently fresh introducers. */
export function memberDiscoverySources(
  presence: PresenceInfo,
): MemberDiscoverySource[] {
  const sources = new Map<string, MemberDiscoverySource>();
  for (const device of presence.devices ?? []) {
    for (const evidence of device.evidence) {
      if (evidence.expires_in_secs <= 0) continue;
      const postOffice =
        evidence.source === "directory" ? evidence.post_office : undefined;
      const key = JSON.stringify([
        evidence.source,
        postOffice?.user_id ?? postOffice?.endpoint ?? null,
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

/** Registration origin is consented evidence, never inferred from a route. */
export function memberRegistrationOrigins(
  presence: PresenceInfo,
): NonNullable<PresenceEvidence["registration_origin"]>[] {
  const origins = new Map<
    string,
    NonNullable<PresenceEvidence["registration_origin"]>
  >();
  for (const device of presence.devices ?? []) {
    for (const evidence of device.evidence) {
      const origin = evidence.registration_origin;
      if (
        evidence.source !== "directory" ||
        evidence.expires_in_secs <= 0 ||
        !origin ||
        origin.expires_in_secs <= 0
      )
        continue;
      const existing = origins.get(origin.user_id);
      if (!existing || existing.expires_in_secs < origin.expires_in_secs)
        origins.set(origin.user_id, origin);
    }
  }
  return [...origins.values()];
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
