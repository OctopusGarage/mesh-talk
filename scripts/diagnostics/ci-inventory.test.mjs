import { execFileSync } from "node:child_process";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

test("scheduled fuzz and glib probes reject incomplete inventories", () => {
  execFileSync(
    process.platform === "win32" ? "python" : "python3",
    ["-B", "scripts/ci/test_inventory_probes.py", "-v"],
    {
      cwd: new URL("../../", import.meta.url),
      stdio: "pipe",
    },
  );
});

test("scheduled workflows invoke checked owners and pass dispatch input through the environment", () => {
  const fuzz = readFileSync(
    new URL("../../.github/workflows/fuzz.yml", import.meta.url),
    "utf8",
  );
  const glib = readFileSync(
    new URL("../../.github/workflows/glib-0.20-watch.yml", import.meta.url),
    "utf8",
  );
  assert.match(
    fuzz,
    /FUZZ_MAX_TOTAL_TIME: \$\{\{ github\.event\.inputs\.max_total_time \|\| '120' \}\}/,
  );
  assert.match(fuzz, /run: python3 -B scripts\/ci\/fuzz-targets\.py/);
  assert.doesNotMatch(fuzz, /for target in \$\(/);
  assert.match(fuzz, /timeout-minutes: 45/);
  assert.match(fuzz, /name: Upload crash artifacts\s+if: failure\(\)/);
  assert.match(
    glib,
    /cargo update -p tauri\s+python3 -B scripts\/ci\/glib-migration\.py/,
  );
  assert.doesNotMatch(glib, /maxglib|sort -V/);
  for (const workflow of [fuzz, glib])
    assert.match(workflow, /python-version: "3\.12"/);
});
