import assert from "node:assert/strict";
import { test } from "node:test";
import {
  addedSourceLines,
  parseLcov,
  summarizePatchCoverage,
  patchCoverageTarget,
  prePrPatchCoverage,
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
async function patchFixture(t, changes, lcov, evaluate) {
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
  await put(
    "codecov.yml",
    await (
      await import("node:fs/promises")
    ).readFile(new URL("../../codecov.yml", import.meta.url), "utf8"),
  );
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
  if (evaluate) return evaluate(root);
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
        PATCH_COVERAGE_MIN: undefined,
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
  assert.match(result.stderr, /Changed frontend source file missing/);
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

test("patch coverage rejects omitted changed files even when another file is covered", () => {
  const added = addedSourceLines(
    "+++ b/frontend/src/covered.ts\n@@ -0,0 +1 @@\n+x\n+++ b/frontend/src/omitted.ts\n@@ -0,0 +1 @@\n+y\n",
  );
  assert.throws(
    () =>
      summarizePatchCoverage(added, [
        parseLcov("SF:src/covered.ts\nDA:1,1\nend_of_record\n"),
      ]),
    /omitted.ts/,
  );
});

test("executed lines with unexecuted branches remain partial until complementary reports cover every branch", () => {
  const added = new Map([["src/real.ts", new Set([1])]]);
  const unit = parseLcov(
    "SF:src/real.ts\nDA:1,3\nBRDA:1,0,0,3\nBRDA:1,0,1,0\nend_of_record\n",
  );
  const browser = parseLcov(
    "SF:src/real.ts\nDA:1,1\nBRDA:1,0,0,0\nBRDA:1,0,1,1\nend_of_record\n",
  );
  assert.deepEqual(summarizePatchCoverage(added, [unit]), {
    covered: 0,
    total: 1,
    missing: [{ file: "src/real.ts", line: 1 }],
  });
  assert.equal(summarizePatchCoverage(added, [unit, browser]).covered, 1);
});

test("LCOV rejects invalid executable line and branch counts", () => {
  for (const row of [
    "DA:0,1",
    "DA:1,-1",
    "DA:1,1.5",
    "BRDA:1,0,0,1.5",
    "BRDA:0,0,0,1",
  ]) {
    assert.throws(
      () => parseLcov(`SF:src/real.ts\n${row}\nend_of_record\n`),
      /Invalid LCOV/,
    );
  }
});

test("negative V8 branch counts remain uncovered without invalidating the report", () => {
  const report = parseLcov("SF:src/real.ts\nDA:125,3\nBRDA:125,14,0,3\nBRDA:125,14,1,-2\nend_of_record\n");
  assert.deepEqual(summarizePatchCoverage(new Map([["src/real.ts", new Set([125])]]), [report]), {
    covered: 0, total: 1, missing: [{ file: "src/real.ts", line: 125 }],
  });
});

test("local target comes exclusively from the explicit Codecov patch policy", () => {
  const config =
    "coverage:\n  status:\n    patch:\n      default:\n        target: 87.5%\n        threshold: 0%\n";
  assert.equal(patchCoverageTarget(config), 87.5);
  assert.throws(
    () => patchCoverageTarget(config.replace("87.5%", "auto")),
    /numeric/,
  );
  assert.throws(
    () =>
      patchCoverageTarget(
        "coverage:\n  status:\n    project:\n      default:\n        target: 100%\n",
      ),
    /Missing/,
  );
});

test("pre-PR coverage skips evidence probes for inapplicable committed changes", async (t) => {
  const result = await patchFixture(
    t,
    { "frontend/src/new.test.ts": "test('works', () => {});\n" },
    "",
    (root) =>
      prePrPatchCoverage({
        root,
        verifyEvidence: () => {
          throw Error("must not probe coverage");
        },
      }),
  );
  assert.deepEqual(result, { applicable: false });
});

test("pre-PR coverage requires current proof and verifies the consumed LCOV digest", async (t) => {
  const { createHash } = await import("node:crypto");
  const { mkdir, writeFile } = await import("node:fs/promises");
  const { join } = await import("node:path");
  const lcov = "SF:src/real.ts\nDA:1,1\nend_of_record\n";
  await patchFixture(
    t,
    { "frontend/src/real.ts": "export const value = 1;\n" },
    lcov,
    async (root) => {
      await mkdir(join(root, "frontend/coverage"), { recursive: true });
      await writeFile(join(root, "frontend/coverage/lcov.info"), lcov);
      for (const reason of ["missing", "stale", "wrong source", "busy"]) {
        await assert.rejects(
          prePrPatchCoverage({
            root,
            verifyEvidence: () => {
              throw Error(reason);
            },
          }),
          /Generate it with: cd frontend/,
        );
      }
      await assert.rejects(
        prePrPatchCoverage({
          root,
          verifyEvidence: () => ({
            artifacts: [["coverage/lcov.info", "wrong"]],
          }),
        }),
        /changed after evidence verification/,
      );
      const result = await prePrPatchCoverage({
        root,
        verifyEvidence: (options) => {
          assert.equal(options.name, "frontend-coverage");
          assert.deepEqual(options.command, ["npm", "run", "test:coverage"]);
          assert.equal(options.profile, "shared");
          assert.equal(options.env.FAST, "0");
          assert.deepEqual(options.artifacts, ["coverage/lcov.info"]);
          return {
            artifacts: [
              [
                "coverage/lcov.info",
                createHash("sha256").update(lcov).digest("hex"),
              ],
            ],
          };
        },
      });
      assert.equal(result.covered, 1);
      assert.equal(result.total, 1);
      assert.equal(result.applicable, true);
    },
  );
});

test(
  "pre-PR gate consumes canonical stage proof and rejects altered coverage without running tests",
  { timeout: 30000 },
  async (t) => {
    const originalCi = process.env.CI;
    t.after(() => {
      if (originalCi === undefined) delete process.env.CI;
      else process.env.CI = originalCi;
    });
    const { execFileSync } = await import("node:child_process");
    const { writeFile, readFile } = await import("node:fs/promises");
    const { join } = await import("node:path");
    const { runStage } = await import("../validation/stage.mjs");
    const { inputGroup } = await import("../validation/stage-inputs.mjs");
    const lcov = "SF:src/real.ts\nDA:1,1\nend_of_record\n";
    await patchFixture(
      t,
      { "frontend/src/real.ts": "export const value = 1;\n" },
      lcov,
      async (root) => {
        await writeFile(
          join(root, ".gitignore"),
          "target/\ncoverage.lcov\nfrontend/coverage/\n",
        );
        await writeFile(
          join(root, "frontend/package.json"),
          JSON.stringify({
            scripts: { "test:coverage": "node coverage-fixture.cjs" },
          }),
        );
        await writeFile(
          join(root, "frontend/coverage-fixture.cjs"),
          `const fs=require('node:fs');fs.mkdirSync('coverage',{recursive:true});fs.writeFileSync('coverage/lcov.info',${JSON.stringify(lcov)});fs.appendFileSync('coverage/executions','run\\n');`,
        );
        execFileSync("git", ["add", "."], { cwd: root });
        execFileSync(
          "git",
          [
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
            "coverage fixture",
          ],
          { cwd: root },
        );
        process.env.CI = "1";
        await assert.rejects(prePrPatchCoverage({ root }), /dirty source, CI, or fresh execution/);
        // This isolated repository exercises local-only evidence, even when the
        // regression itself is running in Actions. Production still rejects CI.
        process.env.CI = "";
        await assert.rejects(prePrPatchCoverage({ root }), /Generate it with:/);
        const proof = await runStage({
          root,
          env: { ...process.env, FAST: "0" },
          cwd: join(root, "frontend"),
          name: "frontend-coverage",
          command: ["npm", "run", "test:coverage"],
          profile: "shared",
          inputs: inputGroup("frontend", "frontend-coverage"),
          artifacts: ["coverage/lcov.info"],
          log: () => {},
        });
        assert.equal(proof.code, 0, proof.error);
        const previousFast = process.env.FAST;
        try {
          for (const ambient of [undefined, "1"]) {
            if (ambient === undefined) delete process.env.FAST;
            else process.env.FAST = ambient;
            assert.equal((await prePrPatchCoverage({ root })).covered, 1);
            assert.equal(
              process.env.FAST,
              ambient,
              "verification must not change ambient FAST",
            );
          }
        } finally {
          if (previousFast === undefined) delete process.env.FAST;
          else process.env.FAST = previousFast;
        }
        await writeFile(
          join(root, "frontend/coverage/lcov.info"),
          lcov.replace("1,1", "1,0"),
        );
        await assert.rejects(prePrPatchCoverage({ root }), /Generate it with:/);
        assert.equal(
          await readFile(join(root, "frontend/coverage/executions"), "utf8"),
          "run\n",
          "proof checks must never execute the coverage producer",
        );
      },
    );
  },
);

test("fresh measured files classify added non-executable lines as inapplicable", async (t) => {
  const { createHash } = await import("node:crypto");
  const { mkdir, writeFile } = await import("node:fs/promises");
  const { join } = await import("node:path");
  const lcov = "SF:src/real.ts\nDA:8,1\nend_of_record\n";
  const result = await patchFixture(
    t,
    { "frontend/src/real.ts": "// documentation only\n" },
    lcov,
    async (root) => {
      await mkdir(join(root, "frontend/coverage"), { recursive: true });
      await writeFile(join(root, "frontend/coverage/lcov.info"), lcov);
      return prePrPatchCoverage({
        root,
        verifyEvidence: () => ({
          artifacts: [
            [
              "coverage/lcov.info",
              createHash("sha256").update(lcov).digest("hex"),
            ],
          ],
        }),
      });
    },
  );
  assert.equal(result.applicable, false);
  assert.match(result.reason, /no added lines as executable/);
});

test("pre-PR cancellation reaches the canonical verifier and cannot produce coverage acceptance", async (t) => {
  await patchFixture(
    t,
    { "frontend/src/real.ts": "export const value = 1;\n" },
    "",
    async (root) => {
      const controller = new AbortController();
      let observed;
      const pending = prePrPatchCoverage({
        root,
        signal: controller.signal,
        verifyEvidence: ({ signal }) => {
          observed = signal;
          controller.abort();
          signal.throwIfAborted();
          throw Error("cancelled verification must not continue");
        },
      });
      await assert.rejects(pending, /aborted/i);
      assert.equal(observed, controller.signal);
      assert.equal(observed.aborted, true);
    },
  );
});

test("complete committed patches larger than Git's default output buffer remain measurable", async (t) => {
  await patchFixture(t, { "frontend/src/large.ts": `export const value = "${"x".repeat(1200000)}";\n` }, "", async (root) => {
    const { writeFile, mkdir } = await import("node:fs/promises");
    const { createHash } = await import("node:crypto");
    const lcov = "SF:src/large.ts\nDA:1,1\nend_of_record\n";
    await mkdir(`${root}/frontend/coverage`, { recursive: true });
    await writeFile(`${root}/frontend/coverage/lcov.info`, lcov);
    const result = await prePrPatchCoverage({ root, verifyEvidence: () => ({ artifacts: [["coverage/lcov.info", createHash("sha256").update(lcov).digest("hex")]] }) });
    assert.equal(result.total, 1);
    assert.equal(result.covered, 1);
  });
});
