import assert from "node:assert/strict";
import test from "node:test";
import { summarize } from "../validation/selection-audit.mjs";

const source = "a".repeat(40);
const plan = { source, dirty: false, rust: [{ package: "mesh-talk-core", filters: ["node::account_sync"] }], frontend: ["src/example.test.ts"], node: ["scripts/diagnostics/example.test.mjs"] };
const results = failures => ({ source, complete: true, failures });

test("classifies exact paths and Rust package substring filters", () => {
  const report = summarize(plan, results([
    { kind: "rust", package: "mesh-talk-core", test: "node::account_sync::restores_account" },
    { kind: "frontend", path: "frontend/src/example.test.ts" },
    { kind: "node", path: "scripts/diagnostics/example.test.mjs" },
  ]));
  assert.equal(report.qualified, true);
  assert.equal(report.selectedFailures.length, 3);
  assert.equal(report.misses.length, 0);
  assert.equal(report.exitCode, 0);
});

test("reports the first missed applicable failure without treating escalations as misses", () => {
  const report = summarize(plan, results([
    { kind: "browser", path: "frontend/e2e/chat.spec.ts" },
    { kind: "rust", package: "mesh-talk-mcp", test: "node::account_sync" },
    { kind: "frontend", path: "src/other.test.ts" },
    { kind: "node", path: "scripts/diagnostics/other.test.mjs" },
    { kind: "native", test: "signed discovery" },
  ]));
  assert.equal(report.misses.length, 3);
  assert.equal(report.escalations.length, 2);
  assert.deepEqual(report.firstMissedRegression, report.misses[0]);
  assert.equal(report.exitCode, 1);
});

for (const [name, change] of [
  ["source mismatch", value => ({ ...value, source: "b".repeat(40) })],
  ["incomplete", value => ({ ...value, complete: false })],
]) test(`${name} is unqualified rather than a zero-miss claim`, () => {
  const report = summarize(plan, change(results([])));
  assert.equal(report.qualified, false);
  assert.equal(report.misses, null);
  assert.equal(report.firstMissedRegression, null);
  assert.equal(report.exitCode, 2);
});

test("dirty or identity-less plans cannot qualify an observation", () => {
  for (const value of [{ ...plan, dirty: true }, { ...plan, dirty: undefined }, { ...plan, source: undefined }]) {
    assert.equal(summarize(value, results([])).qualified, false);
  }
});

test("complete empty failure samples remain finite observations with explicit limitations", () => {
  const report = summarize(plan, results([]));
  assert.equal(report.qualified, true);
  assert.equal(report.misses.length, 0);
  assert.match(report.limitations, /finite sample/i);
});

test("malformed failures and unsafe paths are rejected rather than silently skipped", () => {
  for (const failures of [undefined, null, {}, [{ kind: "unknown" }], [{ kind: "rust", package: "mesh-talk-core" }], [{ kind: "frontend", path: "../src/a.test.ts" }], [{ kind: "node", path: "/tmp/a.test.mjs" }], [{ kind: "node", path: "scripts/../a.test.mjs" }], [{ kind: "node", path: "C:\\temp\\a.test.mjs" }]]) {
    assert.throws(() => summarize(plan, { source, complete: true, failures }), /Invalid/);
  }
  assert.throws(() => summarize({ ...plan, node: ["../escape"] }, results([])), /Invalid/);
});

test("Windows separators normalize without relaxing exact selection", () => {
  const report = summarize(plan, results([{ kind: "frontend", path: "src\\example.test.ts" }, { kind: "node", path: "scripts\\diagnostics\\example.test.mjs" }]));
  assert.equal(report.selectedFailures.length, 2);
});

test("browser/native-only failures require review instead of passing a unit selector", () => {
  const report = summarize(plan, results([{ kind: "native", test: "restart" }]));
  assert.equal(report.qualified, true);
  assert.equal(report.status, "requires-review");
  assert.equal(report.exitCode, 2);
  assert.equal(report.misses.length, 0);
});

test("CLI reports finite observations with distinct miss, unqualified and invalid exits", async t => {
  const { mkdtemp, mkdir, writeFile, rm } = await import("node:fs/promises");
  const { spawnSync } = await import("node:child_process");
  const { fileURLToPath } = await import("node:url");
  const path = await import("node:path");
  const root = fileURLToPath(new URL("../../", import.meta.url));
  await mkdir(path.join(root, "tmp"), { recursive: true });
  const directory = await mkdtemp(path.join(root, "tmp/selection-audit-test-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const planFile = path.join(directory, "plan.json");
  const resultFile = path.join(directory, "results.json");
  const script = fileURLToPath(new URL("../validation/selection-audit.mjs", import.meta.url));
  await writeFile(planFile, JSON.stringify(plan));
  const execute = async (value, json = true) => {
    await writeFile(resultFile, JSON.stringify(value));
    return spawnSync(process.execPath, [script, "--plan", planFile, "--results", resultFile, ...json ? ["--json"] : []], { encoding: "utf8" });
  };
  const empty = await execute(results([]));
  assert.equal(empty.status, 0, empty.stderr);
  assert.equal(JSON.parse(empty.stdout).status, "no-misses-observed");
  const missed = await execute(results([{ kind: "rust", package: "mesh-talk-core", test: "other::regression" }]), false);
  assert.equal(missed.status, 1, missed.stderr);
  assert.match(missed.stdout, /First missed regression/);
  const incomplete = await execute({ ...results([]), complete: false });
  assert.equal(incomplete.status, 2, incomplete.stderr);
  assert.equal(JSON.parse(incomplete.stdout).misses, null);
  const invalid = await execute({ source, complete: true });
  assert.equal(invalid.status, 2);
  assert.match(invalid.stderr, /failures must be an explicit array/);
});

test("blocked plans cannot qualify even complete clean-source observations", () => {
  const report = summarize({ ...plan, blockers: ["Unmapped change needs an owner"] }, results([]));
  assert.equal(report.qualified, false);
  assert.equal(report.exitCode, 2);
  assert.equal(report.misses, null);
  assert.ok(report.reasons.includes("Affected plan is blocked"));
  assert.equal(summarize({ ...plan, blockers: [] }, results([])).qualified, true);
});

test("malformed blocker lists are invalid rather than ignored", () => {
  for (const blockers of [null, "blocked", {}, [true], [null], [{}]]) {
    assert.throws(() => summarize({ ...plan, blockers }, results([])), /Invalid plan blockers/);
  }
});

test("Rust integration targets require scenario review even when their name matches a library filter", () => {
  const failure = { kind: "rust", package: "mesh-talk-core", test: "node::account_sync::regression", target: "integration:two_node_cli", path: "crates/mesh-talk-core/tests/two_node_cli.rs" };
  const report = summarize(plan, results([failure]));
  assert.equal(report.status, "requires-review");
  assert.equal(report.exitCode, 2);
  assert.equal(report.selectedFailures.length, 0);
  assert.equal(report.misses.length, 0);
  assert.deepEqual(report.escalations[0], failure);
  assert.match(report.limitations, /library tests only/i);
  const library = summarize(plan, results([{ ...failure, target: "lib" }]));
  assert.equal(library.selectedFailures.length, 1);
});
