import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { sourceIdentity } from "../evals/source-identity.mjs";

test("evidence identifies real checkout and stamped archive, rejecting stale or missing identity", () => {
  const root = mkdtempSync(join(tmpdir(), "mesh-source-identity-"));
  try {
    const repo = join(root, "repo");
    mkdirSync(repo);
    const git = (...args) => execFileSync("git", args, { cwd: repo, encoding: "utf8" }).trim();
    git("init", "--quiet");
    git("config", "user.name", "Test");
    git("config", "user.email", "test@example.com");
    writeFileSync(join(repo, "source.txt"), "committed source\n");
    git("add", "source.txt");
    git("commit", "--quiet", "-m", "fixture");
    const sha = git("rev-parse", "HEAD");
    assert.equal(sourceIdentity({ cwd: repo, env: {} }), sha);
    assert.throws(() => sourceIdentity({ cwd: repo, env: { EVAL_SOURCE_SHA: "0".repeat(40) } }), /must match/);
    const archive = join(root, "source.tar");
    git("archive", "--format=tar", `--output=${archive}`, sha);
    const env = { MESH_PR_ARCHIVE: archive, EVAL_SOURCE_SHA: sha };
    assert.equal(sourceIdentity({ cwd: root, env }), sha);
    assert.throws(() => sourceIdentity({ cwd: root, env: { ...env, EVAL_SOURCE_SHA: "0".repeat(40) } }), /must match/);
    assert.throws(() => sourceIdentity({ cwd: root, env: { MESH_PR_ARCHIVE: archive } }), /requires a pinned/);
    const invalid = join(root, "invalid.tar");
    writeFileSync(invalid, Buffer.alloc(1024));
    assert.throws(() => sourceIdentity({ cwd: root, env: { ...env, MESH_PR_ARCHIVE: invalid } }));
    assert.throws(() => sourceIdentity({ cwd: root, env: {} }));
    assert.equal(git("status", "--porcelain"), "");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
