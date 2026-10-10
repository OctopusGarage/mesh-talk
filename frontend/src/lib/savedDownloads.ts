import { useSyncExternalStore } from "react";
import { invalidateFileAvailability } from "@/store/fileAvailability";

const KEY = "mesh-talk-downloads";
const CAP = 500;
const listeners = new Set<() => void>();
const EMPTY: Record<string, string> = {};

function load(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(KEY) || "{}");
  } catch {
    return {};
  }
}

let paths: Record<string, string> | null = null;
const snapshot = () => (paths ??= load());
const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

export function savedDownloadPath(fileConv: string): string | undefined {
  return snapshot()[fileConv];
}

export function useSavedDownloads(): Record<string, string> {
  return useSyncExternalStore(subscribe, snapshot, () => EMPTY);
}

export function rememberSavedDownload(fileConv: string, path: string): void {
  const next = { ...snapshot(), [fileConv]: path };
  const keys = Object.keys(next);
  const evicted = keys.slice(0, Math.max(0, keys.length - CAP));
  for (const key of evicted) delete next[key];
  if (evicted.length) invalidateFileAvailability(evicted);
  paths = next;
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    // The in-memory location remains available for this session.
  }
  listeners.forEach((listener) => listener());
}
