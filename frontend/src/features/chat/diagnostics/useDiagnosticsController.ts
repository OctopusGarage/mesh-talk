import { useCallback, useEffect, useRef, useState } from "react";
import type { TFunction } from "i18next";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { diag, obs } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import type { DiagNetworkInfo, DiagPeerInfo, EnvInfo } from "@/lib/types";
import { useSerialPoll } from "../useSerialPoll";
import { readWithDeadline } from "@/lib/readDeadline";
const POLL_MS = 1500;
const LOG_CAP = 200;
interface LogLine {
  ts: number;
  text: string;
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

export function useDiagnosticsController(open: boolean, t: TFunction) {
  const view = useRef({ active: false, generation: 0 });
  const feedbackTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const actionRequests = useRef(new Map<string, number>());
  const capture = useCallback(() => {
    const generation = view.current.generation;
    return () => view.current.active && view.current.generation === generation;
  }, []);
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
  const networkRequest = useRef(0);
  const environmentRequest = useRef(0);

  const loadNetwork = useCallback(async () => {
    const current = capture();
    if (!current()) return;
    const request = ++networkRequest.current;
    setNetworkLoading(true);
    setNetworkError(null);
    try {
      const info = await readWithDeadline(() => diag.networkInfo());
      if (current() && request === networkRequest.current) setInfo(info);
    } catch (e) {
      if (current() && request === networkRequest.current) {
        setInfo(null);
        setNetworkError(errorMessage(e));
      }
    } finally {
      if (current() && request === networkRequest.current)
        setNetworkLoading(false);
    }
  }, [capture]);

  const loadEnvironment = useCallback(async () => {
    const current = capture();
    if (!current()) return;
    const request = ++environmentRequest.current;
    setEnvironmentLoading(true);
    setEnvironmentError(null);
    try {
      const env = await readWithDeadline(() => obs.envInfo());
      if (current() && request === environmentRequest.current) setEnv(env);
    } catch (e) {
      if (current() && request === environmentRequest.current) {
        setEnv(null);
        setEnvironmentError(errorMessage(e));
      }
    } finally {
      if (current() && request === environmentRequest.current)
        setEnvironmentLoading(false);
    }
  }, [capture]);

  // Each action has its own latest request; all are scoped to this opening.
  const action = async (
    name: string,
    run: (current: () => boolean) => Promise<void>,
  ) => {
    const viewCurrent = capture();
    if (!viewCurrent()) return;
    const request = (actionRequests.current.get(name) ?? 0) + 1;
    actionRequests.current.set(name, request);
    const current = () =>
      viewCurrent() && actionRequests.current.get(name) === request;
    setLocalError(null);
    try {
      await run(current);
    } catch (error) {
      if (current()) setLocalError(errorMessage(error));
    }
  };
  const rescan = () =>
    action("rescan", async (current) => {
      await diag.rescan();
      if (!current()) return;
      if (feedbackTimer.current !== null) clearTimeout(feedbackTimer.current);
      setRescanned(true);
      feedbackTimer.current = setTimeout(() => {
        setRescanned(false);
        feedbackTimer.current = null;
      }, 1500);
    });
  const revealLogs = () =>
    action("reveal", async (current) => {
      const path = await readWithDeadline(() => obs.logFile());
      if (current()) await revealItemInDir(path);
    });
  const copyLogTail = () =>
    action("copy", async (current) => {
      const tail = await readWithDeadline(() => obs.logTail());
      if (current()) await navigator.clipboard.writeText(tail);
    });
  const showLogTail = () =>
    action("show", async (current) => {
      const tail = await readWithDeadline(() => obs.logTail());
      if (current()) setLogTail(tail);
    });
  const saveLogTail = () =>
    action("save", async (current) => {
      const path = await saveDialog({ defaultPath: "mesh-talk-log.txt" });
      if (current() && typeof path === "string") await obs.saveLogTail(path);
    });
  useEffect(() => {
    if (!open) return;
    const scope = view.current;
    scope.active = true;
    scope.generation++;
    prevPeers.current = [];
    setInfo(null);
    setEnv(null);
    setPeers([]);
    setPeersLoading(true);
    setPeersError(null);
    setLogTail(null);
    setLocalError(null);
    setRescanned(false);
    void loadNetwork();
    void loadEnvironment();
    return () => {
      scope.active = false;
      scope.generation++;
      if (feedbackTimer.current !== null) clearTimeout(feedbackTimer.current);
      feedbackTimer.current = null;
    };
  }, [open, loadNetwork, loadEnvironment]);
  useSerialPoll(
    open,
    POLL_MS,
    () => diag.getPeers(),
    (next) => {
      const lines = diffPeers(prevPeers.current, next, t);
      prevPeers.current = next;
      setPeers(next);
      setPeersError(null);
      setPeersLoading(false);
      if (lines.length > 0) {
        setLog((l) => [...lines.reverse(), ...l].slice(0, LOG_CAP));
      }
    },
    (error) => {
      setPeersError(errorMessage(error));
      setPeersLoading(false);
    },
  );

  return {
    info,
    peers,
    log,
    env,
    networkLoading,
    networkError,
    environmentLoading,
    environmentError,
    peersLoading,
    peersError,
    logTail,
    error,
    rescanned,
    loadNetwork,
    loadEnvironment,
    rescan,
    revealLogs,
    copyLogTail,
    showLogTail,
    saveLogTail,
    clearLog: () => setLog([]),
    dismissError: () => setLocalError(null),
  };
}
export type DiagnosticsController = ReturnType<typeof useDiagnosticsController>;
