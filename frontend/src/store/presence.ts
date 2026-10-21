import { create } from "zustand";
import type { TFunction } from "i18next";
import { presence as presenceApi } from "@/lib/api";
import type { PresenceInfo, PresenceMap } from "@/lib/types";
import type { PresenceStatus } from "@/lib/presence";
import { captureRuntimeOwner } from "./ownership";

const POLL_MS = 2000;
const MAX_HISTORY = 1024;
const RECOVERY_STABLE_MS = 5000;
const QUALITY_STABLE_MS = 30_000;
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
            last_seen_secs: e.last_seen_secs + Math.floor(elapsed),
            expires_in_secs: Math.max(0, e.expires_in_secs - elapsed),
          }))
          .filter((e) => e.expires_in_secs > 0),
      }))
      .filter((device) => device.evidence.length > 0);
    next[key] = {
      ...value,
      ...(value.reconnect_expires_in_secs == null
        ? {}
        : {
            reconnect_expires_in_secs: Math.max(
              0,
              value.reconnect_expires_in_secs - elapsed,
            ),
          }),
      ...(value.online_expires_in_secs == null
        ? {}
        : {
            online_expires_in_secs: Math.max(
              0,
              value.online_expires_in_secs - elapsed,
            ),
            connection_weak:
              value.connection_weak || (value.online && devices?.length === 0),
          }),
      // The host owns topology tolerance; legacy snapshots retain their short lease.
      online:
        value.online &&
        (value.online_expires_in_secs != null
          ? value.online_expires_in_secs > elapsed
          : devices
            ? devices.length > 0
            : elapsed < 15),
      last_seen_secs:
        value.last_seen_secs == null
          ? null
          : value.last_seen_secs + Math.floor(elapsed),
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
    // Presentation hysteresis only: delivery continues independently.
    const recoveryConfirmations = new Map<
      string,
      { since: number; deadlineAt: number }
    >();
    const qualityConfirmations = new Map<
      string,
      { since: number; deadlineAt: number }
    >();
    let baselineAt = performance.now();
    let expiryTimer: ReturnType<typeof setTimeout> | undefined;
    const current = () => !cancelled && epoch === session && lease.current();
    const age = () =>
      agePresence(
        baseline,
        Math.max(0, (performance.now() - baselineAt) / 1000),
      );
    const publishAge = () => {
      if (current()) {
        set({ map: reconcile(get().map, age()) });
        scheduleExpiry();
      }
    };
    // Deadline ageing is independent of the two-second IPC poll. A grace ending
    // between poll ticks must not remain visible beyond the approved bound.
    const scheduleExpiry = () => {
      clearTimeout(expiryTimer);
      if (!current()) return;
      const elapsed = Math.max(0, (performance.now() - baselineAt) / 1000);
      let next = Infinity;
      const consider = (deadline: number) => {
        if (deadline > elapsed) next = Math.min(next, deadline - elapsed);
      };
      for (const value of Object.values(baseline)) {
        if (value.online_expires_in_secs != null)
          consider(value.online_expires_in_secs);
        if (value.reconnect_expires_in_secs != null)
          consider(value.reconnect_expires_in_secs);
        if (value.devices) {
          for (const device of value.devices)
            for (const evidence of device.evidence)
              consider(evidence.expires_in_secs);
        } else if (value.online) consider(15);
      }
      if (Number.isFinite(next))
        expiryTimer = setTimeout(
          publishAge,
          Math.max(1, Math.ceil(next * 1000)),
        );
    };
    const poll = async () => {
      if (!current() || inFlight >= 2) return;
      inFlight++;
      const pollId = ++nextPoll;
      const requested = performance.now();
      const fail = () => {
        if (current() && pollId > lastApplied) {
          lastApplied = pollId;
          recoveryConfirmations.clear();
          qualityConfirmations.clear();
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
            Math.max(0, (performance.now() - requested) / 1000),
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
          // A single successful refresh must not flash green between losses.
          // Initial discovery and explicit withdrawal retain their immediate behavior.
          const confirmedAt = performance.now();
          for (const key of recoveryConfirmations.keys()) {
            if (!(key in withHistory)) recoveryConfirmations.delete(key);
          }
          for (const [key, value] of Object.entries(withHistory)) {
            if (value.member_keys) continue;
            if (!value.online || (value.reconnect_expires_in_secs ?? 0) <= 0) {
              recoveryConfirmations.delete(key);
              continue;
            }
            const recovering =
              old[key] &&
              !old[key].online &&
              presenceStatus(old[key]) === "reconnecting";
            if (!recovering && !recoveryConfirmations.has(key)) continue;
            if (
              !recoveryConfirmations.has(key) &&
              recoveryConfirmations.size >= MAX_HISTORY
            )
              continue;
            const deadlineAt =
              confirmedAt + (value.reconnect_expires_in_secs ?? 0) * 1000;
            const confirmation = recoveryConfirmations.get(key) ?? {
              since: confirmedAt,
              deadlineAt,
            };
            // Allow one second for integer observation-age rounding. Re-querying
            // one unchanged origin must not count as renewed public evidence.
            const renewed = deadlineAt - confirmation.deadlineAt > 1000;
            if (
              confirmedAt - confirmation.since < RECOVERY_STABLE_MS ||
              !renewed
            ) {
              recoveryConfirmations.set(key, confirmation);
              withHistory[key] = { ...value, online: false };
            } else {
              recoveryConfirmations.delete(key);
            }
          }
          for (const key of qualityConfirmations.keys()) {
            if (!(key in withHistory)) qualityConfirmations.delete(key);
          }
          for (const [key, value] of Object.entries(withHistory)) {
            if (value.member_keys) continue;
            if (!value.online || value.connection_weak !== false) {
              qualityConfirmations.delete(key);
              continue;
            }
            if (!old[key]?.connection_weak && !qualityConfirmations.has(key))
              continue;
            const deadlineAt =
              confirmedAt + (value.reconnect_expires_in_secs ?? 0) * 1000;
            const confirmation = qualityConfirmations.get(key) ?? {
              since: confirmedAt,
              deadlineAt,
            };
            if (
              confirmedAt - confirmation.since < QUALITY_STABLE_MS ||
              deadlineAt - confirmation.deadlineAt <= 1000
            ) {
              if (
                qualityConfirmations.has(key) ||
                qualityConfirmations.size < MAX_HISTORY
              )
                qualityConfirmations.set(key, confirmation);
              withHistory[key] = { ...value, connection_weak: true };
            } else {
              qualityConfirmations.delete(key);
            }
          }
          lastApplied = pollId;
          baseline = agePresence({ ...history, ...withHistory }, 0);
          baselineAt = performance.now();
          set({ map: reconcile(get().map, baseline), unavailable: false });
          scheduleExpiry();
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
      clearTimeout(expiryTimer);
      if (epoch === session) get().clear();
    };
  },
}));

export function presenceStatus(p: PresenceInfo): PresenceStatus {
  return p.online
    ? p.connection_weak
      ? "reconnecting"
      : "online"
    : (p.reconnect_expires_in_secs ?? 0) > 0
      ? "reconnecting"
      : "offline";
}

export function presenceLabel(p: PresenceInfo, t: TFunction): string {
  if (p.online)
    return t(p.connection_weak ? "presence.onlineWeak" : "presence.online");
  if (presenceStatus(p) === "reconnecting") return t("presence.reconnecting");
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

/** Recovery is presentation only and excludes groups and the local account. */
export function hasReconnectingPresence(
  map: PresenceMap,
  ownAccount?: string | null,
): boolean {
  return Object.entries(map).some(
    ([key, p]) =>
      key !== ownAccount &&
      !p.member_keys &&
      !p.online &&
      presenceStatus(p) === "reconnecting",
  );
}
