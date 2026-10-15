import type { PeerInfo } from "./types";

/** Only direct LAN relays are meaningful as a manual local preference. */
export function localPostOffices(peers: readonly PeerInfo[]): PeerInfo[] {
  return peers
    .filter((peer) => peer.post_office && peer.discovered_on_lan)
    .sort(
      (a, b) =>
        a.name.localeCompare(b.name) || a.user_id.localeCompare(b.user_id),
    );
}

/** Keep a previously saved remote choice visible without offering it anew. */
export function savedRemotePostOffice(
  peers: readonly PeerInfo[],
  preferredId: string,
): PeerInfo | null {
  if (!preferredId) return null;
  return (
    peers.find(
      (peer) =>
        peer.user_id === preferredId &&
        peer.post_office &&
        !peer.discovered_on_lan,
    ) ?? null
  );
}
