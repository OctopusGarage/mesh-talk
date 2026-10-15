import type { ChannelMemberInfo, PeerInfo } from "./types";

/** Infrastructure remains in the raw roster for routing and diagnostics. */
export function isContactPeer(peer: Pick<PeerInfo, "post_office">): boolean {
  return !peer.post_office;
}

/** Account-scoped presentation key, with a legacy device fallback. */
export function peerAccountKey(
  peer: Pick<PeerInfo, "account_id" | "user_id">,
): string {
  return peer.account_id ?? peer.user_id;
}

/** Current roster lookups. Channel membership and offline history have separate owners. */
export interface RosterProjection {
  peerForDevice(deviceId: string): PeerInfo | undefined;
  accountForDevice(deviceId: string): string | null;
  nameForDevice(deviceId: string): string | null;
  /** An ordinary device with an advertised account binding. */
  contactForAccount(accountId: string): PeerInfo | undefined;
  devicesForAccount(accountId: string): readonly PeerInfo[];
  contactDevicesForAccount(accountId: string): readonly PeerInfo[];
  accountForMember(
    member: Pick<ChannelMemberInfo, "user_id" | "account_id">,
  ): string | null;
}

// Zustand replaces roster arrays on change. Reusing the projection for an unchanged
// array keeps message-row lookups cheap and gives all UI surfaces the same snapshot.
const cache = new WeakMap<readonly PeerInfo[], RosterProjection>();

export function rosterProjection(peers: readonly PeerInfo[]): RosterProjection {
  const cached = cache.get(peers);
  if (cached) return cached;

  const byDevice = new Map<string, PeerInfo>();
  const contactByAccount = new Map<string, PeerInfo>();
  const byAccount = new Map<string, PeerInfo[]>();
  const contactDevicesByAccount = new Map<string, PeerInfo[]>();
  for (const peer of peers) {
    if (!byDevice.has(peer.user_id)) byDevice.set(peer.user_id, peer);
    if (
      peer.account_id &&
      isContactPeer(peer) &&
      !contactByAccount.has(peer.account_id)
    )
      contactByAccount.set(peer.account_id, peer);
    if (peer.account_id) {
      const devices = byAccount.get(peer.account_id) ?? [];
      devices.push(peer);
      byAccount.set(peer.account_id, devices);
      if (isContactPeer(peer)) {
        const contacts = contactDevicesByAccount.get(peer.account_id) ?? [];
        contacts.push(peer);
        contactDevicesByAccount.set(peer.account_id, contacts);
      }
    }
  }

  const projection: RosterProjection = {
    peerForDevice: (deviceId) => byDevice.get(deviceId),
    accountForDevice: (deviceId) => byDevice.get(deviceId)?.account_id ?? null,
    nameForDevice: (deviceId) => byDevice.get(deviceId)?.name || null,
    contactForAccount: (accountId) => contactByAccount.get(accountId),
    devicesForAccount: (accountId) => byAccount.get(accountId) ?? [],
    contactDevicesForAccount: (accountId) =>
      contactDevicesByAccount.get(accountId) ?? [],
    // A channel snapshot can retain its signed account binding after discovery expires.
    accountForMember: (member) =>
      member.account_id ?? byDevice.get(member.user_id)?.account_id ?? null,
  };
  cache.set(peers, projection);
  return projection;
}
