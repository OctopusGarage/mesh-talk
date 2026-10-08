import { useEffect, useRef, useState } from "react";
import { Smartphone, KeyRound, Loader2, Copy, Check } from "lucide-react";
import { QRCodeSVG } from "qrcode.react";
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
import { Input } from "@/components/ui/input";
import { auth, chat } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { shortId } from "@/lib/format";
import { useChat } from "@/store/chat";

export function LinkDeviceDialog({ menuItem = false }: { menuItem?: boolean }) {
  const { t } = useTranslation();
  const myAccountId = useChat((s) => s.myAccountId);
  const peers = useChat((s) => s.peers);
  const setError = useChat((s) => s.setError);

  const [open, setOpen] = useState(false);
  const [code, setCode] = useState<string | null>(null);
  const [joinPeer, setJoinPeer] = useState("");
  const [joinCode, setJoinCode] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const [msgIsError, setMsgIsError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [creatingCode, setCreatingCode] = useState(false);
  const creatingCodeRef = useRef(false);
  const openRef = useRef(false);
  const stopRef = useRef<Promise<void> | null>(null);
  const [copyStatus, setCopyStatus] = useState<"copied" | "failed" | null>(
    null,
  );

  const stopLinking = () => {
    const pending = chat.stopLinking().catch((e) => {
      setError(errorMessage(e));
    });
    stopRef.current = pending;
    void pending.finally(() => {
      if (stopRef.current === pending) stopRef.current = null;
    });
    return pending;
  };

  const onOpenChange = (v: boolean) => {
    openRef.current = v;
    setOpen(v);
    if (!v) {
      if (code) void stopLinking();
      setCode(null);
      setMsg(null);
      setMsgIsError(false);
      setCopyStatus(null);
      setJoinCode("");
    }
  };

  useEffect(
    () => () => {
      openRef.current = false;
    },
    [],
  );

  const showCode = async () => {
    if (creatingCodeRef.current) return;
    creatingCodeRef.current = true;
    setCreatingCode(true);
    setMsg(null);
    setMsgIsError(false);
    try {
      if (stopRef.current) await stopRef.current;
      if (!openRef.current) return;
      const nextCode = await chat.startLinking();
      if (openRef.current) setCode(nextCode);
      else await stopLinking();
    } catch (e) {
      if (openRef.current) {
        setMsgIsError(true);
        setMsg(errorMessage(e));
      }
    } finally {
      creatingCodeRef.current = false;
      setCreatingCode(false);
    }
  };

  const copyCode = async () => {
    if (!code) return;
    setCopyStatus(null);
    try {
      await navigator.clipboard.writeText(code);
      setCopyStatus("copied");
      setTimeout(
        () =>
          setCopyStatus((current) => (current === "copied" ? null : current)),
        1200,
      );
    } catch {
      setCopyStatus("failed");
    }
  };

  const doLink = async () => {
    if (!joinPeer || !joinCode.trim()) return;
    setBusy(true);
    setMsg(null);
    setMsgIsError(false);
    try {
      await chat.linkDevice(joinPeer, joinCode.trim());
      await auth.adoptLinkedAccount();
      setMsg(t("linkDevice.linked"));
      setJoinCode("");
    } catch (e) {
      setMsgIsError(true);
      setMsg(t("linkDevice.linkFailed", { error: errorMessage(e) }));
    } finally {
      setBusy(false);
    }
  };

  const rekey = async () => {
    setBusy(true);
    setMsg(null);
    setMsgIsError(false);
    try {
      const id = await chat.rekeyAccount();
      setMsg(t("linkDevice.rekeyed", { id: shortId(id, 12) }));
    } catch (e) {
      setMsgIsError(true);
      setMsg(t("linkDevice.rekeyFailed", { error: errorMessage(e) }));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogTrigger asChild>
        <Button
          variant="ghost"
          size={menuItem ? "sm" : "icon"}
          className={menuItem ? "w-full justify-start gap-2.5 px-2" : undefined}
          data-testid="sidebar-action-link"
          title={t("linkDevice.trigger")}
        >
          <Smartphone className="h-4 w-4" />
          {menuItem && <span>{t("linkDevice.trigger")}</span>}
        </Button>
      </DialogTrigger>
      <DialogContent data-testid="link-device-dialog">
        <DialogHeader>
          <DialogTitle>{t("linkDevice.title")}</DialogTitle>
          <DialogDescription>
            {t("linkDevice.thisAccount")}{" "}
            <code className="font-mono text-foreground/80">
              {myAccountId ? `${shortId(myAccountId, 12)}…` : "—"}
            </code>
          </DialogDescription>
        </DialogHeader>

        {/* Show this device's pairing code on the OTHER device. */}
        <section className="space-y-3 rounded-lg border p-4">
          <p className="font-display text-sm font-semibold tracking-tight">
            {t("linkDevice.addDevice")}
          </p>
          {!code ? (
            <>
              <p className="text-xs leading-relaxed text-muted-foreground">
                {t("linkDevice.showOnOther")}
              </p>
              <Button
                variant="secondary"
                size="sm"
                data-testid="link-show-code"
                disabled={creatingCode || busy}
                aria-busy={creatingCode}
                onClick={() => void showCode()}
              >
                {creatingCode && (
                  <Loader2
                    aria-hidden="true"
                    className="h-4 w-4 animate-spin motion-reduce:animate-none"
                  />
                )}
                {t("linkDevice.showCode")}
              </Button>
            </>
          ) : (
            <div className="flex items-center gap-4">
              {/* Display-only QR: this is a desktop app with no scanner, so the QR is just
                  a convenience for transcribing the code via a phone camera / future mobile
                  client. The copyable text remains the primary (desktop) path. */}
              <div className="shrink-0 rounded-lg bg-white p-2">
                <QRCodeSVG value={code} size={104} />
              </div>
              <div className="min-w-0 flex-1 space-y-2">
                <button
                  type="button"
                  onClick={() => void copyCode()}
                  title={t("common.copy")}
                  className="group flex w-full items-start gap-2 rounded-lg border bg-muted/40 px-3 py-2 text-left transition-colors hover:bg-muted"
                >
                  {/* The FULL code must be readable (the user types it on the other device),
                      so it's shown grouped in 4s and WRAPS within the column — never
                      truncated, and never wider than the dialog. */}
                  <span
                    data-testid="pairing-code"
                    className="min-w-0 flex-1 break-words font-mono text-base font-semibold leading-relaxed tracking-wider text-signal"
                  >
                    {code.replace(/(.{4})(?=.)/g, "$1 ")}
                  </span>
                  {copyStatus === "copied" ? (
                    <Check className="mt-0.5 h-4 w-4 shrink-0 text-verified" />
                  ) : (
                    <Copy className="mt-0.5 h-4 w-4 shrink-0 opacity-40 group-hover:opacity-100" />
                  )}
                </button>
                {copyStatus && (
                  <p
                    role={copyStatus === "failed" ? "alert" : "status"}
                    className={
                      copyStatus === "failed"
                        ? "text-xs text-destructive"
                        : "text-xs text-verified"
                    }
                  >
                    {t(
                      copyStatus === "failed"
                        ? "common.copyFailed"
                        : "common.copied",
                    )}
                  </p>
                )}
                <p className="text-xs leading-relaxed text-muted-foreground">
                  {t("linkDevice.qrHint")}
                </p>
              </div>
            </div>
          )}
        </section>

        {/* Enter a code FROM another device here. */}
        <section className="space-y-2 rounded-lg border p-4">
          <p className="font-display text-sm font-semibold tracking-tight">
            {t("linkDevice.haveCode")}
          </p>
          <label
            htmlFor="link-device-peer"
            className="block text-xs font-medium text-muted-foreground"
          >
            {t("linkDevice.device")}
          </label>
          <select
            id="link-device-peer"
            value={joinPeer}
            onChange={(e) => setJoinPeer(e.target.value)}
            className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm transition-colors hover:border-ring focus-visible:border-ring focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background [@media(hover:none)]:min-h-11"
          >
            <option value="">{t("linkDevice.pickDevice")}</option>
            {peers.map((p) => (
              <option key={p.user_id} value={p.user_id}>
                {p.name || t("common.unnamed")} ({shortId(p.user_id)})
              </option>
            ))}
          </select>
          <label
            htmlFor="link-device-code"
            className="block text-xs font-medium text-muted-foreground"
          >
            {t("linkDevice.pairingCode")}
          </label>
          <div className="flex gap-2">
            <Input
              id="link-device-code"
              autoFocus
              placeholder={t("linkDevice.pairingCode")}
              value={joinCode}
              onChange={(e) => setJoinCode(e.target.value)}
              className="font-mono tracking-widest"
            />
            <Button
              disabled={!joinPeer || !joinCode.trim() || busy}
              onClick={doLink}
            >
              {busy && <Loader2 className="h-4 w-4 animate-spin" />}
              {t("common.link")}
            </Button>
          </div>
        </section>

        {/* Lost / compromised device — rotate the account identity. */}
        <section className="flex items-center justify-between gap-3 rounded-lg border border-destructive/30 bg-destructive/5 p-4">
          <div className="min-w-0">
            <p className="text-sm font-medium">{t("linkDevice.lostDevice")}</p>
            <p className="text-xs leading-relaxed text-muted-foreground">
              {t("linkDevice.rotateIdentity")}
            </p>
          </div>
          <Button
            variant="destructive"
            size="sm"
            disabled={busy}
            onClick={rekey}
            className="shrink-0"
          >
            <KeyRound className="h-4 w-4" />
            {t("linkDevice.rekey")}
          </Button>
        </section>

        {msg && (
          <p
            role={msgIsError ? "alert" : "status"}
            className={
              msgIsError
                ? "text-sm font-medium text-destructive"
                : "text-sm font-medium text-signal"
            }
          >
            {msg}
          </p>
        )}
      </DialogContent>
    </Dialog>
  );
}
