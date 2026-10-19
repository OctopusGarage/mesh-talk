import {
  startAutomaticDownloads,
  useAutomaticDownloads,
} from "@/lib/automaticDownloads";
import { PresenceAvailability } from "./PresenceAvailability";
import { useEffect, useState } from "react";
import { AnimatePresence, motion } from "framer-motion";
import { CircleAlert, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Sidebar } from "./Sidebar";
import { ConversationView } from "./ConversationView";
import { CallDialog } from "./CallDialog";
import { chat as chatApi } from "@/lib/api";
import { ensureNotificationPermission } from "@/lib/notify";
import { savePendingUnread, saveUnread } from "@/lib/unreadStorage";
import { useChat } from "@/store/chat";
import { usePresence } from "@/store/presence";
import { useSettings } from "@/store/settings";
import { useAuth } from "@/store/auth";
import { useContactPolicy } from "@/store/contactPolicy";
import { usePrivacy } from "@/store/privacy";
import { ease, useMotionOK } from "@/lib/motion";
import { startLinkedAppearance } from "@/lib/linkedAppearance";
import { startLinkedContacts } from "@/lib/linkedContacts";

export function ChatApp() {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const start = useChat((s) => s.start);
  const owner = useAuth((s) => s.user?.id);
  const generation = useAuth((s) => s.generation);
  const ready = useChat((s) => s.ready);
  const presenceRun = useChat((s) => s.runEpoch);
  const presenceIdentity = useChat((s) => s.identityEpoch);
  const downloadPersistenceError = useAutomaticDownloads(
    (s) => s.persistenceError,
  );
  const [mediaWarning, setMediaWarning] = useState(false);
  const active = useChat((s) => s.active);
  const peers = useChat((s) => s.peers);
  const accountId = useChat((s) => s.myAccountId);
  const unreadOwner = useChat((s) => s.unreadOwner);
  useEffect(() => {
    if (owner) void useContactPolicy.getState().load(owner);
    return () => {
      useContactPolicy.getState().reset();
      usePrivacy.getState().reset();
    };
  }, [owner, generation]);
  useEffect(() => {
    if (owner && ready) return startLinkedAppearance();
  }, [owner, generation, ready]);
  useEffect(() => {
    if (owner && ready) return startLinkedContacts(owner);
  }, [owner, generation, ready]);
  useEffect(() => {
    if (owner && ready) void usePrivacy.getState().load(owner);
  }, [owner, generation, ready]);
  const startPresence = usePresence((s) => s.start);
  const loadSettings = useSettings((s) => s.load);
  const error = useChat((s) => s.error);
  const clearError = useChat((s) => s.clearError);
  const fileSendError =
    error?.startsWith("Couldn't send file: ") ||
    error?.startsWith("This attachment ");
  const unread = useChat((s) => s.unread);
  const markLegacyActiveRead = useChat((s) => s.markLegacyActiveRead);
  const pendingUnknownUnread = useChat((s) => s.pendingUnknownUnread);
  const totalUnread =
    Object.values(unread).reduce((sum, count) => sum + count, 0) +
    Object.values(pendingUnknownUnread).reduce((sum, count) => sum + count, 0);

  useEffect(() => {
    if (owner && owner === unreadOwner && accountId && ready) {
      saveUnread(owner, accountId, unread);
      savePendingUnread(owner, accountId, pendingUnknownUnread);
    }
  }, [owner, unreadOwner, accountId, ready, unread, pendingUnknownUnread]);

  // Request notification authorization once at startup. On macOS the dock unread badge only
  // renders if the app is authorized for notification badges, so this is what makes the badge
  // work out of the box (rather than requiring the user to enable it in System Settings).
  useEffect(() => {
    void ensureNotificationPermission();
  }, []);
  useEffect(() => {
    useSettings.getState().setOwner(owner ?? null);
    if (owner) void loadSettings();
  }, [loadSettings, owner, generation]);

  useEffect(() => {
    if (!ready) return;
    let current = true;
    void chatApi
      .mediaMigrationWarning()
      .then((warning) => {
        if (current) setMediaWarning(warning);
      })
      .catch(() => {});
    return () => {
      current = false;
    };
  }, [ready]);

  useEffect(() => {
    const activePeerDevices =
      active?.kind === "account"
        ? peers
            .filter((peer) => peer.account_id === active.id)
            .map((peer) => peer.user_id)
        : [];
    const activeChannel = active?.kind === "channel" ? active.id : null;
    void chatApi
      .setNotificationContext(activePeerDevices, activeChannel)
      .catch(() => {});
    return () => {
      void chatApi.setNotificationContext([], null).catch(() => {});
    };
  }, [active, peers, generation]);

  useEffect(() => {
    if (!owner) return;
    let focusFrame = 0;
    const syncBadge = () => {
      void chatApi.setBadge(totalUnread, owner).catch(() => {});
    };
    syncBadge();
    // macOS can clear its Dock badge on activation. Restore the sidebar total.
    const onFocus = () => {
      markLegacyActiveRead();
      cancelAnimationFrame(focusFrame);
      focusFrame = requestAnimationFrame(markLegacyActiveRead);
      const count =
        Object.values(useChat.getState().unread).reduce(
          (sum, value) => sum + value,
          0,
        ) +
        Object.values(useChat.getState().pendingUnknownUnread).reduce(
          (sum, value) => sum + value,
          0,
        );
      void chatApi.setBadge(count, owner).catch(() => {});
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onFocus);
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
      cancelAnimationFrame(focusFrame);
    };
  }, [totalUnread, owner, generation, markLegacyActiveRead]);

  useEffect(() => {
    if (!owner) return;
    const stop = start();
    return stop;
  }, [start, owner, generation]);

  useEffect(() => {
    if (owner && ready) return startAutomaticDownloads();
  }, [owner, generation, ready, presenceRun, presenceIdentity]);

  // Presence polls on its own slow interval into an isolated store — kept apart from the
  // chat store so a presence tick never re-renders the virtualized message list.
  useEffect(() => {
    if (!owner) return;
    const stop = startPresence();
    return stop;
  }, [startPresence, owner, generation, presenceRun, presenceIdentity]);

  return (
    <div
      data-testid="chat-shell"
      className="relative flex h-full overflow-hidden"
    >
      <Sidebar />
      <ConversationView />
      <CallDialog />
      <div className="absolute inset-x-4 top-3 z-40 flex flex-col gap-2">
        <PresenceAvailability />
        {downloadPersistenceError && (
          <div
            role="alert"
            className="rounded-md border border-destructive/40 bg-popover px-4 py-3 text-sm text-foreground shadow-elevation"
          >
            {t("files.queuePersistenceFailed", {
              error: downloadPersistenceError,
            })}
          </div>
        )}
        {mediaWarning && (
          <div
            role="alert"
            className="rounded-md border border-destructive/40 bg-popover px-4 py-3 text-sm text-foreground shadow-elevation"
          >
            {t("settings.mediaMigrationWarning")}
          </div>
        )}
      </div>
      <AnimatePresence>
        {error && (
          <motion.div
            role="alert"
            initial={{
              opacity: 0,
              transform: motionOK ? "translateY(-6px)" : "none",
            }}
            animate={{
              opacity: 1,
              transform: motionOK ? "translateY(0px)" : "none",
            }}
            exit={{
              opacity: 0,
              transform: motionOK ? "translateY(-6px)" : "none",
            }}
            transition={{ duration: motionOK ? 0.18 : 0.08, ease }}
            className="absolute right-4 top-20 z-50 flex max-w-[min(26rem,calc(100vw-2rem))] items-start gap-3 rounded-lg border border-destructive/35 bg-popover px-3 py-3 text-[13px] text-foreground shadow-elevation"
          >
            <CircleAlert
              className="mt-0.5 h-4 w-4 shrink-0 text-destructive"
              aria-hidden="true"
            />
            <span className="min-w-0 flex-1">
              <strong className="block font-semibold">
                {t(
                  fileSendError
                    ? "message.fail.unknown"
                    : "redesign.actionFailed",
                )}
              </strong>
              <span className="mt-0.5 block break-words text-muted-foreground">
                {fileSendError
                  ? error.replace(/^Couldn't send file: /, "")
                  : error}
              </span>
            </span>
            <button
              type="button"
              onClick={clearError}
              aria-label={t("conversation.dismissError")}
              className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring [@media(hover:none)]:h-11 [@media(hover:none)]:w-11"
            >
              <X className="h-4 w-4" aria-hidden="true" />
            </button>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}
