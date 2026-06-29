import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Sidebar } from "./Sidebar";
import { ConversationView } from "./ConversationView";
import { chat as chatApi } from "@/lib/api";
import { ensureNotificationPermission } from "@/lib/notify";
import { useChat } from "@/store/chat";
import { usePresence } from "@/store/presence";

export function ChatApp() {
  const { t } = useTranslation();
  const start = useChat((s) => s.start);
  const startPresence = usePresence((s) => s.start);
  const error = useChat((s) => s.error);
  const clearError = useChat((s) => s.clearError);
  // Total unread across all conversations → the OS app-icon badge (dock/taskbar count).
  // A primitive sum keeps the selector stable (re-renders only when the total changes).
  const totalUnread = useChat((s) =>
    Object.values(s.unread).reduce((a, b) => a + b, 0),
  );

  // Request notification authorization once at startup. On macOS the dock unread badge only
  // renders if the app is authorized for notification badges, so this is what makes the badge
  // work out of the box (rather than requiring the user to enable it in System Settings).
  useEffect(() => {
    void ensureNotificationPermission();
  }, []);

  useEffect(() => {
    chatApi.setBadge(totalUnread).catch(() => {});
  }, [totalUnread]);

  useEffect(() => {
    const stop = start();
    return stop;
  }, [start]);

  // Presence polls on its own slow interval into an isolated store — kept apart from the
  // chat store so a presence tick never re-renders the virtualized message list.
  useEffect(() => {
    const stop = startPresence();
    return stop;
  }, [startPresence]);

  return (
    <div
      data-testid="chat-shell"
      className="relative flex h-full overflow-hidden"
    >
      <Sidebar />
      <ConversationView />
      {error && (
        <div
          role="alert"
          className="absolute bottom-4 left-1/2 z-50 flex -translate-x-1/2 items-center gap-3 rounded-md bg-destructive px-4 py-2 text-sm text-destructive-foreground shadow-lg"
        >
          <span>{error}</span>
          <button
            type="button"
            onClick={clearError}
            aria-label={t("conversation.dismissError")}
            className="font-bold opacity-80 hover:opacity-100"
          >
            ×
          </button>
        </div>
      )}
    </div>
  );
}
