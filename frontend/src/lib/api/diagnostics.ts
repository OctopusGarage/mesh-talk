import { invoke } from "@tauri-apps/api/core";
import type {
  DiagNetworkInfo,
  DiagPeerInfo,
  EnvInfo,
  PresenceMap,
} from "../types";

export const diag = {
  getPeers: () => invoke<DiagPeerInfo[]>("diag_get_peers"),
  networkInfo: () => invoke<DiagNetworkInfo>("diag_network_info"),
  /** Force an immediate re-announce + rescan (manual "announce now"). */
  rescan: () => invoke<void>("rescan_peers"),
};

export const presence = {
  /** Per-conversation presence snapshot, keyed by account_id (DMs) and channel_id. */
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
