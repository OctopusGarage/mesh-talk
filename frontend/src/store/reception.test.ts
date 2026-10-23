import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  receiveIdentity,
  viewIdentities,
  reconcileHistory,
  emptyReception,
} from "./reception";
import {
  loadReceptionLedger,
  saveReceptionLedger,
  loadUnreadOverflow,
  saveUnreadOverflow,
} from "@/lib/unreadStorage";
beforeEach(() => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
});
afterEach(() => vi.unstubAllGlobals());
it("deduplicates identities without treating focused conversations as viewed", () => {
  let s = receiveIdentity(emptyReception(), "channel:a", {
    event_id: "one",
    arrival_kind: "live",
  });
  s = receiveIdentity(s, "channel:a", {
    event_id: "one",
    arrival_kind: "catch_up",
  });
  expect(s.unreadIds["channel:a"]).toEqual(["one"]);
});
it("retains late unseen identities after reading newer rows", () => {
  let s = receiveIdentity(emptyReception(), "channel:a", { event_id: "new" });
  s = viewIdentities(s, "channel:a", ["new"]);
  s = receiveIdentity(s, "channel:a", {
    event_id: "old",
    arrival_kind: "catch_up",
  });
  expect(s.unreadIds["channel:a"]).toEqual(["old"]);
  expect(s.lateUnreadIds["channel:a"]).toEqual(["old"]);
  expect(
    viewIdentities(s, "channel:a", ["new"]).unreadIds["channel:a"],
  ).toEqual(["old"]);
});
it("keeps concurrent live traffic separate from a measured recovery batch", () => {
  let s = receiveIdentity(emptyReception(), "channel:a", {
    event_id: "old",
    arrival_kind: "catch_up",
    batch_id: "b",
    batch_complete: false,
  });
  s = receiveIdentity(s, "channel:a", {
    event_id: "live",
    arrival_kind: "live",
  });
  expect(s.reception["channel:a"]).toMatchObject({
    phase: "catching_up",
    received: 1,
  });
  s = receiveIdentity(s, "channel:a", {
    event_id: "old2",
    arrival_kind: "catch_up",
    batch_id: "b",
    batch_complete: true,
  });
  expect(s.reception["channel:a"]).toMatchObject({
    phase: "idle",
    received: 2,
  });
});
it("discovers imported history and skips own-account messages", () => {
  const s = reconcileHistory(
    emptyReception(),
    "channel:a",
    [
      { id: "theirs", fromMe: false },
      { id: "ours", fromMe: true },
    ],
    true,
  );
  expect(s.unreadIds["channel:a"]).toEqual(["theirs"]);
  expect(s.lateUnreadIds["channel:a"]).toEqual(["theirs"]);
  expect(
    reconcileHistory(s, "channel:a", [{ id: "theirs", fromMe: false }], true)
      .unreadIds["channel:a"],
  ).toEqual(["theirs"]);
});
it("persists unread and seen separately across scoped reloads", () => {
  let s = receiveIdentity(emptyReception(), "channel:a", { event_id: "read" });
  s = viewIdentities(s, "channel:a", ["read"]);
  s = receiveIdentity(s, "channel:a", {
    event_id: "late",
    arrival_kind: "history_sync",
  });
  saveReceptionLedger("host", "account", s.ledger);
  const restored = emptyReception(loadReceptionLedger("host", "account"));
  expect(
    receiveIdentity(restored, "channel:a", { event_id: "read" }).unreadIds[
      "channel:a"
    ],
  ).toEqual(["late"]);
  expect(loadReceptionLedger("other", "account")).toEqual({});
});
it("bounds untrusted persisted identifiers and entries", () => {
  const s = emptyReception({
    "channel:a": Object.fromEntries(
      Array.from({ length: 1100 }, (_, i) => [
        `id-${i}`,
        { seen: false, late: false },
      ]),
    ),
  });
  expect(Object.keys(s.ledger["channel:a"])).toHaveLength(1024);
  saveReceptionLedger("host", "account", s.ledger);
  expect(
    Object.keys(loadReceptionLedger("host", "account")["channel:a"]),
  ).toHaveLength(1024);
});
it("keeps total retention bounded after exhausting capacity", () => {
  const entries = Object.fromEntries(
    Array.from({ length: 1024 }, (_, i) => [
      `id-${i}`,
      { seen: true, late: false },
    ]),
  );
  const ledger = Object.fromEntries(
    Array.from({ length: 12 }, (_, i) => [`channel:${i}`, entries]),
  );
  expect(
    Object.values(emptyReception(ledger).ledger).reduce(
      (sum, c) => sum + Object.keys(c).length,
      0,
    ),
  ).toBeLessThanOrEqual(8192);
});
it("completes a batch even when its final event was received already", () => {
  let s = receiveIdentity(emptyReception(), "channel:a", {
    event_id: "a",
    arrival_kind: "catch_up",
    batch_id: "b",
    batch_complete: false,
  });
  s = receiveIdentity(s, "channel:a", {
    event_id: "a",
    arrival_kind: "catch_up",
    batch_id: "b",
    batch_complete: true,
  });
  expect(s.reception["channel:a"]).toMatchObject({
    phase: "idle",
    received: 1,
  });
});
it("retains unread exceptions ahead of evicting viewed identities", () => {
  let s = receiveIdentity(emptyReception(), "channel:a", {
    event_id: "unread",
  });
  s = viewIdentities(
    s,
    "channel:a",
    Array.from({ length: 1024 }, (_, i) => `seen-${i}`),
  );
  expect(s.unreadIds["channel:a"]).toEqual(["unread"]);
});
it("accepts opaque IDs that match prototype property names", () => {
  const s = receiveIdentity(emptyReception(), "channel:a", {
    event_id: "constructor",
  });
  expect(s.unreadIds["channel:a"]).toEqual(["constructor"]);
});
it("adds first receipt diagnostics to a previously known identity without changing read state", () => {
  const known = reconcileHistory(
    emptyReception(),
    "channel:a",
    [{ id: "old", fromMe: false }],
    false,
  );
  const received = receiveIdentity(known, "channel:a", {
    event_id: "old",
    received_at: 42,
    arrival_kind: "catch_up",
  });
  expect(received.ledger["channel:a"].old.receivedAt).toBe(42);
  expect(received.unreadIds["channel:a"]).toEqual([]);
});
it("keeps bounded aggregate unread when exact identities overflow", () => {
  const ledger = {
    "channel:a": Object.fromEntries(
      Array.from({ length: 1024 }, (_, i) => [
        `id-${i}`,
        { seen: false, late: true },
      ]),
    ),
  };
  const s = receiveIdentity(emptyReception(ledger), "channel:a", {
    event_id: "id-1025",
  });
  expect(s.unreadIds["channel:a"]).toHaveLength(1024);
  expect(s.overflowUnread["channel:a"]).toBe(1);
});

it("persists overflow independently from legacy counters and account scopes", () => {
  saveUnreadOverflow("host", "account", { "channel:a": 7 });
  expect(loadUnreadOverflow("host", "account")).toEqual({ "channel:a": 7 });
  expect(loadUnreadOverflow("other", "account")).toEqual({});
  expect(loadUnreadOverflow("host", "other")).toEqual({});
});

it.each([undefined, "erased", "expired", "unknown"])(
  "finishes metadata-only batches without indexing terminal identity %s",
  (id) => {
    const state = {
      ...emptyReception(),
      reception: {
        "channel:a": {
          phase: "catching_up" as const,
          received: 3,
          batchId: "b",
          processing: true,
        },
      },
    };
    const next = receiveIdentity(state, "channel:a", {
      event_id: id,
      arrival_kind: "catch_up",
      own_device: true,
      batch_id: "b",
      batch_complete: true,
      reception_only: true,
    });
    expect(next.reception["channel:a"]).toMatchObject({
      phase: "idle",
      received: 3,
      processing: false,
    });
    expect(next.ledger).toEqual({});
    expect(next.unreadIds).toEqual({});
  },
);
it("does not apply an old terminal to a different active recovery batch", () => {
  const state = {
    ...emptyReception(),
    reception: {
      "channel:a": {
        phase: "catching_up" as const,
        received: 1,
        batchId: "new",
        processing: true,
      },
    },
  };
  expect(
    receiveIdentity(state, "channel:a", {
      batch_id: "old",
      batch_complete: true,
      reception_only: true,
    }),
  ).toBe(state);
});

it("merges disk and boot identities once while keeping viewed evidence and legacy counts", async () => {
  const { mergeReception } = await import("./reception");
  const disk = emptyReception({
    "channel:a": {
      read: { seen: true, late: false },
      unread: { seen: false, late: true },
    },
  });
  let pending = receiveIdentity(emptyReception(), "channel:a", {
    event_id: "read",
  });
  pending = receiveIdentity(pending, "channel:a", { event_id: "unread" });
  pending = receiveIdentity(pending, "channel:a", { event_id: "live" });
  pending = receiveIdentity(pending, "channel:a", {
    event_id: "own",
    own_device: true,
  });
  const merged = mergeReception(
    { ...disk, unread: { "channel:a": 3 } },
    { ...pending, unread: { "channel:a": 4 } },
  );
  expect(new Set(merged.unreadIds["channel:a"])).toEqual(
    new Set(["unread", "live"]),
  );
  expect(merged.unread["channel:a"]).toBe(5); // three legacy counts, two exact identities
  expect(merged.ledger["channel:a"].read.seen).toBe(true);
  expect(merged.ledger["channel:a"].own.seen).toBe(true);
});

it("moves merged boot identities beyond the retained budget into persistent overflow", async () => {
  const { mergeReception } = await import("./reception");
  const ledger = (start: number) => ({
    "channel:a": Object.fromEntries(
      Array.from({ length: 1024 }, (_, i) => [
        `id-${start + i}`,
        { seen: false, late: true },
      ]),
    ),
  });
  const merged = mergeReception(
    { ...emptyReception(ledger(0)), unread: { "channel:a": 1024 } },
    { ...emptyReception(ledger(1024)), unread: { "channel:a": 1024 } },
  );
  expect(merged.unreadIds["channel:a"]).toHaveLength(1024);
  expect(merged.overflowUnread["channel:a"]).toBe(1024);
  expect(merged.unread["channel:a"]).toBe(2048);
  saveReceptionLedger("host", "account", merged.ledger);
  saveUnreadOverflow("host", "account", merged.overflowUnread);
  const restored = emptyReception(
    loadReceptionLedger("host", "account"),
    loadUnreadOverflow("host", "account"),
  );
  expect(
    restored.unreadIds["channel:a"].length +
      restored.overflowUnread["channel:a"],
  ).toBe(2048);
});

it("persists unread mention evidence until that exact message is viewed", () => {
  const key = "channel:a";
  let state = reconcileHistory(
    emptyReception(),
    key,
    [
      { id: "mention", fromMe: false, mentioned: true },
      { id: "ordinary", fromMe: false },
      { id: "own", fromMe: true, mentioned: true },
    ],
    true,
  );
  expect(state.ledger[key].mention.mentioned).toBe(true);
  expect(state.ledger[key].own.mentioned).not.toBe(true);
  saveReceptionLedger("host", "account", state.ledger);
  state = emptyReception(loadReceptionLedger("host", "account"));
  expect(state.ledger[key].mention.mentioned).toBe(true);
  expect(loadReceptionLedger("other", "account")).toEqual({});
  state = viewIdentities(state, key, ["ordinary"]);
  expect(state.ledger[key].mention).toMatchObject({
    mentioned: true,
    seen: false,
  });
  state = viewIdentities(state, key, ["mention"]);
  state = receiveIdentity(state, key, { event_id: "mention" }, true);
  expect(state.ledger[key].mention.seen).toBe(true);
});

it("enriches existing unseen history receipts without reviving read mentions", () => {
  const key = "channel:a";
  let state = receiveIdentity(emptyReception(), key, { event_id: "mention" });
  state = reconcileHistory(
    state,
    key,
    [{ id: "mention", fromMe: false, mentioned: true }],
    false,
  );
  expect(state.ledger[key].mention).toMatchObject({
    mentioned: true,
    seen: false,
  });
  state = viewIdentities(state, key, ["mention"]);
  state = reconcileHistory(
    state,
    key,
    [{ id: "mention", fromMe: false, mentioned: true }],
    true,
  );
  expect(state.unreadIds[key]).toEqual([]);
});

it.each([false, true])(
  "classifies a duplicate with late mention text without changing read state (own=%s)",
  (own) => {
    const key = "channel:a";
    const event = { event_id: "late-text", own_device: own };
    let state = receiveIdentity(emptyReception(), key, event);
    state = receiveIdentity(state, key, event, true);
    expect(state.ledger[key]["late-text"].mentioned === true).toBe(!own);
    expect(state.unreadIds[key]).toEqual(own ? [] : ["late-text"]);
    state = receiveIdentity(state, key, event);
    expect(state.ledger[key]["late-text"].mentioned === true).toBe(!own);
    state = viewIdentities(state, key, ["late-text"]);
    state = receiveIdentity(state, key, event, true);
    expect(state.ledger[key]["late-text"].seen).toBe(true);
    expect(state.unreadIds[key]).toEqual([]);
  },
);
