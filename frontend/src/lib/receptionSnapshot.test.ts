import { afterEach, expect, it, vi } from "vitest";
import {
  loadReceptionSnapshot,
  saveReceptionSnapshot,
  type ReceptionSnapshot,
} from "./receptionSnapshot";
import type { ReceptionScope } from "@/store/receptionDelivery";
const scope: ReceptionScope = {
  owner: "local",
  account_id: "account",
  device_id: "device",
  runtime_id: "session:ticket",
};
const state: ReceptionSnapshot = {
  cursor: {
    epoch: "0123456789abcdef0123456789abcdef",
    sequence: "18446744073709551615",
  },
  ledger: {
    "account:peer": { id: { seen: false, late: true, mentioned: true } },
  },
  overflowUnread: { "account:old": 3 },
  unread: { "account:peer": 1, "account:old": 3 },
  pendingUnknownUnread: { device: 2 },
  historyFullBaselines: ["account:peer"],
  reception: {
    "channel:team": {
      phase: "waiting",
      received: 3,
      batchId: "batch",
      processing: false,
    },
  },
};
afterEach(() => vi.unstubAllGlobals());
function storage() {
  const values = new Map<string, string>();
  const setItem = vi.fn((key: string, value: string) => {
    values.set(key, value);
  });
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem,
  });
  return { values, setItem };
}
it("commits reception state and an exact cursor with one atomic storage operation", () => {
  const { setItem } = storage();
  saveReceptionSnapshot(scope, state);
  expect(setItem).toHaveBeenCalledTimes(1);
  expect(loadReceptionSnapshot(scope)).toEqual(state);
  expect(
    loadReceptionSnapshot({ ...scope, runtime_id: "new-session" }),
  ).toEqual(state);
  for (const key of ["owner", "account_id", "device_id"] as const)
    expect(loadReceptionSnapshot({ ...scope, [key]: "different" })).toBeNull();
});
it("surfaces quota failure and preserves the previous committed cursor and unread state", () => {
  const { setItem } = storage();
  saveReceptionSnapshot(scope, state);
  setItem.mockImplementationOnce(() => {
    throw new DOMException("full", "QuotaExceededError");
  });
  expect(() =>
    saveReceptionSnapshot(scope, {
      ...state,
      cursor: { ...state.cursor, sequence: "1" },
      unread: {},
    }),
  ).toThrow("full");
  expect(loadReceptionSnapshot(scope)).toEqual(state);
});
it("does not silently fall back past unread work when the committed snapshot is corrupt", () => {
  const { values } = storage();
  saveReceptionSnapshot(scope, state);
  values.set([...values.keys()][0], "{broken");
  expect(() => loadReceptionSnapshot(scope)).toThrow();
});
it("rejects an oversized reception snapshot before committing its cursor", () => {
  const { setItem } = storage();
  expect(() =>
    saveReceptionSnapshot(scope, {
      ...state,
      pendingUnknownUnread: { ["x".repeat(8_000_001)]: 1 },
    }),
  ).toThrow();
  expect(setItem).not.toHaveBeenCalled();
});
