import assert from "node:assert/strict";
import { test } from "node:test";
import {
  addedSourceLines,
  parseLcov,
  summarizePatchCoverage,
} from "../patch-coverage.mjs";

test("patch coverage counts added executable lines and ignores deleted hunks", () => {
  const diff = [
    "+++ b/frontend/src/lib/theme.ts",
    "@@ -8,0 +9,2 @@",
    "+run();",
    "+fallback();",
    "@@ -20,2 +22,0 @@",
    "-old();",
  ].join("\n");
  const added = addedSourceLines(diff);
  const unit = parseLcov(
    "SF:src/lib/theme.ts\nDA:9,1\nDA:10,0\nend_of_record\n",
  );

  assert.deepEqual(summarizePatchCoverage(added, [unit]), {
    covered: 1,
    total: 2,
    missing: [{ file: "src/lib/theme.ts", line: 10 }],
  });
});

test("browser coverage can cover lines missed by unit tests", () => {
  const added = addedSourceLines(
    "+++ b/frontend/src/components/AvatarGallery.tsx\n@@ -0,0 +12,1 @@\n+render();\n",
  );
  const unit = parseLcov(
    "SF:src/components/AvatarGallery.tsx\nDA:12,0\nend_of_record\n",
  );
  const browser = parseLcov(
    "SF:src/components/AvatarGallery.tsx\nDA:12,1\nend_of_record\n",
  );

  assert.deepEqual(summarizePatchCoverage(added, [unit, browser]), {
    covered: 1,
    total: 1,
    missing: [],
  });
});

test("patch coverage de-duplicates overlapping hunks", () => {
  const added = addedSourceLines(
    "+++ b/frontend/src/lib/theme.ts\n@@ -0,0 +4,1 @@\n+x\n@@ -0,0 +4,1 @@\n+x\n",
  );
  const unit = parseLcov("SF:src/lib/theme.ts\nDA:4,0\nend_of_record\n");

  assert.equal(summarizePatchCoverage(added, [unit]).total, 1);
});

test("patch coverage excludes the registered unit-test and declaration paths", () => {
  const added = addedSourceLines(
    [
      "+++ b/frontend/src/new.test.ts",
      "@@ -0,0 +1,1 @@",
      "+it('works', () => {});",
      "+++ b/frontend/src/env.d.ts",
      "@@ -0,0 +1,1 @@",
      "+declare const value: number;",
      "+++ b/frontend/src/real.ts",
      "@@ -0,0 +1,1 @@",
      "+export const value = 1;",
    ].join("\n"),
  );
  assert.deepEqual([...added.keys()], ["src/real.ts"]);
});

// Run the actual gate against committed diffs; an empty coverage denominator is
// only inapplicable when no added production lines exist.
async function patchFixture(t, changes, lcov) {
  const { mkdtemp, mkdir, writeFile, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { fileURLToPath } = await import("node:url");
  const { spawnSync } = await import("node:child_process");
  const root = await mkdtemp(join(tmpdir(), "mesh patch coverage "));
  t.after(() => rm(root, { recursive: true, force: true }));
  const git = (...args) => {
    const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  };
  const put = async (file, text) => {
    await mkdir(join(root, file, ".."), { recursive: true });
    await writeFile(join(root, file), text);
  };
  const commit = () =>
    git(
      "-c",
      "core.hooksPath=empty-hooks",
      "-c",
      "commit.gpgsign=false",
      "-c",
      "user.name=Test",
      "-c",
      "user.email=test@example.test",
      "commit",
      "-qm",
      "fixture",
    );
  git("init", "-q");
  await put("frontend/src/existing.ts", "export const existing = 1;\n");
  git("add", ".");
  commit();
  git("update-ref", "refs/remotes/origin/main", "HEAD");
  for (const [file, text] of Object.entries(changes)) {
    if (text === null) await rm(join(root, file));
    else await put(file, text);
  }
  git("add", "-A");
  commit();
  await put("coverage.lcov", lcov);
  return spawnSync(
    process.execPath,
    [
      fileURLToPath(new URL("../patch-coverage.mjs", import.meta.url)),
      "coverage.lcov",
    ],
    {
      cwd: root,
      encoding: "utf8",
      env: {
        ...process.env,
        PATCH_COVERAGE_BASE: "origin/main",
        PATCH_COVERAGE_MIN: "92",
      },
    },
  );
}

for (const [name, changes] of [
  [
    "unit tests",
    { "frontend/src/new.test.ts": "test('example', () => {});\n" },
  ],
  [
    "declarations",
    { "frontend/src/env.d.ts": "declare const value: number;\n" },
  ],
  ["production deletions", { "frontend/src/existing.ts": null }],
]) {
  test(`coverage CLI reports not applicable for only ${name}`, async (t) => {
    const result = await patchFixture(
      t,
      changes,
      "SF:src/existing.ts\nDA:1,1\nend_of_record\n",
    );
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /not applicable/i);
    assert.doesNotMatch(result.stdout, /100(?:\.00)?%/);
  });
}

test("coverage CLI still rejects added production source missing from every report", async (t) => {
  const result = await patchFixture(
    t,
    { "frontend/src/real.ts": "export const value = 1;\n" },
    "SF:src/existing.ts\nDA:1,1\nend_of_record\n",
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /No changed frontend source lines were found/);
});

test("coverage CLI still rejects unexecuted production alongside new tests", async (t) => {
  const result = await patchFixture(
    t,
    {
      "frontend/src/real.ts": "export const value = 1;\n",
      "frontend/src/new.test.ts": "test('example', () => {});\n",
    },
    "SF:src/real.ts\nDA:1,0\nend_of_record\n",
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stdout, /0\/1 \(0\.00%\)/);
});

test("deletion-only hunks in surviving production files add no denominator", () => {
  const added = addedSourceLines(
    "+++ b/frontend/src/real.ts\n@@ -4,1 +4,0 @@\n-old();\n",
  );
  assert.equal(added.size, 0);
});
