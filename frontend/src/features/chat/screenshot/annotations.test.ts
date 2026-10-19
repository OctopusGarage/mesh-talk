import { expect, it, vi } from "vitest";
import { paintMark } from "./annotations";

it("keeps text legible at Retina scale while retaining original image coordinates", () => {
  const ctx = {
    save: vi.fn(),
    restore: vi.fn(),
    fillText: vi.fn(),
    font: "",
    lineWidth: 0,
  };
  paintMark(
    ctx as unknown as CanvasRenderingContext2D,
    {
      kind: "text",
      at: { x: 200, y: 300 },
      value: "note",
      color: "#fa5151",
      width: 3,
      scale: 2,
    },
    {} as HTMLImageElement,
  );
  expect(ctx.font).toBe("44px sans-serif");
  expect(ctx.lineWidth).toBe(6);
  expect(ctx.fillText).toHaveBeenCalledWith("note", 200, 300);
});
