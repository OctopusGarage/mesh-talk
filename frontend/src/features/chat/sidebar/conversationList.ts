import type { TFunction } from "i18next";
import type {
  AccountInfo,
  ChannelInfo,
  FavoriteInfo,
  PeerInfo,
  PresenceMap,
} from "@/lib/types";
import { isContactPeer, peerAccountKey } from "@/lib/rosterProjection";
import { displayIdentity } from "@/lib/displayIdentity";
import type { Conversation } from "@/store/chat";

/** The same row contract drives pinned, ordinary and virtualized sections. */
export interface ConversationListRow {
  id: string;
  conv: Conversation;
  pinned: boolean;
  channel: boolean;
  subtitle: string;
  prioritizeSubtitle: boolean;
  personal?: boolean;
  /** Rename edits the announced name, not the displayed personal alias. */
  renameName: string;
}

type Favorites = Readonly<
  Record<string, Pick<FavoriteInfo, "pinned" | "custom_alias" | "hidden">>
>;

/** Pure projection: stable source order within pins, contacts, groups and restoration. */
export function conversationList(
  accounts: readonly AccountInfo[],
  channels: readonly ChannelInfo[],
  favorites: Favorites,
  t: TFunction,
  ownAccount?: string,
  personalReady = true,
) {
  const pinnedRows: ConversationListRow[] = [];
  const accountRows: ConversationListRow[] = [];
  const channelRows: ConversationListRow[] = [];
  const hiddenChannels: ConversationListRow[] = [];
  const previousAccountRows: ConversationListRow[] = [];
  if (ownAccount && personalReady) {
    accountRows.push({
      id: ownAccount,
      conv: { kind: "account", id: ownAccount, name: t("personal.title") },
      renameName: t("personal.title"),
      channel: false,
      personal: true,
      pinned: false,
      subtitle: t("personal.description"),
      prioritizeSubtitle: false,
    });
  }
  for (const a of accounts) {
    const id = a.account_id;
    if (id === ownAccount) continue;
    const identity = displayIdentity({ accountId: id, accounts, favorites });
    const renameName = identity.canonicalName;
    const row: ConversationListRow = {
      id,
      renameName,
      channel: false,
      conv: {
        kind: "account",
        id,
        name: identity.name,
      },
      pinned: favorites[id]?.pinned ?? false,
      subtitle: t("sidebar.devices", { count: a.device_count }),
      prioritizeSubtitle: false,
    };
    (row.pinned ? pinnedRows : accountRows).push(row);
  }
  for (const c of channels) {
    const id = c.channel_id;
    const row: ConversationListRow = {
      id,
      channel: true,
      renameName: c.name,
      conv: {
        kind: "channel",
        id,
        name: favorites[id]?.custom_alias || c.name,
      },
      pinned: !c.previous_account && (favorites[id]?.pinned ?? false),
      subtitle: c.previous_account
        ? t("conversation.previousAccountChannel")
        : c.shared_reference
          ? t("conversation.sharedReference")
          : c.dissolved
            ? t("conversation.dissolvedChannel")
            : c.archived
              ? t("conversation.archivedChannel")
              : c.left
                ? t("conversation.leftChannel")
                : c.pending_invite
                  ? t("conversation.pendingInvite")
                  : t("conversation.members", { count: c.member_count }),
      prioritizeSubtitle: !!(
        c.previous_account ||
        c.shared_reference ||
        c.dissolved ||
        c.archived ||
        c.left ||
        c.pending_invite
      ),
    };
    if (c.previous_account && !favorites[id]?.hidden) {
      previousAccountRows.push(row);
      continue;
    }
    (favorites[id]?.hidden
      ? hiddenChannels
      : row.pinned
        ? pinnedRows
        : channelRows
    ).push(row);
  }
  return {
    pinnedRows,
    accountRows,
    channelRows,
    hiddenChannels,
    previousAccountRows,
    listSize: pinnedRows.length + accountRows.length + channelRows.length,
  };
}

/** Hidden people still prevent a misleading "stranded" prompt; relays never count. */
export function sidebarPeopleCounts(
  peers: readonly PeerInfo[],
  presence: PresenceMap,
  ownAccount: string | null,
  hidden: Readonly<Record<string, unknown>>,
  policyLoaded: boolean,
) {
  const network = new Set<string>();
  const visible = new Set<string>();
  for (const peer of peers) {
    if (!isContactPeer(peer)) continue;
    const account = peerAccountKey(peer);
    if (account === ownAccount || !presence[account]?.online) continue;
    network.add(account);
    if (policyLoaded && !hidden[account]) visible.add(account);
  }
  return { networkPeople: network.size, onlinePeople: visible.size };
}
