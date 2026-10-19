import assert from "node:assert/strict";
import test from "node:test";
import { buildPlan, hasSelectedTests, parseFlags, selectWindowsCargoHome, validatePlan, validateWindowsRoot } from "../pr-platform/plan.mjs";
import { executePlan, expectedSteps, rustTestCount, runCommand } from "../pr-platform/guest.mjs";
import { collectPlatforms, createArchive, validateGuestReport, windowsBootstrap } from "../pr-platform/run.mjs";

const sha = "a".repeat(40);

test("one explicit test plan preserves selectors as argv data on both platforms", async () => {
  const flags = parseFlags(["--core-filter", "name;$(touch sentinel)", "--host-filter", "owner_commands", "e2e/privacy.spec.ts"]);
  const plan = buildPlan(flags, ["crates/mesh-talk-core/src/node.rs", "src-tauri/src/commands.rs"]);
  assert.deepEqual(plan.coreFilters, ["name;$(touch sentinel)"]);
  assert.deepEqual(plan.uiSpecs, ["e2e/privacy.spec.ts"]);
  assert.equal(plan.frontendBuild, true);
  const executions = [];
  for (const environment of [{ CARGO_HOME: "/cache/cargo" }, { USERPROFILE: "D:/cache/profile", EVAL_SOURCE_SHA: "inherited-stale-sha" }]) {
    const commands = [];
    await executePlan({ ...plan, sourceSha: sha }, { env: environment, run: async (program, args, options) => {
      commands.push([program, args, options.env.EVAL_SOURCE_SHA]);
      return { code: 0, output: "test result: ok. 1 passed; 0 failed; 0 ignored;" };
    } });
    executions.push(commands);
  }
  assert.deepEqual(executions[0], executions[1]);
  assert(executions.every(commands => commands.every(([, , source]) => source === sha)), "both platform guests must pass the pinned source identity");
  assert(executions[0].some(([program, args]) => program === "cargo" && args.includes("name;$(touch sentinel)")));
  assert(!executions[0].some(([program, args]) => program === "npm" && args[0] === "test"), "platform guests must not repeat the full frontend unit suite");
  assert(!Object.hasOwn(plan, "frontendUnit"));
});

test("missing affected Rust selectors and ambiguous flags fail closed", () => {
  assert.throws(() => buildPlan(parseFlags([]), ["crates/mesh-talk-core/src/node.rs"]), /core-filter/);
  assert.throws(() => buildPlan(parseFlags([]), ["src-tauri/src/lib.rs"]), /host-filter/);
  for (const args of [["--wat"], ["--core-filter", ""], ["--core-filter", "--all"], ["../e2e/a.spec.ts"], ["e2e/../a.spec.ts"], ["e2e/a.spec.ts;echo"]]) assert.throws(() => parseFlags(args));
  assert.equal(parseFlags(["--plan", "--base", "origin/main"]).planOnly, true);
  assert.throws(() => buildPlan(parseFlags([]), ["frontend/src/App.tsx"]), /Playwright spec/);
  assert.equal(hasSelectedTests(buildPlan(parseFlags(["e2e/privacy.spec.ts"]), ["frontend/src/App.tsx"])), true);
  assert.equal(hasSelectedTests(buildPlan(parseFlags([]), ["crates/mesh-talk-core/README.md"])), false);
  assert.deepEqual(buildPlan(parseFlags(["--core-test", "decoder_smoke"]), ["crates/mesh-talk-core/tests/decoder_smoke.rs"]).coreTests, ["decoder_smoke"]);
  assert.throws(() => parseFlags(["--core-test", "decoder;echo"]));
});

test("Windows state must stay on a validated D drive and selectors never enter PowerShell", () => {
  assert.equal(validateWindowsRoot("D:/MeshTalk/PRPreflight"), "D:/MeshTalk/PRPreflight");
  for (const value of ["C:/cache", "D:/../escape", "D:/x;write-host", "\\\\server\\share"]) assert.throws(() => validateWindowsRoot(value));
  const script = windowsBootstrap({ root: "D:/MeshTalk/PRPreflight", runId: "abc123", archiveSha: "b".repeat(64), timeoutSeconds: 1800 });
  for (const name of ["CARGO_HOME", "RUSTUP_HOME", "TEMP", "TMP", "USERPROFILE", "LOCALAPPDATA", "PLAYWRIGHT_BROWSERS_PATH"]) assert.match(script, new RegExp(name));
  assert.doesNotMatch(script, /name;\$\(/);
  assert.match(script, /Get-FileHash/);
  assert.match(script, /taskkill/);
  assert.match(script, /--windows-cargo-home/);
  assert(script.indexOf("--windows-cargo-home") < script.indexOf('$env:USERPROFILE='));
  assert.match(script, /\$env:CARGO_HOME=\"\$cargoHome\"/);
});

test("both platform failures are collected even when an adapter throws", async () => {
  const called = [];
  const results = await collectPlatforms({ linux: async () => { called.push("linux"); throw Error("Docker missing"); }, windows: async () => { called.push("windows"); return { ok: false, error: "SSH missing" }; } });
  assert.deepEqual(called.sort(), ["linux", "windows"]);
  assert.equal(results.linux.ok, false);
  assert.equal(results.windows.ok, false);
});

test("guest rejects zero Rust tests and collects later failures using shell-free argv", async () => {
  const seen = [];
  const plan = { ...buildPlan(parseFlags(["--core-filter", "literal;echo nope", "--host-filter", "owner"]), []), sourceSha: sha };
  const report = await executePlan(plan, { cwd: "/source", run: async (program, args, options) => {
    seen.push({ program, args, options });
    if (program === "cargo" && args[0] === "test") return { code: args.includes("mesh-talk-core") ? 0 : 1, output: "test result: ok. 0 passed; 0 failed; 0 ignored;" };
    return { code: 0, output: "ok" };
  } });
  assert.equal(report.ok, false);
  assert.equal(report.results.filter(r => r.kind === "rust").length, 2);
  assert.match(report.results.find(r => r.name.startsWith("core:")).error, /zero/);
  assert(seen.some(r => r.args.includes("literal;echo nope")));
  assert(seen.every(r => r.options.shell === false));
  assert.equal(rustTestCount("test result: ok. 3 passed; 0 failed; 0 ignored;"), 3);
  assert.equal(rustTestCount("test result: ok. 0 passed; 0 failed; 4 ignored;"), 0);
  assert(seen.every(r => r.options.env.CARGO_INCREMENTAL === "0"));
});

test("committed archive includes export-ignored harness without changing repository attributes", async t => {
  const { mkdtemp, mkdir, readFile, rm, writeFile } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const directory = await mkdtemp(join(tmpdir(), "mesh-committed-archive-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const repo = join(directory, "repo");
  await mkdir(join(repo, "scripts/pr-platform"), { recursive: true });
  const attributes = "/scripts export-ignore\n/hidden.txt export-ignore\n*.txt export-subst text eol=crlf\n*.mjs text eol=crlf\n";
  const guest = "export const committed = true;\n";
  await writeFile(join(repo, ".gitattributes"), attributes);
  await writeFile(join(repo, "scripts/pr-platform/guest.mjs"), guest);
  await writeFile(join(repo, "hidden.txt"), "included in verification snapshot\n");
  await writeFile(join(repo, "tracked.txt"), "$Format:%H$\n");
  const git = async args => {
    const result = await runCommand("git", args, { cwd: repo, timeoutMs: 10_000 });
    assert.equal(result.code, 0, result.output);
    return result.output.trim();
  };
  await git(["init", "--quiet"]);
  const infoAttributes = "keep -text\n";
  await writeFile(join(repo, ".git/info/attributes"), infoAttributes);
  await git(["add", "."]);
  await git(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "-m", "fixture"]);
  const committed = await git(["rev-parse", "HEAD"]);
  const archive = join(directory, "source.tar");
  const calls = [];
  assert.equal(await createArchive(repo, archive, { expectedSha: committed, run: async (program, args, options) => {
    calls.push(args);
    return runCommand(program, args, options);
  } }), committed);
  const listing = await runCommand("tar", ["-tf", "source.tar"], { cwd: directory, timeoutMs: 10_000 });
  assert.equal(listing.code, 0, listing.output);
  assert(listing.output.split(/\r?\n/u).includes("scripts/pr-platform/guest.mjs"), "export-ignore must not remove the guest runner");
  assert(listing.output.split(/\r?\n/u).includes("hidden.txt"));
  const archivedGuest = await runCommand("tar", ["-xOf", "source.tar", "scripts/pr-platform/guest.mjs"], { cwd: directory });
  assert.equal(archivedGuest.output, guest);
  const tracked = await runCommand("tar", ["-xOf", "source.tar", "tracked.txt"], { cwd: directory });
  assert.equal(tracked.output, "$Format:%H$\n", "verification source must not apply export-subst");
  assert(calls.some(args => args.includes("archive") && args.at(-1) === committed));
  assert.equal(await readFile(join(repo, ".gitattributes"), "utf8"), attributes);
  assert.equal(await readFile(join(repo, ".git/info/attributes"), "utf8"), infoAttributes);
  assert.equal(await git(["status", "--porcelain"]), "");
  await writeFile(join(repo, "tracked.txt"), "uncommitted\n");
  await assert.rejects(createArchive(repo, join(directory, "dirty.tar")), /clean committed/);
});

test("bounded child execution reports timeout rather than a successful empty run", async () => {
  const result = await runCommand(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { timeoutMs: 30 });
  assert.equal(result.timedOut, true);
  assert.notEqual(result.code, 0);
});


test("guest evidence cannot silently omit tests, broaden selectors, or claim zero tests passed", async () => {
  const plan = { ...buildPlan(parseFlags(["--core-test", "decoder_smoke"]), []), sourceSha: sha, archiveSha: "b".repeat(64) };
  const report = await executePlan(plan, { run: async () => ({ code: 0, output: "test result: ok. 2 passed; 0 failed; 0 ignored;" }) });
  assert.equal(validateGuestReport(report, plan, process.platform), report);
  assert.throws(() => validateGuestReport({ ...report, results: report.results.slice(1) }, plan, process.platform), /mismatched/);
  const broadened = structuredClone(report);
  broadened.results.at(-1).args.push("--ignored");
  assert.throws(() => validateGuestReport(broadened, plan, process.platform), /selection/);
  const empty = structuredClone(report);
  empty.results.at(-1).testCount = 0;
  assert.throws(() => validateGuestReport(empty, plan, process.platform), /Inconsistent/);
  assert.throws(() => validateGuestReport({ ...report, ok: false }, plan, process.platform), /aggregate/);
});

test("revision changes after selection fail before archive creation; docs alone need no platform adapters", async () => {
  let archived = false;
  await assert.rejects(createArchive("/repo", "/archive", { expectedSha: "b".repeat(40), run: async (_, args) => {
    if (args[0] === "archive") archived = true;
    return { code: 0, output: args[0] === "status" ? "" : sha };
  } }), /revision changed/);
  assert.equal(archived, false);
  assert.equal(hasSelectedTests(buildPlan(parseFlags([]), ["docs/README.md"])), false);
  assert.equal(hasSelectedTests(buildPlan(parseFlags(["--core-test", "decoder_smoke"]), [])), true);
});

test("selected Playwright files require real passing executions, not an all-skipped exit zero", async t => {
  const { mkdtemp, rm, writeFile } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const directory = await mkdtemp(join(tmpdir(), "mesh-ui-evidence-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const plan = { ...buildPlan(parseFlags(["e2e/privacy.spec.ts", "e2e/packs-lite.spec.ts"]), []), sourceSha: sha, archiveSha: "b".repeat(64) };
  const json = status => ({
    config: { rootDir: join(directory, "frontend/e2e") }, errors: [],
    stats: { expected: status === "passed" ? 2 : 0, skipped: status === "passed" ? 1 : 2, unexpected: 0, flaky: 0 },
    suites: ["privacy.spec.ts", "packs-lite.spec.ts"].map((file, i) => ({
      title: file, file, specs: [{ title: "runs or explicitly skips", file, id: `test-${i}`, ok: true,
        tests: [{ projectName: "chromium", expectedStatus: status === "passed" ? "passed" : "skipped", status: status === "passed" ? "expected" : "skipped", results: [{ status, errors: [] }] }],
      }, ...(status === "passed" && i === 0 ? [{ title: "individual conditional skip", file, id: "optional-skip", ok: true,
        tests: [{ projectName: "chromium", expectedStatus: "skipped", status: "skipped", results: [{ status: "skipped", errors: [] }] }],
      }] : [])],
    })),
  });
  const runReport = async raw => executePlan(plan, { cwd: directory, reportDir: directory, run: async (_, args, options) => {
    if (args.includes("test") && args.includes("--project=chromium") && options.env.PLAYWRIGHT_JSON_OUTPUT_FILE) {
      await writeFile(options.env.PLAYWRIGHT_JSON_OUTPUT_FILE, JSON.stringify(raw));
    }
    return { code: 0, output: "browser command exited zero" };
  } });
  const skipped = await runReport(json("skipped"));
  assert.equal(skipped.ok, false, "all-skipped selected files must fail despite exit zero");
  const passed = await runReport(json("passed"));
  assert.equal(passed.ok, true, "real passes plus individual skips are accepted");
  assert.equal(validateGuestReport(passed, plan, process.platform), passed);
  const missing = structuredClone(passed);
  missing.results.at(-1).ui.tests = missing.results.at(-1).ui.tests.filter(row => row.file !== "e2e/packs-lite.spec.ts");
  assert.throws(() => validateGuestReport(missing, plan, process.platform), /Playwright|spec/);
});


test("Cargo home reuses canonical D registry configuration but never a physical C home", () => {
  const fallback = "D:/MeshTalk/PRPreflight/cache/cargo";
  assert.equal(selectWindowsCargoHome("D:\\DevData\\kings\\cargo", fallback), "D:/DevData/kings/cargo");
  assert.equal(selectWindowsCargoHome("\\\\?\\D:\\DevData\\kings\\cargo", fallback), "D:/DevData/kings/cargo");
  assert.equal(selectWindowsCargoHome("C:/Users/kings/.cargo", fallback), fallback);
  assert.equal(selectWindowsCargoHome(undefined, fallback), fallback);
  assert.equal(selectWindowsCargoHome("//server/cargo", fallback), fallback);
  assert.equal(selectWindowsCargoHome("D:/../escape", fallback), fallback);
  assert.throws(() => selectWindowsCargoHome("C:/cargo", "C:/new-cache"), /D:/);
});


test("related workflow changes select the same bounded Node diagnostics without npm on both guests", async () => {
  const files = [
    "scripts/diagnostics/pr-platform.test.mjs",
    "scripts/diagnostics/create-pr.test.mjs",
    "scripts/diagnostics/pr-preflight-awake.test.mjs",
    "scripts/diagnostics/source-identity.test.mjs",
    "scripts/diagnostics/linux-target.test.mjs",
    "scripts/diagnostics/pr-preflight-output.test.mjs",
  ];
  for (const changed of ["scripts/pr-platform/guest.mjs", "scripts/pr-preflight.sh", "scripts/create-pr.sh", "scripts/evals/source-identity.mjs", ...files]) {
    const plan = { ...buildPlan(parseFlags([]), [changed]), sourceSha: sha, archiveSha: "b".repeat(64) };
    assert.deepEqual(plan.nodeDiagnostics, files, changed);
    assert.equal(hasSelectedTests(plan), true);
    const step = expectedSteps(plan).find(row => row.name === "workflow-diagnostics");
    assert.deepEqual(step, { name: "workflow-diagnostics", program: "node", args: ["--test", ...files] });
    assert(!expectedSteps(plan).some(row => row.program === "npm"));
    const executions = [];
    for (const env of [{ CARGO_HOME: "/cache/cargo" }, { USERPROFILE: "D:/cache/profile" }]) {
      const commands = [];
      const report = await executePlan(plan, { env, run: async (program, args) => {
        commands.push([program, args]);
        return { code: 0, output: "passed" };
      } });
      assert.equal(validateGuestReport(report, plan, process.platform), report);
      assert.throws(() => validateGuestReport({ ...report, results: report.results.filter(row => row.name !== "workflow-diagnostics") }, plan, process.platform), /mismatched/);
      executions.push(commands);
    }
    assert.deepEqual(executions[0], executions[1]);
    assert.throws(() => validatePlan({ ...plan, nodeDiagnostics: [files[0]] }), /diagnostic/i);
    assert.throws(() => validatePlan({ ...plan, nodeDiagnostics: [...files, "arbitrary.mjs"] }), /diagnostic/i);
    assert.throws(() => validatePlan({ ...plan, nodeDiagnostics: true }), /nodeDiagnostics|diagnostic/i);
  }
  for (const unrelated of ["docs/README.md", "scripts/diagnostics/release-preflight.test.mjs"]) {
    const plan = buildPlan(parseFlags([]), [unrelated]);
    assert.deepEqual(plan.nodeDiagnostics, []);
    assert.equal(hasSelectedTests(plan), false);
  }
});
