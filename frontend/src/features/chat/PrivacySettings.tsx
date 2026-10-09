import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { EyeOff } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { useAuth } from "@/store/auth";
import { useChat } from "@/store/chat";
import { usePrivacy } from "@/store/privacy";
import { shortId } from "@/lib/format";

/** The switch acknowledges the backend's durable policy, never optimistic local state. */
export function PrivacySettings() {
  const { t } = useTranslation();
  const owner = useAuth((s) => s.user?.id);
  const policyOwner = usePrivacy((s) => s.owner);
  const snapshot = usePrivacy((s) => s.snapshot);
  const loaded = usePrivacy((s) => s.loaded);
  const loading = usePrivacy((s) => s.loading);
  const busy = usePrivacy((s) => s.busy);
  const error = usePrivacy((s) => s.error);
  const accounts = useChat((s) => s.accounts);
  const ownAccount = useChat((s) => s.myAccountId);
  const [manage, setManage] = useState(false);
  const [confirmMode, setConfirmMode] = useState(false);
  const [revoke, setRevoke] = useState<{ id: string; name: string } | null>(
    null,
  );
  const [query, setQuery] = useState("");
  const revokeTrigger = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    if (owner) void usePrivacy.getState().load(owner);
  }, [owner, manage]);
  const enabled = loaded && policyOwner === owner && !loading && !busy;
  const contacts = useMemo(() => {
    const merged = new Map<
      string,
      { id: string; name: string; allowed: boolean; source?: string }
    >();
    for (const account of accounts) {
      if (
        account.account_id !== ownAccount &&
        /^[0-9a-f]{32}$/.test(account.account_id)
      )
        merged.set(account.account_id, {
          id: account.account_id,
          name: account.names[0] || shortId(account.account_id),
          allowed: false,
        });
    }
    for (const account of snapshot?.allowed_accounts ?? [])
      merged.set(account.id, { ...account, allowed: true });
    return [...merged.values()].sort(
      (a, b) =>
        Number(b.allowed) - Number(a.allowed) || a.name.localeCompare(b.name),
    );
  }, [accounts, ownAccount, snapshot]);
  const matches = contacts.filter((c) =>
    `${c.name} ${c.id}`
      .toLocaleLowerCase()
      .includes(query.trim().toLocaleLowerCase()),
  );
  const saveMode = async (invisible: boolean) => {
    if (!owner) return;
    const ok = await usePrivacy.getState().setInvisible(owner, invisible);
    if (ok && useAuth.getState().user?.id === owner) setConfirmMode(false);
  };
  const saveRevoke = async () => {
    if (!owner || !revoke) return;
    const ok = await usePrivacy.getState().setAllowed(owner, revoke.id, false);
    if (ok && useAuth.getState().user?.id === owner) setRevoke(null);
  };
  const errors = (
    <>
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {t(`privacy.${error}Error`)}
        </p>
      )}
      {error === "load" && (
        <Button
          variant="outline"
          onClick={() => owner && void usePrivacy.getState().load(owner)}
          disabled={loading || busy}
        >
          {t("privacy.retry")}
        </Button>
      )}
    </>
  );
  return (
    <section className="space-y-2" aria-label={t("privacy.title")}>
      <h3 className="text-[12px] font-semibold text-muted-foreground">
        {t("privacy.section")}
      </h3>
      <div className="space-y-3 border-b pb-3">
        <div className="flex items-center justify-between gap-4">
          <div className="flex min-w-0 items-start gap-3">
            <EyeOff className="mt-0.5 h-4 w-4 shrink-0 text-muted-foreground" />
            <div className="min-w-0">
              <label htmlFor="invisible-switch" className="text-sm font-medium">
                {t("privacy.title")}
              </label>
              <p className="text-xs text-muted-foreground">
                {t("privacy.description")}
              </p>
            </div>
          </div>
          <Switch
            id="invisible-switch"
            data-testid="invisible-switch"
            checked={snapshot?.invisible ?? false}
            disabled={!enabled}
            onCheckedChange={(v) =>
              v ? setConfirmMode(true) : void saveMode(false)
            }
          />
        </div>
        {(loading || (!loaded && !error)) && (
          <p className="text-xs text-muted-foreground">
            {t("privacy.loading")}
          </p>
        )}
        {errors}
        <Dialog
          open={manage}
          onOpenChange={(open) => {
            if (!busy) setManage(open);
          }}
        >
          <DialogTrigger asChild>
            <Button
              variant="outline"
              data-testid="manage-privacy"
              disabled={!enabled}
              className="disabled:opacity-75"
            >
              {t("privacy.manage")}
            </Button>
          </DialogTrigger>
          <DialogContent data-testid="privacy-dialog" className="max-w-lg">
            <DialogHeader>
              <DialogTitle>{t("privacy.manage")}</DialogTitle>
              <DialogDescription>
                {t("privacy.permissionHelp")}
              </DialogDescription>
            </DialogHeader>
            <p className="text-xs text-muted-foreground">
              {t("privacy.localOnly")}
            </p>
            <Input
              id="privacy-search"
              data-testid="privacy-search"
              aria-label={t("privacy.search")}
              placeholder={t("privacy.search")}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            {errors}
            <div role="list" className="max-h-[40vh] space-y-2 overflow-y-auto">
              {matches.length === 0 && (
                <p className="py-6 text-center text-sm text-muted-foreground">
                  {t("privacy.empty")}
                </p>
              )}
              {matches.map((contact) => (
                <div
                  role="listitem"
                  key={contact.id}
                  className="flex items-center gap-3 rounded-lg border p-3"
                >
                  <div className="min-w-0 flex-1">
                    <p
                      className="truncate text-sm font-medium"
                      title={contact.name}
                    >
                      {contact.name}
                    </p>
                    <p
                      className="truncate font-mono text-xs text-muted-foreground"
                      title={contact.id}
                    >
                      {shortId(contact.id, 12)}
                    </p>
                    {contact.allowed && (
                      <p className="text-xs text-muted-foreground">
                        {t(
                          contact.source === "Initiated"
                            ? "privacy.initiated"
                            : "privacy.manual",
                        )}
                      </p>
                    )}
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={!enabled}
                    data-testid={`privacy-${contact.allowed ? "revoke" : "allow"}-${contact.id}`}
                    aria-label={`${t(contact.allowed ? "privacy.revoke" : "privacy.allow")} ${contact.name}`}
                    onClick={(event) => {
                      if (contact.allowed) {
                        revokeTrigger.current = event.currentTarget;
                        setRevoke(contact);
                      } else if (owner) {
                        void usePrivacy
                          .getState()
                          .setAllowed(owner, contact.id, true);
                      }
                    }}
                  >
                    {t(contact.allowed ? "privacy.revoke" : "privacy.allow")}
                  </Button>
                </div>
              ))}
            </div>
          </DialogContent>
        </Dialog>
      </div>
      <Dialog
        open={confirmMode}
        onOpenChange={(open) => {
          if (!busy) setConfirmMode(open);
        }}
      >
        <DialogContent
          className="max-w-md"
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            document.getElementById("invisible-switch")?.focus();
          }}
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            document.getElementById("invisible-cancel")?.focus();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("privacy.enableTitle")}</DialogTitle>
            <DialogDescription>{t("privacy.enableHelp")}</DialogDescription>
          </DialogHeader>
          <p className="text-xs text-muted-foreground">{t("privacy.limits")}</p>
          <p className="text-xs text-muted-foreground">
            {t("privacy.relayHelp")}
          </p>
          {error === "save" && (
            <p role="alert" className="text-sm text-destructive">
              {t("privacy.saveError")}
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button
              id="invisible-cancel"
              data-testid="invisible-cancel"
              variant="outline"
              disabled={busy}
              onClick={() => setConfirmMode(false)}
            >
              {t("common.cancel")}
            </Button>
            <Button
              data-testid="invisible-confirm"
              disabled={!enabled}
              onClick={() => void saveMode(true)}
            >
              {t("privacy.enable")}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
      <Dialog
        open={revoke !== null}
        onOpenChange={(open) => {
          if (!open && !busy) setRevoke(null);
        }}
      >
        <DialogContent
          className="max-w-md"
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            if (revokeTrigger.current?.isConnected)
              revokeTrigger.current.focus();
            else document.getElementById("privacy-search")?.focus();
          }}
          onOpenAutoFocus={(e) => {
            e.preventDefault();
            document.getElementById("privacy-revoke-cancel")?.focus();
          }}
        >
          <DialogHeader>
            <DialogTitle>{t("privacy.revokeTitle")}</DialogTitle>
            <DialogDescription>{t("privacy.revokeHelp")}</DialogDescription>
          </DialogHeader>
          <p className="break-all text-sm">{revoke?.name}</p>
          {error === "save" && (
            <p role="alert" className="text-sm text-destructive">
              {t("privacy.saveError")}
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button
              id="privacy-revoke-cancel"
              data-testid="privacy-revoke-cancel"
              variant="outline"
              disabled={busy}
              onClick={() => setRevoke(null)}
            >
              {t("common.cancel")}
            </Button>
            <Button
              data-testid="privacy-revoke-confirm"
              disabled={!enabled}
              onClick={() => void saveRevoke()}
            >
              {t("privacy.revoke")}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </section>
  );
}
