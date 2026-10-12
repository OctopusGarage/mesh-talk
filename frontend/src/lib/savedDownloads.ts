import { useSyncExternalStore } from "react";
import { invalidateFileAvailability } from "@/store/fileAvailability";
import { useAuth } from "@/store/auth";

const KEY = "mesh-talk-downloads";
const CAP = 500;
const listeners = new Set<() => void>();
const EMPTY: Record<string, string> = {};

function key(owner: string) {
  return `${KEY}:${owner}`;
}

function quarantineLegacy(): void {
  try {
    const old = localStorage.getItem(KEY);
    if (!old) return;
    let archive = `${KEY}:legacy-unassigned`;
    for (let index = 1; index <= 1000 && localStorage.getItem(archive); index++)
      archive = `${KEY}:legacy-unassigned:${index}`;
    if (localStorage.getItem(archive)) return;
    localStorage.setItem(archive, old);
    localStorage.removeItem(KEY);
  } catch {
    // Storage can be unavailable; the unowned key is never read as account data.
  }
}

function load(owner: string): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(key(owner)) || "{}");
  } catch {
    return {};
  }
}

let paths: Record<string, string> | null = null;
let cachedOwner: string | null = null;
const snapshot = (owner: string | null) => {
  if (!owner) {
    cachedOwner = null;
    paths = null;
    return EMPTY;
  }
  if (cachedOwner !== owner) {
    quarantineLegacy();
    cachedOwner = owner;
    paths = load(owner);
  }
  return paths ?? EMPTY;
};
const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

export function savedDownloadPath(fileConv: string): string | undefined {
  return snapshot(useAuth.getState().user?.id ?? null)[fileConv];
}

export function useSavedDownloads(): Record<string, string> {
  const owner = useAuth((s) => s.user?.id ?? null);
  return useSyncExternalStore(
    subscribe,
    () => snapshot(owner),
    () => EMPTY,
  );
}

export function rememberSavedDownload(fileConv: string, path: string): void {
  const owner = useAuth.getState().user?.id;
  if (!owner) return;
  const next = { ...snapshot(owner), [fileConv]: path };
  const keys = Object.keys(next);
  const evicted = keys.slice(0, Math.max(0, keys.length - CAP));
  for (const key of evicted) delete next[key];
  if (evicted.length) invalidateFileAvailability(evicted);
  paths = next;
  try {
    localStorage.setItem(key(owner), JSON.stringify(next));
  } catch {
    // The in-memory location remains available for this session.
  }
  listeners.forEach((listener) => listener());
}
