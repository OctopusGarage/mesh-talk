import { expect, it } from "vitest";
import { createRosterRefresh } from "./rosterRefresh";

it("coalesces busy invalidations to the latest input", async () => {
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const observed: (number | undefined)[] = [];
  const refresh = createRosterRefresh<number>({
    capture: () => ({ current: () => true }),
    refresh: async (_, input) => {
      observed.push(input);
      if (observed.length === 1) await held;
    },
  });
  const pending = refresh(1);
  for (let i = 2; i <= 1000; i++) void refresh(i);
  expect(observed).toEqual([1]);
  release();
  await pending;
  expect(observed).toEqual([1, 1000]);
});
