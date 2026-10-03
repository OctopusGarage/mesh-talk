import assert from "node:assert/strict";

export function assertTrayGeometryExpectation(analysis, expected) {
  assert.ok(["changed", "preserved"].includes(expected), "invalid geometry expectation");
  assert.equal(analysis.rounds, 3);
  assert.equal(analysis.sizeChanges, 0, "window size must not change");
  assert.equal(analysis.positionChanges, expected === "changed" ? 3 : 0,
    "all native rounds must match the expected position behavior");
}

export function analyzeTrayGeometry({ applicationPid, windowId, rounds }) {
  assert.ok(Number.isInteger(applicationPid) && applicationPid > 0 && windowId);
  assert.equal(rounds.length, 3, "three complete native rounds required");
  let positionChanges = 0;
  let sizeChanges = 0;
  for (const round of rounds) {
    assert.equal(round.applicationPid, applicationPid, "original process must survive");
    assert.equal(round.windowId, windowId, "original X11 window must survive");
    assert.equal(round.tray.ownerPid, applicationPid, "menu must belong to the application");
    assert.equal(round.tray.label, "Show");
    assert.equal(round.tray.clicked, true, "actual exported menu must be activated");
    assert.equal(round.before.mapped, true);
    assert.equal(round.hidden.mapped, false, "close-to-tray must unmap the window");
    assert.equal(round.after.mapped, true, "tray menu must remap the window");
    for (const key of ["x", "y", "width", "height"]) {
      assert.ok(Number.isFinite(round.before[key]) && Number.isFinite(round.after[key]));
      assert.equal(round.before[key], round.target[key], "native movement must reach the intended geometry");
    }
    assert.ok(round.before.width > 0 && round.before.height > 0 && round.after.width > 0 && round.after.height > 0);
    if (round.before.x !== round.after.x || round.before.y !== round.after.y) positionChanges++;
    if (round.before.width !== round.after.width || round.before.height !== round.after.height) sizeChanges++;
  }
  return { status: positionChanges || sizeChanges ? "geometry-change-observed" : "geometry-change-not-observed",
    rounds: rounds.length, positionChanges, sizeChanges,
    limitation: "Native X11 observation under the selected window manager; not proof for every Linux desktop or Wayland." };
}
