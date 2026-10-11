import { beforeEach, expect, it } from "vitest";
import { estimateRemainingSeconds, useTransfers } from "./transfers";

beforeEach(() => useTransfers.getState().reset());

it("keeps concurrent transfers separate and removes only the completed file", () => {
  const progress = useTransfers.getState().applyProgress;
  progress({ file_conv: "a", direction: "prepare", done: 3, total: 10 });
  progress({ file_conv: "b", direction: "save", done: 1, total: 8 });
  const other = useTransfers.getState().transfers.b;

  progress({ file_conv: "a", direction: "prepare", done: 10, total: 10 });

  expect(useTransfers.getState().transfers).toEqual({ b: other });
  expect(useTransfers.getState().transfers.b).toBe(other);
  useTransfers.getState().clear("b");
  expect(useTransfers.getState().transfers).toEqual({});
});

it("clearing a missing transfer leaves the store snapshot unchanged", () => {
  const before = useTransfers.getState();
  useTransfers.getState().clear("missing");
  expect(useTransfers.getState()).toBe(before);
});

it("estimates remaining time only from two advancing recipient observations", () => {
  expect(estimateRemainingSeconds(1, 10, 1, 1_000, 2_000)).toBeNull();
  expect(estimateRemainingSeconds(5, 10, 1, 2_000, 12_000)).toBe(13);
  expect(estimateRemainingSeconds(5, 10, 1, 2_000, 2_000)).toBeNull();
  expect(estimateRemainingSeconds(5, 10, 1, 2_000, 20_000)).toBeNull();
  expect(estimateRemainingSeconds(10, 10, 1, 2_000, 12_000)).toBeNull();
});

it("keeps recipient progress separate from local preparation and clears it on confirmation", () => {
  const transfers = useTransfers.getState();
  transfers.applyProgress({
    file_conv: "c1",
    direction: "prepare",
    done: 1,
    total: 8,
  });
  transfers.applyUploadStatus("event-1", {
    done: 0,
    total: 8,
    delivered: false,
  });
  expect(useTransfers.getState().transfers.c1?.direction).toBe("prepare");
  expect(useTransfers.getState().transfers["event-1"]).toMatchObject({
    direction: "send",
    done: 0,
    total: 8,
    etaSeconds: null,
  });
  transfers.applyUploadStatus("event-1", {
    done: 8,
    total: 8,
    delivered: false,
  });
  expect(useTransfers.getState().transfers["event-1"]?.done).toBe(8);
  transfers.applyUploadStatus("event-1", {
    done: 8,
    total: 8,
    delivered: true,
  });
  expect(useTransfers.getState().transfers["event-1"]?.delivered).toBe(true);
});

it("bounds completed upload records without evicting a live transfer", () => {
  const transfers = useTransfers.getState();
  transfers.applyUploadStatus("live", { done: 1, total: 8, delivered: false });
  for (let index = 0; index < 1100; index++)
    transfers.applyUploadStatus(`finished-${index}`, {
      done: 8,
      total: 8,
      delivered: true,
    });
  expect(
    Object.keys(useTransfers.getState().transfers).length,
  ).toBeLessThanOrEqual(1024);
  expect(useTransfers.getState().transfers.live?.delivered).toBeFalsy();
});
