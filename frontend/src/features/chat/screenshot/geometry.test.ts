import { expect, it } from "vitest";
import {
  adjustRegion,
  hitHandle,
  regionBetween,
  toolbarPosition,
} from "./geometry";

const region = { x: 20, y: 30, width: 100, height: 80 };
it("normalizes a selection dragged backwards", () => {
  expect(regionBetween({ x: 90, y: 70 }, { x: 10, y: 20 })).toEqual({
    x: 10,
    y: 20,
    width: 80,
    height: 50,
  });
});
it("moves a selection without changing its size or leaving the image", () => {
  expect(
    adjustRegion(
      region,
      "move",
      { x: -100, y: 200 },
      { width: 300, height: 200 },
    ),
  ).toEqual({ x: 0, y: 120, width: 100, height: 80 });
});
it("resizes from a corner, clamps at the image and prevents inversion", () => {
  expect(
    adjustRegion(region, "nw", { x: -50, y: -60 }, { width: 300, height: 200 }),
  ).toEqual({ x: 0, y: 0, width: 120, height: 110 });
  expect(
    adjustRegion(
      region,
      "se",
      { x: -200, y: -200 },
      { width: 300, height: 200 },
    ),
  ).toEqual({ x: 20, y: 30, width: 4, height: 4 });
});
it("finds edge and corner handles with image-coordinate hit tolerance", () => {
  expect(hitHandle(region, { x: 22, y: 32 }, 5)).toBe("nw");
  expect(hitHandle(region, { x: 70, y: 110 }, 5)).toBe("s");
  expect(hitHandle(region, { x: 70, y: 70 }, 5)).toBeNull();
});
it("keeps the toolbar inside a narrow viewport and moves above a bottom selection", () => {
  expect(
    toolbarPosition(
      { x: 250, y: 260, width: 60, height: 30 },
      { width: 320, height: 320 },
      { width: 300, height: 80 },
    ),
  ).toEqual({ left: 10, top: 172 });
});
