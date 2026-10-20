import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  mkdtemp,
  mkdir,
  writeFile,
  rm,
  rename,
  symlink,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { checkStandards } from "../validation/standards.mjs";
import { contractFiles } from "../validation/contracts.mjs";

const cli = fileURLToPath(
  new URL("../validation/standards.mjs", import.meta.url),
);
async function fixture(t, contracts = []) {
  const root = await mkdtemp(path.join(tmpdir(), "mesh-standards-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const put = async (file, text = "") => {
    await mkdir(path.dirname(path.join(root, file)), { recursive: true });
    await writeFile(path.join(root, file), text);
  };
  for (const [index, file] of contractFiles.entries())
    await put(
      file,
      JSON.stringify({ schema: 1, contracts: index ? [] : contracts }),
    );
  await put("frontend/src/baseline.test.ts");
  const git = (...args) => {
    const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
  };
  git("init", "--quiet");
  return { root, put, git };
}
const owner = (extra = {}) => ({
  id: "feature",
  paths: ["crates/mesh-talk-core/src/"],
  node: ["scripts/example.test.mjs"],
  ...extra,
});

test("untracked Rust sources, manifests and fixtures require executable ownership", async (t) => {
  const f = await fixture(t);
  for (const file of [
    "crates/mesh-talk-core/src/new.rs",
    "src-tauri/Cargo.toml",
    "crates/mesh-talk-mcp/tests/fixtures/data.json",
  ])
    await f.put(file);
  const report = await checkStandards(f.root);
  assert.equal(report.ok, false);
  assert.equal(report.issues.filter((i) => i.code === "ownership").length, 3);
});

test("valid new capability and transitive shared fixture consumers pass", async (t) => {
  const f = await fixture(t, [
    {
      id: "fixture",
      paths: ["crates/mesh-talk-core/tests/fixtures/"],
      consumers: ["feature"],
    },
    owner({
      capability: {
        summary: "Feature guarantee",
        guide: "docs/feature.md#feature",
        invariants: [
          { name: "Stable state", reference: "docs/feature.md#stable-state" },
        ],
      },
    }),
  ]);
  await f.put("scripts/example.test.mjs");
  await f.put("docs/feature.md", "# Feature\n\nStable state\n------------\n");
  await f.put("crates/mesh-talk-core/tests/fixtures/new.json");
  await f.put("crates/mesh-talk-core/src/new.rs");
  assert.deepEqual((await checkStandards(f.root)).issues, []);
});

test("renamed source loses ownership but deleted unreferenced source needs none", async (t) => {
  const f = await fixture(t, [
    owner({ paths: ["crates/mesh-talk-core/src/old.rs"] }),
  ]);
  await f.put("scripts/example.test.mjs");
  await f.put("crates/mesh-talk-core/src/old.rs");
  f.git("add", ".");
  await rename(
    path.join(f.root, "crates/mesh-talk-core/src/old.rs"),
    path.join(f.root, "crates/mesh-talk-core/src/new.rs"),
  );
  const report = await checkStandards(f.root);
  assert.equal(report.issues.filter((i) => i.code === "ownership").length, 1);
  assert.match(report.issues[0].message, /new\.rs/u);
});

test("all explicit tests are checked even for unselected owners and deletions", async (t) => {
  const f = await fixture(t, [owner({ frontend: ["src/missing.test.ts"] })]);
  await f.put("scripts/example.test.mjs");
  f.git("add", ".");
  await rm(path.join(f.root, "scripts/example.test.mjs"));
  const issues = (await checkStandards(f.root)).issues;
  assert.equal(issues.filter((i) => i.code === "test-reference").length, 2);
  assert.ok(issues.every((i) => i.owner === "feature"));
});

test("capability references reject absent docs and absent headings, ignoring code fences", async (t) => {
  const f = await fixture(t, [
    owner({
      capability: {
        summary: "Feature",
        guide: "docs/missing.md",
        invariants: [{ name: "Invariant", reference: "docs/feature.md#fake" }],
      },
    }),
  ]);
  await f.put("scripts/example.test.mjs");
  await f.put("docs/feature.md", "```md\n# Fake\n```\n# Real\n");
  const issues = (await checkStandards(f.root)).issues;
  assert.equal(
    issues.filter((i) => i.code === "capability-reference").length,
    2,
  );
});

test("invalid consumers and malformed schemas report structured contract errors", async (t) => {
  const f = await fixture(t, [owner({ consumers: ["absent"] })]);
  assert.match(
    (await checkStandards(f.root)).issues[0].message,
    /Unknown consumer absent of feature/u,
  );
  await f.put(contractFiles[0], JSON.stringify({ schema: 99, contracts: [] }));
  assert.equal((await checkStandards(f.root)).issues[0].code, "contract");
});

test("frontend ownership reuses import graph including shared fixture consumers", async (t) => {
  const f = await fixture(t);
  await f.put("frontend/src/shared.ts", "export const value = 1;");
  await f.put("frontend/src/feature.ts", 'export {value} from "./shared";');
  await f.put(
    "frontend/src/feature.test.ts",
    'import {value} from "./feature";',
  );
  assert.deepEqual((await checkStandards(f.root)).issues, []);
  await f.put("frontend/src/orphan.ts", "export const orphan = 1;");
  assert.match((await checkStandards(f.root)).issues[0].message, /orphan\.ts/u);
});

test("reference directory links cannot escape the fixture root", async (t) => {
  const outside = await fixture(t);
  await outside.put("scripts/example.test.mjs");
  const f = await fixture(t, [owner({ node: ["external/example.test.mjs"] })]);
  await symlink(
    path.join(outside.root, "scripts"),
    path.join(f.root, "external"),
    process.platform === "win32" ? "junction" : "dir",
  );
  const report = await checkStandards(f.root);
  assert.equal(report.ok, false);
  assert.equal(report.issues[0].code, "test-reference");
  assert.match(report.issues[0].message, /escapes root/u);
  await f.put("scripts/example.test.mjs");
  await f.put(
    contractFiles[0],
    JSON.stringify({ schema: 1, contracts: [owner()] }),
  );
  assert.deepEqual((await checkStandards(f.root)).issues, []);
});

test("CLI JSON returns failure for gaps and succeeds after ownership is supplied", async (t) => {
  const f = await fixture(t);
  await f.put("crates/mesh-talk-core/src/new.rs");
  const run = () =>
    spawnSync(process.execPath, [cli, "--root", f.root, "--json"], {
      encoding: "utf8",
    });
  let result = run();
  assert.equal(result.status, 1, result.stderr);
  assert.equal(JSON.parse(result.stdout).ok, false);
  await f.put(
    contractFiles[0],
    JSON.stringify({ schema: 1, contracts: [owner()] }),
  );
  await f.put("scripts/example.test.mjs");
  result = run();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(JSON.parse(result.stdout).ok, true);
});

test("a mapped owner without tests cannot borrow unrelated ownership", async (t) => {
  const f = await fixture(t, [
    { id: "empty", paths: ["crates/mesh-talk-core/src/empty.rs"] },
    owner({ paths: ["crates/mesh-talk-core/src/owned.rs"] }),
  ]);
  await f.put("scripts/example.test.mjs");
  await f.put("crates/mesh-talk-core/src/empty.rs");
  await f.put("crates/mesh-talk-core/src/owned.rs");
  assert.ok(
    (await checkStandards(f.root)).issues.some((i) =>
      /No executable tests for owner empty/u.test(i.message),
    ),
  );
});

test("shared frontend configuration requires registered tests and keeps global escalation", async (t) => {
  const f = await fixture(t);
  await f.put("frontend/package.json", "{}");
  let report = await checkStandards(f.root);
  assert.equal(report.ok, true);
  assert.ok(
    report.escalation.some((note) =>
      note.includes("Shared frontend input frontend/package.json"),
    ),
  );
  await rm(path.join(f.root, "frontend/src/baseline.test.ts"));
  report = await checkStandards(f.root);
  assert.ok(
    report.issues.some((i) =>
      /Shared frontend input frontend\/package.json has no registered unit tests/u.test(
        i.message,
      ),
    ),
  );
});

test("heading slug suffixes do not collide with literal numbered headings", async (t) => {
  const f = await fixture(t, [
    owner({
      capability: {
        summary: "Feature",
        guide: "docs/feature.md#a-2",
        invariants: [{ name: "Existing", reference: "docs/feature.md#a-1" }],
      },
    }),
  ]);
  await f.put("scripts/example.test.mjs");
  await f.put("docs/feature.md", "# A\n# A-1\n# A\n");
  assert.deepEqual((await checkStandards(f.root)).issues, []);
});

for (const marker of ["```", "~~~"]) {
  test(`${marker} with trailing text does not close a code fence`, async (t) => {
    const f = await fixture(t, [
      owner({
        capability: {
          summary: "Feature",
          guide: "docs/feature.md#real",
          invariants: [
            { name: "Invariant", reference: "docs/feature.md#fake" },
          ],
        },
      }),
    ]);
    await f.put("scripts/example.test.mjs");
    await f.put(
      "docs/feature.md",
      `${marker}md\n${marker}this-is-still-code\n# Fake\n${marker} \t\n# Real\n`,
    );
    const report = await checkStandards(f.root);
    assert.equal(report.ok, false);
    assert.equal(report.issues.length, 1);
    assert.equal(report.issues[0].code, "capability-reference");
    assert.equal(report.issues[0].path, "docs/feature.md#fake");
  });
}

test("backticks in an opening info string do not start a code fence", async (t) => {
  const f = await fixture(t, [
    owner({
      capability: {
        summary: "Feature",
        guide: "docs/feature.md#real",
        invariants: [{ name: "Invariant", reference: "docs/feature.md#real" }],
      },
    }),
  ]);
  await f.put("scripts/example.test.mjs");
  await f.put("docs/feature.md", "```invalid`info\n# Real\n");
  assert.deepEqual((await checkStandards(f.root)).issues, []);
});
