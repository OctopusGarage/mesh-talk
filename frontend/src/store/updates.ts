import { create } from "zustand";
import { updates } from "@/lib/api";
import { prepareUpdateRestart } from "./updateRestart";
import { errorMessage } from "@/lib/error";
import type {
  DownloadedUpdate,
  DownloadProgress,
  UpdateCheck,
  UpdateChannel,
} from "@/lib/types";

export type UpdatePhase =
  | "idle"
  | "checking"
  | "ready"
  | "downloading"
  | "downloaded"
  | "installing"
  | "restarting";

interface UpdateState {
  phase: UpdatePhase;
  channel: UpdateChannel;
  currentVersion: string | null;
  setChannel: (channel: UpdateChannel) => void;
  release: UpdateCheck | null;
  download: DownloadedUpdate | null;
  progress: DownloadProgress | null;
  error: string | null;
  canceling: boolean;
  check: () => Promise<void>;
  startDownload: () => Promise<void>;
  install: () => Promise<void>;
  cancel: () => Promise<void>;
  setError: (error: string) => void;
}

const isBusy = (phase: UpdatePhase) =>
  ["checking", "downloading", "installing", "restarting"].includes(phase);

const channelStorageKey = "mesh-talk-update-channel";
function storedChannel(): UpdateChannel {
  try {
    return localStorage.getItem(channelStorageKey) === "nightly"
      ? "nightly"
      : "stable";
  } catch {
    return "stable";
  }
}

let cancelRequested = false;

/** Keep a running download observable when About is closed and reopened. */
export const useUpdates = create<UpdateState>((set, get) => ({
  channel: storedChannel(),
  currentVersion: null,
  setChannel: (channel) => {
    if (get().channel === channel || isBusy(get().phase)) return;
    try {
      localStorage.setItem(channelStorageKey, channel);
    } catch {
      // Storage can be disabled; keep this session's explicit choice usable.
    }
    set({
      channel,
      phase: "idle",
      release: null,
      download: null,
      progress: null,
      error: null,
      canceling: false,
    });
  },
  phase: "idle",
  release: null,
  download: null,
  progress: null,
  error: null,
  canceling: false,
  check: async () => {
    if (isBusy(get().phase)) return;
    set({ phase: "checking", error: null, release: null, download: null });
    try {
      const release = await updates.check(get().channel);
      set({ release, currentVersion: release.current_version, phase: "ready" });
    } catch (cause) {
      set({ error: errorMessage(cause), phase: "idle" });
    }
  },
  startDownload: async () => {
    const release = get().release;
    if (!release?.is_newer || !release.package_name || get().phase !== "ready")
      return;
    cancelRequested = false;
    set({
      phase: "downloading",
      canceling: false,
      error: null,
      progress: { downloaded: 0, total: release.package_size ?? 0 },
    });
    try {
      const download = await (
        release.automatic_install ? updates.prepare : updates.download
      )(release.latest_version, (progress) => set({ progress }), get().channel);
      if (cancelRequested) {
        set({ phase: "ready", download: null, canceling: false, error: null });
        return;
      }
      set({ download, phase: "downloaded", canceling: false });
      if (download.installable) await get().install();
    } catch (cause) {
      set({
        error: cancelRequested ? null : errorMessage(cause),
        phase: "ready",
        canceling: false,
      });
    }
  },
  install: async () => {
    const { release, download, phase, channel } = get();
    if (phase !== "downloaded" || !release || !download?.installable) return;
    set({ phase: "installing", error: null });
    try {
      await prepareUpdateRestart();
      await updates.install(
        release.latest_version,
        (next) => set({ phase: next }),
        channel,
      );
      set({ phase: "restarting" });
    } catch (cause) {
      set({ phase: "downloaded", error: errorMessage(cause) });
    }
  },
  cancel: async () => {
    if (get().phase !== "downloading" || get().canceling) return;
    cancelRequested = true;
    set({ canceling: true });
    try {
      await updates.cancel();
    } catch (cause) {
      // The user's cancellation also withdraws permission to install, even if
      // the native cancellation request cannot be delivered.
      set({ error: errorMessage(cause), canceling: false });
    }
  },
  setError: (error) => set({ error }),
}));
