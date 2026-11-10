import type { AccountInfo, ChannelInfo, FavoriteInfo } from "@/lib/types";
import { displayIdentity } from "@/lib/displayIdentity";
import type { Conversation } from "@/store/chat";

export interface UnreadSource {
  key: string;
  id: string;
  count: number;
  name: string;
  hidden: boolean;
  sender?: string;
  conversation: Conversation | null;
}

/** The review and app badge cover the same counters, even without directory rows. */
export function unreadSources(
  state: {
    myAccountId?: string;
    unread: Record<string, number>;
    pendingUnknownUnread: Record<string, number>;
    accounts: readonly AccountInfo[];
    channels: readonly Pick<ChannelInfo, "channel_id" | "name">[];
    favorites: Readonly<Record<string, Partial<FavoriteInfo>>>;
  },
  hiddenContacts: Readonly<Record<string, unknown>>,
): UnreadSource[] {
  const accounts = new Map(
    state.accounts.map((account) => [account.account_id, account]),
  );
  const channels = new Map(
    state.channels.map((channel) => [channel.channel_id, channel]),
  );
  const valid = (count: number) => Number.isSafeInteger(count) && count > 0;
  return [
    ...Object.entries(state.unread)
      .filter(([, count]) => valid(count))
      .map(([key, count]) => {
        const separator = key.indexOf(":");
        const kind = key.slice(0, separator);
        const id = key.slice(separator + 1);
        const known = kind === "account" ? accounts.get(id) : channels.get(id);
        const name =
          state.favorites[id]?.custom_alias ||
          (kind === "account"
            ? accounts.has(id)
              ? displayIdentity({
                  accountId: id,
                  accounts: [accounts.get(id)!],
                }).name
              : ""
            : channels.get(id)?.name || "");
        return {
          key,
          id,
          count,
          name,
          hidden:
            kind === "channel"
              ? Boolean(state.favorites[id]?.hidden)
              : Boolean(hiddenContacts[id]),
          conversation:
            (known || (kind === "account" && id === state.myAccountId)) &&
            (kind === "account" || kind === "channel")
              ? ({ kind, id, name } as Conversation)
              : null,
        };
      }),
    ...Object.entries(state.pendingUnknownUnread)
      .filter(([, count]) => valid(count))
      .map(([sender, count]) => ({
        key: `pending:${sender}`,
        id: sender,
        sender,
        count,
        name: "",
        hidden: false,
        conversation: null,
      })),
  ];
}
