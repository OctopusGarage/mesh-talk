import assert from "node:assert/strict";
import test from "node:test";
import { profile, parseArgs, formatReport } from "../validation/libtest-timing.mjs";

const line = (seconds, message) => `checks (windows-latest)\tTest (macOS + Windows)\t2026-10-19T10:00:${String(seconds).padStart(2, "0")}.000Z ${message}`;
const fixture = messages => messages.map(([time, message]) => line(time, message)).join("\n");

test("serial completion gaps preserve context and distinguish success, failure and ignored rows", () => {
  const report = profile(fixture([
    [0, "Running unittests src/lib.rs (target/debug/deps/core.exe)"],
    [1, "running 4 tests"],
    [3, "test node::delivery::first ... ok"],
    [4, "test node::delivery::ignored ... ignored"],
    [8, "test node::delivery::second ... FAILED"],
    [9, "test identity::third ... ok"],
    [10, "test result: FAILED. 2 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 8.00s"],
  ]), { serial: true });
  assert.equal(report.tests[0].estimatedSeconds, 2);
  assert.equal(report.tests[1].estimatedSeconds, null);
  assert.equal(report.tests[2].estimatedSeconds, 5);
  assert.equal(report.tests[3].estimatedSeconds, 1);
  assert.match(report.tests[0].context, /core.exe/);
  assert.equal(report.suites[0].declaredSeconds, 8);
  assert.equal(report.suites[0].estimatedSeconds, 8);
  assert.equal(report.suites[0].differenceSeconds, 0);
  assert.equal(report.counts.failed, 1);
  assert.equal(report.topFamilies[0].family, "node::delivery");
  assert.equal(report.topFamilies[0].estimatedSeconds, 7);
});

test("suite and binary boundaries reset anchors so compilation gaps never become test time", () => {
  const report = profile(fixture([
    [0, "running 1 test"], [2, "test a::one ... ok"],
    [3, "test result: ok. 1 passed; finished in 2s"],
    [30, "Running tests/network.rs (target/debug/deps/network.exe)"],
    [35, "running 1 test"], [38, "test b::two ... ok"],
    [39, "test result: ok. 1 passed; finished in 3s"],
  ]), { serial: true });
  assert.equal(report.suites.length, 2);
  assert.deepEqual(report.tests.map(value => value.estimatedSeconds), [2, 3]);
  assert.match(report.tests[1].context, /network.rs/);
});

test("missing or backwards timestamps do not manufacture duration estimates", () => {
  const report = profile([
    line(10, "running 5 tests"), line(9, "test a::one ... ok"),
    line(12, "test a::two ... ok"), "test a::three ... ok",
    "2026-99-99T10:00:14Z test a::four ... ok", line(15, "test a::five ... ok"),
  ].join("\n"), { serial: true });
  assert.deepEqual(report.tests.map(value => value.estimatedSeconds), [null, null, null, null, null]);
  assert.ok(report.diagnostics.some(value => /nonmonotonic/i.test(value)));
  assert.equal(report.suites[0].complete, false);
});

test("first rows without suite start and incomplete test rows remain unknown", () => {
  const report = profile(fixture([[2, "test a::one ... ok"], [4, "test a::two ... "]]), { serial: true });
  assert.equal(report.tests[0].estimatedSeconds, null);
  assert.equal(report.tests[1].state, "unfinished");
  assert.equal(report.tests[1].estimatedSeconds, null);
  assert.equal(report.counts.unfinished, 1);
});

test("interpretation requires an explicit serial assumption and top is bounded", () => {
  assert.throws(() => profile("", {}), /serial/);
  assert.throws(() => parseArgs(["--input", "log"]), /serial/);
  assert.throws(() => parseArgs(["--input", "log", "--serial", "--top", "0"]), /top/);
  const report = profile(fixture([[0, "running 2 tests"], [1, "test a::one ... ok"], [3, "test b::two ... ok"]]), { serial: true, top: 1 });
  assert.equal(report.topTests.length, 1);
  assert.equal(report.topTests[0].test, "b::two");
  assert.match(formatReport(report), /ESTIMATE/);
  assert.match(report.limitations, /overhead/);
});

test("colored Cargo binary headers preserve readable context and missing gh timestamps stay unknown", () => {
  const report = profile([
    line(0, "\u001b[1m\u001b[92m     Running\u001b[0m unittests src/lib.rs (target/debug/core.exe)"),
    line(1, "running 2 tests"),
    "checks (windows-latest)\tTest (macOS + Windows)\ttest a::one ... ok",
    line(4, "test a::two ... ok"),
  ].join("\n"), { serial: true });
  assert.match(report.tests[0].context, /Running unittests src\/lib.rs/);
  assert.equal(report.tests.length, 2);
  assert.deepEqual(report.tests.map(value => value.estimatedSeconds), [null, null]);
});

test("same timestamp chunks yield labeled zero gaps and mixed streams never share timing anchors", () => {
  const report = profile([
    line(0, "running 2 tests"), line(2, "test a::first ... ok"), line(2, "test a::second ... ok"),
    line(4, "test b::other ... ok").replace("checks (windows-latest)", "another Windows job"),
  ].join("\n"), { serial: true });
  assert.deepEqual(report.tests.map(value => value.estimatedSeconds), [2, 0, null]);
  assert.equal(report.suites.length, 2);
  assert.ok(report.diagnostics.some(value => /stream changed/i.test(value)));
  assert.match(report.tests[2].stream, /another Windows job/);
});

test("observed compilation resets timing even when a binary header is missing", () => {
  const report = profile(fixture([[0, "running 2 tests"], [1, "test a::first ... ok"], [10, "Compiling other v0.1.0"], [40, "test b::second ... ok"]]), { serial: true });
  assert.equal(report.tests[1].estimatedSeconds, null);
});
