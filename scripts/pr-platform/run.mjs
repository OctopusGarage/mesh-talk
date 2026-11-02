#!/usr/bin/env node
/** Archive committed source once, run the same plan on both required platforms. */
import { createHash, randomBytes } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { buildPlan, hasSelectedTests, parseFlags, validateWindowsRoot } from "./plan.mjs";
import { digestFile, expectedSteps, runCommand, requireCommandStdout, validatePlaywrightEvidence } from "./guest.mjs";
import { loadRequirements, requiredBrowserSpecs, committedFiles, verifyRequiredNative } from "../validation/pr-scenarios.mjs";
import { linuxTarget, dockerInvocation, remoteInvocation } from "./linux-target.mjs";

function requireRetiredExecution(result) {
  if (result.cancelled || result.cleanupUnverified)
    throw Error(result.error || "Platform execution cancelled or descendant retirement unverified");
}

async function checked(program, args, options, run = runCommand) {
  const result = await run(program, args, options);
  requireRetiredExecution(result);
  if (result.code !== 0 || result.timedOut || result.error) throw Error(`${program} ${args[0]} failed${result.timedOut ? " (timeout)" : ""}: ${result.error || result.output.slice(-3000)}`);
  return (program === "git" ? requireCommandStdout(result) : result.output).trim();
}

export async function createArchive(cwd, filename, { run = runCommand, expectedSha } = {}) {
  const options = { cwd, timeoutMs: 30_000 };
  if (await checked("git", ["status", "--porcelain"], options, run)) throw Error("Platform preflight requires clean committed HEAD; commit changes before running it.");
  const sha = await checked("git", ["rev-parse", "HEAD"], options, run);
  if (!/^[a-f0-9]{40,64}$/u.test(sha)) throw Error("Invalid committed HEAD SHA");
  if (expectedSha && sha !== expectedSha) throw Error("Source revision changed after test selection");
  // Verification needs every committed file, including release export-ignored
  // harnesses. Override archive attributes in private metadata, never in the
  // shared repository's info/attributes or working tree.
  const objects = await checked("git", ["rev-parse", "--path-format=absolute", "--git-path", "objects"], options, run);
  const format = await checked("git", ["rev-parse", "--show-object-format"], options, run);
  if (!["sha1", "sha256"].includes(format)) throw Error("Unsupported Git object format");
  const metadata = await mkdtemp(path.join(path.dirname(filename), "archive-git-"));
  try {
    await checked("git", ["init", "--bare", "--quiet", `--object-format=${format}`, metadata], options, run);
    await writeFile(path.join(metadata, "objects/info/alternates"), `${objects}\n`);
    await writeFile(path.join(metadata, "info/attributes"), "* -export-ignore -export-subst -text -filter -ident -working-tree-encoding\n");
    await checked("git", [`--git-dir=${metadata}`, "archive", "--format=tar", `--output=${filename}`, sha], options, run);
  } finally { await rm(metadata, { recursive: true, force: true }); }
  if (await checked("git", ["rev-parse", "HEAD"], options, run) !== sha || await checked("git", ["status", "--porcelain"], options, run)) throw Error("Source changed while creating the committed archive");
  return sha;
}

export async function collectPlatforms(adapters) {
  const entries = await Promise.all(Object.entries(adapters).map(async ([name, adapter]) => {
    try { return [name, await adapter()]; }
    catch (error) { return [name, { ok: false, error: error.message }]; }
  }));
  return Object.fromEntries(entries);
}

const encodedPowerShell = script => Buffer.from(script, "utf16le").toString("base64");
const psArgs = script => ["powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", encodedPowerShell(script)];

export function windowsBootstrap({ root, runId, archiveSha, timeoutSeconds }) {
  root = validateWindowsRoot(root);
  if (!/^[a-zA-Z0-9-]+$/u.test(runId) || !/^[a-f0-9]{64}$/u.test(archiveSha) || !Number.isInteger(timeoutSeconds) || timeoutSeconds < 1) throw Error("Invalid Windows bootstrap parameters");
  // Only validated paths, a digest, and a numeric deadline enter this script.
  // Test selectors travel exclusively in plan.json and guest spawn argv.
  return `$ErrorActionPreference='Stop'
$work='${root}/${runId}'
$cache='${root}/cache'
$source="$work/source"
$evidence="$work/evidence"
$node=(Get-Command node.exe).Source
$rustRoot=(& rustc --print sysroot).Trim()
if ($LASTEXITCODE -ne 0 -or !$rustRoot) { throw 'An installed Rust toolchain is required' }
foreach ($dir in @($source,$evidence,"$cache/cargo","$cache/rustup","$cache/target","$cache/npm","$cache/browsers","$cache/temp","$cache/profile","$cache/appdata","$cache/localappdata")) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
$env:TEMP="$cache/temp"
$env:TMP="$cache/temp"
$env:TMPDIR="$cache/temp"
if ((Get-FileHash "$work/source.tar" -Algorithm SHA256).Hash.ToLowerInvariant() -ne '${archiveSha}') { throw 'Source archive digest mismatch' }
& tar -xf "$work/source.tar" -C $source
if ($LASTEXITCODE -ne 0) { throw 'Committed archive extraction failed' }
$cargoHome=(& $node "$source/scripts/pr-platform/plan.mjs" --windows-cargo-home "$cache/cargo").Trim()
if ($LASTEXITCODE -ne 0 -or !$cargoHome) { throw 'Cargo home selection failed' }
# Installed tools may be read from C:. New cache, profile, build and temp data use D:.
$env:PATH="$rustRoot/bin;"+$env:PATH
$env:MESH_PR_CARGO="$rustRoot/bin/cargo.exe"
$env:RUSTC="$rustRoot/bin/rustc.exe"
$env:RUSTDOC="$rustRoot/bin/rustdoc.exe"
$env:CARGO_HOME="$cargoHome"
$env:RUSTUP_HOME="$cache/rustup"
$env:RUSTUP_TOOLCHAIN="$rustRoot"
$env:CARGO_TARGET_DIR="$cache/target"
$env:npm_config_cache="$cache/npm"
$env:npm_config_userconfig="$cache/npm/npmrc"
$env:PLAYWRIGHT_BROWSERS_PATH="$cache/browsers"
$env:USERPROFILE="$cache/profile"
$env:APPDATA="$cache/appdata"
$env:LOCALAPPDATA="$cache/localappdata"
$env:XDG_CACHE_HOME="$cache/localappdata"
$env:MESH_PR_ARCHIVE="$work/source.tar"
$env:MESH_PR_PLATFORM_TIMEOUT_SECONDS='${timeoutSeconds}'
$child=Start-Process -FilePath $node -ArgumentList @("$source/scripts/pr-platform/guest.mjs","$work/plan.json","$evidence/report.json") -WorkingDirectory $source -RedirectStandardOutput "$evidence/guest.stdout.log" -RedirectStandardError "$evidence/guest.stderr.log" -PassThru
if (!$child.WaitForExit(${timeoutSeconds * 1000})) { & taskkill.exe /PID $child.Id /T /F | Out-Null; throw 'Windows guest deadline exhausted' }
exit $child.ExitCode
`;
}

export function validateGuestReport(report, plan, platform) {
  const steps = expectedSteps(plan);
  if (report.schema !== 1 || report.sourceSha !== plan.sourceSha || report.archiveSha !== plan.archiveSha || report.platform !== platform || !report.architecture || !Array.isArray(report.results) || report.results.length < 1 || report.results.length > steps.length) throw Error(`Missing or mismatched ${platform} guest evidence`);
  const terminal = report.results.at(-1);
  for (const flag of ["cancelled", "cleanupUnverified"]) {
    if (report[flag] !== undefined && typeof report[flag] !== "boolean") throw Error(`Invalid ${platform} interruption flag`);
    if (!!report[flag] !== report.results.some(result => result[flag] === true)) throw Error(`Inconsistent ${platform} interruption evidence`);
  }
  const interrupted = !!report.cancelled || !!report.cleanupUnverified;
  if (interrupted && (report.ok !== false || !(terminal.cancelled || terminal.cleanupUnverified) || terminal.ok !== false))
    throw Error(`Interrupted ${platform} evidence requires a failed terminal step`);
  if (report.results.length !== steps.length && !interrupted) throw Error(`Missing or mismatched ${platform} guest evidence`);
  for (const [index, result] of report.results.entries()) {
    const step = steps[index];
    for (const flag of ["cancelled", "cleanupUnverified"]) {
      if (result[flag] !== undefined && typeof result[flag] !== "boolean") throw Error(`Invalid ${platform} interruption flag`);
      if (result[flag] && index !== report.results.length - 1) throw Error(`Interrupted ${platform} evidence must end at the terminal step`);
    }
    if (result.name !== step.name || result.program !== step.program || result.kind !== step.kind || JSON.stringify(result.args) !== JSON.stringify(step.args)) throw Error(`Unexpected ${platform} test selection at step ${index}`);
    const validCount = step.kind !== "rust" || (Number.isInteger(result.testCount) && result.testCount > 0);
    let validUi = true;
    if (step.kind === "ui") {
      try { validatePlaywrightEvidence(result.ui, plan.uiSpecs); }
      catch (error) { if (result.ok) throw error; validUi = false; }
    }
    const ok = result.code === 0 && !result.timedOut && !result.cancelled && !result.cleanupUnverified && !result.error && validCount && validUi;
    if (result.ok !== ok) throw Error(`Inconsistent ${platform} result for ${step.name}`);
  }
  if (report.ok !== report.results.every(result => result.ok)) throw Error(`Inconsistent ${platform} aggregate result`);
  return report;
}

async function platformReport(filename, plan, platform) {
  return validateGuestReport(JSON.parse(await readFile(filename, "utf8")), plan, platform);
}

function budget(directory, platform, seconds) {
  const deadline = Date.now() + seconds * 1000;
  return phase => {
    const timeoutMs = deadline - Date.now();
    if (timeoutMs <= 0) throw Error(`${platform} platform deadline exhausted before ${phase}`);
    return { timeoutMs, logPath: path.join(directory, `${platform}-${phase}.log`) };
  };
}

async function linux(cwd, directory, plan, timeoutSeconds) {
  const imageDirectory = path.join(directory, "image");
  await mkdir(imageDirectory, { recursive: true });
  const dockerfile = path.join(imageDirectory, "Dockerfile");
  const content = await checked("git", ["show", `${plan.sourceSha}:scripts/pr-platform/Dockerfile`], { cwd, timeoutMs: 30_000 });
  await writeFile(dockerfile, content);
  const lock = JSON.parse(await checked("git", ["show", `${plan.sourceSha}:frontend/package-lock.json`], { cwd, timeoutMs: 30_000 }));
  const playwright = lock.packages["node_modules/playwright"].version;
  if (!/^\d+\.\d+\.\d+$/u.test(playwright)) throw Error("Locked Playwright version must be exact");
  const imageHash = createHash("sha256").update(await readFile(dockerfile)).update(playwright).digest("hex").slice(0, 12);
  const image = process.env.MESH_PR_DOCKER_IMAGE || `mesh-talk-pr-platform:${imageHash}`;
  if (!/^[\w][\w./:@-]+$/u.test(image)) throw Error("Invalid Docker image tag");
  const limits = budget(directory, "linux", timeoutSeconds);
  const target = linuxTarget(process.env, plan.runId, directory);
  const platformArgs = target.platform ? ["--platform", target.platform] : [];
  const dockerRun = (_program, args, options) => {
    const invocation = dockerInvocation(target, args);
    return runCommand(invocation.program, invocation.args, options);
  };
  if (target.host) {
    const prepare = remoteInvocation(target.host, "mkdir", ["-p", `${target.directory}/image`, `${target.directory}/linux`]);
    await checked(prepare.program, prepare.args, { cwd, ...limits("prepare") });
    await checked("scp", ["-o", "BatchMode=yes", path.join(directory, "source.tar"), path.join(directory, "plan.json"), `${target.host}:${target.directory}/`], { cwd, ...limits("upload") });
    await checked("scp", ["-o", "BatchMode=yes", dockerfile, `${target.host}:${target.directory}/image/Dockerfile`], { cwd, ...limits("upload-image") });
  }
  const available = await dockerRun("docker", ["image", "inspect", image], { cwd, ...limits("inspect") });
  requireRetiredExecution(available);
  if (available.code !== 0) {
    if (process.env.MESH_PR_DOCKER_IMAGE) throw Error(`Configured Docker image is unavailable: ${image}`);
    const context = path.posix.join(target.directory, "image");
    await checked("docker", ["build", ...platformArgs, "--build-arg", `PLAYWRIGHT_VERSION=${playwright}`, "-t", image, "-f", `${context}/Dockerfile`, context], { cwd, ...limits("build") }, dockerRun);
  }
  const options = { cwd, ...limits("run") };
  const guestSeconds = Math.max(1, Math.floor(options.timeoutMs / 1000));
  const name = `mesh-pr-${plan.runId}`;
  const args = ["run", "--rm", ...platformArgs, "--name", name,
    // Docker can bind IPv6 sockets while disabling ::1. Exercise real IPv6
    // discovery without requiring daemon-wide IPv6 networking configuration.
    "--sysctl", "net.ipv6.conf.lo.disable_ipv6=0",
    "-v", `${target.directory}:/evidence`,
    "-v", "mesh-pr-cargo:/cache/cargo", "-v", "mesh-pr-target:/cache/target", "-v", "mesh-pr-npm:/cache/npm",
    "-e", "CARGO_HOME=/cache/cargo", "-e", "CARGO_TARGET_DIR=/cache/target", "-e", "npm_config_cache=/cache/npm",
    "-e", "MESH_PR_ARCHIVE=/evidence/source.tar", "-e", `MESH_PR_PLATFORM_TIMEOUT_SECONDS=${guestSeconds}`,
    image, "bash", "-c", "mkdir -p /work/source && tar -xf /evidence/source.tar -C /work/source && cd /work/source && node scripts/pr-platform/guest.mjs /evidence/plan.json /evidence/linux/report.json"];
  try {
    const result = await dockerRun("docker", args, options);
    requireRetiredExecution(result);
    if (target.host) {
      await checked("scp", ["-o", "BatchMode=yes", "-r", `${target.host}:${target.directory}/linux/.`, path.join(directory, "linux")], { cwd, timeoutMs: 120_000, logPath: path.join(directory, "linux-download.log") });
    }
    const report = await platformReport(path.join(directory, "linux/report.json"), plan, "linux");
    const expectedArchitecture = { "linux/amd64": "x64", "linux/arm64": "arm64" }[target.platform];
    if (expectedArchitecture && report.architecture !== expectedArchitecture) throw Error(`Linux architecture mismatch: expected ${expectedArchitecture}, got ${report.architecture}`);
    return { ...report, ok: report.ok && result.code === 0 && !result.timedOut, image, host: target.host, requestedPlatform: target.platform };
  } finally {
    await dockerRun("docker", ["rm", "-f", name], { cwd, timeoutMs: 30_000, logPath: path.join(directory, "linux-cleanup.log") });
  }
}

async function windows(cwd, directory, plan, timeoutSeconds) {
  const host = process.env.MESH_PR_WINDOWS_HOST || "windows10";
  if (!/^[\w][\w.@-]*$/u.test(host)) throw Error("Unsafe Windows SSH host; use a configured SSH alias");
  const root = validateWindowsRoot(process.env.MESH_PR_WINDOWS_ROOT || "D:/MeshTalk/PRPreflight");
  const work = `${root}/${plan.runId}`;
  const limits = budget(directory, "windows", timeoutSeconds);
  await checked("ssh", ["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", host, ...psArgs(`$ErrorActionPreference='Stop'; New-Item -ItemType Directory -Force -Path '${work}' | Out-Null`)], { cwd, ...limits("prepare") });
  await checked("scp", ["-o", "BatchMode=yes", path.join(directory, "source.tar"), path.join(directory, "plan.json"), `${host}:${work}/`], { cwd, ...limits("upload") });
  const options = { cwd, ...limits("run") };
  const guestSeconds = Math.max(1, Math.floor(options.timeoutMs / 1000));
  const result = await runCommand("ssh", ["-o", "BatchMode=yes", "-o", "ConnectTimeout=15", host, ...psArgs(windowsBootstrap({ root, runId: plan.runId, archiveSha: plan.archiveSha, timeoutSeconds: guestSeconds }))], { ...options, timeoutMs: options.timeoutMs + 30_000 });
  requireRetiredExecution(result);
  const copied = await runCommand("scp", ["-o", "BatchMode=yes", "-r", `${host}:${work}/evidence/.`, path.join(directory, "windows")], { cwd, timeoutMs: 120_000, logPath: path.join(directory, "windows-download.log") });
  requireRetiredExecution(copied);
  if (copied.code !== 0) throw Error(`Windows evidence collection failed: ${result.output.slice(-2000)} ${copied.output.slice(-2000)}`);
  const report = await platformReport(path.join(directory, "windows/report.json"), plan, "win32");
  return { ...report, ok: report.ok && result.code === 0 && !result.timedOut, host };
}

async function main() {
  const flags = parseFlags(process.argv.slice(2));
  const cwd = await checked("git", ["rev-parse", "--show-toplevel"], { timeoutMs: 30_000 });
  const candidate = await checked("git", ["rev-parse", "HEAD"], { cwd, timeoutMs: 30_000 });
  const base = await checked("git", ["merge-base", candidate, flags.base], { cwd, timeoutMs: 30_000 });
  const changed = committedFiles(cwd, base, candidate);
  const requirements = await loadRequirements(changed, cwd);
  flags.uiSpecs = requiredBrowserSpecs(flags.uiSpecs, requirements);
  const plan = buildPlan(flags, changed);
  plan.scenarios = requirements;
  plan.sourceSha = candidate;
  plan.baseRef = flags.base;
  plan.mergeBase = base;
  for (const spec of plan.uiSpecs) await checked("git", ["cat-file", "-e", `${candidate}:frontend/${spec}`], { cwd, timeoutMs: 30_000 });
  if (flags.planOnly) { console.log(JSON.stringify(plan, null, 2)); return; }
  await verifyRequiredNative(requirements, candidate);
  if (!hasSelectedTests(plan)) {
    if (await checked("git", ["status", "--porcelain"], { cwd, timeoutMs: 30_000 }) || await checked("git", ["rev-parse", "HEAD"], { cwd, timeoutMs: 30_000 }) !== candidate) throw Error("Platform preflight requires a stable clean committed revision");
    const directory = path.join(cwd, "target/pr-platform", `${Date.now()}-not-applicable`);
    await mkdir(directory, { recursive: true });
    const report = { schema: 1, sourceSha: candidate, status: "not-applicable", reason: "No selected app tests or changed runtime", plan, platforms: {} };
    await writeFile(path.join(directory, "report.json"), `${JSON.stringify(report, null, 2)}\n`);
    console.log(`Platform preflight not applicable: no selected app tests or changed runtime (${path.join(directory, "report.json")})`);
    return;
  }
  const timeoutSeconds = Number(process.env.MESH_PR_PLATFORM_TIMEOUT_SECONDS || 1800);
  if (!Number.isInteger(timeoutSeconds) || timeoutSeconds < 1 || timeoutSeconds > 86400) throw Error("MESH_PR_PLATFORM_TIMEOUT_SECONDS must be 1..86400");
  plan.runId = `${Date.now()}-${randomBytes(4).toString("hex")}`;
  const directory = path.join(cwd, "target/pr-platform", plan.runId);
  await mkdir(path.join(directory, "linux"), { recursive: true });
  await mkdir(path.join(directory, "windows"), { recursive: true });
  plan.sourceSha = await createArchive(cwd, path.join(directory, "source.tar"), { expectedSha: candidate });
  plan.archiveSha = await digestFile(path.join(directory, "source.tar"));
  await writeFile(path.join(directory, "plan.json"), `${JSON.stringify(plan, null, 2)}\n`);
  console.log(`Linux Docker: starting selected plan for ${plan.sourceSha}; evidence ${path.join(directory, "linux")}`);
  console.log(`Windows SSH: starting selected plan for ${plan.sourceSha}; evidence ${path.join(directory, "windows")}`);
  const pending = new Set(["linux", "windows"]);
  const started = Date.now();
  const progress = setInterval(() => console.log(`Platform preflight: ${[...pending].join(", ")} running for ${Math.round((Date.now() - started) / 1000)}s; evidence ${directory}`), 30_000);
  const tracked = (name, adapter) => async () => {
    try {
      const result = await adapter();
      const failures = result.results?.filter(step => !step.ok).map(step => `${step.name}: ${step.error}`).join("; ");
      console.log(`${name}: ${result.ok ? "passed" : `failed: ${result.error || failures || "guest process failed"}`}; evidence ${path.join(directory, name)}`);
      return result;
    } catch (error) { console.log(`${name}: failed: ${error.message}; evidence ${path.join(directory, name)}`); throw error; }
    finally { pending.delete(name); }
  };
  let platforms;
  try { platforms = await collectPlatforms({ linux: tracked("linux", () => linux(cwd, directory, plan, timeoutSeconds)), windows: tracked("windows", () => windows(cwd, directory, plan, timeoutSeconds)) }); }
  finally { clearInterval(progress); }
  const report = { schema: 1, sourceSha: plan.sourceSha, archiveSha: plan.archiveSha, plan, platforms, ok: Object.values(platforms).every(result => result.ok) };
  if (await checked("git", ["rev-parse", "HEAD"], { cwd, timeoutMs: 30_000 }) !== candidate || await checked("git", ["status", "--porcelain"], { cwd, timeoutMs: 30_000 })) {
    report.ok = false;
    report.error = "Source revision changed while platform checks ran";
  }
  await writeFile(path.join(directory, "report.json"), `${JSON.stringify(report, null, 2)}\n`);
  console.log(`Platform preflight ${report.ok ? "passed" : "failed"}: ${path.join(directory, "report.json")}`);
  if (!report.ok) process.exitCode = 1;
}
if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) main().catch(error => { console.error(error.message); process.exitCode = 1; });
