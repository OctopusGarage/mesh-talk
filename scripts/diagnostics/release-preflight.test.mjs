import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { rustPackages } from "../pr-platform/rust-packages.mjs";
import { readVersion } from "../release/version.mjs";
import { releaseInputs } from "../release/preflight.mjs";
import { requireSourceChecks, waitForSourceChecks } from "../release/check-source-checks.mjs";

const inputs = { ref: "refs/tags/v0.1.6", runId: "123", version: "0.1.6", desktopVersion: "0.1.6", existingRelease: null };

test("preflight enforces matching versions and never overwrites public releases", () => {
  assert.deepEqual(releaseInputs(inputs), { version: "0.1.6", tag: "v0.1.6" });
  assert.throws(() => releaseInputs({ ...inputs, desktopVersion: "0.1.5" }), /version/);
  assert.throws(() => releaseInputs({ ...inputs, ref: "refs/tags/v0.1.5" }), /version/);
  assert.throws(() => releaseInputs({ ...inputs, existingRelease: { draft: false } }), /published/);
  assert.throws(() => releaseInputs({ ...inputs, existingRelease: {} }), /published/);
  assert.deepEqual(releaseInputs({ ...inputs, existingRelease: { draft: true } }), { version: "0.1.6", tag: "v0.1.6" });
});

test("branch dry runs use safe unique artifact names, not slash-containing branch names", () => {
  assert.deepEqual(releaseInputs({ ...inputs, ref: "refs/heads/chore/quality-gates" }), { version: "0.1.6", tag: "v0.1.6-ci.123" });
  assert.throws(() => releaseInputs({ ...inputs, ref: "refs/pull/1/merge" }), /ref/);
  assert.throws(() => releaseInputs({ ...inputs, runId: "../../bad" }), /run/);
  assert.throws(() => releaseInputs({ ...inputs, ref: "refs/heads/dev", publishRelease: true }), /publication/);
});

test("tag preflight finds a published release even when GitHub's tag endpoint returns 404", { skip: process.platform === "win32" }, (t) => {
  const currentVersion = readVersion();
  const root = mkdtempSync(join(tmpdir(), "mesh-talk-release-preflight-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "bin"));
  writeFileSync(join(root, "bin", "gh"), `#!${process.execPath}\nconst args = process.argv.slice(2); if (args.some(arg => arg.includes('/compare/'))) process.stdout.write(JSON.stringify({status:'identical'})); else if (args.some(arg => arg.includes('/releases/tags/'))) { console.error('gh: Not Found (HTTP 404)'); process.exit(1); } else process.stdout.write(require('node:child_process').execFileSync('jq', ['-cr', args[args.indexOf('--jq') + 1]], {input:JSON.stringify([{id:42,tag_name:'v${currentVersion}',draft:false}]),encoding:'utf8'}));\n`, { mode: 0o755 });
  const output = join(root, "output.txt");
  const result = spawnSync(process.execPath, [resolve("scripts/release/preflight.mjs")], {
    cwd: resolve("."), encoding: "utf8", env: { ...process.env, PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}`, GITHUB_REF: `refs/tags/v${currentVersion}`, GITHUB_RUN_ID: "123", GITHUB_REPOSITORY: "owner/repo", GITHUB_SHA: "a".repeat(40), GITHUB_OUTPUT: output },
  });
  assert.notEqual(result.status, 0, result.stdout);
  assert.match(result.stderr, /published/);
  assert.equal(existsSync(output), false);
});

const names = ["verify", "Playwright UI E2E", "Multi-process E2E", "Code Quality and Health Check (macos-latest)", "Code Quality and Health Check (windows-latest)", "scan", "Scorecard analysis", "Analyze (rust)", "Analyze (actions)", "Analyze (javascript-typescript)"];
const checks = names.map((name, id) => ({ name, id, app: { slug: "github-actions" }, status: "completed", conclusion: "success" }));

test("publication requires successful current-source CI and all CodeQL languages", () => {
  requireSourceChecks(checks, []);
  for (const missing of names) assert.throws(() => requireSourceChecks(checks.filter((check) => check.name !== missing), []), /check/);
  for (const conclusion of ["failure", "cancelled", "skipped", "neutral", null]) assert.throws(() => requireSourceChecks([{ ...checks[0], conclusion }, ...checks.slice(1)], []), /check/);
  assert.throws(() => requireSourceChecks([{ ...checks[0], status: "in_progress" }, ...checks.slice(1)], []), /check/);
  assert.throws(() => requireSourceChecks(checks, [{ number: 1 }]), /alerts/);
});

test("a stale success or third-party check cannot mask a newer failed run", () => {
  assert.throws(() => requireSourceChecks([...checks, { ...checks[0], id: 100, conclusion: "failure" }], []), /check/);
  assert.throws(() => requireSourceChecks([{ ...checks[0], app: { slug: "other" } }, ...checks.slice(1)], []), /check/);
});


test("Nightly mode requires main, positive immutable run identity and separate publication intent", () => {
  const nightly = { ...inputs, ref: "refs/heads/main", nightly: true, runAttempt: "2" };
  assert.deepEqual(releaseInputs(nightly), { version: "0.1.6", tag: "v0.1.6-nightly.123.2" });
  for (const change of [{ ref: "refs/heads/feature" }, { ref: "refs/tags/v0.1.6" }, { runId: "0" }, { runAttempt: "0" }, { runAttempt: "01" }, { publishRelease: true }, { existingRelease: { draft: false } }]) {
    assert.throws(() => releaseInputs({ ...nightly, ...change }));
  }
  assert.throws(() => releaseInputs({ ...inputs, ref: "refs/tags/v0.1.6-nightly.123.2" }), /version/);
});

test("scheduled Nightly skips only a published Nightly at the exact source", { skip: process.platform === "win32" }, (t) => {
  const root = mkdtempSync(join(tmpdir(), "mesh-nightly-preflight-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "bin"));
  writeFileSync(join(root, "bin", "gh"), `#!${process.execPath}\nconst args = process.argv.slice(2); process.stdout.write(require('node:child_process').execFileSync('jq', ['-cr', args[args.indexOf('--jq') + 1]], {input:process.env.RELEASES,encoding:'utf8'}));\n`, { mode: 0o755 });
  const output = join(root, "output.txt");
  const sha = "a".repeat(40);
  const published = { tag_name: "v0.1.6-nightly.122.1", draft: false, prerelease: true, target_commitish: sha };
  const run = (changes = {}, event = "schedule", stable = null) => {
    const result = spawnSync(process.execPath, [resolve("scripts/release/preflight.mjs")], { cwd: resolve("."), encoding: "utf8", env: { ...process.env, PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}`, GITHUB_REF: "refs/heads/main", GITHUB_RUN_ID: "123", GITHUB_RUN_ATTEMPT: "1", GITHUB_REPOSITORY: "owner/repo", GITHUB_SHA: sha, GITHUB_EVENT_NAME: event, GITHUB_OUTPUT: output, RELEASE_NIGHTLY: "true", RELEASE_PUBLICATION: "false", RELEASES: JSON.stringify([{ ...published, ...changes }, ...(stable ? [{ tag_name: `v${stable}`, draft: false, prerelease: false }] : [])]) } });
    if (stable) {
      assert.notEqual(result.status, 0, result.stdout);
      assert.equal(existsSync(output), false, "a rejected target must not emit build outputs");
      return result.stderr;
    }
    assert.equal(result.status, 0, result.stderr);
    const value = readFileSync(output, "utf8");
    rmSync(output);
    return value;
  };
  assert.match(run({}, "schedule", readVersion()), /Nightly target must be newer/);
  assert.match(run(), /skip=true/);
  for (const change of [{ draft: true }, { prerelease: false }, { target_commitish: "b".repeat(40) }, { tag_name: "v0.1.6-beta.1" }]) assert.match(run(change), /skip=false/);
  assert.match(run({}, "workflow_dispatch"), /skip=false/, "manual builds intentionally allow retrying the same source");
});

test("Tauri inherits the canonical target when its config omits version", () => {
  const { desktopVersion: ignored, ...inherited } = inputs;
  assert.deepEqual(releaseInputs(inherited), { version: "0.1.6", tag: "v0.1.6" });
});

test("Nightly targets must sort after the latest published stable", () => {
  const nightly = { ...inputs, ref: "refs/heads/main", nightly: true, runAttempt: "1" };
  assert.throws(() => releaseInputs({ ...nightly, latestStableTag: "v0.1.6" }), /Nightly target/);
  assert.throws(() => releaseInputs({ ...nightly, latestStableTag: "v0.2.0" }), /Nightly target/);
  assert.equal(releaseInputs({ ...nightly, latestStableTag: "v0.1.5" }).tag, "v0.1.6-nightly.123.1");
  assert.equal(releaseInputs({ ...nightly, latestStableTag: null }).version, "0.1.6", "first release may have no published stable yet");
});

test(
  "Stable preflight refuses missing curated notes before querying remote state or emitting build outputs",
  { skip: process.platform === "win32" },
  (t) => {
    const root = mkdtempSync(join(tmpdir(), "mesh-curated-preflight-"));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    for (const file of [
      "Cargo.toml",
      "Cargo.lock",
      "package.json",
      "package-lock.json",
      "frontend/package.json",
      "frontend/package-lock.json",
      "src-tauri/tauri.conf.json",
      ...rustPackages.map((p) => `${p.directory}/Cargo.toml`),
    ]) {
      mkdirSync(dirname(join(root, file)), { recursive: true });
      writeFileSync(join(root, file), readFileSync(resolve(file)));
    }
    writeFileSync(
      join(root, "CHANGELOG.md"),
      "# Changelog\n\n## 0.0.1\n\n- Old version.\n",
    );
    mkdirSync(join(root, "bin"));
    const trace = join(root, "remote-called");
    writeFileSync(
      join(root, "bin", "gh"),
      `#!${process.execPath}\nconst fs=require('node:fs');fs.appendFileSync(process.env.TRACE,'called\\n');if(process.argv.some(a=>a.includes('/compare/')))process.stdout.write(JSON.stringify({status:'identical'}));\n`,
      { mode: 0o755 },
    );
    const output = join(root, "output.txt");
    const result = spawnSync(
      process.execPath,
      [resolve("scripts/release/preflight.mjs")],
      {
        cwd: root,
        encoding: "utf8",
        env: {
          ...process.env,
          PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}`,
          TRACE: trace,
          GITHUB_REF: `refs/tags/v${readVersion()}`,
          GITHUB_RUN_ID: "123",
          GITHUB_RUN_ATTEMPT: "1",
          GITHUB_REPOSITORY: "owner/repo",
          GITHUB_SHA: "a".repeat(40),
          GITHUB_OUTPUT: output,
          RELEASE_NIGHTLY: "false",
          RELEASE_PUBLICATION: "false",
        },
      },
    );
    assert.notEqual(result.status, 0, result.stdout);
    assert.match(result.stderr, /missing changelog section/);
    assert.equal(
      existsSync(trace),
      false,
      "invalid notes must stop before remote lookups",
    );
    assert.equal(
      existsSync(output),
      false,
      "invalid notes must stop before matrix outputs",
    );
  },
);

function checkWait(states, { timeoutMs = 20, intervalMs = 10 } = {}) {
  let elapsed = 0;
  let reads = 0;
  const sleeps = [];
  const messages = [];
  const result = waitForSourceChecks(() => {
    const state = states[Math.min(reads++, states.length - 1)];
    if (state instanceof Error) throw state;
    return state;
  }, { timeoutMs, intervalMs, now: () => elapsed, sleep: async (ms) => { sleeps.push(ms); elapsed += ms; }, log: (message) => messages.push(message) });
  return { result, sleeps, messages, reads: () => reads };
}

const successState = { checks, openAlerts: [] };
const pendingState = { ...successState, checks: [{ ...checks[0], status: "in_progress", conclusion: null }, ...checks.slice(1)] };

test("Nightly waits for pending and missing exact-source checks to become successful", async () => {
  for (const initial of [pendingState, { ...successState, checks: checks.slice(1) }]) {
    const run = checkWait([initial, successState]);
    await run.result;
    assert.equal(run.reads(), 2);
    assert.deepEqual(run.sleeps, [10]);
    assert.match(run.messages.join("\n"), /verify/);
  }
  const ready = checkWait([successState]);
  await ready.result;
  assert.deepEqual(ready.sleeps, []);
});

test("Nightly waits for the newest check instead of accepting a stale success", async () => {
  const run = checkWait([{ ...successState, checks: [...checks, { ...pendingState.checks[0], id: 100 }] }, successState]);
  await run.result;
  assert.equal(run.reads(), 2);
});

test("Nightly immediately rejects unsuccessful completed checks, including when another check is pending", async () => {
  for (const conclusion of ["failure", "cancelled", "skipped", "neutral", "timed_out", null]) {
    const run = checkWait([{ ...pendingState, checks: [...pendingState.checks, { ...checks[1], id: 100, conclusion }] }, successState]);
    await assert.rejects(run.result, /required source check not successful: Playwright UI E2E/);
    assert.equal(run.reads(), 1);
    assert.deepEqual(run.sleeps, []);
  }
});

test("Nightly never retries scanning alerts or API errors and refreshes alerts while waiting", async () => {
  for (const states of [
    [{ ...pendingState, openAlerts: [{ number: 1 }] }],
    [pendingState, { ...successState, openAlerts: [{ number: 1 }] }],
    [new Error("GitHub API unavailable")],
  ]) {
    const run = checkWait(states);
    await assert.rejects(run.result, /alerts|GitHub API unavailable/);
    assert.equal(run.reads(), states.length);
  }
});

test("Nightly waiting expires with the pending names and caps the last sleep to the deadline", async () => {
  const run = checkWait([pendingState], { timeoutMs: 15, intervalMs: 10 });
  await assert.rejects(run.result, /Timed out.*verify/);
  assert.deepEqual(run.sleeps, [10, 5]);
  assert.equal(run.reads(), 3);
});
