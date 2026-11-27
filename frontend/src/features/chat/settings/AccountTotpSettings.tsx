import { useEffect, useRef, useState } from "react";
import { QRCodeSVG } from "qrcode.react";
import { useTranslation } from "react-i18next";
import {
  accountTotp,
  type TotpSetup,
  type TotpStatus,
} from "@/lib/api/accountTotp";
import { captureRuntimeOwner } from "@/store/ownership";
import { useChat } from "@/store/chat";
import { useAuth } from "@/store/auth";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function AccountTotpSettings() {
  const owner = useAuth((s) => s.user?.id);
  const generation = useAuth((s) => s.generation);
  const run = useChat((s) => s.runEpoch);
  const identity = useChat((s) => s.identityEpoch);
  return (
    <AccountTotpSession key={`${owner}:${generation}:${run}:${identity}`} />
  );
}
function AccountTotpSession() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<TotpStatus | null>(null);
  const [setup, setSetup] = useState<TotpSetup | null>(null);
  const [code, setCode] = useState("");
  const [ack, setAck] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);
  const [pending, setPending] = useState(false);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const run = async (
    action: "status" | "begin" | "confirm" | "disable" | "cancel" | "retry",
  ) => {
    const lease = captureRuntimeOwner();
    if (!lease.owner || busy) return;
    const current = () => alive.current && lease.current();
    setBusy(true);
    setError(false);
    try {
      if (action === "begin") {
        const value = await accountTotp.begin(lease.owner);
        if (current()) {
          setSetup(value);
          setAck(false);
          setCode("");
        }
      } else {
        if (action === "cancel") {
          setSetup(null);
          setCode("");
          setAck(false);
          await accountTotp.cancel(lease.owner);
        } else if (action !== "status") {
          const result =
            action === "retry"
              ? await accountTotp.retry(lease.owner)
              : await accountTotp[action](lease.owner, code.trim());
          if (current()) {
            setPending(result.sync_pending);
            setSetup(null);
            setCode("");
            setAck(false);
          }
        }
        const value = await accountTotp.status(lease.owner);
        if (current()) setStatus(value);
      }
    } catch {
      if (current()) {
        setError(true);
        if (action === "confirm") {
          setCode("");
        }
      }
    } finally {
      if (current()) setBusy(false);
    }
  };
  useEffect(() => {
    void run("status"); /* One read per mounted session; no polling. */
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <section className="space-y-3 border-t py-4">
      <h3>{t("totp.title")}</h3>
      <p className="text-xs text-muted-foreground">{t("totp.scope")}</p>
      <p role="status">
        {status
          ? t(status.enabled ? "totp.on" : "totp.off")
          : t("common.loading")}
      </p>
      {error && <p role="alert">{t("totp.failed")}</p>}
      {pending && <p role="status">{t("totp.pending")}</p>}
      {setup && (
        <div className="space-y-3">
          <QRCodeSVG value={setup.otpauth_uri} size={180} />
          <p>{t("totp.seed")}</p>
          <code className="break-all">{setup.manual_seed}</code>
          <p>{t("totp.recovery")}</p>
          <pre>{setup.recovery_codes.join("\n")}</pre>
          <label>
            <input
              type="checkbox"
              checked={ack}
              onChange={(e) => setAck(e.target.checked)}
            />
            {t("totp.ack")}
          </label>
        </div>
      )}
      {(setup || status?.enabled) && (
        <Input
          aria-label={t("totp.code")}
          autoComplete="one-time-code"
          value={code}
          onChange={(e) => setCode(e.target.value)}
        />
      )}
      {setup ? (
        <>
          <Button
            disabled={busy || !ack || !/^\d{6}$/.test(code)}
            onClick={() => void run("confirm")}
          >
            {t("totp.confirm")}
          </Button>
          <Button disabled={busy} onClick={() => void run("cancel")}>
            {t("common.cancel")}
          </Button>
        </>
      ) : (
        status && (
          <Button
            disabled={busy || (status.enabled && !code.trim())}
            onClick={() => void run(status.enabled ? "disable" : "begin")}
          >
            {t(status.enabled ? "totp.disable" : "totp.enable")}
          </Button>
        )
      )}
      {(pending || error) && (
        <Button
          disabled={busy}
          onClick={() => void run(pending ? "retry" : "status")}
        >
          {t("contactVisibility.retry")}
        </Button>
      )}
    </section>
  );
}
