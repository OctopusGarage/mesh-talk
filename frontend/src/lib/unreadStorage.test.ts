import { afterEach, expect, it, vi } from "vitest";
import {
  loadPendingUnread,
  loadUnread,
  savePendingUnread,
  saveUnread,
} from "./unreadStorage";

afterEach(() => vi.unstubAllGlobals());

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
