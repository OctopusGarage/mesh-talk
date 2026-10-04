/** Classify native evidence without treating an absent observation as a fix. */
import assert from "node:assert/strict";

export function assertNetworkConsoleExpectation(analysis, expected) {
  assert.ok(["observed", "hidden"].includes(expected), "invalid native expectation");
  assert.equal(analysis.naturalCalls, 2, "two real natural polls required");
  assert.equal(analysis.manualCalls, 6, "six real manual queries required");
  assert.equal(analysis.applicationOwnedNetshStarts, 8, "all eight child launches required");
  assert.equal(analysis.unverifiedTemporalCalls, 0, "ambiguous console evidence is not accepted");
  assert.equal(analysis.correlatedCalls, expected === "observed" ? 8 : 0,
    "native console observation must match the expected variant");
}

export function analyzeNetworkConsole({ applicationPid, controls, calls, events }) {
  const within = (timestamp, interval, margin = 100) =>
    timestamp >= interval.start - margin && timestamp <= interval.end + margin;
  const visible = events.filter((e) => e.kind === "window" && e.visible);
  assert.equal(controls.positive.consolePresent, true, "positive control must own a console");
  assert.ok(visible.some((e) => e.isFixture && within(e.timestamp, controls.positive)),
    "positive control window must be captured by the observer");
  assert.equal(controls.negative.consolePresent, false, "negative control must not own a console");
  assert.ok(!visible.some((e) => e.isFixture && within(e.timestamp, controls.negative)),
    "negative control must not create a visible console");
  assert.ok(calls.length > 0 && calls.every((c) => c.ok === true && c.end >= c.start),
    "all observed network_name IPC calls must complete successfully");
  const natural = calls.filter((c) => c.phase === "natural");
  assert.ok(natural.length >= 2 && natural[1].start - natural[0].start >= 55000,
    "natural polling must run twice without accelerated timers");
  const children = events.filter((e) => e.kind === "process-start" &&
    e.name === "netsh.exe" && e.parentPid === applicationPid);
  const ownedWindow = (window) => children.some((child) => child.processId === window.processId &&
    Math.abs(child.timestamp - window.timestamp) <= 2000);
  assert.equal(new Set(children.map((e) => e.processId)).size, children.length,
    "child process evidence must contain distinct PIDs");
  // Short-lived hidden children can exit before WMI delivery. Its event generation
  // time is batched (not the OS process start time), so allow only that source a
  // bounded delay. Match one-to-one: one event must never satisfy multiple queries.
  const assignments = new Map();
  const match = (callIndex, visited) => children.some((child, childIndex) => {
    if (visited.has(childIndex)) return false;
    const call = calls[callIndex];
    const timed = child.timestampSource === "wmi-event" ?
      child.timestamp >= call.start - 100 && child.timestamp <= call.end + 2000 :
      within(child.timestamp, call);
    const windowTimed = visible.some((window) => window.processId === child.processId &&
      within(window.timestamp, call) && ownedWindow(window));
    if (!timed && !windowTimed) return false;
    visited.add(childIndex);
    if (!assignments.has(childIndex) || match(assignments.get(childIndex), visited)) {
      assignments.set(childIndex, callIndex);
      return true;
    }
    return false;
  });
  assert.ok(children.length === calls.length && calls.every((_, i) => match(i, new Set())),
    "each query must launch an application-owned netsh process");
  const bindings = events.filter((e) => e.kind === "console-owner" && e.parentPid === applicationPid &&
    children.some((child) => child.processId === e.processId));
  const correlated = calls.filter((c) => visible.some((e) => within(e.timestamp, c) &&
    (ownedWindow(e) || bindings.some((binding) => binding.hwnd === e.hwnd && within(binding.timestamp, c)))));
  const temporal = calls.filter((c) => !correlated.includes(c) &&
    visible.some((e) => e.isNetsh && within(e.timestamp, c)));
  return {
    status: correlated.length ? "console-window-observed" : temporal.length ?
      "console-window-temporal-correlation-only" : "console-window-not-observed",
    naturalCalls: natural.length,
    manualCalls: calls.length - natural.length,
    applicationOwnedNetshStarts: children.length,
    correlatedCalls: correlated.length,
    correlatedNaturalCalls: correlated.filter((c) => c.phase === "natural").length,
    unverifiedTemporalCalls: temporal.length,
    limitation: "Not proof of whole-screen GPU flicker or absence of every brief console. Temporal-only windows have unverified ownership.",
  };
}
