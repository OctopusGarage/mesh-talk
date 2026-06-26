import { useEffect, useState } from "react";
import { motion } from "framer-motion";
import {
  ShieldCheck,
  ShieldAlert,
  ShieldQuestion,
  Loader2,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { IdentityCrest, SafetyNumber } from "@/components/identity";
import { chat } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { fadeSlideUp, useMotionOK } from "@/lib/motion";
import { useChat } from "@/store/chat";
import { presenceStatus, usePresenceFor } from "@/store/presence";
import type { SafetyNumber as SafetyNumberInfo, TrustInfo } from "@/lib/types";

/**
 * Contact verification / safety-number UI — the trust centerpiece. A contact is identified
 * by its account id; its current device fingerprint comes from the discovery roster. We
 * present the contact as a large IdentityCrest, the fingerprint as a readable SafetyNumber
 * (grouped mono digits + a word sequence) compared out-of-band, a "Mark as verified"
 * action, a confident verified state, and a loud-but-elegant warning if a known contact's
 * fingerprint changed since first contact.
 *
 * This is pure trust UX over the existing fingerprints — no crypto changes.
 */
export function VerifyContactDialog({
  accountId,
  name,
}: {
  accountId: string;
  name: string;
}) {
  const { t } = useTranslation();
  const motionOK = useMotionOK();
  const peers = useChat((s) => s.peers);
  const presence = usePresenceFor(accountId);
  const status = presenceStatus(presence);
  // The current device fingerprint presenting this account (any of its devices).
  const fingerprint =
    peers.find((p) => p.account_id === accountId)?.user_id ?? "";

  const [open, setOpen] = useState(false);
  const [trust, setTrust] = useState<TrustInfo | null>(null);
  const [sn, setSn] = useState<SafetyNumberInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  // Load the trust state as soon as the contact's device fingerprint is known (and refresh
  // when the dialog opens), so the header shield reflects an already-verified contact
  // WITHOUT having to open the dialog first. (`open` is a dep only to refresh on open.)
  useEffect(() => {
    if (!fingerprint) return;
    let active = true;
    void chat
      .getTrust(accountId, fingerprint)
      .then((tr) => {
        if (active) setTrust(tr);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, [accountId, fingerprint, open]);

  // The safety number is only shown inside the open dialog, so fetch it lazily.
  useEffect(() => {
    if (!open || !fingerprint) return;
    let active = true;
    setErr(null);
    void chat
      .safetyNumber(fingerprint)
      .then((s) => {
        if (active) setSn(s);
      })
      .catch((e) => {
        if (active) setErr(errorMessage(e));
      });
    return () => {
      active = false;
    };
  }, [open, fingerprint]);

  const verify = async () => {
    if (!fingerprint) return;
    setBusy(true);
    setErr(null);
    try {
      await chat.markVerified(accountId, fingerprint);
      setTrust(await chat.getTrust(accountId, fingerprint));
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const verified = trust?.verified ?? false;
  const changed = trust?.fingerprint_changed ?? false;
  const trusted = verified && !changed;

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          data-testid="verify-trigger"
          data-trust={
            changed ? "changed" : verified ? "verified" : "unverified"
          }
          title={t("verify.trigger")}
          aria-label={t("verify.trigger")}
        >
          {changed ? (
            <ShieldAlert className="h-4 w-4 text-destructive" />
          ) : verified ? (
            <ShieldCheck className="h-4 w-4 text-verified" />
          ) : (
            <ShieldQuestion className="h-4 w-4" />
          )}
        </Button>
      </DialogTrigger>
      <DialogContent data-testid="verify-dialog">
        <DialogHeader>
          <DialogTitle>{t("verify.title")}</DialogTitle>
          <DialogDescription>
            {t("verify.description", { name })}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          {/* The contact, rendered with the large crest. Keyed by the account id (the
              stable contact identity used elsewhere) so a custom photo set here shows for
              this contact everywhere; the glyph here doubles as a set/remove-photo affordance. */}
          <IdentityCrest
            id={accountId}
            name={name}
            verified={trusted}
            status={status}
            variant="large"
            editAvatarId={accountId}
            editAvatarLabel={t("avatar.editContact", { name })}
          />

          {/* Trust state banner. */}
          {changed ? (
            <div className="flex items-start gap-2.5 rounded-lg border border-destructive/50 bg-destructive/10 p-3 text-sm text-destructive">
              <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
              <div className="space-y-0.5">
                <p className="font-display font-semibold">
                  {t("verify.changedTitle")}
                </p>
                <p className="text-xs leading-relaxed text-destructive/90">
                  {t("verify.changedDesc")}
                </p>
              </div>
            </div>
          ) : trusted ? (
            <div
              data-testid="verify-verified-banner"
              className="flex items-center gap-2 rounded-lg border border-verified/40 bg-verified/10 px-3 py-2 text-sm font-medium text-verified"
            >
              <ShieldCheck className="h-4 w-4 shrink-0" />
              {t("verify.verifiedBanner", { name })}
            </div>
          ) : (
            <div className="flex items-start gap-2.5 rounded-lg border bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
              <ShieldQuestion className="mt-0.5 h-4 w-4 shrink-0 text-signal" />
              <p className="leading-relaxed">{t("verify.prompt")}</p>
            </div>
          )}

          {!fingerprint && (
            <p className="text-sm text-muted-foreground">
              {t("verify.offline")}
            </p>
          )}

          {sn && fingerprint && (
            <motion.div
              initial={motionOK ? "hidden" : false}
              animate="visible"
              variants={fadeSlideUp}
              className="space-y-2"
            >
              <p className="font-mono text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
                {t("verify.safetyNumber")}
              </p>
              <SafetyNumber
                value={sn.grouped}
                words={sn.words}
                verified={trusted}
              />
            </motion.div>
          )}

          {err && <p className="text-sm text-destructive">{err}</p>}

          <div className="flex justify-end">
            <Button
              autoFocus
              data-testid="verify-mark-button"
              variant={changed ? "destructive" : "default"}
              disabled={!fingerprint || busy}
              onClick={verify}
            >
              {busy && <Loader2 className="h-4 w-4 animate-spin" />}
              {trusted ? t("verify.reverify") : t("verify.markVerified")}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
