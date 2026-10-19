import assert from "node:assert/strict";
import test from "node:test";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  rmSync,
  renameSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { spawn, spawnSync } from "node:child_process";
import {
  discoverChanges,
  executePlan,
  parseOptions,
  prepareRust,
  validateFrontendReport,
} from "../validation/changed.mjs";

function repository(t) {
  const root = mkdtempSync(path.join(tmpdir(), "mesh-affected-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const git = (args) => {
    const result = spawnSync("git", args, { cwd: root, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  const write = (file, body = "one") => {
    mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    writeFileSync(path.join(root, file), body);
  };
  git(["init", "-q"]);
  write("frontend/src/old.ts");
  write("staged");
  write("dirty");
  write("deleted");
  git(["add", "."]);
  git([
    "-c",
    "user.name=Test",
    "-c",
    "user.email=test@example.test",
    "commit",
    "-qm",
    "fixture",
  ]);
  return { root, git, write };
}

test("change discovery includes dirty, staged, untracked, deleted and both rename paths", (t) => {
  const f = repository(t);
  f.write("dirty", "two");
  f.write("staged", "two");
  f.git(["add", "staged"]);
  f.write("untracked file");
  rmSync(path.join(f.root, "deleted"));
  renameSync(
    path.join(f.root, "frontend/src/old.ts"),
    path.join(f.root, "frontend/src/new.ts"),
  );
  const changes = discoverChanges(f.root, "HEAD");
  assert.deepEqual(changes.files, [
    "deleted",
    "dirty",
    "frontend/src/new.ts",
    "frontend/src/old.ts",
    "staged",
    "untracked file",
  ]);
  assert.match(changes.base, /^[a-f0-9]{40}$/);
});

test("comparison uses merge base and includes committed changes", (t) => {
  const f = repository(t);
  const base = f.git(["rev-parse", "HEAD"]);
  f.write("committed");
  f.git(["add", "."]);
  f.git([
    "-c",
    "user.name=Test",
    "-c",
    "user.email=test@example.test",
    "commit",
    "-qm",
    "next",
  ]);
  assert.deepEqual(discoverChanges(f.root, base).files, ["committed"]);
  assert.throws(() => discoverChanges(f.root, "--output=unsafe"), /base/i);
});

test("CLI defaults to preview and requires explicit run, strict options and selection reason", () => {
  assert.equal(parseOptions([]).run, false);
  assert.equal(parseOptions(["--run"]).run, true);
  assert.throws(() => parseOptions(["--bogus"]), /unknown/i);
  assert.throws(() => parseOptions(["--execute"]), /unknown/i);
  assert.throws(() => parseOptions(["--base"]), /value/i);
  assert.throws(() => parseOptions(["--owner", "profile"]), /reason/i);
});

const plan = {
  rust: [{ package: "core", filters: ["profile::"] }],
  frontend: [],
  node: [],
  blockers: [],
};
const binary = "/tmp/test-core";
const artifact = JSON.stringify({
  reason: "compiler-artifact",
  target: { kind: ["lib"] },
  executable: binary,
});

test("Rust inventory compiles only selected packages and rejects zero/ignored-only filters before execution", async () => {
  const calls = [];
  const run = async (program, args) => {
    calls.push([program, args]);
    return {
      code: 0,
      output:
        program === "cargo"
          ? artifact
          : args.includes("--ignored")
            ? "profile::ignored: test\n"
            : "profile::actual: test\nprofile::ignored: test\n",
    };
  };
  const prepared = await prepareRust(plan.rust, { run, log: () => {} });
  assert.equal(prepared[0].count, 1);
  assert.ok(!calls[0][1].includes("--workspace"));
  assert.ok(
    calls.every(
      ([program, args]) => program === "cargo" || args.includes("--list"),
    ),
  );
  await assert.rejects(
    prepareRust([{ package: "core", filters: ["missing::"] }], {
      run,
      log: () => {},
    }),
    /zero.*missing/,
  );
  await assert.rejects(
    prepareRust([{ package: "core", filters: ["profile::ignored"] }], {
      run,
      log: () => {},
    }),
    /zero.*ignored/,
  );
});

test("failed compilation never authorizes test execution", async () => {
  await assert.rejects(
    prepareRust(plan.rust, {
      run: async () => ({ code: 1, output: "compile failed" }),
      log: () => {},
    }),
    /compile failed/,
  );
});

test("blockers stop all commands and failed selected tests stop later work", async () => {
  let calls = 0;
  await assert.rejects(
    executePlan(
      { ...plan, blockers: ["unmapped"] },
      {
        run: async () => {
          calls++;
        },
        log: () => {},
      },
    ),
    /blocked/i,
  );
  assert.equal(calls, 0);
  await assert.rejects(
    executePlan(
      {
        rust: [],
        blockers: [],
        node: ["scripts/test.test.mjs"],
        frontend: ["src/a.test.ts"],
      },
      {
        run: async () => {
          calls++;
          return { code: 1, output: "test failure" };
        },
        log: () => {},
      },
    ),
    /test failure/,
  );
  assert.equal(calls, 1);
});

test("selected frontend and Node files remain literal argv and no full suite is inferred", async () => {
  const calls = [];
  const root = path.resolve("fixture-repo");
  await executePlan(
    {
      rust: [],
      blockers: [],
      node: ["scripts/a.test.mjs"],
      frontend: ["src/a.test.ts"],
    },
    {
      cwd: root,
      log: () => {},
      run: async (program, args, options) => {
        calls.push([program, args, options.cwd]);
        if (args.includes("--outputFile"))
          writeFileSync(
            args[args.indexOf("--outputFile") + 1],
            JSON.stringify({
              testResults: [
                {
                  name: path.join(root, "frontend/src/a.test.ts"),
                  assertionResults: [{ status: "passed" }],
                },
              ],
            }),
          );
        return {
          code: 0,
          output: args.includes("--outputFile")
            ? "passed"
            : "# Subtest: validates behavior\r\nok 1 - validates behavior\r\n# pass 1\r\n",
        };
      },
    },
  );
  assert.deepEqual(calls[0][1], [
    "--test",
    "--test-reporter=tap",
    "scripts/a.test.mjs",
  ]);
  assert.equal(
    calls[1][1][0],
    path.join(root, "frontend/node_modules/vitest/vitest.mjs"),
  );
  assert.deepEqual(calls[1][1].slice(1, 5), [
    "run",
    "src/a.test.ts",
    "--reporter=json",
    "--outputFile",
  ]);
  assert.equal(calls[1][2], path.join(root, "frontend"));
});

test("every selected frontend file must execute an assertion; missing and skipped files fail", () => {
  const report = {
    testResults: [
      {
        name: "/repo/frontend/src/a.test.ts",
        assertionResults: [{ status: "passed" }],
      },
    ],
  };
  assert.equal(
    validateFrontendReport(report, ["src/a.test.ts"], "/repo/frontend"),
    1,
  );
  assert.throws(
    () => validateFrontendReport(report, ["src/b.test.ts"], "/repo/frontend"),
    /zero.*b.test.ts/,
  );
  assert.throws(
    () =>
      validateFrontendReport(
        {
          testResults: [
            {
              name: "/repo/frontend/src/a.test.ts",
              assertionResults: [{ status: "pending" }],
            },
          ],
        },
        ["src/a.test.ts"],
        "/repo/frontend",
      ),
    /zero/,
  );
  assert.throws(
    () => validateFrontendReport({}, ["src/a.test.ts"], "/repo/frontend"),
    /report/i,
  );
});

test("Node diagnostics with only skipped tests fail rather than authorizing an empty run", async () => {
  await assert.rejects(
    executePlan(
      { rust: [], blockers: [], node: ["scripts/skip.test.mjs"], frontend: [] },
      {
        run: async () => ({
          code: 0,
          output: "# tests 1\n# pass 0\n# skipped 1\n",
        }),
        log: () => {},
      },
    ),
    /zero.*Node/i,
  );
});

test("a synthetic passing Node file wrapper cannot stand in for declared tests", async () => {
  await assert.rejects(
    executePlan(
      {
        rust: [],
        blockers: [],
        node: ["scripts/empty.test.mjs"],
        frontend: [],
      },
      {
        run: async () => ({
          code: 0,
          output:
            "# Subtest: scripts/empty.test.mjs\nok 1 - scripts/empty.test.mjs\n# pass 1\n",
        }),
        log: () => {},
      },
    ),
    /zero.*Node/i,
  );
});

function configureCLI(f, body) {
  const manifests = [
    "scripts/validation/validation.json",
    "frontend/validation.json",
    "crates/mesh-talk-core/validation.json",
    "src-tauri/validation.json",
    "crates/mesh-talk-integrations/validation.json",
    "crates/mesh-talk-mcp/validation.json",
  ];
  for (const file of manifests)
    f.write(
      file,
      JSON.stringify({
        schema: 1,
        contracts:
          file === manifests[0]
            ? [
                {
                  id: "fixture",
                  paths: ["scripts/"],
                  node: ["scripts/fixture.test.mjs"],
                },
              ]
            : [],
      }),
    );
  f.write(".gitignore", "target/\n");
  f.write("scripts/fixture.test.mjs", body);
  f.git(["add", "."]);
  f.git([
    "-c",
    "user.name=Test",
    "-c",
    "user.email=test@example.test",
    "commit",
    "-qm",
    "configure CLI",
  ]);
}
const cli = fileURLToPath(
  new URL("../validation/changed.mjs", import.meta.url),
);

test("real CLI preview is read-only and emits an explainable plan without executing selected tests", (t) => {
  const f = repository(t);
  configureCLI(f, "throw Error('preview executed a test');");
  const result = spawnSync(
    process.execPath,
    [cli, "--json", "--owner", "fixture", "--reason", "preview fixture"],
    { cwd: f.root, encoding: "utf8" },
  );
  assert.equal(result.status, 0, result.stderr);
  const plan = JSON.parse(result.stdout);
  assert.deepEqual(plan.node, ["scripts/fixture.test.mjs"]);
  assert.match(plan.owners[0].reasons[0], /preview fixture/);
  assert.equal(f.git(["status", "--porcelain"]), "");
});

test(
  "cancelling real CLI execution terminates test descendants and releases its resource lock",
  {
    timeout: 20000,
    skip:
      process.platform === "win32"
        ? "POSIX signal delivery; stage cancellation is tested separately on Windows"
        : false,
  },
  async (t) => {
    const f = repository(t);
    configureCLI(
      f,
      "import test from 'node:test'; import {writeFileSync} from 'node:fs'; test('wait for cancellation', async () => { writeFileSync('target/fixture-pid', String(process.pid)); await new Promise(resolve => setTimeout(resolve, 30000)); });",
    );
    const child = spawn(
      process.execPath,
      [cli, "--run", "--owner", "fixture", "--reason", "cancellation fixture"],
      {
        cwd: f.root,
        env: {
          ...process.env,
          NODE_TEST_CONTEXT: undefined,
          MESH_VALIDATION_AFFECTED_CHILD: undefined,
        },
        stdio: ["ignore", "pipe", "pipe"],
      },
    );
    let output = "";
    child.stdout.on("data", (chunk) => (output += chunk));
    child.stderr.on("data", (chunk) => (output += chunk));
    const exited = new Promise((resolve) =>
      child.once("exit", (code, signal) => resolve({ code, signal })),
    );
    let descendant;
    t.after(() => {
      child.kill("SIGKILL");
      if (descendant) {
        try {
          process.kill(descendant, "SIGKILL");
        } catch {}
      }
    });
    const { readFile, readdir } = await import("node:fs/promises");
    for (let attempts = 0; attempts < 150; attempts++) {
      try {
        descendant = Number(
          await readFile(path.join(f.root, "target/fixture-pid"), "utf8"),
        );
        break;
      } catch {
        await delay(50);
      }
    }
    assert.ok(descendant, output);
    child.kill("SIGTERM");
    const result = await exited;
    assert.notEqual(result.code, 0, output);
    let alive = true;
    for (let attempts = 0; attempts < 50; attempts++) {
      try {
        process.kill(descendant, 0);
        await delay(20);
      } catch {
        alive = false;
        break;
      }
    }
    assert.equal(alive, false, "test descendant survived cancellation");
    assert.deepEqual(
      await readdir(path.join(f.root, ".git/validation-resources")),
      [],
    );
  },
);

test(
  "a changed resource class while queued fails before any Rust compilation",
  { timeout: 20000 },
  async (t) => {
    const f = repository(t);
    configureCLI(f, "import test from 'node:test'; test('fixture', () => {});");
    f.write(
      "scripts/fixture.test.mjs",
      "import test from 'node:test'; test('dirty fixture', () => {});",
    );
    const { createHash } = await import("node:crypto");
    const { mkdir, rm } = await import("node:fs/promises");
    const lock = path.join(
      f.root,
      ".git/validation-resources",
      createHash("sha256").update("focused-node").digest("hex"),
    );
    await mkdir(lock, { recursive: true });
    writeFileSync(
      path.join(lock, "owner.json"),
      JSON.stringify({ pid: process.pid }),
    );
    const child = spawn(process.execPath, [cli, "--run"], {
      cwd: f.root,
      env: {
        ...process.env,
        NODE_TEST_CONTEXT: undefined,
        MESH_VALIDATION_AFFECTED_CHILD: undefined,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let output = "";
    child.stdout.on("data", (chunk) => (output += chunk));
    child.stderr.on("data", (chunk) => (output += chunk));
    const exited = new Promise((resolve) =>
      child.once("exit", (code) => resolve(code)),
    );
    t.after(() => child.kill("SIGKILL"));
    for (
      let attempts = 0;
      attempts < 150 &&
      !output.includes("waiting for another validation stage");
      attempts++
    )
      await delay(50);
    assert.match(output, /waiting for another validation stage/);
    const manifest = "scripts/validation/validation.json";
    const { readFile } = await import("node:fs/promises");
    const changed = JSON.parse(
      await readFile(path.join(f.root, manifest), "utf8"),
    );
    changed.contracts[0].rust = [{ package: "core", filters: ["fixture::"] }];
    f.write(manifest, JSON.stringify(changed));
    await rm(lock, { recursive: true });
    assert.notEqual(await exited, 0, output);
    assert.match(output, /resource class changed/i);
  },
);
