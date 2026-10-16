import assert from "node:assert/strict";
import test from "node:test";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

test("PR creation requires successful preflight and preserves argument boundaries", { skip: process.platform === "win32" }, t => {
  const entry = resolve("scripts/create-pr.sh");
  assert.ok(existsSync(entry), "PR creation needs a preflight-enforcing entry point");
  const root = mkdtempSync(join(tmpdir(), "mesh-create-pr-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "scripts"));
  mkdirSync(join(root, "scripts/pr-platform"));
  mkdirSync(join(root, "bin"));
  assert.equal(spawnSync("git", ["init", "-q"], { cwd: root }).status, 0);
  writeFileSync(join(root, ".gitignore"), "*\n");
  assert.equal(spawnSync("git", ["-c", "user.name=Preflight Test", "-c", "user.email=preflight@example.test", "commit", "--allow-empty", "-qm", "fixture"], { cwd: root }).status, 0);
  copyFileSync(entry, join(root, "scripts/create-pr.sh"));
  copyFileSync(resolve("scripts/pr-platform/plan.mjs"), join(root, "scripts/pr-platform/plan.mjs"));
  const trace = join(root, "trace.jsonl");
  const recorder = `#!${process.execPath}\nimport('node:fs').then(({appendFileSync}) => {appendFileSync(process.env.TRACE, JSON.stringify({kind:process.argv[1].endsWith('gh')?'gh':'preflight',args:process.argv.slice(2)})+'\\n');process.exit(Number(process.env.PREFLIGHT_STATUS ?? 0));});\n`;
  writeFileSync(join(root, "scripts/pr-preflight.sh"), recorder, { mode: 0o755 });
  writeFileSync(join(root, "bin/gh"), recorder, { mode: 0o755 });
  const args = ["--core-filter", "relay_", "e2e/ui-visual-regression.spec.ts", "--", "--draft", "--title", "literal $(touch unexpected)"];
  const run = (status, options = args, extraEnv = {}) => spawnSync("bash", [join(root, "scripts/create-pr.sh"), ...options], { cwd: root, encoding: "utf8", env: { ...process.env, TRACE: trace, PREFLIGHT_STATUS: String(status), PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}`, ...extraEnv } });
  const failed = run(7);
  assert.equal(failed.status, 7, failed.stderr);
  assert.deepEqual(readFileSync(trace, "utf8").trim().split("\n").map(JSON.parse), [{ kind: "preflight", args: args.slice(0, 3) }]);
  writeFileSync(trace, "");
  const passed = run(0);
  assert.equal(passed.status, 0, passed.stderr);
  assert.deepEqual(readFileSync(trace, "utf8").trim().split("\n").map(JSON.parse), [
    { kind: "preflight", args: args.slice(0, 3) },
    { kind: "gh", args: ["pr", "create", "--base", "main", ...args.slice(4)] },
  ]);
  assert.equal(existsSync(join(root, "unexpected")), false);

  for (const override of [["--head", "other"], ["--head=other"], ["-Hother"], ["--repo", "other/repo"], ["--repo=other/repo"], ["-Rother/repo"], ["--base", "release"], ["--base=release"], ["-Brelease"]]) {
    writeFileSync(trace, "");
    assert.notEqual(run(0, [...args, ...override]).status, 0, "cannot publish another head or repository");
    assert.equal(readFileSync(trace, "utf8"), "", "reject candidate overrides before expensive preflight");
  }
  assert.notEqual(run(0, args, { GH_REPO: "other/repo" }).status, 0);
  assert.equal(readFileSync(trace, "utf8"), "", "environment overrides cannot select an unverified repository");
  assert.notEqual(run(0, ["--base", "HEAD", ...args]).status, 0);
  assert.equal(readFileSync(trace, "utf8"), "", "a local revision cannot masquerade as the PR target branch");
  const releaseArgs = ["--base", "origin/release", ...args];
  assert.equal(run(0, releaseArgs).status, 0);
  const releaseTrace = readFileSync(trace, "utf8").trim().split("\n").map(JSON.parse);
  assert.deepEqual(releaseTrace[0].args, releaseArgs.slice(0, 5));
  assert.deepEqual(releaseTrace[1].args, ["pr", "create", "--base", "release", ...args.slice(4)]);

  writeFileSync(trace, "");
  writeFileSync(join(root, "scripts/pr-preflight.sh"), "#!/usr/bin/env bash\ngit -c user.name=Test -c user.email=preflight@example.test commit --allow-empty -qm changed\n", { mode: 0o755 });
  const changed = run(0);
  assert.notEqual(changed.status, 0, "cannot publish a revision changed during verification");
  assert.equal(readFileSync(trace, "utf8"), "", "gh must not run for an unverified revision");
});
