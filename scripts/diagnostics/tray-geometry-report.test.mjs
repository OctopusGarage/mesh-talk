import assert from "node:assert/strict";
import test from "node:test";
import { analyzeTrayGeometry, assertTrayGeometryExpectation } from "./tray-geometry-report.mjs";

test("fixed expectation rejects any position or size change and baseline requires all rounds", () => {
  const fixed = { rounds: 3, positionChanges: 0, sizeChanges: 0 };
  assert.doesNotThrow(() => assertTrayGeometryExpectation(fixed, "preserved"));
  assert.throws(() => assertTrayGeometryExpectation({ ...fixed, positionChanges: 1 }, "preserved"));
  assert.throws(() => assertTrayGeometryExpectation({ ...fixed, sizeChanges: 1 }, "preserved"));
  assert.throws(() => assertTrayGeometryExpectation(fixed, "changed"));
  assert.doesNotThrow(() => assertTrayGeometryExpectation({ ...fixed, positionChanges: 3 }, "changed"));
  assert.throws(() => assertTrayGeometryExpectation(fixed, "invalid"));
});

function evidence() {
  const geometry = { x: 180, y: 140, width: 900, height: 700 };
  return { applicationPid: 42, windowId: "123", rounds: [0, 1, 2].map(() => ({
    target: geometry, before: { ...geometry, mapped: true }, hidden: { mapped: false },
    after: { ...geometry, mapped: true }, applicationPid: 42, windowId: "123",
    tray: { ownerPid: 42, label: "Show", clicked: true },
  })) };
}
test("valid native rounds with unchanged geometry are observations, not proof on every desktop", () => {
  assert.equal(analyzeTrayGeometry(evidence()).status, "geometry-change-not-observed");
});
test("classifies a restored top-left shift separately from size changes", () => {
  const data = evidence(); data.rounds[1].after.x = 0; data.rounds[1].after.y = 0;
  assert.equal(analyzeTrayGeometry(data).positionChanges, 1);
  assert.equal(analyzeTrayGeometry(data).sizeChanges, 0);
  assert.equal(analyzeTrayGeometry(data).status, "geometry-change-observed");
});
test("rejects failed movement, missing menu evidence, process replacement and incomplete rounds", () => {
  for (const mutate of [
    d => { d.rounds[0].before.x = 0; },
    d => { d.rounds[0].tray.ownerPid = 99; },
    d => { d.rounds[0].tray.clicked = false; },
    d => { d.rounds[0].applicationPid = 99; },
    d => { d.rounds[0].windowId = "124"; },
    d => { d.rounds.pop(); },
    d => { d.rounds[0].hidden.mapped = true; },
    d => { d.rounds[0].after.mapped = false; },
    d => { d.rounds[0].after.width = NaN; },
  ]) {
    const data = evidence(); mutate(data);
    assert.throws(() => analyzeTrayGeometry(data));
  }
});
