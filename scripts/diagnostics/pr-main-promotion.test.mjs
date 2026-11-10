import assert from "node:assert/strict";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { verifyMainPromotion } from "../pr-platform/main-promotion.mjs";

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), "main-promotion-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const git = (...args) =>
    execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();
  git("init", "-q");
  git("config", "core.hooksPath", "no-hooks");
  git("config", "commit.gpgsign", "false");
  git("config", "user.name", "Test");
  git("config", "user.email", "test@example.test");
  await writeFile(join(root, "source"), "accepted main");
  git("add", ".");
  git("commit", "-qm", "source");
  const sha = git("rev-parse", "HEAD");
  git("update-ref", "refs/remotes/origin/main", sha);
  return {
    root,
    git,
    sha,
    base: "origin/dev",
    env: { ...process.env, GH_REPO: undefined, GH_HOST: undefined },
  };
}

for (const scenario of [
  "accepted",
  "ordinary",
  "other-base",
  "dirty",
  "wrong-main",
  "advanced-main",
  "proof-failed",
  "local-change",
  "cancelled",
  "override",
  "invalid-repo",
  "deadline",
]) {
  test(`trusted main promotion: ${scenario}`, async (t) => {
    const f = await fixture(t);
    const controller = new AbortController();
    let refs = 0,
      proofs = 0,
      calls = 0,
      time = 0;
    if (scenario === "ordinary") {
      await writeFile(join(f.root, "source"), "new source");
      f.git("add", ".");
      f.git("commit", "-qm", "new");
    }
    if (scenario === "other-base") f.base = "origin/main";
    if (scenario === "dirty") await writeFile(join(f.root, "untracked"), "new");
    if (scenario === "override") f.env.GH_REPO = "other/repo";
    const run = async (command, { signal }) => {
      signal.throwIfAborted();
      calls++;
      if (command[1] === "repo")
        return scenario === "invalid-repo" ? "../repo" : "owner/repo";
      if (command[1] === "api") {
        refs++;
        return scenario === "wrong-main" ||
          (scenario === "advanced-main" && refs > 1)
          ? "0".repeat(40)
          : f.sha;
      }
      proofs++;
      assert.equal(command[0], process.execPath);
      assert.match(command[1], /release[/\\]check-source-checks\.mjs$/u);
      assert.deepEqual(command.slice(2), ["owner/repo", f.sha]);
      if (scenario === "proof-failed")
        throw Error("required main-push suite is absent/failed/stale");
      if (scenario === "local-change")
        await writeFile(join(f.root, "source"), "changed during proof");
      if (scenario === "cancelled") controller.abort();
      if (scenario === "deadline") time = 120001;
      return "Current-source CI passed";
    };
    const options = { ...f, run, signal: controller.signal, now: () => time };
    if (["ordinary", "other-base"].includes(scenario)) {
      assert.equal(await verifyMainPromotion(options), false);
      assert.equal(calls, 0);
    } else if (scenario === "accepted") {
      assert.equal(await verifyMainPromotion(options), true);
      assert.equal(proofs, 1);
      assert.equal(refs, 2);
    } else await assert.rejects(verifyMainPromotion(options));
  });
}
