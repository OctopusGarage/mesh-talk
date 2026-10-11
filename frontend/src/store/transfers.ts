import { create } from "zustand";
import type { FileProgressEvent, OutgoingFileStatus } from "@/lib/types";

export interface Transfer {
  fileConv: string;
  direction: "prepare" | "send" | "save";
  done: number;
  total: number;
  etaSeconds?: number | null;
  lastAdvancedAt?: number;
  delivered?: boolean;
  consent?: "pending" | "accepted" | "declined";
}

interface TransfersState {
  // Keyed by per-file id for saves, or optimistic message key for sends.
  transfers: Record<string, Transfer>;
  applyProgress: (e: FileProgressEvent) => void;
  applyUploadStatus: (id: string, status: OutgoingFileStatus) => void;
  clear: (key: string) => void;
  reset: () => void;
}

const TRANSFER_RECORD_LIMIT = 1024;

function boundTransfers(
  transfers: Record<string, Transfer>,
): Record<string, Transfer> {
  const keys = Object.keys(transfers);
  if (keys.length <= TRANSFER_RECORD_LIMIT) return transfers;
  let excess = keys.length - TRANSFER_RECORD_LIMIT;
  for (const key of keys) {
    if (excess === 0) break;
    if (transfers[key].delivered) {
      delete transfers[key];
      excess--;
    }
  }
  for (const key of keys) {
    if (excess === 0) break;
    if (key in transfers) {
      delete transfers[key];
      excess--;
    }
  }
  return transfers;
}

/** A rate needs two distinct positive recipient observations. Never infer it
 * from local preparation time or time spent waiting for an offline peer. */
export function estimateRemainingSeconds(
  done: number,
  total: number,
  previousDone: number,
  previousAt: number,
  now: number,
): number | null {
  if (
    previousDone <= 0 ||
    done <= previousDone ||
    !Number.isFinite(done) ||
    !Number.isFinite(total) ||
    done >= total ||
    now <= previousAt ||
    now - previousAt > 10_000
  )
    return null;
  return Math.ceil(
    ((total - done) * (now - previousAt)) / ((done - previousDone) * 1000),
  );
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
      return { transfers: boundTransfers(next) };
    }),
  applyUploadStatus: (id, status) =>
    set((s) => {
      const next = { ...s.transfers };
      if (status.delivered) {
        if (s.transfers[id]?.delivered) return s;
        next[id] = {
          fileConv: id,
          direction: "send",
          done: status.total,
          total: status.total,
          delivered: true,
        };
        return { transfers: boundTransfers(next) };
      }
      const total = Math.max(0, Math.floor(status.total));
      const done = Math.min(total, Math.max(0, Math.floor(status.done)));
      const now = Date.now();
      const previous = s.transfers[id];
      const sameRun =
        previous?.direction === "send" && previous.total === total;
      const advancing = sameRun && done > previous.done;
      const regressed = sameRun && done < previous.done;
      const etaSeconds = advancing
        ? estimateRemainingSeconds(
            done,
            total,
            previous.done,
            previous.lastAdvancedAt ?? now,
            now,
          )
        : sameRun &&
            !regressed &&
            now - (previous.lastAdvancedAt ?? now) < 10_000
          ? (previous.etaSeconds ?? null)
          : null;
      next[id] = {
        fileConv: id,
        direction: "send",
        done,
        total,
        etaSeconds,
        lastAdvancedAt:
          advancing || regressed || !sameRun ? now : previous.lastAdvancedAt,
        consent: status.consent,
      };
      return { transfers: boundTransfers(next) };
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
