export type Point = { x: number; y: number };
export type Size = { width: number; height: number };
export type Region = Point & Size;
export const handles = ["nw", "n", "ne", "e", "se", "s", "sw", "w"] as const;
export type Handle = (typeof handles)[number];
export type Adjustment = Handle | "move";
const clamp = (value: number, min: number, max: number) =>
  Math.max(min, Math.min(max, value));

export function regionBetween(a: Point, b: Point): Region {
  return {
    x: Math.min(a.x, b.x),
    y: Math.min(a.y, b.y),
    width: Math.abs(a.x - b.x),
    height: Math.abs(a.y - b.y),
  };
}
export function handlePoint(region: Region, handle: Handle): Point {
  return {
    x:
      region.x +
      (handle.includes("w")
        ? 0
        : handle.includes("e")
          ? region.width
          : region.width / 2),
    y:
      region.y +
      (handle.includes("n")
        ? 0
        : handle.includes("s")
          ? region.height
          : region.height / 2),
  };
}
export function hitHandle(
  region: Region,
  point: Point,
  tolerance: number,
): Handle | null {
  return (
    handles.find((handle) => {
      const p = handlePoint(region, handle);
      return (
        Math.abs(point.x - p.x) <= tolerance &&
        Math.abs(point.y - p.y) <= tolerance
      );
    }) ?? null
  );
}
export function adjustRegion(
  region: Region,
  adjustment: Adjustment,
  delta: Point,
  bounds: Size,
): Region {
  if (adjustment === "move")
    return {
      ...region,
      x: clamp(region.x + delta.x, 0, bounds.width - region.width),
      y: clamp(region.y + delta.y, 0, bounds.height - region.height),
    };
  let { x, y } = region;
  let right = x + region.width;
  let bottom = y + region.height;
  if (adjustment.includes("w")) x = clamp(x + delta.x, 0, right - 4);
  if (adjustment.includes("e"))
    right = clamp(right + delta.x, x + 4, bounds.width);
  if (adjustment.includes("n")) y = clamp(y + delta.y, 0, bottom - 4);
  if (adjustment.includes("s"))
    bottom = clamp(bottom + delta.y, y + 4, bounds.height);
  return { x, y, width: right - x, height: bottom - y };
}
export function toolbarPosition(
  region: Region,
  viewport: Size,
  toolbar: Size,
): { left: number; top: number } {
  const margin = 8;
  const below = region.y + region.height + margin;
  return {
    left: clamp(
      region.x + region.width - toolbar.width,
      margin,
      viewport.width - toolbar.width - margin,
    ),
    top: clamp(
      below + toolbar.height <= viewport.height - margin
        ? below
        : region.y - toolbar.height - margin,
      margin,
      viewport.height - toolbar.height - margin,
    ),
  };
}
