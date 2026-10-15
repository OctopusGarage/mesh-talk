import type { PeerInfo, SearchHitInfo } from "./types";
import { isContactPeer, rosterProjection } from "./rosterProjection";
export { isContactPeer } from "./rosterProjection";

export function visiblePeers(
  peers: PeerInfo[],
  hidden: Record<string, unknown>,
) {
  return peers.filter(
    (p) => isContactPeer(p) && !hidden[p.account_id ?? p.user_id],
  );
}

export function visibleSearchHits(
  hits: SearchHitInfo[],
  peers: PeerInfo[],
  hidden: Record<string, unknown>,
  loaded: boolean,
) {
  const roster = rosterProjection(peers);
  return hits.filter(
    (h) =>
      h.is_channel ||
      (loaded &&
        !hidden[h.account_id ?? roster.accountForDevice(h.target) ?? h.target]),
  );
}
