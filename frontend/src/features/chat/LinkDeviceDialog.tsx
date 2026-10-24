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
import { Switch } from "@/components/ui/switch";
import { auth, chat, diag } from "@/lib/api";
import { accountDocuments } from "@/lib/api/accountDocuments";
import type {
  AccountLoginSyncStatus,
  DeviceSyncPolicy,
  DeviceSyncStatus,
  SyncCategory,
} from "@/lib/api/chat";
import { errorKind, errorMessage } from "@/lib/error";
import { copySensitiveText } from "@/lib/sensitiveClipboard";
import { shortId } from "@/lib/format";
import { isContactPeer } from "@/lib/contactVisibility";
import { publishAppearanceBeforeLink } from "@/lib/linkedAppearance";
import { publishContactsBeforeLink } from "@/lib/linkedContacts";
import { publishDraftsBeforeLink, useDrafts } from "@/store/drafts";
import { useCopyFeedback } from "./useCopyFeedback";
import { useSerialPoll } from "./useSerialPoll";
import { useChat } from "@/store/chat";
import { useAuth } from "@/store/auth";

const SYNC_OPTIONS = [
  { category: "history", label: "historySync" },
  { category: "attachments", label: "attachmentsSync" },
  { category: "profile", label: "profileSync" },
  { category: "appearance", label: "appearanceSync" },
  { category: "contacts", label: "contactsSync" },
] as const satisfies readonly { category: SyncCategory; label: string }[];

function combinedSyncPhase(
  device: DeviceSyncStatus,
): DeviceSyncStatus["phase"] {
  if (device.phase !== "up_to_date" && device.phase !== "waiting_for_files")
    return device.phase;
  if (device.document_requires_upgrade) return "error";
  if (device.document_phase === "disabled") return "disabled";
  if (device.document_error || device.document_phase === "error")
    return "error";
  return device.document_phase && device.document_phase !== "up_to_date"
    ? device.document_phase
    : device.phase;
}

function linkFailureKey(error: unknown) {
  switch (errorKind(error)) {
    case "link-no-direct-route":
      return "linkDevice.noDirectRoute";
    case "link-connection":
    case "relay-unreachable":
      return "linkDevice.connectionFailed";
    case "link-timeout":
      return "linkDevice.responseTimedOut";
    case "link-rejected":
      return "linkDevice.requestClosed";
    case "link-upgrade-required":
      return "linkDevice.credentialsUpgrade";
    case "link-protocol":
      return "linkDevice.invalidResponse";
    case "invalid-input":
      return "linkDevice.invalidCode";
    case "peer-unknown":
      return "linkDevice.deviceUnavailable";
    default:
      return "linkDevice.couldNotLink";
  }
}

export function LinkDeviceDialog({ menuItem = false }: { menuItem?: boolean }) {
  const owner = useAuth((state) => state.user?.id);
  const generation = useAuth((state) => state.generation);
  const runtime = useChat(
    (state) => `${state.myAccountId}:${state.runEpoch}:${state.identityEpoch}`,
  );
  if (!owner) return null;
  return (
    <OwnedLinkDeviceDialog
      key={`${owner}:${generation}:${runtime}`}
      owner={owner}
      menuItem={menuItem}
    />
  );
}

function OwnedLinkDeviceDialog({
  owner,
  menuItem,
}: {
  owner: string;
  menuItem: boolean;
}) {
  const { t } = useTranslation();
  const generation = useRef(useAuth.getState().generation).current;
  const ownsSession = () => {
    const current = useAuth.getState();
    return current.user?.id === owner && current.generation === generation;
  };
  const translate = useRef(t);
  useEffect(() => {
    translate.current = t;
  }, [t]);
  const user = useAuth((state) => state.user);
  const myAccountId = useChat((s) => s.myAccountId);
  const peers = useChat((s) => s.peers);
  const setError = useChat((s) => s.setError);

  const [open, setOpen] = useState(false);
  const [code, setCode] = useState<string | null>(null);
  const [joinPeer, setJoinPeer] = useState("");
  const [joinCode, setJoinCode] = useState("");
  const [accountChangeConfirmed, setAccountChangeConfirmed] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [msgIsError, setMsgIsError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [linkError, setLinkError] = useState<unknown>(null);
  const linkFeedback = useRef<HTMLDivElement>(null);
  const [refreshStatus, setRefreshStatus] = useState<
    "idle" | "loading" | "done"
  >("idle");
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const contactPeers = peers.filter(isContactPeer);
  const pairingPeers = contactPeers.filter(
    (peer) => peer.directly_reachable !== false,
  );
  const hasUnavailablePeers = pairingPeers.length < contactPeers.length;
  const selectedPeer = pairingPeers.find((peer) => peer.user_id === joinPeer);
  const targetLabel = selectedPeer
    ? `${selectedPeer.name || t("common.unnamed")} (${shortId(selectedPeer.user_id)})`
    : "";
  useEffect(() => {
    setAccountChangeConfirmed(false);
  }, [open, joinPeer, selectedPeer?.user_id, selectedPeer?.account_id]);
  const [creatingCode, setCreatingCode] = useState(false);
  const creatingCodeRef = useRef(false);
  const openRef = useRef(false);
  const dialogSession = useRef(0);
  const stopRef = useRef<Promise<void> | null>(null);
  const { status: copyStatus, copy, reset: resetCopy } = useCopyFeedback();
  const [syncAll, setSyncAll] = useState(true);
  const [syncPolicy, setSyncPolicy] = useState<DeviceSyncPolicy | null>(null);
  const [syncBusy, setSyncBusy] = useState(false);
  const [syncError, setSyncError] = useState<string | null>(null);
  const [syncStatus, setSyncStatus] = useState<DeviceSyncStatus[]>([]);
  const [syncStatusError, setSyncStatusError] = useState<string | null>(null);
  const [loginStatus, setLoginStatus] = useState<AccountLoginSyncStatus | null>(
    null,
  );
  const [loginStatusFailed, setLoginStatusFailed] = useState(false);

  useEffect(() => {
    if (linkError !== null)
      linkFeedback.current?.scrollIntoView?.({ block: "nearest" });
  }, [linkError]);

  useEffect(() => {
    if (!open) return;
    let active = true;
    setSyncPolicy(null);
    setSyncError(null);
    setSyncStatus([]);
    setSyncStatusError(null);
    void chat.getDeviceSyncPolicy(owner).then(
      (policy) => {
        if (active) setSyncPolicy(policy);
      },
      (error) => {
        if (active)
          setSyncError(
            translate.current("linkDevice.syncLoadFailed", {
              error: errorMessage(error),
            }),
          );
      },
    );
    return () => {
      active = false;
    };
  }, [open, owner]);

  useSerialPoll(
    open,
    5_000,
    () => chat.getDeviceSyncStatus(owner),
    (statuses) => {
      setSyncStatus(statuses);
      setSyncStatusError(null);
    },
    (error) => setSyncStatusError(errorMessage(error)),
  );

  useSerialPoll(
    open,
    5_000,
    () => chat.getAccountLoginSyncStatus(owner),
    (status) => {
      if (!ownsSession() || !status || status.account_id !== myAccountId) {
        setLoginStatus(null);
        setLoginStatusFailed(true);
        return;
      }
      setLoginStatus(status);
      setLoginStatusFailed(false);
    },
    () => {
      setLoginStatus(null);
      setLoginStatusFailed(true);
    },
  );

  const setSyncCategory = async (category: SyncCategory, enabled: boolean) => {
    if (!syncPolicy || syncBusy) return;
    setSyncAll(false);
    const session = dialogSession.current;
    setSyncBusy(true);
    setSyncError(null);
    try {
      const policy = await chat.setDeviceSyncCategory(category, enabled, owner);
      if (openRef.current && session === dialogSession.current)
        setSyncPolicy(policy);
    } catch (error) {
      if (openRef.current && session === dialogSession.current)
        setSyncError(
          t("linkDevice.syncSaveFailed", { error: errorMessage(error) }),
        );
    } finally {
      setSyncBusy(false);
    }
  };

  const stopLinking = () => {
    if (!ownsSession()) return Promise.resolve();
    const pending = chat.stopLinking().catch((e) => {
      if (ownsSession()) setError(errorMessage(e));
    });
    stopRef.current = pending;
    void pending.finally(() => {
      if (stopRef.current === pending) stopRef.current = null;
    });
    return pending;
  };

  const onOpenChange = (v: boolean) => {
    if (v !== openRef.current) dialogSession.current++;
    openRef.current = v;
    setOpen(v);
    if (v) {
      setSyncAll(true);
      setLoginStatus(null);
      setLoginStatusFailed(false);
    }
    if (!v) {
      if (code) void stopLinking();
      setCode(null);
      setMsg(null);
      setMsgIsError(false);
      resetCopy();
      setJoinCode("");
      setLinkError(null);
      setRefreshError(null);
      setRefreshStatus("idle");
    }
  };

  useEffect(
    () => () => {
      openRef.current = false;
      dialogSession.current++;
    },
    [],
  );

  const enableAllSync = async (current: () => boolean) => {
    for (const { category } of SYNC_OPTIONS) {
      if (!current()) return false;
      const policy = await chat.setDeviceSyncCategory(category, true, owner);
      if (!current()) return false;
      setSyncPolicy(policy);
    }
    return true;
  };

  const showCode = async () => {
    if (creatingCodeRef.current) return;
    const session = dialogSession.current;
    const sourceAccount = myAccountId;
    const ownsSourceAccount = () =>
      ownsSession() && useChat.getState().myAccountId === sourceAccount;
    creatingCodeRef.current = true;
    setCreatingCode(true);
    setMsg(null);
    setMsgIsError(false);
    try {
      if (stopRef.current) await stopRef.current;
      if (
        !ownsSourceAccount() ||
        !openRef.current ||
        session !== dialogSession.current
      )
        return;
      if (
        syncAll &&
        !(await enableAllSync(
          () =>
            ownsSourceAccount() &&
            openRef.current &&
            session === dialogSession.current,
        ))
      )
        return;
      if (syncAll || syncPolicy?.appearance)
        await publishAppearanceBeforeLink(owner);
      if (
        !ownsSourceAccount() ||
        !openRef.current ||
        session !== dialogSession.current
      )
        return;
      if (syncAll || syncPolicy?.contacts)
        await publishContactsBeforeLink(owner);
      if (
        !ownsSourceAccount() ||
        !openRef.current ||
        session !== dialogSession.current
      )
        return;
      if (syncAll || syncPolicy?.history) {
        if (!sourceAccount) throw new Error(t("linkDevice.couldNotLink"));
        await accountDocuments.publishLegacyControls(owner, sourceAccount);
        if (
          !ownsSourceAccount() ||
          !openRef.current ||
          session !== dialogSession.current
        )
          return;
        const report = await publishDraftsBeforeLink(owner);
        if (
          !ownsSourceAccount() ||
          !openRef.current ||
          session !== dialogSession.current
        )
          return;
        if (report.unmapped)
          setMsg(t("linkDevice.draftsUnmapped", { count: report.unmapped }));
      }
      const nextCode = await chat.startLinking();
      if (
        ownsSourceAccount() &&
        openRef.current &&
        session === dialogSession.current
      )
        setCode(nextCode);
      else if (ownsSourceAccount()) await stopLinking();
    } catch (e) {
      if (
        ownsSourceAccount() &&
        openRef.current &&
        session === dialogSession.current
      ) {
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
    await copy(() => copySensitiveText(code, t("settings.confirmCopyPrompt")));
  };

  const doLink = async () => {
    const pairingCode = joinCode.replace(/\s+/g, "");
    if (!selectedPeer || !pairingCode || !accountChangeConfirmed || busy)
      return;
    const session = dialogSession.current;
    setBusy(true);
    setMsg(null);
    setMsgIsError(false);
    setLinkError(null);
    try {
      const current = () =>
        ownsSession() && openRef.current && session === dialogSession.current;
      await useDrafts.getState().flush();
      if (!current()) return;
      if (useDrafts.getState().error) throw new Error(t("drafts.saveFailed"));
      if (syncAll && !(await enableAllSync(current))) return;
      if (!current()) return;
      const peer = useChat
        .getState()
        .peers.find((candidate) => candidate.user_id === selectedPeer.user_id);
      if (
        !peer ||
        !isContactPeer(peer) ||
        peer.directly_reachable === false ||
        peer.account_id !== selectedPeer.account_id
      )
        throw {
          kind: "peer-unknown",
          message: "Selected device changed during account setup",
        };
      await chat.linkDevice(joinPeer, pairingCode);
      if (!ownsSession()) return;
      await auth.adoptLinkedAccount();
      if (openRef.current && session === dialogSession.current) {
        setMsg(t("linkDevice.linked"));
        setJoinCode("");
      }
    } catch (e) {
      if (openRef.current && session === dialogSession.current) {
        setLinkError(() => e ?? new Error(errorMessage(e)));
      }
    } finally {
      setBusy(false);
    }
  };

  const refreshDevices = async () => {
    if (refreshStatus === "loading" || busy) return;
    const session = dialogSession.current;
    setRefreshStatus("loading");
    setRefreshError(null);
    try {
      await diag.rescan();
      if (ownsSession() && openRef.current && session === dialogSession.current)
        setRefreshStatus("done");
    } catch (error) {
      if (
        ownsSession() &&
        openRef.current &&
        session === dialogSession.current
      ) {
        setRefreshStatus("idle");
        setRefreshError(errorMessage(error));
      }
    }
  };

  const rekey = async () => {
    const session = dialogSession.current;
    setBusy(true);
    setMsg(null);
    setMsgIsError(false);
    try {
      const id = await chat.rekeyAccount();
      if (openRef.current && session === dialogSession.current)
        setMsg(t("linkDevice.rekeyed", { id: shortId(id, 12) }));
    } catch (e) {
      if (openRef.current && session === dialogSession.current) {
        setMsgIsError(true);
        setMsg(t("linkDevice.rekeyFailed", { error: errorMessage(e) }));
      }
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

        <label
          className="flex items-start gap-3 text-sm leading-relaxed"
          htmlFor="link-sync-all"
        >
          <input
            id="link-sync-all"
            type="checkbox"
            checked={syncAll}
            disabled={busy || creatingCode || syncBusy}
            onChange={(event) => setSyncAll(event.target.checked)}
            className="mt-1 h-4 w-4 shrink-0 accent-signal"
          />
          <span>
            <span className="block font-medium">{t("linkDevice.syncAll")}</span>
            <span className="text-xs text-muted-foreground">
              {t("linkDevice.syncAllHint")}
            </span>
          </span>
        </label>

        {/* Show this device's pairing code on the OTHER device. */}
        <section className="space-y-3 rounded-lg border p-4">
          <p className="font-display text-sm font-semibold tracking-tight">
            {t("linkDevice.addDevice")}
          </p>
          <p className="text-sm leading-relaxed">
            {t("linkDevice.keepAccountHint")}
          </p>
          {user?.username && (
            <div
              data-testid="link-source-identity"
              className="space-y-1 break-words text-sm leading-relaxed [overflow-wrap:anywhere]"
            >
              <p>{t("linkDevice.sourceLoginName", { login: user.username })}</p>
              {user.display_name?.trim() && (
                <p>
                  {t("linkDevice.sourceNickname", {
                    nickname: user.display_name,
                  })}
                </p>
              )}
            </div>
          )}
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
          <p className="text-sm leading-relaxed">
            {t("linkDevice.adoptAccountHint")}
          </p>
          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("linkDevice.directConnectionHint")}
          </p>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <label
              htmlFor="link-device-peer"
              className="block text-xs font-medium text-muted-foreground"
            >
              {t("linkDevice.device")}
            </label>
            <Button
              variant="ghost"
              size="sm"
              data-testid="link-refresh-devices"
              disabled={busy || refreshStatus === "loading"}
              onClick={() => void refreshDevices()}
            >
              {t(
                refreshStatus === "loading"
                  ? "linkDevice.refreshing"
                  : "linkDevice.refreshDevices",
              )}
            </Button>
          </div>
          <select
            id="link-device-peer"
            value={selectedPeer?.user_id ?? ""}
            disabled={busy}
            aria-describedby="link-device-route-hint"
            onChange={(e) => {
              setJoinPeer(e.target.value);
              setLinkError(null);
            }}
            className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm transition-colors hover:border-ring focus-visible:border-ring focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background [@media(hover:none)]:min-h-11"
          >
            <option value="">{t("linkDevice.pickDevice")}</option>
            {pairingPeers.map((p) => (
              <option key={p.user_id} value={p.user_id}>
                {p.name || t("common.unnamed")} ({shortId(p.user_id)})
              </option>
            ))}
          </select>
          <p
            id="link-device-route-hint"
            className="text-xs leading-relaxed text-muted-foreground"
          >
            {t(
              !selectedPeer && hasUnavailablePeers
                ? "linkDevice.relayOnlyHint"
                : "linkDevice.deviceHint",
            )}
          </p>
          {refreshStatus === "done" && (
            <p role="status" className="text-xs text-muted-foreground">
              {t("linkDevice.refreshStarted")}
            </p>
          )}
          {refreshError && (
            <p role="alert" className="break-words text-xs text-destructive">
              {t("linkDevice.refreshFailed", { error: refreshError })}
            </p>
          )}
          {selectedPeer && (
            <div
              data-testid="link-account-change"
              className="space-y-1 break-words rounded-md bg-muted/50 p-3 text-sm leading-relaxed"
            >
              <p className="font-medium">
                {t("linkDevice.accountChange", { device: targetLabel })}
              </p>
              <p>
                {t("linkDevice.accountChangeIds", {
                  current: myAccountId ? shortId(myAccountId, 12) : "—",
                  target: shortId(
                    selectedPeer.account_id || selectedPeer.user_id,
                    12,
                  ),
                })}
              </p>
              <p className="text-xs text-muted-foreground">
                {t("linkDevice.localProfileHint")}
              </p>
            </div>
          )}
          <label
            htmlFor="link-device-code"
            className="block text-xs font-medium text-muted-foreground"
          >
            {t("linkDevice.pairingCode")}
          </label>
          <div className="space-y-3">
            <Input
              id="link-device-code"
              autoFocus
              placeholder={t("linkDevice.pairingCode")}
              value={joinCode}
              disabled={busy}
              autoComplete="off"
              autoCapitalize="none"
              spellCheck={false}
              aria-describedby={
                linkError !== null
                  ? "link-device-feedback"
                  : "link-device-code-hint"
              }
              onChange={(e) => {
                setJoinCode(e.target.value);
                setLinkError(null);
              }}
              className="min-w-0 font-mono tracking-widest"
            />
            {selectedPeer && (
              <label
                className="flex items-start gap-2 text-sm leading-relaxed"
                htmlFor="link-account-consent"
              >
                <input
                  id="link-account-consent"
                  type="checkbox"
                  className="mt-1 h-4 w-4 shrink-0 accent-signal"
                  checked={accountChangeConfirmed}
                  disabled={busy}
                  onChange={(event) =>
                    setAccountChangeConfirmed(event.target.checked)
                  }
                />
                <span className="min-w-0 [overflow-wrap:anywhere]">
                  {t("linkDevice.confirmAccountChange", {
                    device: targetLabel,
                  })}
                </span>
              </label>
            )}
            <Button
              data-testid="link-adopt-account"
              disabled={
                !selectedPeer ||
                !joinCode.trim() ||
                !accountChangeConfirmed ||
                busy
              }
              onClick={doLink}
              aria-busy={busy}
              className="h-auto min-h-10 w-full whitespace-normal py-2"
            >
              {busy && (
                <Loader2
                  aria-hidden="true"
                  className="h-4 w-4 animate-spin motion-reduce:animate-none"
                />
              )}
              <span
                aria-live="polite"
                className="min-w-0 [overflow-wrap:anywhere]"
              >
                {t(busy ? "linkDevice.connecting" : "linkDevice.adoptAccount", {
                  device: targetLabel || t("linkDevice.selectedDevice"),
                })}
              </span>
            </Button>
          </div>
          <p
            id="link-device-code-hint"
            className="text-xs leading-relaxed text-muted-foreground"
          >
            {t("linkDevice.codeHint")}
          </p>
          {linkError !== null && (
            <div
              ref={linkFeedback}
              id="link-device-feedback"
              role="alert"
              className="space-y-2 break-words text-sm"
            >
              <p className="font-medium text-destructive">
                {t(linkFailureKey(linkError))}
              </p>
              <p className="text-xs leading-relaxed text-muted-foreground">
                {t("linkDevice.retryHint")}
              </p>
              <details className="text-xs text-muted-foreground">
                <summary className="cursor-pointer py-1">
                  {t("linkDevice.technicalDetails")}
                </summary>
                <p className="mt-1 whitespace-pre-wrap break-words">
                  {errorMessage(linkError)}
                </p>
              </details>
            </div>
          )}
          <details className="text-xs text-muted-foreground">
            <summary className="cursor-pointer py-1">
              {t("linkDevice.connectionTips")}
            </summary>
            <ul className="mt-2 list-disc space-y-1 pl-4 leading-relaxed">
              <li>{t("linkDevice.networkTip")}</li>
              <li>{t("linkDevice.awakeTip")}</li>
              <li>{t("linkDevice.firewallTip")}</li>
            </ul>
          </details>
        </section>

        <section
          className="space-y-2 rounded-lg border p-4"
          data-testid="account-login-sync-status"
          aria-live="polite"
        >
          <p className="font-display text-sm font-semibold tracking-tight">
            {t("linkDevice.loginSyncTitle")}
          </p>
          <p className="text-sm leading-relaxed">
            {t(
              loginStatusFailed
                ? "linkDevice.loginSyncUnavailable"
                : !loginStatus
                  ? "linkDevice.loginSyncChecking"
                  : loginStatus.available_revision === null
                    ? "linkDevice.loginSyncWaiting"
                    : loginStatus.applied_revision ===
                        loginStatus.available_revision
                      ? "linkDevice.loginSyncApplied"
                      : "linkDevice.loginSyncApplying",
            )}
          </p>
          {loginStatus?.devices.map((device) => (
            <p
              key={device.device_id}
              className="text-xs leading-relaxed text-muted-foreground"
            >
              <span className="font-mono">{shortId(device.device_id, 8)}</span>
              {" · "}
              {t(
                device.phase === "unsupported"
                  ? "linkDevice.credentialsUpgrade"
                  : device.phase === "permission_denied"
                    ? "linkDevice.loginSyncAccessDenied"
                    : device.phase === "retry_scheduled"
                      ? "linkDevice.loginSyncRetry"
                      : device.phase === "up_to_date"
                        ? "linkDevice.loginSyncReceived"
                        : device.phase === "synchronizing"
                          ? "linkDevice.loginSyncReceiving"
                          : "linkDevice.loginSyncWaiting",
              )}
            </p>
          ))}
        </section>

        <section className="space-y-3 rounded-lg border p-4">
          <p className="font-display text-sm font-semibold tracking-tight">
            {t("linkDevice.syncTitle")}
          </p>
          {SYNC_OPTIONS.map(({ category, label }) => (
            <div
              key={category}
              className="flex items-center justify-between gap-4"
            >
              <div className="min-w-0">
                <p className="text-sm font-medium">
                  {t(`linkDevice.${label}`)}
                </p>
                <p className="text-xs leading-relaxed text-muted-foreground">
                  {t(`linkDevice.${label}Description`)}
                </p>
              </div>
              <Switch
                checked={syncPolicy?.[category] ?? false}
                disabled={!syncPolicy || syncBusy || busy || creatingCode}
                aria-label={t(`linkDevice.${label}`)}
                onCheckedChange={(enabled) =>
                  void setSyncCategory(category, enabled)
                }
              />
            </div>
          ))}
          {syncError && (
            <p role="alert" className="text-xs text-destructive">
              {syncError}
            </p>
          )}
          {syncStatus.length > 0 && (
            <div className="space-y-1 border-t pt-3" aria-live="polite">
              {syncStatus.map((device) => (
                <div
                  key={device.device_id}
                  className="text-xs text-muted-foreground"
                >
                  <span className="font-mono">
                    {shortId(device.device_id, 8)}
                  </span>{" "}
                  <span>
                    {t(`linkDevice.syncPhase.${combinedSyncPhase(device)}`)}
                  </span>
                  {combinedSyncPhase(device) === "waiting_for_files" && (
                    <p>{t("linkDevice.waitingForFilesHint")}</p>
                  )}
                  {device.document_requires_upgrade ? (
                    <p role="alert">{t("linkDevice.documentsUpgrade")}</p>
                  ) : device.document_phase === "disabled" ? (
                    <p>{t("linkDevice.documentsDisabled")}</p>
                  ) : device.document_error ||
                    device.document_phase === "error" ? (
                    <p role="alert">{t("linkDevice.documentsError")}</p>
                  ) : null}
                  {device.legacy_file_metadata && (
                    <p>{t("linkDevice.legacyFileMetadata")}</p>
                  )}
                  {device.pending_attachments > 0 && (
                    <span>
                      {" "}
                      ·{" "}
                      {t("linkDevice.pendingAttachments", {
                        count: device.pending_attachments,
                      })}
                    </span>
                  )}
                  {device.error && (
                    <p role="alert" className="text-destructive">
                      {device.error}
                    </p>
                  )}
                </div>
              ))}
            </div>
          )}
          {syncStatusError && (
            <p role="alert" className="text-xs text-destructive">
              {t("linkDevice.syncStatusFailed", { error: syncStatusError })}
            </p>
          )}
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
