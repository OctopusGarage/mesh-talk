import { create } from "zustand";
import { updates } from "@/lib/api";
import { errorMessage } from "@/lib/error";
import type {
  DownloadedUpdate,
  DownloadProgress,
  UpdateCheck,
} from "@/lib/types";

export type UpdatePhase =
  "idle" | "checking" | "ready" | "downloading" | "downloaded";

interface UpdateState {
  phase: UpdatePhase;
  release: UpdateCheck | null;
  download: DownloadedUpdate | null;
  progress: DownloadProgress | null;
  error: string | null;
  canceling: boolean;
  check: () => Promise<void>;
  startDownload: () => Promise<void>;
  cancel: () => Promise<void>;
  setError: (error: string) => void;
}

let cancelRequested = false;

/** Keep a running download observable when About is closed and reopened. */
export const useUpdates = create<UpdateState>((set, get) => ({
  phase: "idle",
  release: null,
  download: null,
  progress: null,
  error: null,
  canceling: false,
  check: async () => {
    if (get().phase === "checking" || get().phase === "downloading") return;
    set({ phase: "checking", error: null, release: null, download: null });
    try {
      const release = await updates.check();
      set({ release, phase: "ready" });
    } catch (cause) {
      set({ error: errorMessage(cause), phase: "idle" });
    }
  },
  startDownload: async () => {
    const release = get().release;
    if (!release?.package_name || get().phase === "downloading") return;
    cancelRequested = false;
    set({
      phase: "downloading",
      canceling: false,
      error: null,
      progress: { downloaded: 0, total: release.package_size ?? 0 },
    });
    try {
      const download = await updates.download(
        release.latest_version,
        (progress) => set({ progress }),
      );
      set({ download, phase: "downloaded", canceling: false });
    } catch (cause) {
      set({
        error: cancelRequested ? null : errorMessage(cause),
        phase: "ready",
        canceling: false,
      });
    }
  },
  cancel: async () => {
    if (get().phase !== "downloading" || get().canceling) return;
    cancelRequested = true;
    set({ canceling: true });
    try {
      await updates.cancel();
    } catch (cause) {
      cancelRequested = false;
      set({ error: errorMessage(cause), canceling: false });
    }
  },
  setError: (error) => set({ error }),
}));
