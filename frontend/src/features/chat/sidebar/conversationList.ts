import {
  canParticipateChannel,
  isChannelInvitation,
} from "../channelPermissions";
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
    const participant = canParticipateChannel(c);
    const access = c.account_access;
    const roleKnown = access !== undefined;
    const accessPending = roleKnown && (!access || access.role === "unknown");
    const referenceOnly = c.shared_reference && !roleKnown;
    const dissolved = roleKnown ? access?.role === "dissolved" : c.dissolved;
    const archived = !roleKnown && c.archived;
    const left = roleKnown ? access?.role === "left" : c.left;
    const lifecycleCaption = c.previous_account
      ? t("conversation.previousAccountChannel")
      : participant
        ? null
        : accessPending
          ? t("conversation.groupAccessPending", {
              defaultValue: "Updating group access…",
            })
          : referenceOnly
            ? t("conversation.sharedReference")
            : dissolved
              ? t("conversation.dissolvedChannel")
              : archived
                ? t("conversation.archivedChannel")
                : left
                  ? t("conversation.leftChannel")
                  : isChannelInvitation(c)
                    ? t("conversation.pendingInvite")
                    : null;
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
      subtitle:
        lifecycleCaption ??
        t("conversation.members", { count: c.member_count }),
      prioritizeSubtitle: lifecycleCaption !== null,
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
