import type { AccountInfo, FavoriteInfo, PeerInfo } from "./types";
import { rosterProjection } from "./rosterProjection";
import { shortId } from "./format";

/** Display labels never replace canonical profile data. Account names are supplied
 * most-recent first by the durable identity directory, independently of presence. */
export function displayIdentity({
  accountId,
  deviceId,
  fallbackName,
  accounts = [],
  peers = [],
  favorites = {},
}: {
  accountId?: string | null;
  deviceId?: string | null;
  fallbackName?: string | null;
  accounts?: readonly AccountInfo[];
  peers?: readonly PeerInfo[];
  favorites?: Readonly<Record<string, Pick<FavoriteInfo, "custom_alias">>>;
}) {
  const roster = rosterProjection(peers);
  const verifiedAccount =
    accountId || (deviceId ? roster.accountForDevice(deviceId) : null);
  const id = verifiedAccount || deviceId || "";
  const account = accounts.find(
    (entry) => entry.account_id === verifiedAccount,
  );
  const devicePeer = deviceId ? roster.peerForDevice(deviceId) : undefined;
  const peer =
    devicePeer &&
    (!verifiedAccount || devicePeer.account_id === verifiedAccount)
      ? devicePeer
      : verifiedAccount
        ? roster.contactForAccount(verifiedAccount)
        : undefined;
  const canonicalName =
    [...(account?.names ?? []), peer?.name, fallbackName]
      .find((name) => name?.trim() && name !== id && name !== deviceId)
      ?.trim() || shortId(id);
  const alias = favorites[id]?.custom_alias?.trim() || null;
  return {
    id,
    accountId: verifiedAccount,
    canonicalName,
    alias,
    name: alias || canonicalName,
  };
}
