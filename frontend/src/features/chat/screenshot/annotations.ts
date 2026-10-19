import { regionBetween, type Point } from "./geometry";
export type Ink = { color: string; width: number };
export type Mark = Ink & { scale: number } & (
    | { kind: "pen"; points: Point[] }
    | { kind: "rect" | "ellipse" | "arrow" | "mosaic"; from: Point; to: Point }
    | { kind: "text"; at: Point; value: string }
  );
export type Tool = "select" | Mark["kind"];
export const colors = [
  "#fa5151",
  "#ffb400",
  "#07c160",
  "#10a8e5",
  "#6467f0",
  "#ffffff",
  "#222222",
];

export function paintMark(
  ctx: CanvasRenderingContext2D,
  mark: Mark,
  image: HTMLImageElement,
) {
  ctx.save();
  ctx.strokeStyle = mark.color;
  ctx.fillStyle = mark.color;
  const width = mark.width * mark.scale;
  ctx.lineWidth = width;
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  if (mark.kind === "text") {
    ctx.font = `${(16 + mark.width * 2) * mark.scale}px sans-serif`;
    ctx.textBaseline = "top";
    ctx.fillText(mark.value, mark.at.x, mark.at.y);
  } else if (mark.kind === "pen") {
    ctx.beginPath();
    const first = mark.points[0];
    if (first) {
      ctx.moveTo(first.x, first.y);
      for (const p of mark.points.slice(1)) ctx.lineTo(p.x, p.y);
      if (mark.points.every((p) => p.x === first.x && p.y === first.y)) {
        ctx.arc(first.x, first.y, width / 2, 0, Math.PI * 2);
        ctx.fill();
      } else ctx.stroke();
    }
  } else if (mark.kind === "arrow") {
    const angle = Math.atan2(mark.to.y - mark.from.y, mark.to.x - mark.from.x);
    const head = Math.max(10, mark.width * 4) * mark.scale;
    ctx.beginPath();
    ctx.moveTo(mark.from.x, mark.from.y);
    ctx.lineTo(mark.to.x, mark.to.y);
    for (const side of [-1, 1]) {
      ctx.moveTo(mark.to.x, mark.to.y);
      ctx.lineTo(
        mark.to.x - head * Math.cos(angle + (side * Math.PI) / 6),
        mark.to.y - head * Math.sin(angle + (side * Math.PI) / 6),
      );
    }
    ctx.stroke();
  } else {
    const r = regionBetween(mark.from, mark.to);
    if (mark.kind === "rect") ctx.strokeRect(r.x, r.y, r.width, r.height);
    else if (mark.kind === "ellipse") {
      ctx.beginPath();
      ctx.ellipse(
        r.x + r.width / 2,
        r.y + r.height / 2,
        r.width / 2,
        r.height / 2,
        0,
        0,
        Math.PI * 2,
      );
      ctx.stroke();
    } else if (r.width && r.height) {
      // Render actual pixelation into the exported PNG, rather than a visual CSS filter.
      const sample = document.createElement("canvas");
      const block = (8 + mark.width * 2) * mark.scale;
      sample.width = Math.max(1, Math.ceil(r.width / block));
      sample.height = Math.max(1, Math.ceil(r.height / block));
      const source = sample.getContext("2d");
      if (source) {
        source.drawImage(
          image,
          r.x,
          r.y,
          r.width,
          r.height,
          0,
          0,
          sample.width,
          sample.height,
        );
        ctx.imageSmoothingEnabled = false;
        ctx.drawImage(sample, r.x, r.y, r.width, r.height);
      }
    }
  }
  ctx.restore();
}
