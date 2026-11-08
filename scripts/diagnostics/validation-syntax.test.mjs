import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync, chmodSync, readFileSync, copyFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { checkSyntax } from "../validation/syntax.mjs";
function fixture(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), "mesh syntax "));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const git = (...args) => {
    const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  };
  git("init", "-q");
  git("config", "user.email", "fixture@example.invalid");
  git("config", "user.name", "fixture");
  function put(file, text) { mkdirSync(path.dirname(path.join(root, file)), { recursive: true }); writeFileSync(path.join(root, file), text); }
  put("README", "fixture\n"); git("add", "."); git("commit", "-qm", "fixture");
  return { root, git, put };
}
test("real parsers reject invalid JS, shell and Python without importing or producing caches", t => {
  const { root, git, put } = fixture(t);
  for (const [file, good, bad] of [
    ["scripts/space name.mjs", "export const n = 1;", "export const = ;"],
    ["scripts/space name.cjs", "module.exports = 1;", "const = ;"],
    ["scripts/space name.js", "const n = 1;", "const = ;"],
    ["hooks/pre-commit", "#!/bin/bash\ntrue\n", "if then\n"],
    ["scripts/space name.py", "raise RuntimeError('must not execute')\n", "def broken(:\n"],
  ]) {
    put(file, good); git("add", file);
    assert.deepEqual(checkSyntax({ root, files: [file] }), [file]);
    put(file, bad);
    assert.throws(() => checkSyntax({ root, files: [file] }), /rejected syntax/);
    put(file, good);
  }
  assert.equal(existsSync(path.join(root, "scripts/__pycache__")), false);
});
test("staged selection parses worktree bytes and ignores unstaged unrelated paths and deletions", t => {
  const { root, git, put } = fixture(t);
  put("scripts/staged.mjs", "export const valid = true;"); git("add", ".");
  put("scripts/staged.mjs", "export const = ;");
  assert.throws(() => checkSyntax({ root, staged: true }), /rejected syntax/);
  put("scripts/staged.mjs", "export const valid = true;");
  put("scripts/untracked.mjs", "const = ;");
  assert.deepEqual(checkSyntax({ root, staged: true }), ["scripts/staged.mjs"]);
  git("commit", "-qm", "script");
  rmSync(path.join(root, "scripts/staged.mjs"));
  assert.deepEqual(checkSyntax({ root, files: ["scripts/staged.mjs"] }), []);
  assert.throws(() => checkSyntax({ root, files: ["scripts/untracked.mjs"] }), /rejected syntax/);
  put(".gitignore", "ignored/\n");
  put("ignored/broken.mjs", "const = ;");
  assert.deepEqual(checkSyntax({ root, files: ["ignored/broken.mjs"] }), []);
});
test("base selection includes committed changes and excludes untracked sources", t => {
  const { root, git, put } = fixture(t);
  put("scripts/change.mjs", "const n = 1;"); git("add", "."); git("commit", "-qm", "change");
  put("scripts/untracked.mjs", "const = ;");
  assert.deepEqual(checkSyntax({ root, base: "HEAD~1" }), ["scripts/change.mjs"]);
});
test("actionlint receives exact paths and propagates rejection through a POSIX executable fixture", {
  skip: process.platform === "win32" && "Windows cannot spawn a POSIX hashbang fixture; real parsers and base selection run separately",
}, t => {
  const { root, git, put } = fixture(t);
  put("scripts/change.mjs", "const n = 1;"); git("add", "."); git("commit", "-qm", "change");
  assert.deepEqual(checkSyntax({ root, base: "HEAD~1" }), ["scripts/change.mjs"]);
  put(".github/workflows/space name.yml", "name: fixture\n"); git("add", ".");
  const bin = path.join(root, "bin"); mkdirSync(bin);
  const executable = path.join(bin, "actionlint");
  writeFileSync(executable, '#!/bin/bash\nprintf "%s\\n" "$@" > "$SYNTAX_ARGS"\nexit "${SYNTAX_EXIT:-0}"\n'); chmodSync(executable, 0o755);
  const before = { ...process.env };
  t.after(() => { for (const key of ["PATH", "SYNTAX_ARGS", "SYNTAX_EXIT"]) { if (before[key] === undefined) delete process.env[key]; else process.env[key] = before[key]; } });
  process.env.PATH = `${bin}${path.delimiter}${process.env.PATH}`;
  process.env.SYNTAX_ARGS = path.join(root, "argv");
  assert.deepEqual(checkSyntax({ root, staged: true }), [".github/workflows/space name.yml"]);
  assert.equal(readFileSync(process.env.SYNTAX_ARGS, "utf8"), ".github/workflows/space name.yml\n");
  process.env.SYNTAX_EXIT = "1";
  assert.throws(() => checkSyntax({ root, staged: true }), /actionlint rejected/);
  const localAction = ".github/actions/ci-scope/action.yml";
  put(localAction, "name: Scope\nruns:\n  using: composite\n  steps: []\n");
  git("add", localAction);
  process.env.SYNTAX_EXIT = "0";
  assert.deepEqual(checkSyntax({ root, files: [localAction] }), [localAction]);
  assert.equal(readFileSync(process.env.SYNTAX_ARGS, "utf8"), "\n", "local metadata edits recheck callers with actionlint's local-action parser");
  process.env.SYNTAX_EXIT = "1";
  assert.throws(() => checkSyntax({ root, files: [localAction] }), /actionlint rejected/);
});

test("installing hooks does not launch the comprehensive health suite", t => {
  const { root, put } = fixture(t);
  put("hooks/pre-commit", "#!/bin/bash\ntrue\n");
  put("scripts/check-health.sh", "#!/bin/bash\ntouch health-was-run\n");
  copyFileSync(new URL("../setup-hooks.sh", import.meta.url), path.join(root, "scripts/setup-hooks.sh"));
  const result = spawnSync("bash", ["scripts/setup-hooks.sh"], { cwd: root, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(existsSync(path.join(root, "health-was-run")), false);
  const config = spawnSync("git", ["config", "core.hooksPath"], { cwd: root, encoding: "utf8" });
  assert.equal(config.stdout.trim(), "hooks");
});
