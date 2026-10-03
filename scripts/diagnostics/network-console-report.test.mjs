import assert from "node:assert/strict";
import test from "node:test";
import { analyzeNetworkConsole, assertNetworkConsoleExpectation } from "./network-console-report.mjs";

test("fixed expectation requires successful child observations and no visible correlations", () => {
  const analysis = { naturalCalls: 2, manualCalls: 6, applicationOwnedNetshStarts: 8,
    correlatedCalls: 0, unverifiedTemporalCalls: 0 };
  assert.doesNotThrow(() => assertNetworkConsoleExpectation(analysis, "hidden"));
  assert.throws(() => assertNetworkConsoleExpectation({ ...analysis, correlatedCalls: 1 }, "hidden"));
  assert.throws(() => assertNetworkConsoleExpectation({ ...analysis, unverifiedTemporalCalls: 1 }, "hidden"));
  assert.throws(() => assertNetworkConsoleExpectation({ ...analysis, applicationOwnedNetshStarts: 7 }, "hidden"));
  assert.throws(() => assertNetworkConsoleExpectation(analysis, "observed"));
  assert.doesNotThrow(() => assertNetworkConsoleExpectation({ ...analysis, correlatedCalls: 8 }, "observed"));
  assert.throws(() => assertNetworkConsoleExpectation(analysis, "invalid"));
});
import { installNetworkTrace } from "./network-console-trace.mjs";

function evidence() {
  return {
    applicationPid: 42,
    controls: {
      positive: { start: 100, end: 500, consolePresent: true },
      negative: { start: 600, end: 1000, consolePresent: false },
    },
    calls: [
      { phase: "natural", start: 2000, end: 2100, ok: true },
      { phase: "natural", start: 62000, end: 62100, ok: true },
      { phase: "manual", start: 65000, end: 65100, ok: true },
    ],
    events: [
      { kind: "window", timestamp: 200, visible: true, isFixture: true },
      { kind: "process-start", timestamp: 2010, processId: 51, parentPid: 42, name: "netsh.exe" },
      { kind: "window", timestamp: 2020, hwnd: 101, visible: true, isNetsh: true },
      { kind: "console-owner", timestamp: 2015, hwnd: 101, processId: 51, parentPid: 42 },
      { kind: "process-start", timestamp: 62010, processId: 52, parentPid: 42, name: "netsh.exe" },
      { kind: "process-start", timestamp: 65010, processId: 53, parentPid: 42, name: "netsh.exe" },
    ],
  };
}

test("correlates real app child and visible console during a natural query", () => {
  const result = analyzeNetworkConsole(evidence());
  assert.equal(result.status, "console-window-observed");
  assert.equal(result.correlatedCalls, 1);
  assert.equal(result.naturalCalls, 2);
});

test("absence of a console is an observation, not proof of a fix", () => {
  const data = evidence();
  data.events = data.events.filter((e) => !e.isNetsh);
  assert.equal(analyzeNetworkConsole(data).status, "console-window-not-observed");
});

test("requires a positive control to validate the observer", () => {
  const data = evidence();
  data.events = data.events.filter((e) => !e.isFixture);
  assert.throws(() => analyzeNetworkConsole(data), /positive control/);
});

test("rejects a visible negative control", () => {
  const data = evidence();
  data.events.push({ kind: "window", timestamp: 700, visible: true, isFixture: true });
  assert.throws(() => analyzeNetworkConsole(data), /negative control/);
});

test("an unrelated netsh parent cannot establish an application cause", () => {
  const data = evidence();
  data.events.filter((e) => e.kind === "process-start").forEach((e) => { e.parentPid = 99; });
  assert.throws(() => analyzeNetworkConsole(data), /application-owned netsh/);
});

test("a console outside query intervals cannot establish correlation", () => {
  const data = evidence();
  data.events.find((e) => e.isNetsh).timestamp = 3000;
  assert.equal(analyzeNetworkConsole(data).status, "console-window-not-observed");
});

test("requires unaccelerated natural polling and successful IPC", () => {
  const data = evidence();
  data.calls[1].start = 3000;
  assert.throws(() => analyzeNetworkConsole(data), /natural polling/);
  data.calls[1].start = 62000;
  data.calls[0].ok = false;
  assert.throws(() => analyzeNetworkConsole(data), /IPC/);
});

test("an unbound netsh-titled window is temporal correlation, not verified ownership", () => {
  const data = evidence();
  data.events.find((e) => e.isNetsh).hwnd = 999;
  const result = analyzeNetworkConsole(data);
  assert.equal(result.status, "console-window-temporal-correlation-only");
  assert.equal(result.correlatedCalls, 0);
  assert.equal(result.unverifiedTemporalCalls, 1);
});

test("a console binding for another parent cannot verify window ownership", () => {
  const data = evidence();
  data.events.find((e) => e.kind === "console-owner").parentPid = 99;
  assert.equal(analyzeNetworkConsole(data).status, "console-window-temporal-correlation-only");
});

test("native event time, not delayed delivery time, determines correlation", () => {
  const data = evidence();
  data.events.find((e) => e.isNetsh).deliveredAt = 4000;
  assert.equal(analyzeNetworkConsole(data).status, "console-window-observed");
});

test("observes real fetch responses without replacing immutable Tauri invoke", async () => {
  const response = new Response("null", { headers: { "Tauri-Response": "ok" } });
  const seen = [];
  const internals = Object.freeze({ invoke: () => {} });
  const original = internals.invoke;
  const target = {
    __TAURI_INTERNALS__: internals,
    location: { href: "http://tauri.localhost" },
    fetch: async (...args) => { seen.push(args); return response; },
  };
  assert.equal(installNetworkTrace(target), true);
  const options = { method: "POST", body: "{}" };
  assert.equal(await target.fetch("http://ipc.localhost/network_name", options), response);
  assert.equal(internals.invoke, original);
  assert.deepEqual(seen, [["http://ipc.localhost/network_name", options]]);
  assert.equal(target.nativeNetworkCalls[0].ok, true);
  assert.ok(target.nativeNetworkCalls[0].end >= target.nativeNetworkCalls[0].start);
});

test("transport tracing ignores other endpoints and preserves backend failures", async () => {
  const failed = new Response("null", { headers: { "Tauri-Response": "error" } });
  const target = { location: { href: "http://tauri.localhost" }, fetch: async () => failed };
  assert.equal(installNetworkTrace(target), true);
  await target.fetch("http://ipc.localhost/login");
  await target.fetch("https://example.com/network_name");
  assert.equal(target.nativeNetworkCalls.length, 0);
  assert.equal(await target.fetch("http://ipc.localhost/network_name"), failed);
  assert.equal(target.nativeNetworkCalls[0].ok, false);
});

test("native window PID can establish ownership even if console attachment is too late", () => {
  const data = evidence();
  data.events = data.events.filter((e) => e.kind !== "console-owner");
  data.events.find((e) => e.isNetsh).processId = 51;
  assert.equal(analyzeNetworkConsole(data).status, "console-window-observed");
});
