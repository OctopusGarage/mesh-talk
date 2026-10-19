import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  DiagNetworkInfo,
  DiagPeerInfo,
  EnvInfo,
  PresenceMap,
  UpdateCheck,
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
  check: () => invoke<UpdateCheck>("check_for_update"),
  download: (
    version: string,
    onProgress: (progress: DownloadProgress) => void,
  ) => {
    const channel = new Channel<DownloadProgress>();
    channel.onmessage = onProgress;
    return invoke<DownloadedUpdate>("download_update", {
      version,
      onProgress: channel,
    });
  },
  cancel: () => invoke<void>("cancel_update_download"),
};
