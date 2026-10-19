import type { PeerInfo } from "./types";

/** New backends distinguish direct transport from LAN discovery provenance. */
function hasDirectRoute(peer: PeerInfo): boolean {
  return peer.directly_reachable ?? peer.discovered_on_lan ?? false;
}

/** LAN and routed/Tailscale candidates qualify; indirect relay hints do not. */
export function selectablePostOffices(peers: readonly PeerInfo[]): PeerInfo[] {
  return peers
    .filter((peer) => peer.post_office && hasDirectRoute(peer))
    .sort(
      (a, b) =>
        a.name.localeCompare(b.name) || a.user_id.localeCompare(b.user_id),
    );
}

/** Preserve an unavailable preference without presenting it as a direct candidate. */
export function savedIndirectPostOffice(
  peers: readonly PeerInfo[],
  preferredId: string,
): PeerInfo | null {
  if (!preferredId) return null;
  return (
    peers.find(
      (peer) =>
        peer.user_id === preferredId &&
        peer.post_office &&
        !hasDirectRoute(peer),
    ) ?? null
  );
}
