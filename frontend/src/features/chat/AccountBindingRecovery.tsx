import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ShieldAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { privacy, type AccountBindingChange } from "@/lib/api/privacy";
import { errorKind, errorMessage } from "@/lib/error";
import { useChat } from "@/store/chat";
import { useAuth } from "@/store/auth";
import { captureRuntimeOwner } from "@/store/ownership";

/** Explicit recipient recovery; observing a new certificate never grants trust. */
export function AccountBindingRecovery({
  accountId,
  onOpenAccount,
}: {
  accountId: string;
  onOpenAccount?: (accountId: string) => void;
}) {
  const { t } = useTranslation();
  const owner = useAuth((state) => state.user?.id);
  const authGeneration = useAuth((state) => state.generation);
  const runtime = useChat(
    (state) => `${state.runEpoch}:${state.identityEpoch}`,
  );
  const refreshRoster = useChat((state) => state.refreshRoster);
  const scope = `${owner}:${authGeneration}:${runtime}:${accountId}`;
  const currentScope = useRef(scope);
  currentScope.current = scope;
  const [changes, setChanges] = useState<AccountBindingChange[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [accepted, setAccepted] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const [loadedScope, setLoadedScope] = useState(scope);
  const mounted = useRef(false);

  useEffect(() => {
    const lease = captureRuntimeOwner();
    let active = true;
    mounted.current = true;
    setLoadedScope(scope);
    setChanges([]);
    setError(null);
    setBusy(null);
    setAccepted(null);
    if (owner && owner === lease.owner) {
      void privacy
        .bindingChanges(owner, accountId)
        .then((result) => {
          if (
            active &&
            lease.current() &&
            currentScope.current === scope &&
            result.owner === owner
          )
            setChanges(result.changes);
        })
        .catch((failure) => {
          if (active && lease.current() && currentScope.current === scope)
            setError(errorMessage(failure));
        });
    }
    return () => {
      active = false;
      mounted.current = false;
    };
  }, [owner, authGeneration, runtime, accountId, scope, reload]);

  const accept = async (change: AccountBindingChange) => {
    const lease = captureRuntimeOwner();
    if (!owner || owner !== lease.owner || busy) return;
    setBusy(change.device_id);
    setError(null);
    try {
      const result = await privacy.acceptBindingChange(owner, change);
      if (
        !mounted.current ||
        !lease.current() ||
        currentScope.current !== scope
      )
        return;
      if (result.owner !== owner) return;
      void refreshRoster().catch(() => {});
      setChanges((current) =>
        current.filter((item) => item.device_id !== change.device_id),
      );
      setAccepted(change.current_account_id);
    } catch (failure) {
      if (mounted.current && lease.current() && currentScope.current === scope)
        setError(
          errorKind(failure) === "invalid-input"
            ? t("accountChange.unavailable")
            : errorMessage(failure),
        );
    } finally {
      if (mounted.current && lease.current() && currentScope.current === scope)
        setBusy(null);
    }
  };

  if (loadedScope !== scope || (!changes.length && !error && !accepted))
    return null;
  return (
    <section
      className="space-y-3 border-t pt-4"
      data-testid="account-binding-recovery"
      aria-label={t("accountChange.title")}
    >
      {changes.length > 0 && (
        <>
          <h3 className="flex items-start gap-2 text-sm font-semibold">
            <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
            {t("accountChange.title")}
          </h3>
          <p className="text-sm leading-relaxed text-muted-foreground">
            {t("accountChange.description")}
          </p>
          {changes.map((change) => (
            <div
              className="space-y-3 border-b pb-3 last:border-b-0"
              key={change.device_id}
            >
              <dl className="space-y-2 text-xs">
                {(
                  [
                    ["previousAccount", change.previous_account_id],
                    ["currentAccount", change.current_account_id],
                    ["device", change.device_id],
                  ] as const
                ).map(([label, value]) => (
                  <div key={label}>
                    <dt className="text-muted-foreground">
                      {t(`accountChange.${label}`)}
                    </dt>
                    <dd className="select-text break-all font-mono">{value}</dd>
                  </div>
                ))}
              </dl>
              <Button
                className="w-full"
                disabled={busy !== null}
                onClick={() => void accept(change)}
                data-testid="accept-account-change"
              >
                {busy === change.device_id
                  ? t("accountChange.accepting")
                  : t("accountChange.accept")}
              </Button>
            </div>
          ))}
          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("accountChange.verificationHint")}
          </p>
        </>
      )}
      {accepted && (
        <div className="space-y-2">
          <p role="status" className="text-sm">
            {t("accountChange.accepted")}
          </p>
          {onOpenAccount && (
            <>
              <p className="text-xs leading-relaxed text-muted-foreground">
                {t("accountChange.currentAccountHint")}
              </p>
              <Button
                variant="outline"
                className="w-full"
                data-testid="open-current-account"
                onClick={() => {
                  const lease = captureRuntimeOwner();
                  if (
                    lease.current() &&
                    currentScope.current === scope &&
                    mounted.current
                  )
                    onOpenAccount(accepted);
                }}
              >
                {t("accountChange.openCurrentAccount")}
              </Button>
            </>
          )}
        </div>
      )}
      {error && (
        <div role="alert" className="space-y-2 text-sm text-destructive">
          <p className="[overflow-wrap:anywhere]">{error}</p>
          <Button
            variant="outline"
            onClick={() => setReload((value) => value + 1)}
            disabled={busy !== null}
          >
            {t("privacy.retry")}
          </Button>
        </div>
      )}
    </section>
  );
}
