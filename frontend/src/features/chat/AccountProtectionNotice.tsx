import { useState } from "react";
import { CircleAlert, ShieldCheck } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { DataRecoverySettings } from "./DataRecoverySettings";

/** Session-local guidance. Password storage and portable recovery are separate promises. */
export function AccountProtectionNotice() {
  const { t } = useTranslation();
  const newAccount = useAuth((s) => s.newAccount);
  const saved = useAuth((s) => s.savedSession);
  const dismiss = useAuth((s) => s.dismissProtection);
  const dismissBackup = useAuth((s) => s.dismissBackupReminder);
  const retry = useAuth((s) => s.retrySavedSession);
  const ready = useChat((s) => s.ready);
  const [backupOpen, setBackupOpen] = useState(false);
  const [backupCompleted, setBackupCompleted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [retryError, setRetryError] = useState(false);
  if (!newAccount && saved !== "failed") return null;
  const failed = saved === "failed";
  return (
    <section
      aria-label={t("login.protectTitle")}
      data-testid="account-protection"
      className="max-h-[45vh] shrink-0 overflow-y-auto border-b border-border bg-card px-5 pb-4 pt-9 text-sm text-foreground"
    >
      <div className="mx-auto flex max-w-3xl items-start gap-3">
        {failed ? (
          <CircleAlert className="mt-0.5 h-5 w-5 shrink-0" aria-hidden="true" />
        ) : (
          <ShieldCheck className="mt-0.5 h-5 w-5 shrink-0" aria-hidden="true" />
        )}
        <div className="min-w-0 flex-1 space-y-2">
          <h2 className="font-semibold">{t("login.protectTitle")}</h2>
          <p role="status" className="leading-5">
            {t(
              failed
                ? "login.saveFailed"
                : saved === "saved"
                  ? "login.savedOnDevice"
                  : saved === "disabled"
                    ? "login.notSaved"
                    : "login.saveUnknown",
            )}
          </p>
          {newAccount && (
            <p className="text-xs leading-5 text-foreground/80">
              {t("login.backupGuidance")}
            </p>
          )}
          {retryError && (
            <p role="alert" className="text-sm text-destructive">
              {t("login.retrySaveFailed")}
            </p>
          )}
          <div className="flex flex-wrap items-center gap-2">
            {failed && (
              <Button
                variant="outline"
                size="sm"
                disabled={busy || !ready}
                onClick={async () => {
                  setBusy(true);
                  setRetryError(false);
                  try {
                    setRetryError(!(await retry()));
                  } catch {
                    setRetryError(true);
                  } finally {
                    setBusy(false);
                  }
                }}
              >
                {t(busy ? "login.savingCredential" : "login.retrySave")}
              </Button>
            )}
            {newAccount && (
              <DataRecoverySettings
                enabled={ready}
                open={backupOpen}
                onBackupCreated={() => setBackupCompleted(true)}
                onOpenChange={(open) => {
                  setBackupOpen(open);
                  // Keep the verified completion visible until the user closes
                  // it; a successful backup must not dismiss a credential failure.
                  if (!open && backupCompleted) dismissBackup();
                }}
              />
            )}
            <Button
              data-testid="account-protection-later"
              variant="ghost"
              size="sm"
              disabled={busy}
              onClick={dismiss}
            >
              {t(newAccount ? "login.protectLater" : "common.dismiss")}
            </Button>
          </div>
        </div>
      </div>
    </section>
  );
}
