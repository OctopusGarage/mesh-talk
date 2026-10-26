import type { PeerInfo, SearchHitInfo } from "./types";
import {
  isContactPeer,
  peerAccountKey,
  rosterProjection,
} from "./rosterProjection";
export { isContactPeer } from "./rosterProjection";

/** Presentation policy; callers bind readiness to the current local owner. */
export function isContactVisible(
  accountId: string,
  hidden: Record<string, unknown>,
  loaded: boolean,
) {
  return loaded && !hidden[accountId];
}

export function visiblePeers(
  peers: PeerInfo[],
  hidden: Record<string, unknown>,
) {
  return peers.filter(
    (p) =>
      isContactPeer(p) && isContactVisible(peerAccountKey(p), hidden, true),
  );
}

export function visibleSearchHits<T extends SearchHitInfo>(
  hits: T[],
  peers: PeerInfo[],
  hidden: Record<string, unknown>,
  loaded: boolean,
) {
  const roster = rosterProjection(peers);
  return hits.filter(
    (h) =>
      h.is_channel ||
      isContactVisible(
        h.account_id ?? roster.accountForDevice(h.target) ?? h.target,
        hidden,
        loaded,
      ),
  );
}
