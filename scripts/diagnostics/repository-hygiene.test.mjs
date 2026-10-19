import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  rmSync,
  readFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const checker = fileURLToPath(
  new URL("../check-repository-hygiene.mjs", import.meta.url),
);
const policy = fileURLToPath(new URL("../../.gitignore", import.meta.url));
const env = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_")),
);
function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), "mesh-hygiene-"));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  function git(...args) {
    const result = spawnSync("git", args, { cwd, env, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  }
  git("init", "--quiet");
  // Use the actual repository policy, including future additions.
  writeFileSync(join(cwd, ".gitignore"), readFileSync(policy));
  git("add", ".gitignore");
  const add = (path, force = false) => {
    mkdirSync(dirname(join(cwd, path)), { recursive: true });
    writeFileSync(join(cwd, path), "fixture\n");
    git("add", ...(force ? ["-f"] : []), "--", path);
  };
  return {
    cwd,
    git,
    add,
    run: () =>
      spawnSync(process.execPath, [checker], { cwd, env, encoding: "utf8" }),
  };
}

test("allows maintained scripts, guides and executable evaluation fixtures", (t) => {
  const f = fixture(t);
  for (const path of [
    "scripts/release/verify.mjs",
    "docs/module-boundaries.md",
    "docs/evals/ai-eval-suite.json",
    "docs/multi-device-sync-design.md",
  ])
    f.add(path);
  assert.equal(f.run().status, 0);
});

test("rejects force-added implementation artifacts using the shared ignore policy", (t) => {
  const f = fixture(t);
  for (const path of [
    "docs/superpowers/plans/task.md",
    "plans/task.md",
    "docs/research/probe.md",
    "docs/protocol-research.md",
    "docs/device-sync-plan.md",
    "docs/investigations/trace.md",
    ".brooks-lint-history.json",
    "tmp/probe.sh",
    "frontend/test-results/result.json",
    "scripts/probe.py.orig",
    "docs/evals/new-review.md",
    "docs/evals/report.json",
    "docs/evals/evidence/screen.png",
  ])
    f.add(path, true);
  const result = f.run();
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /docs\/research\/probe.md/);
  assert.match(result.stderr, /scripts\/probe.py.orig/);
});

test("also checks ignored artifacts already present in the index", (t) => {
  const f = fixture(t);
  f.add("docs/research/old.md", true);
  f.add("docs/current-guide.md");
  assert.equal(f.run().status, 1);
});

test("allows local ignored files and their staged removal", (t) => {
  const f = fixture(t);
  f.add("tmp/local.md", true);
  f.git("rm", "--cached", "--", "tmp/local.md");
  assert.equal(f.run().status, 0);
});

test("handles spaces in artifact filenames", (t) => {
  const f = fixture(t);
  f.add("tmp/my note.md", true);
  const result = f.run();
  assert.equal(result.status, 1);
  assert.match(result.stderr, /my note.md/);
});

// Win32 filenames cannot contain control characters; retain this real-filesystem
// escaping assertion on platforms that can represent the input.
test("escapes newlines in artifact filenames", { skip: process.platform === "win32" }, (t) => {
  const f = fixture(t);
  f.add("tmp/my note\nsecond line.md", true);
  const result = f.run();
  assert.equal(result.status, 1);
  assert.match(result.stderr, /my note\\nsecond line/);
});

test("fails when Git cannot inspect the index", (t) => {
  const f = fixture(t);
  rmSync(join(f.cwd, ".git"), { recursive: true, force: true });
  assert.equal(f.run().status, 1);
});

test("uses project policy independently of personal Git excludes", (t) => {
  const f = fixture(t);
  f.add("docs/maintained-guide.md");
  writeFileSync(join(f.cwd, ".git", "info", "exclude"), "docs/*.md\n");
  const personal = join(f.cwd, "personal-excludes");
  writeFileSync(personal, "*.md\n");
  f.git("config", "core.excludesFile", personal);
  assert.equal(f.run().status, 0);
});

test("the existing commit, health and CI gates enforce the same policy", () => {
  for (const file of [
    "../../hooks/pre-commit",
    "../check-health.sh",
    "../../.github/workflows/ci.yml",
  ]) {
    assert.match(
      readFileSync(new URL(file, import.meta.url), "utf8"),
      /node scripts\/check-repository-hygiene\.mjs/,
    );
  }
});

test("rejects generated Tauri schemas while retaining capability source files", (t) => {
  const f = fixture(t);
  f.add("src-tauri/capabilities/default.json");
  assert.equal(f.run().status, 0);
  for (const platform of ["linux", "windows", "macOS", "desktop"]) {
    f.add(`src-tauri/gen/schemas/${platform}-schema.json`, true);
  }
  f.add("src-tauri/gen/schemas/acl-manifests.json", true);
  f.add("src-tauri/gen/schemas/capabilities.json", true);
  const result = f.run();
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /src-tauri\/gen\/schemas\/linux-schema.json/);
  assert.match(result.stderr, /src-tauri\/gen\/schemas\/capabilities.json/);
});
