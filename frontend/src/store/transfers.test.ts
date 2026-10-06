import { beforeEach, expect, it } from "vitest";
import { useTransfers } from "./transfers";

beforeEach(() => useTransfers.getState().reset());

it("keeps concurrent transfers separate and removes only the completed file", () => {
  const progress = useTransfers.getState().applyProgress;
  progress({ file_conv: "a", direction: "send", done: 3, total: 10 });
  progress({ file_conv: "b", direction: "save", done: 1, total: 8 });
  const other = useTransfers.getState().transfers.b;

  progress({ file_conv: "a", direction: "send", done: 10, total: 10 });

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
