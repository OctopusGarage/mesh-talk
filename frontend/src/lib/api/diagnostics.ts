import { Channel, invoke } from "@tauri-apps/api/core";
import type {
  DiagNetworkInfo,
  DiagPeerInfo,
  EnvInfo,
  PresenceMap,
  PresenceDelta,
  UpdateCheck,
  UpdateInstallPhase,
  UpdateChannel,
  DownloadedUpdate,
  DownloadProgress,
} from "../types";

export interface ProcessResourceReading {
  pid: number;
  birth: string;
  cpu_ms: number;
  resident_bytes: number | null;
  disk_write_bytes: number | null;
  io_write_bytes: number | null;
}

export interface HostResourceSample {
  sampled_at_ms: number;
  processes: ProcessResourceReading[];
  bounded: boolean;
  authenticated_channels: number;
  sent_frame_bytes: number;
  received_frame_bytes: number;
}

export const diag = {
  getPeers: () => invoke<DiagPeerInfo[]>("diag_get_peers"),
  networkInfo: () => invoke<DiagNetworkInfo>("diag_network_info"),
  /** Force an immediate re-announce + rescan (manual "announce now"). */
  rescan: () => invoke<void>("rescan_peers"),
  probeEndpoint: (endpoint: string) => {
    const address = endpoint.trim();
    // A bare IPv6 literal has no port. Brackets disambiguate an IPv6 port.
    const qualified =
      /^\[([^\]]+)\](?::([0-9]{1,5}))?$/u.exec(address) ??
      /^([^:]+):([0-9]{1,5})$/u.exec(address);
    return invoke<void>("probe_peer_endpoint", {
      host: qualified?.[1] ?? address,
      port: qualified?.[2] === undefined ? undefined : Number(qualified[2]),
    });
  },
};

export const presence = {
  observeRoster: (owner: string) =>
    invoke<import("../types").RosterObservation>("owner_roster_observation", {
      owner,
    }),
  groupsIfChanged: (owner: string, knownGroupRevision: string) =>
    invoke<PresenceDelta | { unchanged: true }>(
      "owner_presence_if_groups_changed",
      { owner, knownGroupRevision },
    ),
  /** Shared person/device observations and accepted-account group count summaries. */
  get: (knownGroupRevision?: string) =>
    invoke<PresenceMap | PresenceDelta>("get_presence", { knownGroupRevision }),
};

/** Observability: logs + static environment facts for the Diagnostics dialog. */
export const obs = {
  envInfo: () => invoke<EnvInfo>("env_info"),
  startHostResourceSampling: () =>
    invoke<number>("start_host_resource_sampling"),
  hostResources: (ticket: number) =>
    invoke<HostResourceSample | null>("host_resource_sample", { ticket }),
  stopHostResourceSampling: (ticket: number) =>
    invoke<void>("stop_host_resource_sampling", { ticket }),
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
