import { create } from "zustand";
import type { FileProgressEvent } from "@/lib/types";

export interface Transfer {
  fileConv: string;
  direction: "send" | "save";
  done: number;
  total: number;
}

interface TransfersState {
  // Keyed by `fileConv` (or the send target label until the per-file id resolves).
  transfers: Record<string, Transfer>;
  applyProgress: (e: FileProgressEvent) => void;
  clear: (key: string) => void;
  reset: () => void;
}

/** Fast-changing file-transfer progress lives in its OWN store so emitting ~10/s does
 *  NOT re-render the message list (which subscribes only to `useChat`). Components read
 *  a single transfer via the `useTransfer` selector below (stable per-key reference). */
export const useTransfers = create<TransfersState>((set) => ({
  transfers: {},
  applyProgress: (e) =>
    set((s) => {
      const next = { ...s.transfers };
      if (e.done >= e.total) {
        // Terminal: drop it so the bar disappears (the file then shows as ready).
        delete next[e.file_conv];
      } else {
        next[e.file_conv] = {
          fileConv: e.file_conv,
          direction: e.direction,
          done: e.done,
          total: e.total,
        };
      }
      return { transfers: next };
    }),
  clear: (key) =>
    set((s) => {
      if (!(key in s.transfers)) return s;
      const next = { ...s.transfers };
      delete next[key];
      return { transfers: next };
    }),
  reset: () => set({ transfers: {} }),
}));

/** Select a single transfer by key (stable reference — returns `undefined` when none). */
export const useTransfer = (key: string | undefined): Transfer | undefined =>
  useTransfers((s) => (key ? s.transfers[key] : undefined));
