import { useSyncExternalStore } from "react";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import {
  announcementSeen,
  subscribeAnnouncementSeen,
} from "@/lib/groupAnnouncements";
import { displayIdentity } from "@/lib/displayIdentity";

/** Reuse ordinary account labels and private aliases without displaying a raw key. */
export function useAnnouncementAuthor(author: string | null | undefined) {
  const ownName = useAuth(
    (s) => s.user?.display_name || s.user?.username || "",
  );
  return useChat((s) => {
    if (!author) return "";
    if (author === s.myAccountId) return ownName;
    return displayIdentity({
      accountId: author,
      fallbackName: s.members?.find(
        (m) => m.account_id === author || m.user_id === author,
      )?.name,
      accounts: s.accounts,
      peers: s.peers,
      favorites: s.favorites,
    }).name;
  });
}

export function useNewGroupAnnouncement(channelId: string) {
  const owner = useAuth((s) => s.user?.id ?? "");
  const account = useChat((s) => s.myAccountId);
  const channel = useChat((s) =>
    s.channels.find((entry) => entry.channel_id === channelId),
  );
  const seen = useSyncExternalStore(
    subscribeAnnouncementSeen,
    () => announcementSeen(owner, account, channelId),
    () => "",
  );
  const value = channel?.metadata?.announcement;
  return (
    !!owner &&
    !!account &&
    !channel?.shared_reference &&
    !channel?.pending_invite &&
    !channel?.left &&
    !channel?.dissolved &&
    !channel?.archived &&
    !!value?.content &&
    value.revision !== seen &&
    value.author !== account
  );
}
