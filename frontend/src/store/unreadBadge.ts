import { create } from "zustand";
import { chat } from "@/lib/api/chat";
import { createBadgeSync } from "@/lib/badgeSync";
import { captureRuntimeOwner } from "./ownership";
import { useChat } from "./chat";

export function unreadTotal(state: {
  unread: Record<string, number>;
  pendingUnknownUnread: Record<string, number>;
}): number {
  return [
    ...Object.values(state.unread),
    ...Object.values(state.pendingUnknownUnread),
  ].reduce(
    (sum, count) =>
      sum + (Number.isSafeInteger(count) && count > 0 ? count : 0),
    0,
  );
}

export const useUnreadBadge = create<{
  error: string | null;
  retry: () => void;
}>(() => ({ error: null, retry: () => {} }));

/** Bind badge work once to the exact desktop/account/runtime lifetime. */
export function startUnreadBadge(owner: string): () => void {
  const lease = captureRuntimeOwner();
  const account = useChat.getState().myAccountId;
  const current = () =>
    lease.owner === owner &&
    lease.current() &&
    useChat.getState().myAccountId === account;
  const sync = createBadgeSync({
    current,
    write: (count) => chat.setBadge(Math.min(0xffffffff, count), owner),
    status: (error) => {
      if (current()) useUnreadBadge.setState({ error });
    },
  });
  const retry = () => {
    if (current()) sync.update(unreadTotal(useChat.getState()), true);
  };
  useUnreadBadge.setState({ error: null, retry });
  const unsubscribe = useChat.subscribe((state, previous) => {
    if (
      state.unread !== previous.unread ||
      state.pendingUnknownUnread !== previous.pendingUnknownUnread
    )
      sync.update(unreadTotal(state));
  });
  let frame = 0;
  const onFocus = () => {
    if (!current()) return;
    useChat.getState().markLegacyActiveRead();
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => {
      if (!current()) return;
      useChat.getState().markLegacyActiveRead();
      retry();
    });
    retry();
  };
  window.addEventListener("focus", onFocus);
  document.addEventListener("visibilitychange", onFocus);
  retry();
  return () => {
    unsubscribe();
    sync.stop();
    cancelAnimationFrame(frame);
    window.removeEventListener("focus", onFocus);
    document.removeEventListener("visibilitychange", onFocus);
    if (useUnreadBadge.getState().retry === retry)
      useUnreadBadge.setState({ error: null, retry: () => {} });
  };
}
