import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, copyFileSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { join, delimiter, resolve } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

test("macOS preflight scopes power assertions to its child and preserves selectors", { skip: process.platform === "win32" }, () => {
  const root = mkdtempSync(join(tmpdir(), "mesh-preflight-awake-"));
  try {
    mkdirSync(join(root, "scripts"));
    mkdirSync(join(root, "bin"));
    copyFileSync(resolve("scripts/pr-preflight.sh"), join(root, "scripts/pr-preflight.sh"));
    writeFileSync(join(root, "bin/uname"), "#!/bin/sh\necho Darwin\n", { mode: 0o755 });
    const trace = join(root, "args.json");
    writeFileSync(join(root, "bin/caffeinate"), `#!${process.execPath}\nrequire('node:fs').writeFileSync(process.env.TRACE,JSON.stringify(process.argv.slice(2)));process.exit(17);\n`, { mode: 0o755 });
    const args = ["--core-filter", "relay_", "e2e/settings-categories.spec.ts"];
    const result = spawnSync("bash", [join(root, "scripts/pr-preflight.sh"), ...args], { cwd: root, encoding: "utf8", env: { ...process.env, MESH_PR_KEEP_AWAKE: "0", TRACE: trace, PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}` } });
    assert.equal(result.status, 17, result.stderr);
    assert.deepEqual(JSON.parse(readFileSync(trace, "utf8")), ["-is", "env", "MESH_PR_KEEP_AWAKE=1", join(root, "scripts/pr-preflight.sh"), ...args]);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("macOS preparation keeps the assertion through full health, not only platform preflight", { skip: process.platform === "win32" }, t => {
  const root = mkdtempSync(join(tmpdir(), "mesh-prepare-awake-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "scripts")); mkdirSync(join(root, "bin"));
  copyFileSync(resolve("scripts/prepare-pr.sh"), join(root, "scripts/prepare-pr.sh"));
  writeFileSync(join(root, "bin/uname"), "#!/bin/sh\necho Darwin\n", { mode: 0o755 });
  const trace = join(root, "args.json");
  writeFileSync(join(root, "bin/caffeinate"), `#!${process.execPath}\nrequire('node:fs').writeFileSync(process.env.TRACE,JSON.stringify(process.argv.slice(2)));process.exit(17);\n`, { mode: 0o755 });
  const args = ["--core-filter", "file_consent_", "e2e/chat.spec.ts"];
  const result = spawnSync("bash", [join(root, "scripts/prepare-pr.sh"), ...args], { cwd: root, encoding: "utf8", env: { ...process.env, MESH_PR_KEEP_AWAKE: "0", TRACE: trace, PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}` } });
  assert.equal(result.status, 17, result.stderr);
  assert.deepEqual(JSON.parse(readFileSync(trace, "utf8")), ["-is", "env", "MESH_PR_KEEP_AWAKE=1", "bash", join(root, "scripts/prepare-pr.sh"), ...args]);
});
