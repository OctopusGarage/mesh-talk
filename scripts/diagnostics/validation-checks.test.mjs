import assert from "node:assert/strict";
import test from "node:test";
import { buildStaticChecks, ciPolicyTest, ciDocsPolicyTest, pendingNodeTests, phasePlan } from "../validation/checks.mjs";
import { executeCheapChecks, executePlan, parseOptions } from "../validation/changed.mjs";

const plan = overrides => ({ files: [], rust: [], frontend: [], node: [], blockers: [], ...overrides });
test("test-only mode remains available and checks are explicitly requested", () => {
  assert.equal(parseOptions([]).checks, false);
  assert.equal(parseOptions(["--checks"]).checks, true);
  assert.equal(parseOptions(["--checks"]).run, false);
  assert.equal(parseOptions(["--managed"]).managed, true);
  assert.equal(parseOptions(["--cheap"]).checks, true);
  assert.equal(parseOptions(["--cheap"]).run, false);
  assert.throws(() => parseOptions(["--cheap", "--run"]), /do not combine/);
});
test("documentation-only checks do not start compilers or browsers", () => {
  assert.deepEqual(buildStaticChecks(plan({ files: ["docs/ARCHITECTURE.md"] })), []);
});
test("workflow and shell edits add their existing static checks but removed scripts are not executed", () => {
  assert.deepEqual(buildStaticChecks(plan({ files: ["scripts/removed.sh"], deleted: ["scripts/removed.sh"] })), []);
  const checks = buildStaticChecks(plan({ files: [".github/workflows/ci.yml", "scripts/changed.sh"] }));
  assert.deepEqual(checks.map(check => check.name), ["changed-syntax", "ci-policy", "ci-docs-policy", "shell-lint"]);
  assert.deepEqual(checks[0].command, ["node", "scripts/validation/syntax.mjs", "--files", ".github/workflows/ci.yml", "scripts/changed.sh"]);
  assert.deepEqual(checks[1].command, ["node", "--test", "scripts/diagnostics/ci-latency.test.mjs"]);
  assert.equal(checks[1].kind, "policy");
  assert.deepEqual(checks[2].command, ["node", "--test", ciDocsPolicyTest]);
  assert.deepEqual(checks[3].command, ["shellcheck", "--severity=warning", "--", "scripts/changed.sh"]);
});
test("cheap workflow policy failures reject publication without admitting application suites", async () => {
  const selected = plan({ files: [".github/workflows/ci.yml"], node: ["never-run.test.mjs"] });
  selected.checks = buildStaticChecks(selected);
  const calls = [];
  await assert.rejects(executeCheapChecks(selected, { log() {}, run: async (_program, args) => {
    calls.push(args[1]);
    return args[1] === "ci-policy" ? { code: 1, output: "stale workflow policy" } : { code: 0, output: "syntax pass" };
  } }), /stale workflow policy/);
  assert.deepEqual(calls, ["changed-syntax", "ci-policy"]);
  const removed = buildStaticChecks(plan({ files: [".github/workflows/removed.yml"], deleted: [".github/workflows/removed.yml"] }));
  assert.deepEqual(removed.map(check => check.name), ["ci-policy", "ci-docs-policy"]);
});
test("policy checks execute once while test-only plans retain the same assertions", async () => {
  const selected = plan({ files: [".github/workflows/ci.yml"], node: [ciPolicyTest] });
  assert.deepEqual(pendingNodeTests(selected), [ciPolicyTest]);
  selected.checks = buildStaticChecks(selected);
  assert.deepEqual(pendingNodeTests(selected), []);
  for (const replacement of [
    { name: "ci-policy", kind: "policy", cwd: ".", command: ["node", "--test", ciPolicyTest, "--test-name-pattern=partial"] },
    { name: "ci-policy", kind: "policy", cwd: "frontend", command: ["node", "--test", ciPolicyTest] },
    { name: "ci-policy", kind: "policy", resource: "heavy", cwd: ".", command: ["node", "--test", ciPolicyTest] },
    { name: "ci-policy", cwd: ".", command: ["node", "--test", ciPolicyTest] },
  ]) assert.deepEqual(pendingNodeTests({ ...selected, checks: [replacement] }), [ciPolicyTest], "a name alone cannot remove a test obligation");
  assert.deepEqual(phasePlan(selected, "tests").node, []);
  assert.deepEqual(selected.node, [ciPolicyTest], "selection remains visible in the original plan");
  const calls = [];
  await executePlan(selected, { log() {}, run: async (_program, args) => {
    calls.push(args);
    return { code: 0, output: "policy pass" };
  } });
  assert.deepEqual(calls.map(args => args[1]), ["changed-syntax", "ci-policy", "ci-docs-policy"]);
});
test("scope source, composite actions and scope tests run policy locally exactly once", () => {
  for (const file of ["scripts/ci/docs-scope.mjs", ciDocsPolicyTest, ".github/actions/ci-scope/action.yml"]) {
    const selected = plan({ files: [file], node: [ciPolicyTest, ciDocsPolicyTest] });
    selected.checks = buildStaticChecks(selected);
    assert.ok(selected.checks.some(check => check.name === "ci-docs-policy"));
    assert.deepEqual(pendingNodeTests(selected), []);
    for (const altered of [
      { name: "ci-docs-policy", kind: "policy", cwd: "frontend", command: ["node", "--test", ciDocsPolicyTest] },
      { name: "ci-docs-policy", kind: "policy", cwd: ".", command: ["node", "--test", ciDocsPolicyTest, "--test-name-pattern=partial"] },
      { name: "ci-docs-policy", cwd: ".", command: ["node", "--test", ciDocsPolicyTest] },
    ]) {
      const checks = selected.checks.map(check => check.name === "ci-docs-policy" ? altered : check);
      assert.deepEqual(pendingNodeTests({ ...selected, checks }), [ciDocsPolicyTest]);
    }
  }
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
  assert.deepEqual(checks.find(check => check.name === "frontend-format").command, ["npm", "run", "format:check"]);
  assert(!checks.some(check => check.command[0] === "npx"), "cheap checks must not auto-install missing tools");
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
  assert.deepEqual(checks.map(check => check.command), [["node", "scripts/validation/syntax.mjs", "--files", "scripts/build-retention.py"], ["python3", "-m", "unittest", "discover", "-s", "scripts", "-p", "test_build_retention.py", "-v"]]);
  assert.deepEqual(phasePlan(plan({ checks }), "static").checks, checks);
});
