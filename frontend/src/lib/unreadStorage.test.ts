import { afterEach, expect, it, vi } from "vitest";
import {
  loadPendingUnread,
  loadUnread,
  savePendingUnread,
  saveUnread,
  queueReceptionPersistence,
  flushReceptionPersistence,
} from "./unreadStorage";

afterEach(() => {
  flushReceptionPersistence();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

it("coalesces receipt snapshots without postponing writes under sustained traffic", async () => {
  vi.useFakeTimers();
  const values = new Map<string, string>();
  const write = vi.fn((key: string, value: string) => values.set(key, value));
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: write,
  });
  queueReceptionPersistence("alice", "account", { unread: { "channel:a": 1 } });
  await vi.advanceTimersByTimeAsync(50);
  queueReceptionPersistence("alice", "account", { unread: { "channel:a": 2 } });
  expect(write).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(50);
  expect(write).toHaveBeenCalledTimes(1);
  expect(loadUnread("alice", "account")).toEqual({ "channel:a": 2 });
});

it("flushes the original owner's snapshot on replacement and before same-owner restoration", () => {
  vi.useFakeTimers();
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
  queueReceptionPersistence("alice", "account", { unread: { "channel:a": 1 } });
  queueReceptionPersistence("bob", "other", { unread: { "channel:b": 2 } });
  expect(values.get("mesh-talk-unread-v1:alice:account")).toBe(
    '{"channel:a":1}',
  );
  expect(loadUnread("bob", "other")).toEqual({ "channel:b": 2 });
  expect(loadUnread("bob", "account")).toEqual({});
});

it("restores only valid unread counts for the same host and mesh account", () => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
  saveUnread("host-a", "account-a", {
    "account:alice": 2,
    "channel:team": 3,
    "account:read": 0,
    "account:invalid": -1,
  });
  expect(loadUnread("host-a", "account-a")).toEqual({
    "account:alice": 2,
    "channel:team": 3,
  });
  expect(loadUnread("host-b", "account-a")).toEqual({});
  expect(loadUnread("host-a", "account-b")).toEqual({});

  values.set("mesh-talk-unread-v1:host-a:account-a", "invalid JSON");
  expect(loadUnread("host-a", "account-a")).toEqual({});
});

it("restores undiscovered sender counts only for the same account", () => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
  });
  savePendingUnread("host", "account-a", { "device-early": 2 });
  expect(loadPendingUnread("host", "account-a")).toEqual({
    "device-early": 2,
  });
  expect(loadPendingUnread("host", "account-b")).toEqual({});
});
