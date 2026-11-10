import assert from "node:assert/strict";
import test from "node:test";
import {
  selectRequirements,
  verifyNativeReports,
  requiredBrowserSpecs,
} from "../validation/pr-scenarios.mjs";

const rules = [
  {
    id: "shell",
    paths: ["frontend/src/components/"],
    browser: ["e2e/ui-visual-regression.spec.ts"],
  },
  {
    id: "session",
    paths: ["src-tauri/src/desktop_session.rs"],
    native: ["host"],
  },
];
test("owning scenario rules select exact/prefix paths and deduplicate browser evidence", () => {
  assert.deepEqual(
    selectRequirements(["frontend/src/components/Sidebar.tsx"], rules).browser,
    ["e2e/ui-visual-regression.spec.ts"],
  );
  assert.deepEqual(selectRequirements(["docs/README.md"], rules), {
    browser: [],
    native: [],
    owners: [],
  });
  assert.deepEqual(
    selectRequirements(["src-tauri/src/desktop_session.rs"], rules).native,
    [process.platform],
  );
});
test("scenario contracts reject unsafe specs, unknown fields and invalid platforms", () => {
  for (const rule of [
    { id: "x", paths: ["x"], browser: ["../escape"] },
    { id: "x", paths: ["x"], native: ["other"] },
    { id: "x", paths: ["x"], typo: [] },
  ])
    assert.throws(() => selectRequirements(["x"], [rule]));
});
test("required native evidence rejects missing, stale, mocked and incomplete reports", async () => {
  await assert.rejects(
    verifyNativeReports(["win32"], [], "a".repeat(40)),
    /win32/,
  );
  for (const report of [
    {
      schema: 2,
      cleanSource: true,
      buildVerified: true,
      native: true,
      mocked: false,
      platform: "win32",
      sourceSha: "b".repeat(40),
      scenarios: {},
    },
    {
      schema: 2,
      cleanSource: true,
      buildVerified: true,
      native: false,
      mocked: true,
      platform: "win32",
      sourceSha: "a".repeat(40),
      scenarios: {},
    },
  ]) {
    await assert.rejects(
      verifyNativeReports(["win32"], ["report.json"], "a".repeat(40), {
        read: async () => JSON.stringify(report),
      }),
      /native evidence/i,
    );
  }
});
test("documentation does not demand native evidence or inspect unrelated reports", async () => {
  await verifyNativeReports([], ["missing"], "a".repeat(40), {
    read: async () => {
      throw Error("unexpected read");
    },
  });
});

test("maintained owners require visual and native scenarios for changed boundaries", async () => {
  const { loadRequirements } = await import("../validation/pr-scenarios.mjs");
  assert.ok(
    (await loadRequirements(["frontend/src/index.css"])).browser.includes(
      "e2e/ui-visual-regression.spec.ts",
    ),
  );
  assert.ok(
    (
      await loadRequirements(["src-tauri/src/desktop_session.rs"])
    ).native.includes(process.platform),
  );
  assert.ok(
    (
      await loadRequirements(["crates/mesh-talk-core/src/discovery/mod.rs"])
    ).native.includes(process.platform),
  );
});

test("an unrelated explicit spec cannot replace an owning visual requirement", () => {
  const required = selectRequirements(
    ["frontend/src/components/Shell.tsx"],
    rules,
  );
  assert.deepEqual(requiredBrowserSpecs(["e2e/privacy.spec.ts"], required), [
    "e2e/privacy.spec.ts",
    "e2e/ui-visual-regression.spec.ts",
  ]);
  assert.deepEqual(
    requiredBrowserSpecs(required.browser, required),
    required.browser,
  );
});

test("committed rename selection retains the old owning boundary", async (t) => {
  const { mkdtempSync, mkdirSync, writeFileSync, renameSync, rmSync } =
    await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { execFileSync } = await import("node:child_process");
  const { committedFiles } = await import("../validation/pr-scenarios.mjs");
  const root = mkdtempSync(join(tmpdir(), "mesh-scenario-rename-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const git = (args) =>
    execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();
  git(["init", "-q"]);
  mkdirSync(join(root, "frontend/src/components"), { recursive: true });
  writeFileSync(
    join(root, "frontend/src/components/Shell.tsx"),
    "export const shell = true;\n",
  );
  git(["add", "."]);
  git([
    "-c",
    "user.name=Test",
    "-c",
    "user.email=test@example.test",
    "commit",
    "-qm",
    "old",
  ]);
  const base = git(["rev-parse", "HEAD"]);
  renameSync(
    join(root, "frontend/src/components/Shell.tsx"),
    join(root, "moved.tsx"),
  );
  git(["add", "-A"]);
  git([
    "-c",
    "user.name=Test",
    "-c",
    "user.email=test@example.test",
    "commit",
    "-qm",
    "move",
  ]);
  const files = committedFiles(root, base, git(["rev-parse", "HEAD"]));
  assert.ok(files.includes("frontend/src/components/Shell.tsx"));
  assert.ok(
    selectRequirements(files, rules).browser.includes(
      "e2e/ui-visual-regression.spec.ts",
    ),
  );
});

test("publication rejects development native runs and unverified prebuilt binaries", async () => {
  for (const patch of [
    { cleanSource: false, buildVerified: true },
    { cleanSource: true, buildVerified: false },
  ]) {
    await assert.rejects(
      verifyNativeReports(["win32"], ["report.json"], "a".repeat(40), {
        read: async () =>
          JSON.stringify({
            sourceSha: "a".repeat(40),
            platform: "win32",
            ...patch,
          }),
      }),
      /clean committed source|native-build/,
    );
  }
});

test("changed pack and composer browser scenarios require their own acceptance specs", async () => {
  const { loadRequirements } = await import("../validation/pr-scenarios.mjs");
  const { fileURLToPath } = await import("node:url");
  for (const name of ["packs", "packs-empty", "stickers", "theme-picker", "theme-contrast", "avatar-gallery", "avatar-gallery-loading", "ui-audit-composer"]) {
    const spec = `e2e/${name}.spec.ts`;
    const requirements = await loadRequirements([`frontend/${spec}`], fileURLToPath(new URL("../../", import.meta.url)));
    const expected = name === "theme-picker" ? [spec, "e2e/ui-visual-regression.spec.ts"] : [spec];
    assert.deepEqual(requirements.browser, expected, name);
  }
});

test("production native harness changes require all desktop platforms while test-only edits stay focused", async () => {
  const { loadRequirements } = await import("../validation/pr-scenarios.mjs");
  const { fileURLToPath } = await import("node:url");
  const root = fileURLToPath(new URL("../../", import.meta.url));
  for (const name of ["hidden-contacts", "hidden-contacts-report", "native-core-scenarios", "native-capture"]) {
    assert.deepEqual((await loadRequirements([`scripts/diagnostics/${name}.mjs`], root)).native, ["darwin", "linux", "win32"], name);
    assert.deepEqual((await loadRequirements([`scripts/diagnostics/${name}.test.mjs`], root)).native, [], name);
  }
});
