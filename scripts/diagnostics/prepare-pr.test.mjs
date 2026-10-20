import assert from "node:assert/strict";
import test from "node:test";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  copyFileSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

test(
  "preparation requires full health and platform preflight before certifying a stable candidate",
  { skip: process.platform === "win32" },
  (t) => {
    const root = mkdtempSync(join(tmpdir(), "mesh-prepare-"));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    for (const dir of ["scripts/pr-platform", "scripts/validation"])
      mkdirSync(join(root, dir), { recursive: true });
    const git = (args) =>
      spawnSync("git", args, { cwd: root, encoding: "utf8" });
    assert.equal(git(["init", "-q"]).status, 0);
    writeFileSync(join(root, ".gitignore"), "*\n");
    assert.equal(
      git([
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.test",
        "commit",
        "--allow-empty",
        "-qm",
        "fixture",
      ]).status,
      0,
    );
    assert.equal(
      git(["update-ref", "refs/remotes/origin/main", "HEAD"]).status,
      0,
    );
    copyFileSync(
      resolve("scripts/prepare-pr.sh"),
      join(root, "scripts/prepare-pr.sh"),
    );
    for (const file of ["plan.mjs", "rust-packages.mjs"])
      copyFileSync(
        resolve("scripts/pr-platform", file),
        join(root, "scripts/pr-platform", file),
      );
    writeFileSync(join(root, "scripts/pr-platform/run.mjs"), "");
    writeFileSync(join(root, "scripts/validation/pr-scenarios.mjs"), "");
    writeFileSync(join(root, "scripts/validation/standards.mjs"), "process.exit(Number(process.env.STANDARDS_STATUS || 0));\n");
    const trace = join(root, "trace");
    writeFileSync(
      join(root, "scripts/check-health.sh"),
      '#!/bin/bash\necho "health:$FAST" >> "$TRACE"\nexit "${HEALTH_STATUS:-0}"\n',
    );
    writeFileSync(
      join(root, "scripts/pr-preflight.sh"),
      '#!/bin/bash\necho "preflight:$MESH_VALIDATION_PREFLIGHT" >> "$TRACE"\nif [ "${CHANGE_BASE:-0}" = 1 ]; then git update-ref refs/remotes/origin/main 0000000000000000000000000000000000000000; fi\nexit "${PREFLIGHT_STATUS:-0}"\n',
      { mode: 0o755 },
    );
    const run = (env) =>
      spawnSync("bash", [join(root, "scripts/prepare-pr.sh")], {
        cwd: root,
        encoding: "utf8",
        env: {
          ...process.env,
          TRACE: trace,
          FAST: "1",
          MESH_VALIDATION_PREFLIGHT: "1",
          ...env,
        },
      });
    writeFileSync(trace, "");
    assert.equal(run({ STANDARDS_STATUS: "19" }).status, 19);
    assert.equal(readFileSync(trace, "utf8"), "", "standards failure must stop before expensive preflight or health");
    assert.equal(run({ HEALTH_STATUS: "9" }).status, 9);
    assert.equal(readFileSync(trace, "utf8"), "preflight:0\nhealth:0\n");
    writeFileSync(trace, "");
    assert.equal(run({ PREFLIGHT_STATUS: "8" }).status, 8);
    assert.equal(readFileSync(trace, "utf8"), "preflight:0\n");
    writeFileSync(trace, "");
    const success = run({});
    assert.equal(success.status, 0, success.stderr);
    assert.equal(readFileSync(trace, "utf8"), "preflight:0\nhealth:0\n");
    assert.notEqual(
      run({ CHANGE_BASE: "1" }).status,
      0,
      "moving comparison ref cannot certify the previous base",
    );
  },
);
