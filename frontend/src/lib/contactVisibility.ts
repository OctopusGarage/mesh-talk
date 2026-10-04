import type { PeerInfo, SearchHitInfo } from "./types";

export function visiblePeers(
  peers: PeerInfo[],
  hidden: Record<string, unknown>,
) {
  return peers.filter((p) => !hidden[p.account_id ?? p.user_id]);
}

export function visibleSearchHits(
  hits: SearchHitInfo[],
  peers: PeerInfo[],
  hidden: Record<string, unknown>,
  loaded: boolean,
) {
  return hits.filter(
    (h) =>
      h.is_channel ||
      (loaded &&
        !hidden[
          h.account_id ??
            peers.find((p) => p.user_id === h.target)?.account_id ??
            h.target
        ]),
  );
}
