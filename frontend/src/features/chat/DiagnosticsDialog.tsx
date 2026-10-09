import { useCallback, useEffect, useRef, useState } from "react";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  Activity,
  Copy,
  FileText,
  FolderOpen,
  Info,
  Radar,
  RefreshCw,
  ShieldAlert,
  Trash2,
  Wifi,
  WifiOff,
  Loader2,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { IdentityGlyph, PresenceDot } from "@/components/identity";
import { OfflineConnectDialog } from "./OfflineConnectDialog";
import { diag, obs } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import { formatAgo, formatTime, shortId } from "@/lib/format";
import { useChat } from "@/store/chat";
import { CONNECTION_LABEL, connectionState } from "./connectionState";
import type { DiagNetworkInfo, DiagPeerInfo, EnvInfo } from "@/lib/types";

const POLL_MS = 1500;
const LOG_CAP = 200;
/** A peer seen within this window reads as "online" (breathing teal). */
const ONLINE_SECS = 30;

interface LogLine {
  ts: number;
  text: string;
}

/** A monospace value that copies itself to the clipboard when clicked. */
function CopyValue({ value, label }: { value: string; label?: string }) {
  const { t } = useTranslation();
  const [copyStatus, setCopyStatus] = useState<"copied" | "failed" | null>(
    null,
  );
  const copy = async () => {
    setCopyStatus(null);
    try {
      await navigator.clipboard.writeText(value);
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
  return (
    <button
      type="button"
      onClick={() => void copy()}
      title={t("common.copy")}
      className="group inline-flex max-w-full items-center gap-1 font-mono text-xs hover:text-foreground"
    >
      <span className="truncate">{label ?? value}</span>
      <Copy className="h-3 w-3 shrink-0 opacity-40 group-hover:opacity-100" />
      {copyStatus && (
        <span
          role={copyStatus === "failed" ? "alert" : "status"}
          className={
            copyStatus === "failed"
              ? "text-[10px] text-destructive"
              : "text-[10px] text-signal"
          }
        >
          {t(copyStatus === "failed" ? "common.copyFailed" : "common.copied")}
        </span>
      )}
    </button>
  );
}

/** A boxed section with a display-font heading + icon. */
function Panel({
  icon,
  title,
  action,
  children,
}: {
  icon: React.ReactNode;
  title: string;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section className="space-y-2 rounded-lg border bg-card/40 p-3">
      <div className="flex items-center justify-between gap-2">
        <p className="flex items-center gap-2 font-display text-sm font-semibold tracking-tight">
          <span className="text-signal">{icon}</span>
          {title}
        </p>
        {action}
      </div>
      {children}
    </section>
  );
}

function diffPeers(
  prev: DiagPeerInfo[],
  next: DiagPeerInfo[],
  t: TFunction,
): LogLine[] {
  const ts = Date.now();
  const prevIds = new Set(prev.map((p) => p.user_id));
  const nextIds = new Set(next.map((p) => p.user_id));
  const lines: LogLine[] = [];
  for (const p of next) {
    if (!prevIds.has(p.user_id)) {
      lines.push({
        ts,
        text: t("diagnostics.peerAppeared", {
          name: p.name || t("common.unnamed"),
          ip: p.ip,
        }),
      });
    }
  }
  for (const p of prev) {
    if (!nextIds.has(p.user_id)) {
      lines.push({
        ts,
        text: t("diagnostics.peerDisappeared", {
          name: p.name || t("common.unnamed"),
        }),
      });
    }
  }
  return lines;
}

export function DiagnosticsDialog({
  open: controlledOpen,
  onOpenChange,
  initialTab = "overview",
}: {
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  initialTab?: "overview" | "peers" | "logs" | "help";
} = {}) {
  const { t } = useTranslation();
  const ready = useChat((s) => s.ready);
  const bootFailed = useChat((s) => s.bootFailed);
  const [internalOpen, setInternalOpen] = useState(false);
  const open = controlledOpen ?? internalOpen;
  const setOpen = onOpenChange ?? setInternalOpen;
  const [info, setInfo] = useState<DiagNetworkInfo | null>(null);
  const [peers, setPeers] = useState<DiagPeerInfo[]>([]);
  const [log, setLog] = useState<LogLine[]>([]);
  const [env, setEnv] = useState<EnvInfo | null>(null);
  const [networkLoading, setNetworkLoading] = useState(true);
  const [networkError, setNetworkError] = useState<string | null>(null);
  const [environmentLoading, setEnvironmentLoading] = useState(true);
  const [environmentError, setEnvironmentError] = useState<string | null>(null);
  const [peersLoading, setPeersLoading] = useState(true);
  const [peersError, setPeersError] = useState<string | null>(null);
  const [logTail, setLogTail] = useState<string | null>(null);
  const [error, setLocalError] = useState<string | null>(null);
  const [rescanned, setRescanned] = useState(false);
  const prevPeers = useRef<DiagPeerInfo[]>([]);

  const loadNetwork = useCallback(async () => {
    setNetworkLoading(true);
    setNetworkError(null);
    try {
      setInfo(await diag.networkInfo());
    } catch (e) {
      setInfo(null);
      setNetworkError(errorMessage(e));
    } finally {
      setNetworkLoading(false);
    }
  }, []);

  const loadEnvironment = useCallback(async () => {
    setEnvironmentLoading(true);
    setEnvironmentError(null);
    try {
      setEnv(await obs.envInfo());
    } catch (e) {
      setEnv(null);
      setEnvironmentError(errorMessage(e));
    } finally {
      setEnvironmentLoading(false);
    }
  }, []);

  const rescan = async () => {
    setLocalError(null);
    try {
      await diag.rescan();
      setRescanned(true);
      setTimeout(() => setRescanned(false), 1500);
    } catch (e) {
      setLocalError(errorMessage(e));
    }
  };

  const revealLogs = async () => {
    setLocalError(null);
    try {
      await revealItemInDir(await obs.logFile());
    } catch (e) {
      setLocalError(errorMessage(e));
    }
  };

  const copyLogTail = async () => {
    setLocalError(null);
    try {
      const tail = await obs.logTail();
      await navigator.clipboard.writeText(tail);
    } catch (e) {
      setLocalError(errorMessage(e));
    }
  };

  const showLogTail = async () => {
    setLocalError(null);
    try {
      setLogTail(await obs.logTail());
    } catch (e) {
      setLocalError(errorMessage(e));
    }
  };

  const saveLogTail = async () => {
    setLocalError(null);
    try {
      const dest = await saveDialog({ defaultPath: "mesh-talk-log.txt" });
      if (typeof dest === "string") await obs.saveLogTail(dest);
    } catch (e) {
      setLocalError(errorMessage(e));
    }
  };

  const poll = useCallback(async () => {
    try {
      const next = await diag.getPeers();
      const lines = diffPeers(prevPeers.current, next, t);
      prevPeers.current = next;
      setPeers(next);
      setPeersError(null);
      if (lines.length > 0) {
        setLog((l) => [...lines.reverse(), ...l].slice(0, LOG_CAP));
      }
    } catch (e) {
      setPeersError(errorMessage(e));
    } finally {
      setPeersLoading(false);
    }
  }, [t]);

  useEffect(() => {
    if (!open) return;
    // Reset the diff baseline each time the panel opens so we don't replay a stale snapshot.
    prevPeers.current = [];
    setInfo(null);
    setEnv(null);
    setPeers([]);
    setPeersLoading(true);
    setPeersError(null);
    void loadNetwork();
    void loadEnvironment();
    setLogTail(null);
    setLocalError(null);
    void poll();
    const id = setInterval(() => void poll(), POLL_MS);
    return () => clearInterval(id);
  }, [open, poll, loadNetwork, loadEnvironment]);

  // Group discovered peers by account (devices sharing an account_id are one user).
  const grouped = new Map<string, DiagPeerInfo[]>();
  const solo: DiagPeerInfo[] = [];
  for (const p of peers) {
    if (p.account_id) {
      const arr = grouped.get(p.account_id) ?? [];
      arr.push(p);
      grouped.set(p.account_id, arr);
    } else {
      solo.push(p);
    }
  }
  const onlineAccounts = new Set(
    peers
      .filter(
        (p) =>
          !p.post_office &&
          p.last_seen_secs <= ONLINE_SECS &&
          p.account_id !== info?.account_id,
      )
      .map((p) => p.account_id ?? p.user_id),
  ).size;
  const connection = connectionState({
    bootFailed,
    ready,
    noNetwork: info !== null && info.interfaces.length === 0,
    onlinePeople: onlineAccounts,
  });

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      {controlledOpen === undefined && (
        <DialogTrigger asChild>
          <Button
            variant="ghost"
            data-testid="sidebar-nav-connection"
            title={t("diagnostics.trigger")}
            aria-label={t("redesign.connection")}
            className="h-10 w-full justify-start gap-3 px-3 text-muted-foreground hover:text-foreground"
          >
            <Radar className="h-4 w-4" />
            <span>{t("redesign.connection")}</span>
          </Button>
        </DialogTrigger>
      )}
      <DialogContent
        className="max-w-[min(50rem,calc(100vw-2rem))]"
        data-testid="diagnostics-dialog"
      >
        <DialogHeader>
          <DialogTitle>{t("redesign.connection")}</DialogTitle>
          <DialogDescription>{t("diagnostics.description")}</DialogDescription>
        </DialogHeader>
        <div
          data-testid="diagnostics-status"
          role="status"
          className="flex items-center gap-2 rounded-md border bg-muted/40 px-3 py-2 text-sm"
        >
          {connection === "starting" || connection === "searching" ? (
            <Loader2
              className="h-4 w-4 animate-spin text-signal motion-reduce:animate-none"
              aria-hidden="true"
            />
          ) : (
            <span
              aria-hidden="true"
              className={`h-2 w-2 shrink-0 rounded-full ${connection === "failed" ? "bg-destructive" : connection === "ready" ? "bg-signal" : connection === "no-network" ? "bg-attention" : "bg-muted-foreground"}`}
            />
          )}
          <span
            className={`font-medium ${connection === "no-network" ? "text-attention" : ""}`}
          >
            {t(CONNECTION_LABEL[connection])}
          </span>
          {connection === "ready" && (
            <span className="ml-auto text-xs text-muted-foreground">
              {t("sidebar.peopleOnLan", { count: onlineAccounts })}
            </span>
          )}
        </div>
        {error && (
          <div
            role="alert"
            data-testid="diagnostics-action-error"
            className="flex items-start justify-between gap-3 rounded-md border border-destructive/35 bg-destructive/5 px-3 py-2 text-sm text-destructive"
          >
            <span>{t("diagnostics.actionFailed", { error })}</span>
            <button
              type="button"
              onClick={() => setLocalError(null)}
              className="shrink-0 font-medium underline underline-offset-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              {t("common.dismiss")}
            </button>
          </div>
        )}

        <Tabs defaultValue={initialTab} className="w-full">
          <TabsList className="grid w-full grid-cols-4">
            <TabsTrigger
              value="overview"
              data-testid="diagnostics-tab-overview"
            >
              {t("diagnostics.tabOverview")}
            </TabsTrigger>
            <TabsTrigger value="peers" data-testid="diagnostics-tab-peers">
              {t("diagnostics.tabPeers")}
              <Badge className="ml-1.5 bg-muted text-muted-foreground">
                {peers.length}
              </Badge>
            </TabsTrigger>
            <TabsTrigger value="logs" data-testid="diagnostics-tab-logs">
              {t("diagnostics.tabLogs")}
            </TabsTrigger>
            <TabsTrigger value="help" data-testid="diagnostics-tab-help">
              {t("diagnostics.tabHelp")}
            </TabsTrigger>
          </TabsList>

          <div className="max-h-[60vh] overflow-y-auto pr-0.5">
            {/* Overview: this device + environment */}
            <TabsContent value="overview" className="space-y-4">
              <Panel
                icon={<Wifi className="h-4 w-4" />}
                title={t("diagnostics.thisDevice")}
              >
                {info ? (
                  <dl className="grid grid-cols-[auto,1fr] gap-x-4 gap-y-1.5 text-sm">
                    <dt className="text-muted-foreground">
                      {t("diagnostics.name")}
                    </dt>
                    <dd className="truncate">{info.own_name || "—"}</dd>
                    <dt className="text-muted-foreground">
                      {t("diagnostics.fingerprint")}
                    </dt>
                    <dd>
                      <CopyValue value={info.own_user_id} />
                    </dd>
                    <dt className="text-muted-foreground">
                      {t("diagnostics.account")}
                    </dt>
                    <dd>
                      <CopyValue value={info.account_id} />
                    </dd>
                    <dt className="text-muted-foreground">
                      {t("diagnostics.listenPort")}
                    </dt>
                    <dd className="font-mono text-xs">
                      {info.listen_tcp_port}
                    </dd>
                    <dt className="text-muted-foreground">
                      {t("diagnostics.discoveryPort")}
                    </dt>
                    <dd className="font-mono text-xs">{info.discovery_port}</dd>
                    <dt className="text-muted-foreground">
                      {t("diagnostics.multicastGroup")}
                    </dt>
                    <dd className="font-mono text-xs">
                      {info.multicast_group}
                    </dd>
                    <dt className="text-muted-foreground">
                      {t("diagnostics.interfaces")}
                    </dt>
                    <dd className="flex flex-wrap gap-2">
                      {info.interfaces.length === 0 ? (
                        <span className="text-xs text-muted-foreground">
                          {t("diagnostics.noneFound")}
                        </span>
                      ) : (
                        info.interfaces.map((ip) => (
                          <CopyValue key={ip} value={ip} />
                        ))
                      )}
                    </dd>
                  </dl>
                ) : networkLoading ? (
                  <p role="status" className="text-sm text-muted-foreground">
                    {t("diagnostics.loading")}
                  </p>
                ) : networkError ? (
                  <div
                    role="alert"
                    className="space-y-2 text-sm text-destructive"
                  >
                    <p>
                      {t("diagnostics.networkLoadFailed", {
                        error: networkError,
                      })}
                    </p>
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => void loadNetwork()}
                    >
                      {t("contactVisibility.retry")}
                    </Button>
                  </div>
                ) : (
                  <p className="text-sm text-muted-foreground">
                    {t("diagnostics.nodeNotReady")}
                  </p>
                )}
              </Panel>

              <Panel
                icon={<Info className="h-4 w-4" />}
                title={t("diagnostics.environment")}
              >
                {env ? (
                  <dl className="grid grid-cols-[auto,1fr] gap-x-4 gap-y-1.5 text-sm">
                    <dt className="text-muted-foreground">
                      {t("diagnostics.appVersion")}
                    </dt>
                    <dd>
                      <CopyValue value={env.app_version} />
                    </dd>
                    <dt className="text-muted-foreground">
                      {t("diagnostics.platform")}
                    </dt>
                    <dd>
                      <CopyValue
                        value={`${env.os}/${env.arch} (${env.target}, ${env.build_profile})`}
                      />
                    </dd>
                    <dt className="text-muted-foreground">
                      {t("diagnostics.dataDir")}
                    </dt>
                    <dd>
                      <CopyValue value={env.data_dir} />
                    </dd>
                    <dt className="text-muted-foreground">
                      {t("diagnostics.logsDir")}
                    </dt>
                    <dd>
                      <CopyValue value={env.logs_dir} />
                    </dd>
                  </dl>
                ) : environmentLoading ? (
                  <p role="status" className="text-sm text-muted-foreground">
                    {t("diagnostics.loading")}
                  </p>
                ) : environmentError ? (
                  <div
                    role="alert"
                    className="space-y-2 text-sm text-destructive"
                  >
                    <p>
                      {t("diagnostics.environmentLoadFailed", {
                        error: environmentError,
                      })}
                    </p>
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() => void loadEnvironment()}
                    >
                      {t("contactVisibility.retry")}
                    </Button>
                  </div>
                ) : (
                  <p className="text-sm text-muted-foreground">
                    {t("diagnostics.nodeNotReady")}
                  </p>
                )}
              </Panel>
            </TabsContent>

            {/* Peers: discovered roster */}
            <TabsContent value="peers" className="space-y-4">
              <Panel
                icon={<Radar className="h-4 w-4" />}
                title={t("diagnostics.discoveredPeers")}
                action={
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => void rescan()}
                    title={t("diagnostics.rescanHint")}
                  >
                    <RefreshCw className="h-3.5 w-3.5" />
                    {rescanned
                      ? t("diagnostics.rescanned")
                      : t("diagnostics.rescan")}
                  </Button>
                }
              >
                {peersLoading && peers.length === 0 && (
                  <p role="status" className="text-sm text-muted-foreground">
                    {t("diagnostics.loading")}
                  </p>
                )}
                {peersError && (
                  <p role="alert" className="text-sm text-destructive">
                    {t("diagnostics.peersLoadFailed", { error: peersError })}
                  </p>
                )}
                {!peersLoading && !peersError && peers.length === 0 && (
                  <p className="text-sm text-muted-foreground">
                    {t("diagnostics.noPeers")}
                  </p>
                )}
                <div className="space-y-2">
                  {[...grouped.entries()].map(([acct, devices]) => (
                    <div key={acct} className="rounded-md bg-muted/40 p-2">
                      <div className="mb-1 flex items-center gap-2 text-xs text-muted-foreground">
                        <span>{t("diagnostics.account")}</span>
                        <CopyValue
                          value={acct}
                          label={`${shortId(acct, 12)}…`}
                        />
                        <span>
                          ·{" "}
                          {t("diagnostics.deviceCount", {
                            count: devices.length,
                          })}
                        </span>
                      </div>
                      {devices.map((p) => (
                        <PeerRow key={p.user_id} p={p} />
                      ))}
                    </div>
                  ))}
                  {solo.map((p) => (
                    <div key={p.user_id} className="rounded-md bg-muted/40 p-2">
                      <PeerRow p={p} />
                    </div>
                  ))}
                </div>
              </Panel>

              <Panel
                icon={<Activity className="h-4 w-4" />}
                title={t("diagnostics.activityLog")}
                action={
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setLog([])}
                    disabled={log.length === 0}
                  >
                    <Trash2 className="h-3.5 w-3.5" /> {t("common.clear")}
                  </Button>
                }
              >
                {log.length === 0 ? (
                  <p className="text-sm text-muted-foreground">
                    {t("diagnostics.noActivity")}
                  </p>
                ) : (
                  <ul className="max-h-40 space-y-0.5 overflow-y-auto text-xs">
                    {log.map((l, i) => (
                      <li key={i} className="flex gap-2">
                        <span className="shrink-0 font-mono text-muted-foreground">
                          {formatTime(l.ts)}
                        </span>
                        <span>{l.text}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </Panel>
            </TabsContent>

            {/* Logs */}
            <TabsContent value="logs" className="space-y-4">
              <Panel
                icon={<FileText className="h-4 w-4" />}
                title={t("diagnostics.logs")}
              >
                <div className="flex flex-wrap gap-2">
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => void revealLogs()}
                  >
                    <FolderOpen className="h-3.5 w-3.5" />
                    {t("diagnostics.revealLogs")}
                  </Button>
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => void copyLogTail()}
                  >
                    <Copy className="h-3.5 w-3.5" />
                    {t("diagnostics.copyLogTail")}
                  </Button>
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => void saveLogTail()}
                  >
                    {t("diagnostics.saveLogTail")}
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => void showLogTail()}
                  >
                    {t("diagnostics.showLogTail")}
                  </Button>
                </div>
                {logTail !== null && (
                  <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-words rounded-md border bg-muted/50 p-2 font-mono text-[11px]">
                    {logTail || t("diagnostics.logEmpty")}
                  </pre>
                )}
              </Panel>
            </TabsContent>

            {/* Troubleshoot */}
            <TabsContent value="help">
              <Troubleshoot info={info} />
            </TabsContent>
          </div>
        </Tabs>
      </DialogContent>
    </Dialog>
  );
}

/** A copyable shell command block, rendered as a mono code block. */
function CommandLine({ cmd }: { cmd: string }) {
  const { t } = useTranslation();
  const [copyStatus, setCopyStatus] = useState<"copied" | "failed" | null>(
    null,
  );
  const copy = async () => {
    setCopyStatus(null);
    try {
      await navigator.clipboard.writeText(cmd);
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
  return (
    <button
      type="button"
      onClick={() => void copy()}
      title={t("common.copy")}
      className="group flex w-full items-start gap-2 rounded-md border bg-muted/50 p-2.5 text-left font-mono text-[11px] leading-relaxed hover:bg-muted"
    >
      <span className="min-w-0 flex-1 whitespace-pre-wrap break-all">
        {cmd}
      </span>
      <Copy className="mt-0.5 h-3 w-3 shrink-0 opacity-40 group-hover:opacity-100" />
      {copyStatus && (
        <span
          role={copyStatus === "failed" ? "alert" : "status"}
          className={
            copyStatus === "failed"
              ? "text-[10px] text-destructive"
              : "text-[10px] text-signal"
          }
        >
          {t(copyStatus === "failed" ? "common.copyFailed" : "common.copied")}
        </span>
      )}
    </button>
  );
}

/** Per-OS, port-interpolated firewall allow-rules so peers can find each other on the LAN.
 * Static + copyable (no elevated auto-run): the discovery port is UDP multicast, the listen
 * port is the per-session TCP port. */
function Troubleshoot({ info }: { info: DiagNetworkInfo | null }) {
  const { t } = useTranslation();
  const [offlineOpen, setOfflineOpen] = useState(false);
  const udp = info?.discovery_port ?? 47474;
  const tcp = info?.listen_tcp_port ?? 0;
  const noNetwork = info?.interfaces.length === 0;
  const app = "mesh-talk";
  return (
    <Panel
      icon={<ShieldAlert className="h-4 w-4" />}
      title={t("diagnostics.troubleshoot")}
    >
      <p className="text-xs leading-relaxed text-muted-foreground">
        {t("diagnostics.troubleshootHint", { udp, tcp })}
      </p>
      <Button
        variant="secondary"
        size="sm"
        data-testid="diagnostics-offline-connect"
        onClick={() => setOfflineOpen(true)}
      >
        <WifiOff className="h-3.5 w-3.5" />
        {t("offlineConnect.trigger")}
      </Button>
      <OfflineConnectDialog
        open={offlineOpen}
        onOpenChange={setOfflineOpen}
        noNetwork={noNetwork}
      />
      <div className="space-y-3">
        <div className="space-y-1">
          <p className="font-mono text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
            Windows
          </p>
          <CommandLine
            cmd={`netsh advfirewall firewall add rule name="${app} UDP discovery" dir=in action=allow protocol=UDP localport=${udp}\nnetsh advfirewall firewall add rule name="${app} TCP" dir=in action=allow protocol=TCP localport=${tcp}`}
          />
        </div>
        <div className="space-y-1">
          <p className="font-mono text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
            Linux (ufw / firewalld)
          </p>
          <CommandLine
            cmd={`sudo ufw allow ${udp}/udp comment '${app} discovery'\nsudo ufw allow ${tcp}/tcp comment '${app}'`}
          />
          <CommandLine
            cmd={`sudo firewall-cmd --add-port=${udp}/udp --permanent\nsudo firewall-cmd --add-port=${tcp}/tcp --permanent\nsudo firewall-cmd --reload`}
          />
        </div>
        <div className="space-y-1">
          <p className="font-mono text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
            macOS
          </p>
          <p className="text-xs leading-relaxed text-muted-foreground">
            {t("diagnostics.macosFirewall")}
          </p>
        </div>
      </div>
    </Panel>
  );
}

export function PeerRow({ p }: { p: DiagPeerInfo }) {
  const { t } = useTranslation();
  const status = p.last_seen_secs <= ONLINE_SECS ? "online" : "recent";
  return (
    <div className="flex items-center justify-between gap-2 py-1.5">
      <div className="flex min-w-0 items-center gap-2.5">
        <div className="relative shrink-0">
          <IdentityGlyph
            seed={p.account_id ?? p.user_id}
            size={28}
            title={p.name}
          />
          <PresenceDot
            status={status}
            size="sm"
            className="absolute -bottom-0.5 -right-0.5"
          />
        </div>
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <span className="truncate font-display text-sm font-semibold tracking-tight">
              {p.name || t("common.unnamed")}
            </span>
            {p.post_office && (
              <Badge className="bg-signal/15 text-signal">
                {t("diagnostics.postOffice")}
              </Badge>
            )}
          </div>
          <div className="flex flex-wrap items-center gap-x-3 text-xs text-muted-foreground">
            <CopyValue value={p.user_id} label={shortId(p.user_id, 12)} />
            <CopyValue value={`${p.ip}:${p.tcp_port}`} />
          </div>
        </div>
      </div>
      <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
        {formatAgo(p.last_seen_secs)}
      </span>
    </div>
  );
}
