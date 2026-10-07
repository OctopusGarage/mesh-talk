import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { isIntegrated, isEligible } from "../maintenance/prune-integrated-branches.mjs";

function git(cwd, ...args) {
  return execFileSync("git", args, { cwd, encoding: "utf8" }).trim();
}

test("identifies ancestor and equivalent patches but preserves unique work", () => {
  const cwd = mkdtempSync(join(tmpdir(), "mesh-talk-prune-"));
  try {
    git(cwd, "init", "-q", "-b", "main");
    git(cwd, "config", "user.name", "Branch Test");
    git(cwd, "config", "user.email", "branch-test@example.invalid");
    git(cwd, "commit", "--allow-empty", "-qm", "base");

    git(cwd, "branch", "ancestor");
    git(cwd, "switch", "-qc", "equivalent");
    writeFileSync(join(cwd, "shared.txt"), "shared\n");
    git(cwd, "add", "shared.txt");
    git(cwd, "commit", "-qm", "equivalent change");

    git(cwd, "switch", "-q", "main");
    writeFileSync(join(cwd, "shared.txt"), "shared\n");
    git(cwd, "add", "shared.txt");
    git(cwd, "commit", "-qm", "ported change");

    git(cwd, "switch", "-qc", "unique", "equivalent");
    writeFileSync(join(cwd, "unique.txt"), "unique\n");
    git(cwd, "add", "unique.txt");
    git(cwd, "commit", "-qm", "unique change");

    assert.equal(isIntegrated(cwd, "main", "ancestor"), true);
    assert.equal(isIntegrated(cwd, "main", "equivalent"), true);
    assert.equal(isIntegrated(cwd, "main", "unique"), false);
  } finally {
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("never selects protected, permanent, or open-PR branches", () => {
  const openRefs = new Set(["active-head", "active-base"]);
  assert.equal(isEligible({ name: "main", protected: false }, "main", openRefs), false);
  assert.equal(isEligible({ name: "dev", protected: false }, "main", openRefs), false);
  assert.equal(isEligible({ name: "feature", protected: true }, "main", openRefs), false);
  assert.equal(isEligible({ name: "active-head", protected: false }, "main", openRefs), false);
  assert.equal(isEligible({ name: "active-base", protected: false }, "main", openRefs), false);
  assert.equal(isEligible({ name: "finished", protected: false }, "main", openRefs), true);
});
