import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import { releaseInputs } from "../release/preflight.mjs";
import { requireSourceChecks } from "../release/check-source-checks.mjs";

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
  const currentVersion = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8")).version;
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
  const run = (changes = {}, event = "schedule") => {
    const result = spawnSync(process.execPath, [resolve("scripts/release/preflight.mjs")], { cwd: resolve("."), encoding: "utf8", env: { ...process.env, PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}`, GITHUB_REF: "refs/heads/main", GITHUB_RUN_ID: "123", GITHUB_RUN_ATTEMPT: "1", GITHUB_REPOSITORY: "owner/repo", GITHUB_SHA: sha, GITHUB_EVENT_NAME: event, GITHUB_OUTPUT: output, RELEASE_NIGHTLY: "true", RELEASE_PUBLICATION: "false", RELEASES: JSON.stringify([{ ...published, ...changes }]) } });
    assert.equal(result.status, 0, result.stderr);
    const value = readFileSync(output, "utf8");
    rmSync(output);
    return value;
  };
  assert.match(run(), /skip=true/);
  for (const change of [{ draft: true }, { prerelease: false }, { target_commitish: "b".repeat(40) }, { tag_name: "v0.1.6-beta.1" }]) assert.match(run(change), /skip=false/);
  assert.match(run({}, "workflow_dispatch"), /skip=false/, "manual builds intentionally allow retrying the same source");
});
