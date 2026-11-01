import assert from "node:assert/strict";
import test from "node:test";
import { buildPlan, hasSelectedTests, parseFlags, selectWindowsCargoHome, validatePlan, validateWindowsRoot } from "../pr-platform/plan.mjs";
import * as rustSelection from "../pr-platform/rust-packages.mjs";
import { executePlan, expectedSteps, rustTestCount, runCommand } from "../pr-platform/guest.mjs";
import { collectPlatforms, createArchive, validateGuestReport, windowsBootstrap } from "../pr-platform/run.mjs";

const sha = "a".repeat(40);
const capturedOutput = (stdout, stderr = "") => ({ stdout, stderr, output: stdout + stderr, outputTruncated: false });

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
      return { code: 0, ...capturedOutput("test result: ok. 1 passed; 0 failed; 0 ignored;") };
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
    if (program === "cargo" && args[0] === "test") return { code: args.includes("mesh-talk-core") ? 0 : 1, ...capturedOutput("test result: ok. 0 passed; 0 failed; 0 ignored;") };
    return { code: 0, ...capturedOutput("ok") };
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
  const result = await runCommand(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { timeoutMs: 30, env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" } });
  assert.equal(result.timedOut, true);
  assert.notEqual(result.code, 0);
});


test("guest evidence cannot silently omit tests, broaden selectors, or claim zero tests passed", async () => {
  const plan = { ...buildPlan(parseFlags(["--core-test", "decoder_smoke"]), []), sourceSha: sha, archiveSha: "b".repeat(64) };
  const report = await executePlan(plan, { run: async () => ({ code: 0, ...capturedOutput("test result: ok. 2 passed; 0 failed; 0 ignored;") }) });
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
    return { code: 0, ...capturedOutput(args[0] === "status" ? "" : sha) };
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
  const plan = { ...buildPlan(parseFlags(["e2e/privacy.spec.ts", "e2e/packs-empty.spec.ts"]), []), sourceSha: sha, archiveSha: "b".repeat(64) };
  const json = status => ({
    config: { rootDir: join(directory, "frontend/e2e") }, errors: [],
    stats: { expected: status === "passed" ? 2 : 0, skipped: status === "passed" ? 1 : 2, unexpected: 0, flaky: 0 },
    suites: ["privacy.spec.ts", "packs-empty.spec.ts"].map((file, i) => ({
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
    return { code: 0, ...capturedOutput("browser command exited zero") };
  } });
  const skipped = await runReport(json("skipped"));
  assert.equal(skipped.ok, false, "all-skipped selected files must fail despite exit zero");
  const passed = await runReport(json("passed"));
  assert.equal(passed.ok, true, "real passes plus individual skips are accepted");
  assert.equal(validateGuestReport(passed, plan, process.platform), passed);
  const missing = structuredClone(passed);
  missing.results.at(-1).ui.tests = missing.results.at(-1).ui.tests.filter(row => row.file !== "e2e/packs-empty.spec.ts");
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
    "scripts/diagnostics/prepare-pr.test.mjs",
    "scripts/diagnostics/pr-scenarios.test.mjs",
    "scripts/diagnostics/native-build.test.mjs",
    "scripts/diagnostics/create-pr.test.mjs",
    "scripts/diagnostics/pr-preflight-awake.test.mjs",
    "scripts/diagnostics/source-identity.test.mjs",
    "scripts/diagnostics/linux-target.test.mjs",
    "scripts/diagnostics/pr-preflight-output.test.mjs",
  ];
  for (const changed of ["scripts/pr-platform/guest.mjs", "scripts/pr-preflight.sh", "scripts/create-pr.sh", "scripts/prepare-pr.sh", "scripts/validation/pr-scenarios.mjs", "scripts/validation/native-build.mjs", "scripts/evals/source-identity.mjs", ...files]) {
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
        return { code: 0, ...capturedOutput("passed") };
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


test("all four Rust packages require explicit selectors for sources, manifests and build scripts", () => {
  const packages = [
    ["core", "crates/mesh-talk-core", "coreFilters"],
    ["host", "src-tauri", "hostFilters"],
    ["integration", "crates/mesh-talk-integrations", "integrationFilters"],
    ["mcp", "crates/mesh-talk-mcp", "mcpFilters"],
  ];
  for (const [kind, directory, key] of packages) {
    for (const file of ["src/lib.rs", "tests/smoke.rs", "Cargo.toml", "build.rs"]) {
      assert.throws(() => buildPlan(parseFlags([]), [`${directory}/${file}`]), new RegExp(`--${kind}-filter`));
      const plan = buildPlan(parseFlags([`--${kind}-filter`, "tests::literal;$(noop)"]), [`${directory}/${file}`]);
      assert.deepEqual(plan[key], ["tests::literal;$(noop)"]);
      assert.equal(hasSelectedTests(plan), true);
      assert.equal(expectedSteps(plan).filter(step => step.args[0] === "clippy").length, 1);
    }
  }
  const all = packages.flatMap(([kind]) => [`--${kind}-filter`, "tests::"]);
  for (const file of ["Cargo.toml", "Cargo.lock", ".cargo/config.toml", "rust-toolchain", "rust-toolchain.toml", "clippy.toml", ".clippy.toml"]) {
    for (const [kind] of packages) {
      assert.throws(() => buildPlan(parseFlags(all.filter((_, i) => Math.floor(i / 2) !== packages.findIndex(row => row[0] === kind))), [file]), new RegExp(`--${kind}-filter`));
    }
    assert.equal(expectedSteps(buildPlan(parseFlags(all), [file])).filter(step => step.args[0] === "clippy").length, 4);
  }
});

test("new package selectors remain literal, reject zero tests and retain platform lint evidence", async () => {
  const plan = { ...buildPlan(parseFlags(["--integration-filter", "grant;$(noop)", "--mcp-filter", "tools::"]), []), sourceSha: sha, archiveSha: "b".repeat(64) };
  const seen = [];
  const report = await executePlan(plan, { run: async (program, args, options) => {
    seen.push({ program, args, options });
    return { code: args[0] === "clippy" ? 1 : 0, ...capturedOutput(args[0] === "test" ? "test result: ok. 0 passed; 0 failed; 0 ignored;" : "platform lint") };
  } });
  assert.equal(report.ok, false);
  assert.equal(report.results.filter(step => step.kind === "rust").length, 2);
  assert(report.results.filter(step => step.kind === "rust").every(step => /zero/.test(step.error)));
  assert(seen.every(({ options }) => options.shell === false && options.env.EVAL_SOURCE_SHA === sha));
  for (const crate of ["mesh-talk-integrations", "mesh-talk-mcp"]) {
    assert(seen.some(({ args }) => JSON.stringify(args) === JSON.stringify(["clippy", "--locked", "-p", crate, "--all-targets", "--", "-D", "warnings"])));
  }
  assert.throws(() => validatePlan({ ...plan, mcpFilters: "tools::" }), /mcpFilters/);
  assert.throws(() => validatePlan({ ...plan, integrationFilters: ["bad\nfilter"] }), /literal/);
});


test("platform reports require successful lint evidence for integration and MCP selections", async () => {
  const plan = { ...buildPlan(parseFlags(["--integration-filter", "grants::", "--mcp-filter", "tools::"]), []), sourceSha: sha, archiveSha: "b".repeat(64) };
  const report = await executePlan(plan, { run: async () => ({ code: 0, ...capturedOutput("test result: ok. 1 passed; 0 failed; 0 ignored;") }) });
  assert.equal(report.ok, true);
  assert.equal(validateGuestReport(report, plan, process.platform), report);
  const omitted = { ...report, results: report.results.filter(step => step.name !== "mcp-clippy") };
  assert.throws(() => validateGuestReport(omitted, plan, process.platform), /mismatched/);
  const broadened = structuredClone(report);
  broadened.results.find(step => step.name === "integration-clippy").args = ["clippy", "--workspace"];
  assert.throws(() => validateGuestReport(broadened, plan, process.platform), /selection/);
});


test("all executable targets and package-local compiler configs require Rust selection", () => {
  for (const pkg of rustSelection.rustPackages) {
    for (const relative of ["examples/demo.rs", "benches/throughput.rs", "rust-toolchain", "rust-toolchain.toml", "clippy.toml", ".clippy.toml", ".cargo/config.toml"]) {
      assert.throws(() => buildPlan(parseFlags([]), [`${pkg.directory}/${relative}`]), new RegExp(pkg.flag));
      assert.equal(expectedSteps(buildPlan(parseFlags([pkg.flag, "tests::"]), [`${pkg.directory}/${relative}`])).filter(step => step.args[0] === "clippy").length, 1);
    }
  }
});

test("Tauri native build inputs require host selection and keep documentation outside Rust validation", () => {
  for (const relative of ["examples/native-contact-eval.rs", "tauri.conf.json", "tauri.conf.custom.json", "tauri.windows.conf.json", "capabilities/default.json", "windows.manifest", "icons/icon.png", "icons/icon.ico", "Info.plist", "entitlements.plist"]) {
    assert.throws(() => buildPlan(parseFlags([]), [`src-tauri/${relative}`]), /--host-filter/);
  }
  for (const relative of ["README.md", "icons/README.md", "validation.json", "gen/schemas/capabilities.json"]) {
    assert.equal(hasSelectedTests(buildPlan(parseFlags([]), [`src-tauri/${relative}`])), false);
  }
});

test("the shared classifier exposes the same selection requirement to the local gate", async () => {
  assert.equal(typeof rustSelection.sharedRustChange, "function");
  assert.equal(typeof rustSelection.rustSelectionNeeded, "function");
  assert.equal(rustSelection.sharedRustChange("rust-toolchain.toml"), true);
  assert.equal(rustSelection.sharedRustChange("src-tauri/rust-toolchain.toml"), false);
  for (const file of ["src-tauri/build.rs", "src-tauri/examples/native-contact-eval.rs", "src-tauri/capabilities/default.json", "crates/mesh-talk-mcp/.clippy.toml", ".cargo/config.toml"]) {
    assert.equal(rustSelection.rustSelectionNeeded([file]), true, file);
  }
  assert.equal(rustSelection.rustSelectionNeeded(["docs/README.md", "crates/mesh-talk-core/README.md"]), false);
  const needed = await runCommand(process.execPath, ["scripts/pr-platform/rust-packages.mjs", "--selection-needed", "src-tauri/examples/native-contact-eval.rs"]);
  const unneeded = await runCommand(process.execPath, ["scripts/pr-platform/rust-packages.mjs", "--selection-needed", "docs/README.md"]);
  assert.equal(needed.code, 0);
  assert.equal(unneeded.code, 1);
});

for (const signal of ["SIGTERM", "SIGINT"]) test(`guest cancellation ${signal} retires an ignored-stdio descendant`, { skip: process.platform === "win32", timeout: 15000 }, async t => {
  const { spawn, spawnSync } = await import("node:child_process");
  const { mkdtemp, readFile, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { setTimeout: pause } = await import("node:timers/promises");
  const directory = await mkdtemp(join(tmpdir(), "mesh-guest-cancel-"));
  const marker = join(directory, "ready-quote\"-apostrophe'-中文-\n");
  const leaf = "process.on('SIGTERM',()=>{});process.on('SIGINT',()=>{});require('node:fs').writeFileSync(process.argv[1],String(process.pid));setInterval(()=>{},1000)";
  const leader = "process.on('SIGTERM',()=>process.exit(0));process.on('SIGINT',()=>process.exit(0));require('node:child_process').spawn(process.execPath,['-e',process.argv[1],process.argv[2]],{stdio:'ignore'});setInterval(()=>{},1000)";
  const module = new URL("../pr-platform/guest.mjs", import.meta.url).href;
  const program = "const {runCommand}=await import(process.argv[1]);const result=await runCommand(process.execPath,['-e',process.argv[2],process.argv[3],process.argv[4]]);process.exitCode=result.code;";
  const supervisor = spawn(process.execPath, ["--input-type=module", "-e", program, module, leader, leaf, marker], { stdio: "ignore", env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" } });
  let pid;
  t.after(async () => { supervisor.kill("SIGKILL"); if (pid) { const group = Number(spawnSync("ps", ["-o", "pgid=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim()); if (group > 1) try { process.kill(-group, "SIGKILL"); } catch {} } await rm(directory, { recursive: true, force: true }); });
  for (let i = 0; i < 300; i++) { try { pid = Number(await readFile(marker, "utf8")); break; } catch {} await pause(10); }
  assert.ok(pid, "descendant must acknowledge installed signal handlers");
  const closed = new Promise(resolve => supervisor.once("close", resolve));
  supervisor.kill(signal);
  await closed;
  const state = spawnSync("ps", ["-o", "stat=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim();
  assert.ok(!state || state.startsWith("Z"), `owned descendant remains runnable: ${state}`);
});

test("a nested guest timeout notifies its existing stage owner and retires the group", { skip: process.platform === "win32", timeout: 15000 }, async t => {
  const { spawn, spawnSync } = await import("node:child_process");
  const { mkdtemp, readFile, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { setTimeout: pause } = await import("node:timers/promises");
  const directory = await mkdtemp(join(tmpdir(), "mesh-nested-guest-"));
  const marker = join(directory, "ready-quote\"-apostrophe'-中文-\n");
  const leaf = "process.on('SIGTERM',()=>{});require('node:fs').writeFileSync(process.argv[1],String(process.pid));setInterval(()=>{},1000)";
  const guest = new URL("../pr-platform/guest.mjs", import.meta.url).href;
  const stage = new URL("../validation/stage.mjs", import.meta.url).href;
  const inner = "const {runCommand}=await import(process.argv[1]);console.log('GROUP='+process.pid);await runCommand(process.execPath,['-e',process.argv[2],process.argv[3]],{timeoutMs:1500});";
  const outer = "const {execute}=await import(process.argv[1]);const controller=new AbortController();process.on('SIGTERM',()=>controller.abort());const r=await execute([process.execPath,'--input-type=module','-e',process.argv[2],process.argv[3],process.argv[4],process.argv[5]],{env:process.env,signal:controller.signal});process.exitCode=r.code;";
  const supervisor = spawn(process.execPath, ["--input-type=module", "-e", outer, stage, inner, guest, leaf, marker], { stdio: ["ignore", "pipe", "pipe"], env: { ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" } });
  let group, pid;
  supervisor.stdout.on("data", chunk => { const match = chunk.toString().match(/GROUP=(\d+)/); if (match) group = Number(match[1]); });
  t.after(async () => { supervisor.kill("SIGKILL"); if (group) try { process.kill(-group, "SIGKILL"); } catch {} if (pid) try { process.kill(pid, "SIGKILL"); } catch {} await rm(directory, { recursive: true, force: true }); });
  const closed = new Promise(resolve => supervisor.once("close", resolve));
  for (let i = 0; i < 300; i++) { try { pid = Number(await readFile(marker, "utf8")); break; } catch {} await pause(10); }
  assert.ok(pid && group, "real nested child must acknowledge startup");
  const code = await Promise.race([closed, pause(6000).then(() => "still running")]);
  assert.equal(code, 130, "nested timeout must reach the process-group owner");
  const state = spawnSync("ps", ["-o", "stat=", "-p", String(pid)], { encoding: "utf8" }).stdout.trim();
  assert.ok(!state || state.startsWith("Z"), `nested worker remains runnable: ${state}`);
});

test("guest cancellation stops admission of subsequent plan steps", async () => {
  const plan = { ...buildPlan(parseFlags(["--core-filter", "node::account_sync"]), ["crates/mesh-talk-core/src/node.rs"]), sourceSha: sha };
  for (const stopped of [{ code: 130, cancelled: true }, { code: 0, cleanupUnverified: true }]) {
    let calls = 0;
    const report = await executePlan(plan, { run: async () => { calls++; return { ...capturedOutput(""), ...stopped }; } });
    assert.equal(calls, 1);
    assert.equal(report.ok, false);
  }
});

test("interrupted execution reports retain an exact failed selection prefix", async () => {
  const plan = { ...buildPlan(parseFlags(["--core-filter", "fixture::"]), ["crates/mesh-talk-core/src/node.rs"]), sourceSha: sha, archiveSha: "b".repeat(64) };
  for (const flag of ["cancelled", "cleanupUnverified"]) {
    for (const stopAt of [1, expectedSteps(plan).length]) {
      let calls = 0;
      const report = await executePlan(plan, { run: async () => ++calls === stopAt ? { code: 0, ...capturedOutput(""), [flag]: true } : { code: 0, ...capturedOutput("test result: ok. 2 passed; 0 failed;") } });
      assert.equal(validateGuestReport(report, plan, process.platform), report);
      assert.equal(report[flag], true);
      assert.equal(report.results.at(-1)[flag], true);
      for (const mutate of [
        r => { r.ok = true; },
        r => { r[flag] = false; },
        r => { r.results.at(-1)[flag] = false; },
        r => { r.results[0].args.push("--ignored"); },
        r => { r.sourceSha = "c".repeat(40); },
        r => { r.archiveSha = "c".repeat(64); },
      ]) { const bad = structuredClone(report); mutate(bad); assert.throws(() => validateGuestReport(bad, plan, process.platform)); }
    }
  }
  const complete = await executePlan(plan, { run: async () => ({ code: 0, ...capturedOutput("test result: ok. 2 passed; 0 failed;") }) });
  const truncated = { ...complete, ok: false, results: complete.results.slice(0, 1) };
  assert.throws(() => validateGuestReport(truncated, plan, process.platform), /mismatched|interrupt/i);
  const forged = structuredClone(complete); forged.cancelled = true; forged.ok = false; forged.results[0].cancelled = true; forged.results[0].ok = false;
  assert.throws(() => validateGuestReport(forged, plan, process.platform), /interrupt|terminal/i);
});


test("guest Rust evidence requires complete stdout rather than combined or stderr harness text", async () => {
  const plan = { ...buildPlan(parseFlags(["--core-filter", "fixture::actual"]), []), sourceSha: sha };
  const valid = "test result: ok. 2 passed; 0 failed; 0 ignored;";
  const split = valid.indexOf("2 passed");
  const missing = capturedOutput(valid); delete missing.stdout;
  const unspecified = capturedOutput(valid); delete unspecified.outputTruncated;
  for (const evidence of [
    capturedOutput("", valid),
    missing,
    { ...capturedOutput(valid), outputTruncated: true },
    unspecified,
    capturedOutput(valid.slice(0, split), valid.slice(split)),
  ]) {
    const report = await executePlan(plan, { run: async (_, args) => ({ code: 0,
      ...(args[0] === "test" ? evidence : capturedOutput("tool output")) }) });
    assert.equal(report.ok, false, "diagnostic output cannot substitute for complete stdout");
    const rust = report.results.find(result => result.kind === "rust");
    assert.equal(rust.ok, false);
    assert.match(rust.error, /stdout|truncat|zero/i);
  }
  const report = await executePlan(plan, { run: async (_, args) => ({ code: 0,
    ...capturedOutput(args[0] === "test" ? valid : "tool output", "test result: ok. 99 passed; 0 failed;\n") }) });
  assert.equal(report.ok, true);
  assert.equal(report.results.find(result => result.kind === "rust").testCount, 2);
});

test("guest execution failures take precedence over missing machine stdout", async () => {
  const plan = { ...buildPlan(parseFlags(["--core-filter", "fixture::actual"]), []), sourceSha: sha, archiveSha: "b".repeat(64) };
  for (const [failure, expected] of [
    [{ code: 37 }, /exited 37/i],
    [{ code: null, error: "original launch unavailable" }, /original launch unavailable/],
    [{ code: 0, timedOut: true }, /timed out/i],
    [{ code: 0, cancelled: true }, /cancelled/i],
    [{ code: 0, cleanupUnverified: true }, /retirement/i],
  ]) {
    const report = await executePlan(plan, { run: async (_, args) => args[0] === "test"
      ? { output: "human diagnostics", ...failure }
      : { code: 0, ...capturedOutput("tool output") } });
    assert.equal(report.ok, false);
    assert.match(report.results.find(result => result.kind === "rust").error, expected);
    if (failure.cancelled || failure.cleanupUnverified) {
      assert.equal(report.cancelled, !!failure.cancelled);
      assert.equal(report.cleanupUnverified, !!failure.cleanupUnverified);
      assert.equal(validateGuestReport(report, plan, process.platform), report);
    }
  }
});
