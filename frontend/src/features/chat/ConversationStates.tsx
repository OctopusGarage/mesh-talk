import { useEffect, useState } from "react";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { CircleAlert, Loader2, MessagesSquare } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { obs } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { useAuth } from "@/store/auth";
import { useContactPolicy } from "@/store/contactPolicy";
import { useChat } from "@/store/chat";
import { DiagnosticsDialog } from "./DiagnosticsDialog";
import { HiddenContactsDialog } from "./HiddenContactsDialog";
import { OfflineConnectDialog } from "./OfflineConnectDialog";

export function EmptyState() {
  const { t } = useTranslation();
  const accounts = useChat((s) => s.accounts);
  const channels = useChat((s) => s.channels);
  const favorites = useChat((s) => s.favorites);
  const hidden = useContactPolicy((s) => s.contacts);
  const policyLoaded = useContactPolicy((s) => s.loaded);
  const policyError = useContactPolicy((s) => s.error);
  const owner = useAuth((s) => s.user?.id);
  const visibleCount =
    channels.filter((channel) => !favorites[channel.channel_id]?.hidden)
      .length +
    (policyLoaded
      ? accounts.filter((account) => !hidden[account.account_id]).length
      : 0);
  const hasConversations = visibleCount > 0;
  const hiddenOnly = policyLoaded && !hasConversations && accounts.length > 0;
  const policyUnavailable = !policyLoaded && !hasConversations;
  const [guideOpen, setGuideOpen] = useState(false);
  const [connectOpen, setConnectOpen] = useState(false);
  return (
    <div className="conversation-welcome flex flex-1 flex-col items-center justify-center gap-3 px-6 text-center">
      <div className="flex h-11 w-11 items-center justify-center rounded-lg bg-muted text-signal">
        <MessagesSquare className="h-5 w-5" />
      </div>
      <div>
        <p className="font-display text-base font-semibold tracking-tight">
          {t(
            hasConversations
              ? "conversation.noneSelectedTitle"
              : policyUnavailable
                ? policyError
                  ? "contactVisibility.loadError"
                  : "redesign.loadingContacts"
                : hiddenOnly
                  ? "contactVisibility.allHidden"
                  : "redesign.noPeersTitle",
          )}
        </p>
        {hasConversations && (
          <p className="mt-1 text-[13px] leading-5 text-muted-foreground">
            {t("conversation.noneSelectedDesc")}
          </p>
        )}
        {!hasConversations && !hiddenOnly && !policyUnavailable && (
          <p className="mt-1 max-w-sm text-[13px] leading-5 text-muted-foreground">
            {t("redesign.noPeersBody")}
          </p>
        )}
      </div>
      {policyUnavailable && policyError && owner && (
        <button
          type="button"
          onClick={() => void useContactPolicy.getState().load(owner)}
          className="rounded-md px-3 py-2 text-sm font-medium text-signal hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {t("contactVisibility.retry")}
        </button>
      )}
      {hiddenOnly && <HiddenContactsDialog />}
      {!hasConversations && !hiddenOnly && !policyUnavailable && (
        <>
          <Button
            type="button"
            data-testid="empty-connect"
            onClick={() => setConnectOpen(true)}
          >
            {t("redesign.connectToSomeone")}
          </Button>
          <button
            type="button"
            onClick={() => setGuideOpen(true)}
            className="rounded-md px-2 py-1 text-sm font-medium text-signal underline-offset-4 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {t("redesign.connectionHelp")}
          </button>
          <DiagnosticsDialog
            open={guideOpen}
            onOpenChange={setGuideOpen}
            initialTab="help"
          />
          <OfflineConnectDialog
            open={connectOpen}
            onOpenChange={setConnectOpen}
            includeSharedNetwork
          />
        </>
      )}
    </div>
  );
}

// Shown on first boot while the node's encrypted stores are still being unlocked in the
// background (the heavy argon2 KDF + store opens). The login invoke already returned, so
// this is a non-blocking "two-phase startup" state — not a frozen window.
export function UnlockingState() {
  const { t } = useTranslation();
  const [slow, setSlow] = useState(false);
  const [guideOpen, setGuideOpen] = useState(false);
  useEffect(() => {
    const id = window.setTimeout(() => setSlow(true), 15_000);
    return () => window.clearTimeout(id);
  }, []);
  return (
    <div
      role="status"
      className="flex flex-1 flex-col items-center justify-center gap-4 px-6 text-center"
    >
      <div className="relative flex h-16 w-16 items-center justify-center">
        <Loader2 className="h-7 w-7 animate-spin text-signal" />
      </div>
      <div>
        <p className="font-display text-lg font-semibold tracking-tight">
          {t("conversation.unlockingTitle")}
        </p>
        <p className="text-sm text-muted-foreground">
          {t("conversation.unlockingDesc")}
        </p>
        {slow && (
          <p className="mx-auto mt-3 max-w-sm text-sm leading-relaxed text-muted-foreground">
            {t("redesign.slowStartup")}
          </p>
        )}
      </div>
      {slow && (
        <>
          <button
            type="button"
            onClick={() => setGuideOpen(true)}
            className="rounded-md px-3 py-2 text-sm font-medium text-signal hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          >
            {t("redesign.openConnection")}
          </button>
          <DiagnosticsDialog
            open={guideOpen}
            onOpenChange={setGuideOpen}
            initialTab="help"
          />
        </>
      )}
    </div>
  );
}

export function StartupFailureState() {
  const { t } = useTranslation();
  const setError = useChat((s) => s.setError);
  const [guideOpen, setGuideOpen] = useState(false);
  const revealLogs = async () => {
    try {
      await revealItemInDir(await obs.logFile());
    } catch (e) {
      setError(t("diagnostics.actionFailed", { error: errorMessage(e) }));
    }
  };

  return (
    <div
      role="alert"
      className="flex flex-1 flex-col items-center justify-center gap-4 px-6 text-center"
    >
      <div className="flex h-12 w-12 items-center justify-center rounded-lg border border-destructive/30 bg-destructive/5 text-destructive">
        <CircleAlert className="h-6 w-6" aria-hidden="true" />
      </div>
      <div className="max-w-md">
        <p className="font-display text-lg font-semibold tracking-tight">
          {t("conversation.startFailedTitle")}
        </p>
        <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
          {t("conversation.startFailedDesc")}
        </p>
      </div>
      <div className="flex flex-wrap items-center justify-center gap-2">
        <button
          type="button"
          onClick={() => setGuideOpen(true)}
          className="rounded-md bg-signal px-3 py-2 text-sm font-medium text-primary-foreground hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {t("redesign.openConnection")}
        </button>
        <button
          type="button"
          onClick={() => void revealLogs()}
          className="rounded-md px-3 py-2 text-sm font-medium text-signal hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          {t("diagnostics.revealLogs")}
        </button>
      </div>
      <DiagnosticsDialog
        open={guideOpen}
        onOpenChange={setGuideOpen}
        initialTab="help"
      />
    </div>
  );
}
