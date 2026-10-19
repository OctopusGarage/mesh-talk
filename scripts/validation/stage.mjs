#!/usr/bin/env node
/** Local acceleration only: hosted checks always execute. Shell arguments stay argv data. */
import { createHash, randomUUID } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { createWriteStream, existsSync } from "node:fs";
import { mkdir, readFile, readdir, writeFile, rename, rm, stat } from "node:fs/promises";
import path from "node:path";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";
import { inputGroup, inputIdentity, frontendEnvironmentIdentity } from "./stage-inputs.mjs";

const hash = value => createHash("sha256").update(value).digest("hex");
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
function git(root, args) {
  const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8" });
  if (result.status !== 0) throw Error(result.stderr.trim() || "Git identity unavailable");
  return result.stdout.trim();
}
function source(root) {
  return { head: git(root, ["rev-parse", "HEAD"]), dirty: git(root, ["status", "--porcelain", "--untracked-files=normal"]) !== "" };
}
async function digest(filename) {
  const info = await stat(filename);
  if (!info.isDirectory()) return hash(await readFile(filename));
  const entries = [];
  for (const name of (await readdir(filename)).sort()) entries.push([name, await digest(path.join(filename, name))]);
  return hash(JSON.stringify(entries));
}
async function artifactDigests(cwd, artifacts) {
  return Promise.all(artifacts.map(async name => {
    if (!name.startsWith("@")) return [name, await digest(path.resolve(cwd, name))];
    const manifest = path.resolve(cwd, name.slice(1));
    const names = JSON.parse(await readFile(manifest, "utf8"));
    if (!Array.isArray(names) || names.some(item => typeof item !== "string")) throw Error("Invalid artifact manifest");
    return [name, hash(JSON.stringify([await digest(manifest), await Promise.all(names.map(async item => [item, await digest(path.resolve(cwd, item))]))]))];
  }));
}
async function recordAttempt(directory, record) {
  const destination = path.join(directory, "attempts");
  await mkdir(destination, { recursive: true });
  const filename = path.join(destination, `${record.name}-${Date.now()}-${randomUUID()}.json`);
  const temporary = `${filename}.tmp`;
  await writeFile(temporary, `${JSON.stringify(record, null, 2)}\n`);
  await rename(temporary, filename);
}
function environmentIdentity(env, profile) {
  // Values are hashed in memory; receipts never expose credentials/configuration.
  return Object.fromEntries(Object.entries(env).filter(([key]) => /^(PATH$|NODE_|RUST|CARGO_|MESH_|VITE_|VITEST_|PLAYWRIGHT_|EVAL_|PATCH_|FAST$|CI$|TZ$|LANG$|LC_|npm_config_|NPM_CONFIG_)/u.test(key) && !/^MESH_VALIDATION_/u.test(key) && !(profile !== "all" && /^MESH_PR_/u.test(key))).sort(([a], [b]) => a.localeCompare(b)));
}
async function lock(directory, { log, signal, timeoutMs = 7_200_000 }) {
  await mkdir(path.dirname(directory), { recursive: true });
  const started = Date.now();
  let announced = 0;
  while (true) {
    signal?.throwIfAborted();
    try {
      await mkdir(directory);
      await writeFile(path.join(directory, "owner.json"), JSON.stringify({ pid: process.pid, started: Date.now() }));
      return async () => rm(directory, { recursive: true, force: true });
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      try {
        const owner = JSON.parse(await readFile(path.join(directory, "owner.json"), "utf8"));
        try { process.kill(owner.pid, 0); }
        catch (probe) {
          if (probe.code === "ESRCH") {
            // One reaper rechecks ownership, so a second waiter cannot remove a new lock.
            const reaper = `${directory}.reaper`;
            let acquired = false;
            try {
              await mkdir(reaper); acquired = true;
              const current = JSON.parse(await readFile(path.join(directory, "owner.json"), "utf8"));
              try { process.kill(current.pid, 0); }
              catch (check) { if (check.code === "ESRCH") await rm(directory, { recursive: true, force: true }); }
            } catch (failure) { if (!["EEXIST", "ENOENT"].includes(failure.code)) throw failure; }
            finally { if (acquired) await rm(reaper, { recursive: true, force: true }); }
            continue;
          }
        }
      } catch { /* An owner may still be publishing its identity. */ }
      if (Date.now() - announced > 30_000) { log(`waiting for another validation stage (${Math.round((Date.now() - started) / 1000)}s queued)`); announced = Date.now(); }
      if (Date.now() - started > timeoutMs) throw Error(`Validation lock timed out: ${directory}`);
      await delay(100);
    }
  }
}
function executableCommand(command, env) {
  if (!["npm", "npx"].includes(command[0])) return command;
  const name = command[0] === "npm" ? "npm-cli.js" : "npx-cli.js";
  const candidates = [path.join(path.dirname(process.execPath), "node_modules/npm/bin", name), path.resolve(path.dirname(process.execPath), "../lib/node_modules/npm/bin", name)];
  const cli = candidates.find(candidate => existsSync(candidate));
  return cli ? [process.execPath, cli, ...command.slice(1)] : command;
}
async function execute(command, { cwd, env, logPath, signal }) {
  signal?.throwIfAborted();
  return new Promise(resolve => {
    const output = createWriteStream(logPath);
    let error;
    let failureKind;
    const argv = executableCommand(command, env);
    // Only the outer stage owns a POSIX process group and its kill timer.
    // Nested stage commands join that group so escalation cannot orphan a
    // detached grandchild when the outer timer kills an inner supervisor.
    const nestedGroup = process.platform !== "win32" && !!env.MESH_VALIDATION_PROCESS_GROUP_OWNER;
    const childEnv = process.platform === "win32" ? env : {
      ...env,
      MESH_VALIDATION_PROCESS_GROUP_OWNER: env.MESH_VALIDATION_PROCESS_GROUP_OWNER || String(process.pid),
    };
    const child = spawn(argv[0], argv.slice(1), { cwd, env: childEnv, shell: false, windowsHide: true, detached: process.platform !== "win32" && !nestedGroup });
    const forward = chunk => { output.write(chunk); process.stdout.write(chunk); };
    child.stdout?.on("data", forward); child.stderr?.on("data", forward);
    output.on("error", failure => { error = failure.message; child.kill(); });
    child.on("error", failure => { error = failure.message; failureKind = ["ENOENT", "EACCES"].includes(failure.code) ? "unavailable" : "failed"; });
    let cleanup;
    const abort = () => {
      if (cleanup) return;
      if (!child.pid) { cleanup = Promise.resolve(); return; }
      if (process.platform === "win32") {
        cleanup = new Promise(done => {
          const killer = spawn("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], { windowsHide: true, stdio: "ignore" });
          killer.on("error", failure => { error = `Process tree cleanup failed: ${failure.message}`; child.kill(); });
          killer.on("close", code => { if (code !== 0) child.kill(); done(); });
        });
      } else if (nestedGroup) {
        // The owner signals and escalates the whole group. Keep this stage's
        // leases until it kills us too: a closed command leader does not prove
        // ignored-stdio descendants have exited. Dead-owner locks are reaped
        // by the next waiter after the group's processes have stopped.
        cleanup = new Promise(() => {});
        child.kill("SIGTERM");
      } else {
        try { process.kill(-child.pid, "SIGTERM"); } catch { child.kill(); }
        // The leader may exit while descendants with independent stdio remain.
        // Keep the group escalation and resource lease until cleanup completes.
        cleanup = new Promise(done => setTimeout(() => {
          try { process.kill(-child.pid, "SIGKILL"); } catch { /* Already exited. */ }
          done();
        }, 2000));
      }
    };
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
    child.on("close", async code => {
      signal?.removeEventListener("abort", abort);
      await cleanup;
      output.end(() => resolve({ code: signal?.aborted ? 130 : Number.isInteger(code) && code >= 0 ? code : 1, error, failureKind }));
    });
  });
}

export async function runStage({ root, cwd = root, name, command, env = process.env, artifacts = [], context = {}, resource, profile = "all", inputs, fresh = false, signal, log = message => console.log(`[validation:${name}] ${message}`) }) {
  if (!/^[\w:.-]+$/u.test(name) || !command?.length) throw Error("A stage needs a safe name and command");
  if (!["all", "shared"].includes(profile)) throw Error("Unknown validation environment profile");
  env = { ...env, VITEST_MAX_WORKERS: env.VITEST_MAX_WORKERS || "2", RUST_TEST_THREADS: env.RUST_TEST_THREADS || "1" };
  root = path.resolve(root); cwd = path.resolve(cwd);
  const started = Date.now();
  let initial;
  try { initial = source(root); } catch { initial = { dirty: true }; }
  const declared = inputs === undefined ? undefined : await inputIdentity(root, inputs);
  const frontendEnvironment = await frontendEnvironmentIdentity(root);
  const identity = { schema: 2, source: declared ? undefined : initial.head, inputs: declared, cwd: path.relative(root, cwd), name, command, context, platform: process.platform, arch: process.arch,
    node: process.version, executable: process.execPath, environment: environmentIdentity(env, profile), frontendEnvironment, profile, artifacts };
  // Executable versions invalidate cached results after toolchain upgrades.
  const versions = {};
  for (const tool of ["cargo", "rustc", "npm"]) {
    const argv = executableCommand([tool, "--version"], env);
    const probe = spawnSync(argv[0], argv.slice(1), { env, encoding: "utf8", timeout: 10_000, shell: false });
    versions[tool] = `${probe.status}:${probe.stdout || ""}`;
  }
  identity.tools = versions;
  const configuration = [path.join(env.CARGO_HOME || path.join(homedir(), ".cargo"), "config.toml"),
    path.join(env.CARGO_HOME || path.join(homedir(), ".cargo"), "config"),
    env.npm_config_userconfig || env.NPM_CONFIG_USERCONFIG || path.join(homedir(), ".npmrc"),
    path.join(root, "frontend/node_modules/.package-lock.json")];
  identity.configuration = await Promise.all(configuration.map(async filename => {
    try { return [filename, await digest(filename)]; } catch (error) { if (error.code !== "ENOENT") throw error; return [filename, null]; }
  }));
  const key = hash(JSON.stringify(identity));
  const directory = path.join(root, "target/validation");
  const receipt = path.join(directory, "receipts", `${key}.json`);
  const reusable = !initial.dirty && !env.CI && !fresh && env.MESH_VALIDATION_FRESH !== "1";
  const release = await lock(path.join(directory, "locks", key), { log, signal });
  let releaseResource;
  try {
    if (reusable) {
      try {
        const record = JSON.parse(await readFile(receipt, "utf8"));
        if (record.schema === 1 && record.key === key && record.code === 0 && Date.now() >= record.finished && Date.now() - record.finished < 86_400_000 &&
            await digest(record.logPath) === record.logDigest && JSON.stringify(await artifactDigests(cwd, artifacts)) === JSON.stringify(record.artifacts)) {
          const current = source(root);
          if (!current.dirty && current.head === initial.head && JSON.stringify(await frontendEnvironmentIdentity(root)) === JSON.stringify(frontendEnvironment) && (!declared || JSON.stringify(await inputIdentity(root, inputs)) === JSON.stringify(declared))) {
            const reused = { ...record, source: initial.head, reusedFromSource: record.source, reused: true, receipt,
              savedExecutionMs: record.executionMs ?? record.elapsedMs - record.queuedMs,
              executionMs: 0, queuedMs: Date.now() - started, elapsedMs: Date.now() - started, finished: Date.now() };
            await recordAttempt(directory, reused);
            log(`reused successful evidence (${Math.round(reused.savedExecutionMs / 1000)}s saved) from ${record.source}: ${receipt}`);
            return reused;
          }
        }
      } catch { /* Missing, stale or damaged evidence requires execution. */ }
    }
    if (resource) {
      let shared = directory;
      try { shared = git(root, ["rev-parse", "--path-format=absolute", "--git-common-dir"]); } catch { /* Non-Git development runs use their own directory. */ }
      releaseResource = await lock(path.join(shared, "validation-resources", hash(resource)), { log, signal });
    }
    if (!initial.dirty) {
      const current = source(root);
      if (current.dirty || current.head !== initial.head) throw Error("Source changed while waiting for validation");
      if (JSON.stringify(await frontendEnvironmentIdentity(root)) !== JSON.stringify(frontendEnvironment)) throw Error("Frontend environment changed while waiting for validation");
      if (declared && JSON.stringify(await inputIdentity(root, inputs)) !== JSON.stringify(declared)) throw Error("Stage inputs changed while waiting for validation");
    }
    // A new attempt supersedes previous proof even if it fails or is cancelled.
    await rm(receipt, { force: true });
    const runId = `${Date.now()}-${randomUUID()}`;
    const logPath = path.join(directory, "logs", `${name}-${runId}.log`);
    await mkdir(path.dirname(logPath), { recursive: true });
    const queuedMs = Date.now() - started;
    log(`started after ${Math.round(queuedMs / 1000)}s queue; log ${logPath}`);
    const timer = setInterval(() => log(`running for ${Math.round((Date.now() - started) / 1000)}s; log ${logPath}`), 30_000);
    let result;
    try { result = await execute(command, { cwd, env, logPath, signal }); }
    finally { clearInterval(timer); }
    let measured = [];
    if (result.code === 0) {
      try { measured = await artifactDigests(cwd, artifacts); }
      catch (error) { result = { code: 1, error: `Required artifact missing: ${error.message}` }; }
    }
    if (result.code === 0 && !initial.dirty) {
      const current = source(root);
      if (current.dirty || current.head !== initial.head) result = { code: 1, error: "Source changed during validation", failureKind: "invalidated" };
      else if (JSON.stringify(await frontendEnvironmentIdentity(root)) !== JSON.stringify(frontendEnvironment)) result = { code: 1, error: "Frontend environment changed during validation", failureKind: "invalidated" };
      else if (declared && JSON.stringify(await inputIdentity(root, inputs)) !== JSON.stringify(declared)) result = { code: 1, error: "Stage inputs changed during validation", failureKind: "invalidated" };
    }
    const record = { schema: 1, key, name, platform: process.platform, arch: process.arch, source: initial.head, inputs: declared, reused: false, code: result.code, error: result.error, failureKind: result.failureKind, artifacts: measured, logPath, logDigest: await digest(logPath), elapsedMs: Date.now() - started, executionMs: Date.now() - started - queuedMs, queuedMs, finished: Date.now() };
    await recordAttempt(directory, record);
    if (result.code === 0 && !initial.dirty && !signal?.aborted) {
      await mkdir(path.dirname(receipt), { recursive: true });
      const temporary = `${receipt}.${randomUUID()}.tmp`;
      await writeFile(temporary, `${JSON.stringify(record, null, 2)}\n`); await rename(temporary, receipt);
    }
    log(`${result.code === 0 ? "passed" : "failed"} in ${Math.round(record.elapsedMs / 1000)}s${result.error ? `: ${result.error}` : ""}`);
    return { ...record, reused: false, receipt };
  } finally { await releaseResource?.(); await release(); }
}

async function main(args) {
  const separator = args.indexOf("--");
  if (separator < 1) throw Error("Usage: stage.mjs <name> [--artifact path] [--resource name] [--context text] -- command [args]");
  const name = args[0]; const artifacts = []; let resource; let context = {}; let profile = "all"; let inputs; let resultFile;
  for (let index = 1; index < separator; index += 2) {
    const value = args[index + 1];
    if (!value) throw Error(`Missing value: ${args[index]}`);
    if (args[index] === "--artifact") artifacts.push(value);
    else if (args[index] === "--result") resultFile = path.resolve(value);
    else if (args[index] === "--resource") resource = value;
    else if (args[index] === "--context") context = { value };
    else if (args[index] === "--profile") profile = value;
    else if (args[index] === "--inputs") inputs = inputGroup(value, name);
    else throw Error(`Unknown option: ${args[index]}`);
  }
  const root = git(process.cwd(), ["rev-parse", "--show-toplevel"]);
  const controller = new AbortController();
  const stop = () => controller.abort();
  process.on("SIGINT", stop); process.on("SIGTERM", stop);
  try {
    const result = await runStage({ root, name, command: args.slice(separator + 1), cwd: process.cwd(), artifacts, resource, context, profile, inputs, signal: controller.signal });
    if (resultFile) {
      await mkdir(path.dirname(resultFile), { recursive: true });
      const temporary = `${resultFile}.${randomUUID()}.tmp`;
      await writeFile(temporary, `${JSON.stringify(result, null, 2)}\n`);
      await rename(temporary, resultFile);
    }
    process.exitCode = result.code;
  } finally { process.off("SIGINT", stop); process.off("SIGTERM", stop); }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1; });
