import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  DiagNetworkInfo,
  DiagPeerInfo,
  EnvInfo,
  PresenceMap,
  UpdateCheck,
  UpdateInstallPhase,
  UpdateChannel,
  DownloadedUpdate,
  DownloadProgress,
} from "../types";

export const diag = {
  getPeers: () => invoke<DiagPeerInfo[]>("diag_get_peers"),
  networkInfo: () => invoke<DiagNetworkInfo>("diag_network_info"),
  /** Force an immediate re-announce + rescan (manual "announce now"). */
  rescan: () => invoke<void>("rescan_peers"),
  probeEndpoint: (host: string) =>
    invoke<void>("probe_peer_endpoint", { host }),
};

export const presence = {
  /** Shared person/device observations and accepted-account group count summaries. */
  get: () => invoke<PresenceMap>("get_presence"),
};

/** Observability: logs + static environment facts for the Diagnostics dialog. */
export const obs = {
  envInfo: () => invoke<EnvInfo>("env_info"),
  logsDir: () => invoke<string>("get_logs_dir"),
  logFile: () => invoke<string>("get_log_file"),
  logTail: () => invoke<string>("read_log_tail"),
  saveLogTail: (dest: string) => invoke<void>("save_log_tail", { dest }),
};

export const updates = {
  check: (updateChannel: UpdateChannel = "stable") =>
    invoke<UpdateCheck>("check_for_update", { updateChannel }),
  download: (
    version: string,
    onProgress: (progress: DownloadProgress) => void,
    updateChannel: UpdateChannel = "stable",
  ) => {
    const channel = new Channel<DownloadProgress>();
    channel.onmessage = onProgress;
    return invoke<DownloadedUpdate>("download_update", {
      version,
      updateChannel,
      onProgress: channel,
    });
  },
  prepare: (
    version: string,
    onProgress: (progress: DownloadProgress) => void,
    updateChannel: UpdateChannel = "stable",
  ) => {
    const channel = new Channel<DownloadProgress>();
    channel.onmessage = onProgress;
    return invoke<DownloadedUpdate>("prepare_update", {
      version,
      updateChannel,
      onProgress: channel,
    });
  },
  install: (
    version: string,
    onProgress: (phase: UpdateInstallPhase) => void,
    updateChannel: UpdateChannel = "stable",
  ) => {
    const channel = new Channel<UpdateInstallPhase>();
    channel.onmessage = onProgress;
    return invoke<void>("install_update", {
      version,
      updateChannel,
      onProgress: channel,
    });
  },
  cancel: () => invoke<void>("cancel_update_download"),
};
