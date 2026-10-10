import { useEffect } from "react";
import { create } from "zustand";
import { chat } from "@/lib/api";
import type { FileStatus } from "@/lib/types";

interface AvailabilityState {
  statuses: Record<string, FileStatus>;
  merge: (statuses: FileStatus[]) => void;
  reset: () => void;
}

const STATUS_CACHE_CAP = 1000;

export const useFileAvailability = create<AvailabilityState>((set) => ({
  statuses: {},
  merge: (statuses) =>
    set((state) => {
      const next = { ...state.statuses };
      for (const status of statuses) {
        delete next[status.file_conv];
        next[status.file_conv] = status;
      }
      return {
        statuses: Object.fromEntries(
          Object.entries(next).slice(-STATUS_CACHE_CAP),
        ),
      };
    }),
  reset: () => set({ statuses: {} }),
}));

const watched = new Map<string, number>();
let timer: ReturnType<typeof setInterval> | undefined;
let inFlight = false;
let generation = 0;

export async function refreshFileAvailability(fileConvs: string[]) {
  const current = generation;
  for (let i = 0; i < fileConvs.length; i += 500) {
    const statuses = await chat.fileStatuses(fileConvs.slice(i, i + 500));
    if (current !== generation) return;
    useFileAvailability.getState().merge(statuses);
  }
}

async function poll() {
  if (inFlight) return;
  const pending = [...watched.keys()].filter(
    (key) => !useFileAvailability.getState().statuses[key]?.ready,
  );
  if (!pending.length) return;
  inFlight = true;
  try {
    await refreshFileAvailability(pending);
  } catch {
    // A disconnected node must never make an incomplete file actionable.
  } finally {
    inFlight = false;
  }
}

export function watchFileAvailability(fileConv: string) {
  watched.set(fileConv, (watched.get(fileConv) ?? 0) + 1);
  if (!timer) timer = setInterval(() => void poll(), 750);
  void poll();
  return () => {
    const count = watched.get(fileConv) ?? 0;
    if (count <= 1) watched.delete(fileConv);
    else watched.set(fileConv, count - 1);
    if (!watched.size && timer) {
      clearInterval(timer);
      timer = undefined;
    }
  };
}

export function resetFileAvailability() {
  generation++;
  useFileAvailability.getState().reset();
  if (watched.size) void poll();
}

export function useFileStatus(fileConv: string | undefined) {
  useEffect(() => {
    if (fileConv) return watchFileAvailability(fileConv);
  }, [fileConv]);
  return useFileAvailability((s) =>
    fileConv ? s.statuses[fileConv] : undefined,
  );
}
