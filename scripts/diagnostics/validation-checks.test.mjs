import assert from "node:assert/strict";
import test from "node:test";
import { buildStaticChecks, phasePlan } from "../validation/checks.mjs";
import { executePlan, parseOptions } from "../validation/changed.mjs";

const plan = overrides => ({ files: [], rust: [], frontend: [], node: [], blockers: [], ...overrides });
test("test-only mode remains available and checks are explicitly requested", () => {
  assert.equal(parseOptions([]).checks, false);
  assert.equal(parseOptions(["--checks"]).checks, true);
  assert.equal(parseOptions(["--checks"]).run, false);
  assert.equal(parseOptions(["--managed"]).managed, true);
});
test("documentation-only checks do not start compilers or browsers", () => {
  assert.deepEqual(buildStaticChecks(plan({ files: ["docs/ARCHITECTURE.md"] })), []);
});
test("workflow and shell edits add their existing static checks but removed scripts are not executed", () => {
  assert.deepEqual(buildStaticChecks(plan({ files: ["scripts/removed.sh"], deleted: ["scripts/removed.sh"] })), []);
  const checks = buildStaticChecks(plan({ files: [".github/workflows/ci.yml", "scripts/changed.sh"] }));
  assert.deepEqual(checks.map(check => check.name), ["workflow-lint", "shell-lint"]);
  assert.deepEqual(checks[1].command, ["shellcheck", "--severity=warning", "--", "scripts/changed.sh"]);
});
test("Rust static checks lint actual affected packages with CI target coverage", () => {
  const checks = buildStaticChecks(plan({ rust: [{ package: "mesh-talk-core" }] }));
  assert.deepEqual(checks.map(check => check.name), ["rust-format", "rust-clippy"]);
  assert.deepEqual(checks[1].command, ["cargo", "clippy", "--locked", "-p", "mesh-talk-core", "--all-targets", "--", "-D", "warnings"]);
});
test("desktop checks prepare frontend assets before Tauri compilation", () => {
  const checks = buildStaticChecks(plan({ rust: [{ package: "mesh-talk" }] }));
  assert(checks.findIndex(check => check.name === "frontend-build") < checks.findIndex(check => check.name === "rust-clippy"));
  assert.equal(checks.find(check => check.name === "frontend-build").cwd, "frontend");
  assert.deepEqual(checks.find(check => check.name === "frontend-build").artifacts, ["dist"]);
});

test("build stages forward required output artifacts to the evidence runner", async () => {
  const calls = [];
  await executePlan(plan({ checks: [{ name: "frontend-build", cwd: "frontend", command: ["npm", "run", "build"], inputs: "frontend", artifacts: ["dist"] }] }), {
    run: async (program, args) => { calls.push(args); return { code: 0, output: "built" }; }, log: () => {},
  });
  assert.deepEqual(calls[0].slice(calls[0].indexOf("--artifact"), calls[0].indexOf("--artifact") + 2), ["--artifact", "dist"]);
});
test("frontend validation composes formatting, lint, source/E2E types and production build", () => {
  const checks = buildStaticChecks(plan({ files: ["frontend/src/store/chat.ts"] }));
  assert.deepEqual(checks.map(check => check.name), ["frontend-format", "frontend-lint", "frontend-types", "frontend-build"]);
  assert(checks.every(check => check.inputs === "frontend"));
  assert(!JSON.stringify(checks).includes("playwright"));
});
test("static failures stop test compilation and execution", async () => {
  const calls = [];
  await assert.rejects(executePlan(plan({
    checks: [{ name: "frontend-types", cwd: "frontend", command: ["npm", "run", "typecheck"], inputs: "frontend" }],
    rust: [{ package: "core", filters: ["node::"] }],
  }), { run: async (program, args) => { calls.push([program, args]); return { code: 1, output: "type failure" }; }, log: () => {} }), /type failure/);
  assert.equal(calls.length, 1);
  assert(calls[0][1].includes("frontend-types"));
});

test("cheap static feedback runs separately from heavy builds and test execution", async () => {
  const original = plan({ files: ["frontend/src/store/chat.ts"], node: ["scripts/a.test.mjs"] });
  original.checks = buildStaticChecks(original);
  const fast = phasePlan(original, "static");
  const heavy = phasePlan(original, "tests");
  assert.deepEqual(fast.checks.map(check => check.name), ["frontend-format", "frontend-lint", "frontend-types"]);
  assert.deepEqual(fast.node, []);
  assert.deepEqual(fast.frontend, []);
  assert.deepEqual(fast.rust, []);
  assert.deepEqual(heavy.checks.map(check => check.name), ["frontend-build"]);
  assert.deepEqual(heavy.node, original.node);
  assert.deepEqual([...fast.checks, ...heavy.checks], original.checks);
  assert.throws(() => phasePlan(original, "unknown"), /phase/);
});


test("retention guard edits run its real subprocess and filesystem regressions before heavy work", () => {
  const checks = buildStaticChecks(plan({ files: ["scripts/build-retention.py"] }));
  assert.deepEqual(checks.map(check => check.command), [["python3", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_build_retention.py", "-v"]]);
  assert.deepEqual(phasePlan(plan({ checks }), "static").checks, checks);
});
