import { create } from "zustand";
import type { TFunction } from "i18next";
import { presence as presenceApi } from "@/lib/api";
import type { PresenceInfo, PresenceMap } from "@/lib/types";
import type { PresenceStatus } from "@/components/identity";

/**
 * Isolated presence slice. Kept deliberately separate from the chat store so the slow
 * (~5s) presence poll never re-renders the (expensive, virtualized) message list — only
 * the small Sidebar rows / Conversation header that read presence subscribe here.
 *
 * Online = the account/channel has ≥1 relevant device seen within the backend TTL.
 * Derived status: online (breathing teal) → recent (dim amber) → offline (muted).
 */

const POLL_MS = 5000;
/** Anything fresher than this (but past the online TTL) reads as "recently seen". */
const RECENT_SECS = 5 * 60;

interface PresenceState {
  /** Per-conversation presence keyed by account_id (DMs) and channel_id (channels). */
  map: PresenceMap;
  /** Begin polling; returns a stop fn. Safe to start once per session. */
  start: () => () => void;
}

/** Stable empty presence for an unknown key (constant ref → no spurious re-renders). */
export const OFFLINE: PresenceInfo = { online: false, last_seen_secs: null };
const EMPTY: PresenceMap = {};

/** Merge a fresh snapshot into the prior map, REUSING the prior record object for any
 *  key whose value is unchanged. This keeps `usePresenceFor` selectors referentially
 *  stable across ticks, so a poll only re-renders rows whose presence actually moved. */
function reconcile(prev: PresenceMap, next: PresenceMap): PresenceMap {
  let changed = Object.keys(prev).length !== Object.keys(next).length;
  const merged: PresenceMap = {};
  for (const [k, v] of Object.entries(next)) {
    const old = prev[k];
    if (
      old &&
      old.online === v.online &&
      old.last_seen_secs === v.last_seen_secs
    ) {
      merged[k] = old; // unchanged → keep the stable reference
    } else {
      merged[k] = v;
      changed = true;
    }
  }
  return changed ? merged : prev;
}

export const usePresence = create<PresenceState>((set, get) => ({
  map: EMPTY,
  start: () => {
    set({ map: EMPTY });
    let cancelled = false;
    const poll = async () => {
      try {
        const next = await presenceApi.get();
        if (!cancelled) set({ map: reconcile(get().map, next) });
      } catch {
        // node may still be starting, or got torn down — ignore (next tick retries).
      }
    };
    void poll();
    const id = setInterval(() => void poll(), POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  },
}));

/** Derive the display status for a presence record. */
export function presenceStatus(p: PresenceInfo): PresenceStatus {
  if (p.online) return "online";
  if (p.last_seen_secs != null && p.last_seen_secs < RECENT_SECS)
    return "recent";
  return "offline";
}

/** A localized presence label ("Online" / "Last seen 3m ago" / "Offline"). */
export function presenceLabel(p: PresenceInfo, t: TFunction): string {
  if (p.online) return t("presence.online");
  const s = p.last_seen_secs;
  if (s == null) return t("presence.offline");
  if (s < 5) return t("presence.lastSeenJustNow");
  if (s < 60) return t("presence.lastSeenSecs", { count: s });
  if (s < 3600)
    return t("presence.lastSeenMins", { count: Math.floor(s / 60) });
  return t("presence.lastSeenHours", { count: Math.floor(s / 3600) });
}

/**
 * Subscribe to one conversation's presence by id. Returns the raw record (a stable
 * OFFLINE ref when unknown), so a component only re-renders when that id's snapshot
 * object changes — a presence tick that doesn't touch this id is a no-op here.
 */
export function usePresenceFor(id: string | undefined): PresenceInfo {
  return usePresence((s) => (id ? (s.map[id] ?? OFFLINE) : OFFLINE));
}
