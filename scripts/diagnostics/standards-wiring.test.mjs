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

test("CI checks standards after dependency setup and before building frontend", () => {
  const workflow = readFileSync(resolve(".github/workflows/ci.yml"), "utf8");
  const step = workflow
    .split(/^      - /m)
    .find((value) =>
      value.startsWith(
        "name: Install frontend dependencies and check standards\n",
      ),
    );
  assert.ok(
    step,
    "standards must be a required step in the existing platform checks",
  );
  assert.match(step, /shell: bash/);
  assert.ok(
    step.indexOf("npm ci --prefix frontend") <
      step.indexOf("node scripts/validation/standards.mjs"),
  );
  assert.ok(
    workflow.indexOf(
      "name: Install frontend dependencies and check standards",
    ) < workflow.indexOf("name: Build frontend"),
  );
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
