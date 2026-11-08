import assert from "node:assert/strict";
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "mesh standards wiring "));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, "scripts/validation"), { recursive: true });
  writeFileSync(
    join(root, "scripts/validation/standards.mjs"),
    "console.error('fixture standards gap');process.exit(23);\n",
  );
  return root;
}

test(
  "pre-commit stops on structural gaps before formatting or success",
  { skip: process.platform === "win32" },
  (t) => {
    const root = fixture(t);
    spawnSync("git", ["init", "-q"], { cwd: root });
    writeFileSync(join(root, "scripts/check-repository-hygiene.mjs"), "");
    copyFileSync(resolve("hooks/pre-commit"), join(root, "hook"));
    const run = spawnSync("bash", ["hook"], { cwd: root, encoding: "utf8" });
    assert.equal(run.status, 23, run.stderr || run.stdout);
    assert.match(run.stderr, /fixture standards gap/);
    assert.doesNotMatch(run.stdout, /Source checks passed/);
  },
);

test("health rejects standards gaps before acquiring managed build resources", (t) => {
  const root = fixture(t);
  for (const dir of ["frontend/node_modules", "src-tauri"])
    mkdirSync(join(root, dir), { recursive: true });
  writeFileSync(join(root, "frontend/node_modules/.package-lock.json"), "{}");
  writeFileSync(join(root, "Makefile"), "");
  copyFileSync(resolve("scripts/check-health.sh"), join(root, "health.sh"));
  const run = spawnSync("bash", ["health.sh", "--fast"], {
    cwd: root,
    encoding: "utf8",
    env: { ...process.env, MESH_VALIDATION_HEALTH_GUARDED: "0" },
  });
  assert.equal(run.status, 23, run.stderr || run.stdout);
  assert.match(run.stderr, /fixture standards gap/);
});

test(
  "make standards-check propagates checker failure",
  { skip: process.platform === "win32" },
  (t) => {
    const root = fixture(t);
    copyFileSync(resolve("Makefile"), join(root, "Makefile"));
    const run = spawnSync("make", ["standards-check"], {
      cwd: root,
      encoding: "utf8",
    });
    assert.notEqual(run.status, 0);
    assert.match(run.stderr, /fixture standards gap/);
  },
);

test("Health Linux owns full-source standards after dependencies", () => {
  const workflow = readFileSync(resolve(".github/workflows/check-health.yml"), "utf8");
  const platform = workflow.split("  platform:\n")[1].split("  source:\n")[0];
  const steps = platform.split(/^      - /m);
  const gate = steps.find(value => value.startsWith("name: Check full-source standards and hygiene\n"));
  assert.ok(gate, "full-source standards must have one canonical Health gate");
  assert.match(gate, /if: \$\{\{ steps\.scope\.outputs\.full == 'true' && \(runner\.os == 'Linux'\) \}\}/u);
  assert.match(gate, /run: \|\n          node scripts\/validation\/standards\.mjs\n          node scripts\/check-repository-hygiene\.mjs/u);
  assert.match(platform, /node-version: '22'/u);
  assert.match(platform, /shell: bash/u);
  assert.ok(platform.indexOf("uses: actions/setup-node@") < platform.indexOf("name: Install frontend dependencies"));
  const install = steps.findIndex(value => value.startsWith("name: Install frontend dependencies\n"));
  assert.match(steps[install], /working-directory: frontend\n        run: npm ci/u);
  assert.equal(steps[install + 1], gate);
  assert.equal((platform.match(/node scripts\/validation\/standards\.mjs/gu) ?? []).length, 1);
});

test("composition owners retain conservative unit selections and mandatory browser boundaries", async () => {
  const { buildAffectedPlan } = await import("../validation/affected.mjs");
  const { loadContracts } = await import("../validation/contracts.mjs");
  const { loadRequirements } = await import("../validation/pr-scenarios.mjs");
  const root = resolve(".");
  const contracts = await loadContracts(root);
  for (const [paths, unit, browser] of [
    [
      [
        "App.tsx",
        "main.tsx",
        "index.css",
        "vite-env.d.ts",
        "components/ErrorBoundary.tsx",
      ],
      "src/store/appLock.test.ts",
      "e2e/chat.spec.ts",
    ],
    [
      [
        "features/auth/LoginScreen.tsx",
        "features/auth/RestoreBackup.tsx",
        "components/ui/label.tsx",
      ],
      "src/store/auth.test.ts",
      "e2e/recovery.spec.ts",
    ],
    [
      ["features/chat/ChatApp.tsx"],
      "src/store/chat.test.ts",
      "e2e/message-lifecycle.spec.ts",
    ],
    [
      ["components/WindowControls.tsx"],
      "src/lib/platform.test.ts",
      "e2e/window-controls.spec.ts",
    ],
  ]) {
    for (const path of paths) {
      const file = `frontend/src/${path}`;
      const plan = buildAffectedPlan([file], { contracts });
      assert.deepEqual(plan.blockers, [], file);
      assert.ok(plan.frontend.includes(unit), `${file}: missing ${unit}`);
      assert.match(plan.escalation.join(" "), /browser|native/);
      const requirements = await loadRequirements([file], root);
      assert.ok(
        requirements.browser.includes(browser),
        `${file}: missing required ${browser}`,
      );
    }
  }
});
