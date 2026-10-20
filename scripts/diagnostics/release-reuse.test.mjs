import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { delimiter, resolve as resolvePath } from "node:path";

const repo = "owner/mesh-talk";
const sha = "a".repeat(40);
const tag = "v0.4.1";
const workflow = ".github/workflows/release.yml";
const run = (id, overrides = {}) => ({
  id, run_attempt: 1, head_sha: sha, head_branch: tag, path: workflow,
  repository: { full_name: repo }, head_repository: { full_name: repo },
  status: "completed", conclusion: "success", run_started_at: `2026-10-${id}T00:00:00Z`,
  ...overrides,
});
const jobs = () => [
  { name: "build-release (ubuntu-latest, default)", status: "completed", conclusion: "success" },
  { name: "verify-release", status: "completed", conclusion: "success", steps: [
    { name: "Verify actual draft release assets", status: "completed", conclusion: "success" },
  ] },
];
const artifact = () => ({ id: 77, name: "verified-release-evidence", expired: false, size_in_bytes: 15 });

async function resolve(runs = [run(10)], options = {}) {
  const { resolveVerifiedBuild } = await import("../release/verified-build.mjs");
  const calls = [];
  const read = async (endpoint) => {
    calls.push(endpoint);
    if (endpoint.includes("/workflows/")) return [{ workflow_runs: runs.slice(0, 1) }, { workflow_runs: runs.slice(1) }];
    const id = Number(endpoint.match(/\/runs\/(\d+)/)?.[1]);
    if (endpoint.includes("/jobs")) return [{ jobs: options.jobs?.[id] ?? jobs() }];
    if (endpoint.includes("/artifacts")) return [{ artifacts: options.artifacts?.[id] ?? [artifact()] }];
    throw new Error(`unexpected endpoint ${endpoint}`);
  };
  return { id: await resolveVerifiedBuild({ repo, sha, tag, currentRunId: 99, read, ...options.input }), calls };
}

test("reuses the exact successful tag build and its immutable verification evidence", async () => {
  const result = await resolve();
  assert.equal(result.id, 10);
  assert.ok(result.calls.some((path) => path.includes(`head_sha=${sha}`)));
  assert.ok(result.calls.some((path) => path.includes("runs/10/attempts/1/jobs")));
});

test("ignores the current publication and previous explicitly skipped publication builds", async () => {
  assert.equal((await resolve([run(99), run(11), run(10)], { jobs: { 11: [{ name: "build-release", conclusion: "skipped", status: "completed" }] } })).id, 10);
});

test("ranks reruns by attempt start instead of original run ID", async () => {
  assert.equal((await resolve([run(11), run(10, { run_attempt: 2, run_started_at: "2026-10-12T00:00:00Z" })])).id, 10);
});

for (const [name, overrides] of Object.entries({
  repository: { repository: { full_name: "other/repo" } },
  fork: { head_repository: { full_name: "fork/repo" } },
  source: { head_sha: "b".repeat(40) },
  tag: { head_branch: "main" },
  workflow: { path: ".github/workflows/other.yml" },
  "workflow failure": { conclusion: "failure" },
  "incomplete workflow": { status: "in_progress", conclusion: null },
  "invalid attempt": { run_attempt: 0 },
  "invalid ID": { id: "10\nrun_id=99" },
  "missing attempt time": { run_started_at: undefined },
})) {
  test(`rejects ${name} rather than falling back to older success`, async () => {
    await assert.rejects(resolve([run(11, overrides), run(10)]));
  });
}

for (const [name, list] of Object.entries({
  "missing build jobs": [],
  "failed build": [{ ...jobs()[0], conclusion: "failure" }, jobs()[1]],
  "missing verification": [jobs()[0]],
  "failed verification": [jobs()[0], { ...jobs()[1], conclusion: "failure" }],
  "missing real release step": [jobs()[0], { ...jobs()[1], steps: [] }],
  "skipped real release step": [jobs()[0], { ...jobs()[1], steps: [{ ...jobs()[1].steps[0], conclusion: "skipped" }] }],
  "duplicate verification": [...jobs(), jobs()[1]],
})) {
  test(`rejects ${name}`, async () => {
    await assert.rejects(resolve([run(11), run(10)], { jobs: { 11: list } }));
  });
}

for (const [name, list] of Object.entries({
  missing: [], expired: [{ ...artifact(), expired: true }], empty: [{ ...artifact(), size_in_bytes: 0 }],
  ambiguous: [artifact(), { ...artifact(), id: 78 }],
})) {
  test(`rejects ${name} evidence artifacts`, async () => {
    await assert.rejects(resolve([run(11), run(10)], { artifacts: { 11: list } }));
  });
}

test("fails closed on absent candidates and API errors", async () => {
  await assert.rejects(resolve([]));
  const { resolveVerifiedBuild } = await import("../release/verified-build.mjs");
  await assert.rejects(resolveVerifiedBuild({ repo, sha, tag, read: async () => { throw new Error("permission denied"); } }), /permission denied/);
});


test("reads all job and artifact pages before deciding verification is absent or ambiguous", async () => {
  const { resolveVerifiedBuild } = await import("../release/verified-build.mjs");
  const read = async (endpoint) => {
    if (endpoint.includes("/workflows/")) return [{ workflow_runs: [run(10)] }];
    if (endpoint.includes("/jobs")) return [{ jobs: [jobs()[0]] }, { jobs: [jobs()[1]] }];
    return [{ artifacts: [] }, { artifacts: [artifact()] }];
  };
  assert.equal(await resolveVerifiedBuild({ repo, sha, tag, read }), 10);
  await assert.rejects(resolveVerifiedBuild({ repo, sha, tag, read: async (endpoint) => endpoint.includes("/artifacts")
    ? [{ artifacts: [artifact()] }, { artifacts: [{ ...artifact(), id: 78 }] }] : read(endpoint) }));
});

test("rejects malformed paginated responses", async () => {
  for (const response of [{ workflow_runs: [] }, [{}], [null]]) {
    await assert.rejects(resolve([], { input: { read: async () => response } }));
  }
});

test("CLI appends only a numeric run output and hides API failure details", () => {
  mkdirSync("tmp", { recursive: true });
  const directory = mkdtempSync(resolvePath("tmp/release-reuse-"));
  try {
    const gh = `${directory}/gh`;
    const output = `${directory}/output`;
    const reply = `
if (process.env.FIXTURE_FAIL) { console.error("sensitive API failure"); process.exit(1); }
const endpoint = process.argv.at(-1);
const result = endpoint.includes("/workflows/") ? [{ workflow_runs: ${JSON.stringify([run(10)])} }]
  : endpoint.includes("/jobs") ? [{ jobs: ${JSON.stringify(jobs())} }]
  : [{ artifacts: ${JSON.stringify([artifact()])} }];
console.log(JSON.stringify(result));
`;
    const fixtureEnv = {};
    if (process.platform === "win32") {
      // execFileSync cannot launch a shebang or .cmd stub on Windows. Use a real
      // executable with a preload that replies only inside the fake gh process.
      copyFileSync(process.execPath, `${gh}.exe`);
      const preload = `${directory}/reply.cjs`;
      writeFileSync(preload, `if (process.execPath.toLowerCase().endsWith('gh.exe')) { ${reply}\nprocess.exit(0); }`);
      fixtureEnv.NODE_OPTIONS = `${process.env.NODE_OPTIONS ?? ""} --require "${preload}"`;
    } else {
      writeFileSync(gh, `#!/usr/bin/env node\n${reply}`);
      chmodSync(gh, 0o755);
    }
    writeFileSync(output, "existing=value\n");
    const env = { ...process.env, ...fixtureEnv, PATH: `${directory}${delimiter}${process.env.PATH}`, SOURCE_REPO: repo,
      SOURCE_SHA: sha, RELEASE_TAG: tag, GITHUB_RUN_ID: "99", GITHUB_OUTPUT: output };
    assert.equal(execFileSync(process.execPath, ["scripts/release/verified-build.mjs"], { env, encoding: "utf8" }), "");
    assert.equal(readFileSync(output, "utf8"), "existing=value\nrun_id=10\n");
    assert.throws(() => execFileSync(process.execPath, ["scripts/release/verified-build.mjs"], {
      env: { ...env, FIXTURE_FAIL: "1" }, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"],
    }), (error) => {
      assert.doesNotMatch(error.stderr, /sensitive/);
      assert.equal(readFileSync(output, "utf8"), "existing=value\nrun_id=10\n");
      return true;
    });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
