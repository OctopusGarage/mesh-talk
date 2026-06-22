import { useState } from "react";
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

export function LinkDeviceDialog() {
  const { t } = useTranslation();
  const myAccountId = useChat((s) => s.myAccountId);
  const peers = useChat((s) => s.peers);

  const [open, setOpen] = useState(false);
  const [code, setCode] = useState<string | null>(null);
  const [joinPeer, setJoinPeer] = useState("");
  const [joinCode, setJoinCode] = useState("");
  const [msg, setMsg] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);

  const onOpenChange = (v: boolean) => {
    setOpen(v);
    if (!v) {
      if (code) void chat.stopLinking();
      setCode(null);
      setMsg(null);
      setJoinCode("");
    }
  };

  const showCode = async () => {
    setMsg(null);
    try {
      setCode(await chat.startLinking());
    } catch (e) {
      setMsg(errorMessage(e));
    }
  };

  const copyCode = () => {
    if (!code) return;
    void navigator.clipboard?.writeText(code).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    });
  };

  const doLink = async () => {
    if (!joinPeer || !joinCode.trim()) return;
    setBusy(true);
    setMsg(null);
    try {
      await chat.linkDevice(joinPeer, joinCode.trim());
      await auth.adoptLinkedAccount();
      setMsg(t("linkDevice.linked"));
      setJoinCode("");
    } catch (e) {
      setMsg(t("linkDevice.linkFailed", { error: errorMessage(e) }));
    } finally {
      setBusy(false);
    }
  };

  const rekey = async () => {
    setBusy(true);
    setMsg(null);
    try {
      const id = await chat.rekeyAccount();
      setMsg(t("linkDevice.rekeyed", { id: shortId(id, 12) }));
    } catch (e) {
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
          size="icon"
          data-testid="sidebar-action-link"
          title={t("linkDevice.trigger")}
        >
          <Smartphone className="h-4 w-4" />
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
              <Button variant="secondary" size="sm" onClick={showCode}>
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
                  onClick={copyCode}
                  title={t("common.copy")}
                  className="group flex w-full items-center gap-2 rounded-lg border bg-muted/40 px-3 py-2 text-left transition-colors hover:bg-muted"
                >
                  <span className="min-w-0 flex-1 truncate font-mono text-xl font-semibold tracking-[0.2em] text-signal">
                    {code}
                  </span>
                  {copied ? (
                    <Check className="h-4 w-4 shrink-0 text-verified" />
                  ) : (
                    <Copy className="h-4 w-4 shrink-0 opacity-40 group-hover:opacity-100" />
                  )}
                </button>
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
          <select
            value={joinPeer}
            onChange={(e) => setJoinPeer(e.target.value)}
            className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm transition-colors hover:border-ring focus-visible:border-ring focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
          >
            <option value="">{t("linkDevice.pickDevice")}</option>
            {peers.map((p) => (
              <option key={p.user_id} value={p.user_id}>
                {p.name || t("common.unnamed")} ({shortId(p.user_id)})
              </option>
            ))}
          </select>
          <div className="flex gap-2">
            <Input
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

        {msg && <p className="text-sm font-medium text-signal">{msg}</p>}
      </DialogContent>
    </Dialog>
  );
}
