export function matchesRenderedInput(text) {
  return typeof text === "string" && text.trim() === "renderprobe";
}

export function analyzeRenderer(data) {
  const limitation = "X11/Xvfb observation only; not a VMware GPU reproduction or proof of remediation.";
  const inconclusive = { status: "inconclusive", limitation };
  if (data.error || !["sessionReady", "ownerVerified", "mapped", "domReady", "interaction"]
    .every(key => data[key] === true)) return inconclusive;
  const colors = [[255, 0, 255], [0, 255, 0]];
  if (!Array.isArray(data.paints) || data.paints.length !== colors.length) return inconclusive;
  for (const [index, paint] of data.paints.entries()) {
    if (!paint || !Array.isArray(paint.expected) || !Array.isArray(paint.actual) ||
      paint.expected.length !== 3 || paint.actual.length !== 3 ||
      !paint.expected.every((value, channel) => value === colors[index][channel]) ||
      !paint.actual.every(value => Number.isInteger(value) && value >= 0 && value <= 255)) return inconclusive;
  }
  const painted = data.paints.filter(paint =>
    paint.actual.every((value, channel) => Math.abs(value - paint.expected[channel]) <= 4)).length;
  if (painted === 2 && (data.interactionPainted !== true || data.markerRemovedPainted !== true)) return inconclusive;
  return { status: painted === 2 ? "presentation-observed" : "presentation-failure-observed",
    painted, attempted: 2, limitation };
}
