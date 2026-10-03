import assert from "node:assert/strict";
import test from "node:test";
import { analyzeRenderer, matchesRenderedInput } from "./renderer-report.mjs";

function evidence() {
  return { sessionReady: true, ownerVerified: true, mapped: true, domReady: true,
    interaction: true, interactionPainted: true, markerRemovedPainted: true, paints: [
      { expected: [255, 0, 255], actual: [255, 0, 255] },
      { expected: [0, 255, 0], actual: [0, 255, 0] },
    ] };
}
test("only exact rendered probe text establishes input presentation, not placeholder or caret changes", () => {
  assert.equal(matchesRenderedInput("renderprobe\n"), true);
  for (const text of ["", "alice", "renderer-probe", "renderprobe extra", null]) {
    assert.equal(matchesRenderedInput(text), false);
  }
});
test("two independently observed colors and native input establish scoped presentation success", () => {
  assert.equal(analyzeRenderer(evidence()).status, "presentation-observed");
});
test("responsive DOM with missing desktop paint is a presentation failure, not a loading error", () => {
  const data = evidence(); data.paints[1].actual = [255, 255, 255];
  data.interactionPainted = false;
  assert.equal(analyzeRenderer(data).status, "presentation-failure-observed");
});
test("missing readiness, ownership, mapping or native interaction is inconclusive", () => {
  for (const key of ["sessionReady", "ownerVerified", "mapped", "domReady", "interaction", "interactionPainted", "markerRemovedPainted"]) {
    const data = evidence(); data[key] = false;
    assert.equal(analyzeRenderer(data).status, "inconclusive");
  }
});
test("missing, malformed or incomplete pixel evidence cannot establish a rendering failure", () => {
  for (const mutate of [
    d => { d.paints.pop(); },
    d => { d.paints[0] = null; },
    d => { d.paints[0].actual = null; },
    d => { d.paints[0].actual = [NaN, 0, 0]; },
    d => { d.paints[0].actual = [-1, 0, 0]; },
    d => { d.paints[1].expected = [255, 0, 255]; },
    d => { d.error = "driver disconnected"; },
  ]) {
    const data = evidence(); mutate(data);
    assert.equal(analyzeRenderer(data).status, "inconclusive");
  }
});
test("allows a small pixel rounding tolerance, not arbitrary near-white colors", () => {
  const data = evidence(); data.paints[0].actual = [252, 2, 253];
  assert.equal(analyzeRenderer(data).status, "presentation-observed");
  data.paints[0].actual = [245, 0, 245];
  assert.equal(analyzeRenderer(data).status, "presentation-failure-observed");
});
