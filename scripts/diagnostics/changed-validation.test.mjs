import assert from "node:assert/strict";
import test from "node:test";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  rmSync,
  renameSync,
  chmodSync,
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
  executionFingerprint,
} from "../validation/changed.mjs";

const isolatedEnv = { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined };

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

test("phase fingerprints bind dirty source bytes, absent inputs and mode changes", async t => {
  const f = repository(t);
  const selected = { files: ["dirty", "deleted"] };
  const before = await executionFingerprint(f.root, selected);
  f.write("dirty", "two");
  assert.notEqual(await executionFingerprint(f.root, selected), before);
  f.write("dirty", "one");
  assert.equal(await executionFingerprint(f.root, selected), before);
  rmSync(path.join(f.root, "deleted"));
  const removed = await executionFingerprint(f.root, selected);
  assert.notEqual(removed, before);
  f.write("deleted", "absent");
  assert.notEqual(await executionFingerprint(f.root, selected), removed);
  if (process.platform !== "win32") {
    const regular = await executionFingerprint(f.root, selected);
    chmodSync(path.join(f.root, "dirty"), 0o755);
    assert.notEqual(await executionFingerprint(f.root, selected), regular);
  }
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

for (const { editWhileQueued, managed } of [{ editWhileQueued: false }, { editWhileQueued: true }, { editWhileQueued: true, managed: true }]) test(
  `cheap static checks run before a busy ${managed ? "managed" : "heavy"} lock${editWhileQueued ? " and edits invalidate their proof" : ""}`,
  { timeout: 20000, skip: process.platform === "win32" ? "POSIX executable fixture; phase partitioning is platform-independent" : false },
  async t => {
    const f = repository(t);
    configureCLI(f, "import test from 'node:test'; test('fixture', () => {});");
    const { readFile, mkdir, rm, access } = await import("node:fs/promises");
    const { createHash } = await import("node:crypto");
    for (const name of ["stage.mjs", "stage-inputs.mjs"])
      f.write(`scripts/validation/${name}`, await readFile(new URL(`../validation/${name}`, import.meta.url), "utf8"));
    const manifest = "scripts/validation/validation.json";
    const data = JSON.parse(await readFile(path.join(f.root, manifest), "utf8"));
    data.contracts[0].rust = [{ package: "core", filters: ["fixture::"] }];
    f.write(manifest, JSON.stringify(data));
    if (managed) {
      f.write("scripts/build-retention.py", "import pathlib,subprocess,sys,time\npathlib.Path('target/managed-wrapper-entered').write_text('waiting')\nwhile not pathlib.Path('target/allow-managed').exists(): time.sleep(.05)\nsys.exit(subprocess.call(sys.argv[sys.argv.index('--')+1:]))\n");
      // This is fixture infrastructure, not a retention implementation change.
      // Keep it in the baseline so selection cannot add an unrelated Python
      // test stage before the formatting marker this regression observes.
      f.git(["add", "scripts/build-retention.py"]);
      f.git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "configure managed wrapper"]);
    }
    await mkdir(path.join(f.root, "target/bin"), { recursive: true });
    const cargo = path.join(f.root, "target/bin/cargo");
    f.write("target/bin/cargo", `#!/usr/bin/env node\nconst fs=require('node:fs'); if(process.argv[2]==='--version'){console.log('cargo fixture');}else if(process.argv[2]==='fmt'){fs.writeFileSync('target/fast-static-ran','yes');}else{fs.writeFileSync('target/heavy-command-ran','yes');process.exit(9);}`);
    chmodSync(cargo, 0o755);
    const lock = path.join(f.root, ".git/validation-resources", createHash("sha256").update("heavy").digest("hex"));
    await mkdir(lock, { recursive: true });
    writeFileSync(path.join(lock, "owner.json"), JSON.stringify({ pid: process.pid }));
    const child = spawn(process.execPath, [cli, "--checks", "--run", "--report", "target/validation/feedback/queued.json", ...(managed ? ["--managed"] : []), "--owner", "fixture", "--reason", "phase fixture"], {
      cwd: f.root, env: { ...isolatedEnv, NODE_TEST_CONTEXT: undefined, MESH_VALIDATION_AFFECTED_CHILD: undefined, MESH_VALIDATION_AFFECTED_PHASE: undefined, MESH_VALIDATION_AFFECTED_INPUTS: undefined, PATH: `${path.dirname(cargo)}${path.delimiter}${process.env.PATH}` },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let output = "";
    child.stdout.on("data", chunk => output += chunk);
    child.stderr.on("data", chunk => output += chunk);
    const exited = new Promise(resolve => child.once("exit", code => resolve(code)));
    t.after(() => child.kill("SIGKILL"));
    let ready = false;
    for (let tries = 0; tries < 150; tries++) {
      try { await access(path.join(f.root, "target/fast-static-ran")); ready = true; break; }
      catch { await delay(50); }
    }
    assert(ready, `cheap formatting waited for the heavy lock: ${output}`);
    // Wait until the complete fast phase passed, so this tests the queued handoff.
    if (managed) {
      let waiting = false;
      for (let tries = 0; tries < 150; tries++) {
        try { await access(path.join(f.root, "target/managed-wrapper-entered")); waiting = true; break; }
        catch { await delay(50); }
      }
      assert(waiting, output);
    } else {
      for (let tries = 0; tries < 150 && !output.includes("waiting for another validation stage"); tries++) await delay(50);
      assert.match(output, /waiting for another validation stage/);
    }
    await assert.rejects(access(path.join(f.root, "target/heavy-command-ran")));
    if (editWhileQueued) {
      f.write("scripts/fixture.test.mjs", "import test from 'node:test'; test('changed fixture', () => {});");
      if (managed) f.write("target/allow-managed", "release");
      await rm(lock, { recursive: true });
      assert.notEqual(await exited, 0);
      assert.match(output, /inputs changed|plan changed/i);
      const feedback = JSON.parse(await readFile(path.join(f.root, "target/validation/feedback/queued.json"), "utf8"));
      assert.equal(feedback.status, "invalidated");
      assert.equal(feedback.phases[0].status, "passed");
      assert.equal(feedback.phases[1].status, "invalidated");
      await assert.rejects(access(path.join(f.root, "target/heavy-command-ran")));
    } else {
      child.kill("SIGTERM");
      assert.notEqual(await exited, 0);
    }
  },
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

test("an empty recomputed child plan still validates the queued phase handoff", t => {
  const f = repository(t);
  configureCLI(f, "import test from 'node:test'; test('fixture', () => {});");
  const result = spawnSync(process.execPath, [cli, "--checks"], {
    cwd: f.root, encoding: "utf8", env: { ...isolatedEnv,
      MESH_VALIDATION_AFFECTED_CHILD: "focused-node", MESH_VALIDATION_AFFECTED_PHASE: "tests",
      MESH_VALIDATION_AFFECTED_INPUTS: "old-source-fingerprint" },
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /inputs changed/i);
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
      [cli, "--run", "--owner", "fixture", "--reason", "cancellation fixture", "--report", "target/validation/feedback/cancelled.json"],
      {
        cwd: f.root,
        env: {
          ...isolatedEnv,
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
    const feedback = JSON.parse(await readFile(path.join(f.root, "target/validation/feedback/cancelled.json"), "utf8"));
    assert.equal(feedback.status, "cancelled");
    assert.equal(feedback.candidateReady, false);
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
        ...isolatedEnv,
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
    assert.match(output, /resource class changed|inputs changed/i);
  },
);

test("boundary additions require an explanation and report output requires execution", () => {
  assert.throws(() => parseOptions(["--browser", "e2e/a.spec.ts"]), /reason/i);
  assert.throws(() => parseOptions(["--native", "host"]), /reason/i);
  assert.throws(() => parseOptions(["--report", "tmp/result.json"]), /run/i);
  const options = parseOptions(["--browser", "e2e/a.spec.ts", "--native", "host", "--reason", "IPC consumer review"]);
  assert.deepEqual(options.browser, ["e2e/a.spec.ts"]);
  assert.deepEqual(options.native, ["host"]);
});

test("real preview unions mandatory boundary evidence with semantic additions without executing it", async t => {
  const f = repository(t);
  configureCLI(f, "throw Error('must not execute preview');");
  f.write("scripts/fixture.test.mjs", "throw Error('must not execute preview'); // changed");
  f.write("frontend/e2e/mandatory.spec.ts", "// browser");
  f.write("frontend/e2e/additional.spec.ts", "// browser");
  f.write("frontend/validation.json", JSON.stringify({ schema: 1, contracts: [], prScenarios: [{ id: "required-visual", paths: ["scripts/fixture.test.mjs"], browser: ["e2e/mandatory.spec.ts"], native: ["host"] }] }));
  f.git(["add", "."]);
  f.git(["-c", "user.name=Test", "-c", "user.email=test@example.test", "commit", "-qm", "boundary fixtures"]);
  f.write("scripts/fixture.test.mjs", "throw Error('must not execute preview'); // another change");
  const result = spawnSync(process.execPath, [cli, "--json", "--owner", "fixture", "--browser", "e2e/additional.spec.ts", "--reason", "dynamic behavior"], { cwd: f.root, encoding: "utf8", env: isolatedEnv });
  assert.equal(result.status, 0, result.stderr);
  const plan = JSON.parse(result.stdout);
  assert.deepEqual(plan.requirements.browser, ["e2e/additional.spec.ts", "e2e/mandatory.spec.ts"]);
  assert.deepEqual(plan.requirements.native, [process.platform]);
  const missing = spawnSync(process.execPath, [cli, "--json", "--owner", "fixture", "--browser", "e2e/missing.spec.ts", "--reason", "dynamic behavior"], { cwd: f.root, encoding: "utf8", env: isolatedEnv });
  assert.notEqual(missing.status, 0);
  assert.match(missing.stderr, /missing.*browser/i);
});

for (const failing of [false, true]) test(`real run reports ${failing ? "failing" : "passing"} development tests while candidate acceptance remains pending`, async t => {
  const f = repository(t);
  configureCLI(f, `import test from 'node:test'; test('observed assertion', () => { ${failing ? "console.log('private output sentinel'); throw Error('assertion failed');" : ""} });`);
  const reportPath = path.join(f.root, "target/validation/feedback/result.json");
  const result = spawnSync(process.execPath, [cli, "--run", "--owner", "fixture", "--reason", "report fixture", "--report", reportPath], { cwd: f.root, encoding: "utf8", env: { ...isolatedEnv, NODE_TEST_CONTEXT: undefined }, timeout: 15000 });
  assert.equal(result.status, failing ? 1 : 0, result.stdout + result.stderr);
  const { readFile } = await import("node:fs/promises");
  const report = JSON.parse(await readFile(reportPath, "utf8"));
  assert.equal(report.status, failing ? "failed" : "passed");
  assert.equal(report.timingOrigin, "invocation");
  assert(report.timing.firstCommandSeconds >= 0);
  assert(report.timing.completionSeconds >= report.timing.firstCommandSeconds);
  assert.equal(report.timing.firstIssue?.kind ?? null, failing ? "failed" : null);
  assert(!JSON.stringify(report).includes("private output sentinel"));
  assert.equal(report.candidateReady, false);
  assert.equal(report.phases[0].commands[0].status, failing ? "failed" : "passed");
  assert(report.next.some(item => item.kind === "candidate" && item.status === "not-run"));
});

for (const blocked of [false, true]) test(`real ${blocked ? "blocked" : "empty"} run produces a report without claiming tests passed`, async t => {
  const f = repository(t);
  configureCLI(f, "throw Error('unselected test must not execute');");
  f.write(blocked ? "unmapped.rs" : "docs/example.md", "changed");
  const result = spawnSync(process.execPath, [cli, "--run", "--report", "target/validation/feedback/terminal.json"], { cwd: f.root, encoding: "utf8", env: isolatedEnv, timeout: 10000 });
  assert.equal(result.status, blocked ? 2 : 0, result.stdout + result.stderr);
  const { readFile } = await import("node:fs/promises");
  const report = JSON.parse(await readFile(path.join(f.root, "target/validation/feedback/terminal.json"), "utf8"));
  assert.equal(report.status, blocked ? "blocked" : "no-checks");
  assert.equal(report.candidateReady, false);
});

test("clean receipt reuse survives changing only the report destination and invents no commands", async t => {
  const f = repository(t);
  configureCLI(f, "import test from 'node:test'; test('reusable assertion', () => {});");
  const { readFile } = await import("node:fs/promises");
  for (const name of ["first", "second"]) {
    const result = spawnSync(process.execPath, [cli, "--run", "--owner", "fixture", "--reason", "reuse", "--report", `target/validation/feedback/${name}.json`], { cwd: f.root, encoding: "utf8", env: { ...isolatedEnv, CI: "", NODE_TEST_CONTEXT: undefined }, timeout: 15000 });
    assert.equal(result.status, 0, result.stdout + result.stderr);
  }
  const report = JSON.parse(await readFile(path.join(f.root, "target/validation/feedback/second.json"), "utf8"));
  assert.equal(report.status, "passed");
  assert.equal(report.phases[0].reused, true);
  assert.equal(report.timing.firstCommandSeconds, null);
  assert.equal(report.timing.reusedPhases, 1);
  assert.deepEqual(report.phases[0].commands, []);
  assert(report.phases[0].evidence.receipt);
});

test("report destinations cannot overwrite source or keep the supervisor alive after rejection", t => {
  const f = repository(t);
  configureCLI(f, "throw Error('must not execute');");
  const result = spawnSync(process.execPath, [cli, "--run", "--report", "frontend/source.json"], { cwd: f.root, encoding: "utf8", env: isolatedEnv, timeout: 3000 });
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stderr, /ignored target/);
  assert.equal(result.error, undefined);
});

test("dirty edits during a test-only run invalidate its report even after assertions pass", async t => {
  const f = repository(t);
  configureCLI(f, "import test from 'node:test'; import {writeFileSync} from 'node:fs'; test('changing input', async () => { await new Promise(resolve => setTimeout(resolve, 50)); writeFileSync('dirty','changed during validation'); });");
  f.write("dirty", "dirty before validation");
  const result = spawnSync(process.execPath, [cli, "--run", "--owner", "fixture", "--reason", "dirty source identity", "--report", "target/validation/feedback/dirty.json"], { cwd: f.root, encoding: "utf8", env: { ...isolatedEnv, NODE_TEST_CONTEXT: undefined }, timeout: 15000 });
  assert.equal(result.status, 1, result.stdout + result.stderr);
  const { readFile } = await import("node:fs/promises");
  const report = JSON.parse(await readFile(path.join(f.root, "target/validation/feedback/dirty.json"), "utf8"));
  assert.equal(report.status, "invalidated");
  assert.equal(report.timing.firstIssue.kind, "invalidated");
  assert.equal(report.dirty, true);
  assert.equal(report.phases[0].commands[0].status, "passed");
  assert.equal(report.candidateReady, false);
});

test("a malformed planning contract produces blocked feedback instead of a passing or missing report", async t => {
  const f = repository(t);
  configureCLI(f, "throw Error('must not execute');");
  f.write("frontend/validation.json", JSON.stringify({ schema: 1, contracts: [{ id: "broken", paths: [], capability: { summary: "missing required fields" } }] }));
  const result = spawnSync(process.execPath, [cli, "--run", "--report", "target/validation/feedback/blocked.json"], { cwd: f.root, encoding: "utf8", env: isolatedEnv, timeout: 10000 });
  assert.equal(result.status, 1, result.stdout + result.stderr);
  const { readFile } = await import("node:fs/promises");
  const report = JSON.parse(await readFile(path.join(f.root, "target/validation/feedback/blocked.json"), "utf8"));
  assert.equal(report.status, "blocked");
  assert(report.blockers.some(item => /capability/i.test(item)));
});
