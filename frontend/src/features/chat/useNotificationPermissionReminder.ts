import { useEffect, useState } from "react";
import {
  useNotificationPermission,
  watchNotificationPermission,
} from "@/lib/notify";
import { useChat } from "@/store/chat";
import { useSettings } from "@/store/settings";

/** Observe admitted text/sticker/file intents without entering their send path.
 * Keep this hook above ConversationView's early returns so navigation retains dismissal. */
export function useNotificationPermissionReminder(identity: string) {
  const enabled = useSettings((s) => s.notificationsEnabled);
  const status = useNotificationPermission((s) => s.permission?.status);
  const [attemptedIdentity, setAttemptedIdentity] = useState<string | null>(
    null,
  );
  const [dismissedIdentity, setDismissedIdentity] = useState<string | null>(
    null,
  );
  const dismissed = dismissedIdentity === identity;
  useEffect(() => {
    if (enabled !== true) return;
    return watchNotificationPermission();
  }, [enabled, identity]);
  useEffect(() => {
    if (enabled !== true || dismissed) return;
    let current = true;
    const unsubscribe = useChat.subscribe((state, previous) => {
      if (!state.ready || state.intents === previous.intents) return;
      const submitted = Object.entries(state.intents).some(
        ([id, intent]) =>
          !previous.intents[id] &&
          !(
            intent.conversation.kind === "account" &&
            intent.conversation.id === state.myAccountId
          ),
      );
      if (!submitted) return;
      void useNotificationPermission
        .getState()
        .refresh()
        .then(() => {
          if (current) setAttemptedIdentity(identity);
        });
    });
    return () => {
      current = false;
      unsubscribe();
    };
  }, [enabled, dismissed, identity]);
  return {
    visible:
      enabled === true &&
      attemptedIdentity === identity &&
      !dismissed &&
      (status === "denied" || status === "prompt" || status === "limited"),
    dismiss: () => setDismissedIdentity(identity),
  };
}
