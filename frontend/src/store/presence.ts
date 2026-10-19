import { create } from "zustand";
import type { TFunction } from "i18next";
import { presence as presenceApi } from "@/lib/api";
import type { PresenceInfo, PresenceMap } from "@/lib/types";
import type { PresenceStatus } from "@/lib/presence";
import { captureRuntimeOwner } from "./ownership";

const POLL_MS = 2000;
const MAX_HISTORY = 1024;
let epoch = 0;
interface PresenceState {
  map: PresenceMap;
  unavailable: boolean;
  clear: () => void;
  start: () => () => void;
}
export const OFFLINE: PresenceInfo = { online: false, last_seen_secs: null };
const EMPTY: PresenceMap = {};

function reconcile(prev: PresenceMap, next: PresenceMap): PresenceMap {
  let changed = Object.keys(prev).length !== Object.keys(next).length;
  const merged: PresenceMap = {};
  for (const [key, value] of Object.entries(next)) {
    if (prev[key] && JSON.stringify(prev[key]) === JSON.stringify(value)) {
      merged[key] = prev[key];
    } else {
      merged[key] = value;
      changed = true;
    }
  }
  return changed ? merged : prev;
}

/** Age each source independently, including during IPC failure or silence. */
export function agePresence(
  snapshot: PresenceMap,
  elapsed: number,
): PresenceMap {
  const next: PresenceMap = {};
  for (const [key, value] of Object.entries(snapshot)) {
    const devices = value.devices
      ?.map((device) => ({
        ...device,
        evidence: device.evidence
          .map((e) => ({
            ...e,
            last_seen_secs: e.last_seen_secs + elapsed,
            expires_in_secs: Math.max(0, e.expires_in_secs - elapsed),
          }))
          .filter((e) => e.expires_in_secs > 0),
      }))
      .filter((device) => device.evidence.length > 0);
    next[key] = {
      ...value,
      // Compatibility snapshots also have a bounded lease; no frozen green dots.
      online: devices ? devices.length > 0 : value.online && elapsed < 30,
      last_seen_secs:
        value.last_seen_secs == null ? null : value.last_seen_secs + elapsed,
      ...(devices ? { devices } : {}),
    };
  }
  for (const value of Object.values(next)) {
    if (value.member_keys) {
      value.online = false; // Groups have counts, never person-level status.
      value.online_count = value.member_keys.filter(
        (key) => next[key]?.online,
      ).length;
    }
  }
  return next;
}

export const usePresence = create<PresenceState>((set, get) => ({
  map: EMPTY,
  unavailable: false,
  clear: () => {
    epoch++;
    set({ map: EMPTY, unavailable: false });
  },
  start: () => {
    const lease = captureRuntimeOwner();
    get().clear();
    const session = ++epoch;
    let cancelled = false;
    let nextPoll = 0;
    let lastApplied = 0;
    let inFlight = 0;
    let baseline: PresenceMap = {};
    let baselineAt = performance.now();
    const current = () => !cancelled && epoch === session && lease.current();
    const age = () =>
      agePresence(
        baseline,
        Math.max(0, Math.floor((performance.now() - baselineAt) / 1000)),
      );
    const publishAge = () => {
      if (current()) set({ map: reconcile(get().map, age()) });
    };
    const poll = async () => {
      if (!current() || inFlight >= 2) return;
      inFlight++;
      const pollId = ++nextPoll;
      const requested = performance.now();
      const fail = () => {
        if (current() && pollId > lastApplied) {
          lastApplied = pollId;
          set({ unavailable: true });
          publishAge();
        }
      };
      const timeout = setTimeout(fail, 10000);
      try {
        const next = await presenceApi.get();
        if (!next || typeof next !== "object" || Array.isArray(next))
          throw new Error("invalid presence snapshot");
        if (current() && pollId > lastApplied) {
          const old = age();
          // A successful empty snapshot withdraws presence immediately, while
          // retaining only bounded, session-local history that was actually observed.
          const history: PresenceMap = {};
          let historyCount = 0;
          for (const [key, value] of Object.entries(old)) {
            if (
              !(key in next) &&
              !value.member_keys &&
              value.last_seen_secs != null &&
              historyCount < MAX_HISTORY
            ) {
              historyCount++;
              history[key] = {
                online: false,
                last_seen_secs: value.last_seen_secs,
                devices: [],
              };
            }
          }
          const aged = agePresence(
            next,
            Math.max(0, Math.floor((performance.now() - requested) / 1000)),
          );
          const withHistory = Object.fromEntries(
            Object.entries(aged).map(([key, value]) => [
              key,
              value.last_seen_secs == null &&
              !value.member_keys &&
              old[key]?.last_seen_secs != null
                ? { ...value, last_seen_secs: old[key].last_seen_secs }
                : value,
            ]),
          );
          lastApplied = pollId;
          baseline = { ...history, ...withHistory };
          baselineAt = performance.now();
          set({ map: reconcile(get().map, baseline), unavailable: false });
        }
      } catch {
        fail();
      } finally {
        inFlight--;
        clearTimeout(timeout);
      }
    };
    void poll();
    const id = setInterval(() => {
      void poll();
      queueMicrotask(publishAge);
    }, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
      if (epoch === session) get().clear();
    };
  },
}));

export function presenceStatus(p: PresenceInfo): PresenceStatus {
  return p.online ? "online" : "offline";
}

export function presenceLabel(p: PresenceInfo, t: TFunction): string {
  if (p.online) return t("presence.online");
  const s = p.last_seen_secs;
  if (s == null) return t("presence.offline");
  const history =
    s < 5
      ? t("presence.lastSeenJustNow")
      : s < 60
        ? t("presence.lastSeenSecs", { count: s })
        : s < 3600
          ? t("presence.lastSeenMins", { count: Math.floor(s / 60) })
          : t("presence.lastSeenHours", { count: Math.floor(s / 3600) });
  return t("presence.offlineHistory", { history });
}

export function groupPresenceLabel(p: PresenceInfo, t: TFunction): string {
  if (p.member_count == null) return t("presence.groupUpdating");
  const summary = t("presence.groupSummary", {
    total: p.member_count,
    online: p.online_count ?? 0,
  });
  return p.unbound_member_count
    ? t("presence.groupUnbound", { summary, count: p.unbound_member_count })
    : summary;
}

export function usePresenceFor(id: string | undefined): PresenceInfo {
  return usePresence((s) => (id ? (s.map[id] ?? OFFLINE) : OFFLINE));
}
