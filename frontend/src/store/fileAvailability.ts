import {
  subscribeSyncInvalidation,
  supportsPushState,
} from "@/lib/syncInvalidation";
import { readWithDeadline } from "@/lib/readDeadline";
import { useEffect } from "react";
import { create } from "zustand";
import { chat } from "@/lib/api";
import { errorMessage } from "@/lib/error";
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
        const previous = next[status.file_conv];
        delete next[status.file_conv];
        const merged =
          !status.ready &&
          (status.total === 0 || status.done >= status.total) &&
          status.consent !== "pending" &&
          status.consent !== "declined" &&
          (previous?.ready || previous?.unavailable)
            ? { ...status, unavailable: true }
            : status;
        next[status.file_conv] =
          JSON.stringify(previous) === JSON.stringify(merged)
            ? previous
            : merged;
      }
      if (
        Object.keys(next).length === Object.keys(state.statuses).length &&
        Object.keys(next).every((key) => next[key] === state.statuses[key])
      )
        return state;
      return {
        statuses: Object.fromEntries(
          Object.entries(next).slice(-STATUS_CACHE_CAP),
        ),
      };
    }),
  reset: () => set({ statuses: {} }),
}));

/** A saved file may have its chunks pruned, so a cached ready result must not outlive
 * the remembered local destination. */
export function invalidateFileAvailability(fileConvs: string[]) {
  // Reject any reply that began before the local copy was evicted. Otherwise an
  // old ready snapshot could restore a Save action after chunks were pruned.
  generation++;
  useFileAvailability.setState((state) => {
    const statuses = { ...state.statuses };
    for (const fileConv of fileConvs) delete statuses[fileConv];
    return { statuses };
  });
}

const watched = new Map<string, number>();
let timer: ReturnType<typeof setInterval> | undefined;
let inFlight = false;
let dirty = false;
let nextFallback = 0;
let generation = 0;
let invalidationCleanup: (() => void) | undefined;

export async function refreshFileAvailability(fileConvs: string[]) {
  const current = generation;
  for (let i = 0; i < fileConvs.length; i += 500) {
    const statuses = await readWithDeadline(() =>
      chat.fileStatuses(fileConvs.slice(i, i + 500)),
    );
    if (current !== generation) return;
    useFileAvailability.getState().merge(statuses);
  }
}

async function poll(force = false) {
  if (inFlight) {
    if (force) dirty = true;
    return;
  }
  if (!force && supportsPushState() && performance.now() < nextFallback) return;
  // Ready manifests can expire or be removed while a card remains mounted.
  const pending = [...watched.keys()];
  if (!pending.length) return;
  inFlight = true;
  dirty = false;
  nextFallback = performance.now() + 30000;
  try {
    await refreshFileAvailability(pending);
  } catch {
    // A disconnected node must never make an incomplete file actionable.
  } finally {
    inFlight = false;
    if (dirty) {
      dirty = false;
      void poll(true);
    }
  }
}

export function watchFileAvailability(fileConv: string) {
  watched.set(fileConv, (watched.get(fileConv) ?? 0) + 1);
  if (!timer) {
    timer = setInterval(() => void poll(), 750);
    invalidationCleanup = subscribeSyncInvalidation((change) => {
      if (change.kind === "files" || change.kind === "state") void poll(true);
    });
  }
  void poll(true);
  return () => {
    const count = watched.get(fileConv) ?? 0;
    if (count <= 1) watched.delete(fileConv);
    else watched.set(fileConv, count - 1);
    if (!watched.size && timer) {
      clearInterval(timer);
      timer = undefined;
      invalidationCleanup?.();
      invalidationCleanup = undefined;
    }
  };
}

/** Readiness can change before a native save. Missing content is unavailable;
 * incomplete transfers remain pending so background recovery can resume them. */
export function markUnavailableFileError(
  fileConv: string,
  error: unknown,
): boolean {
  const message = errorMessage(error);
  if (/incomplete/i.test(message)) {
    const previous = useFileAvailability.getState().statuses[fileConv];
    useFileAvailability.getState().merge([
      {
        file_conv: fileConv,
        ready: false,
        done: 0,
        total: Math.max(1, previous?.total ?? 1),
        consent: previous?.consent,
      },
    ]);
    return true;
  }
  if (!/unknown\s*file|expired/i.test(message)) return false;
  useFileAvailability.getState().merge([
    {
      file_conv: fileConv,
      ready: false,
      done: 0,
      total: 0,
      unavailable: true,
    },
  ]);
  return true;
}

export function resetFileAvailability() {
  generation++;
  nextFallback = 0;
  useFileAvailability.getState().reset();
  if (watched.size) void poll(true);
}

export function useFileStatus(fileConv: string | undefined) {
  useEffect(() => {
    if (fileConv) return watchFileAvailability(fileConv);
  }, [fileConv]);
  return useFileAvailability((s) =>
    fileConv ? s.statuses[fileConv] : undefined,
  );
}
