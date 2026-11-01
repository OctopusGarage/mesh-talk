#!/usr/bin/env node
/** Local acceleration only: hosted checks always execute. Shell arguments stay argv data. */
import { createHash, randomUUID } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { createWriteStream, existsSync } from "node:fs";
import { mkdir, readFile, readdir, writeFile, rename, rm, stat } from "node:fs/promises";
import path from "node:path";
import { Writable } from "node:stream";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";
import { inputGroup, inputIdentity, frontendEnvironmentIdentity } from "./stage-inputs.mjs";

const hash = value => createHash("sha256").update(value).digest("hex");
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function observeInputs(root, configuration) {
  const tracked = git(root, ["ls-files", "--cached", "--others", "--exclude-standard", "-z"]).split("\0").filter(Boolean);
  const filenames = [...tracked.map(name => path.join(root, name)), ...configuration];
  try { for (const name of await readdir(path.join(root, "frontend"))) if (/^\.env(?:\.|$)/u.test(name)) filenames.push(path.join(root, "frontend", name)); }
  catch (error) { if (error.code !== "ENOENT") throw error; }
  const metadata = () => Promise.all(filenames.map(async filename => {
    try {
      const info = await stat(filename, { bigint: true });
      return [filename, ...[info.dev, info.ino, info.size, info.mtimeNs, info.ctimeNs].map(String)];
    } catch (error) { if (error.code === "ENOENT") return [filename, null]; throw error; }
  }));
  const initial = JSON.stringify(await metadata());
  return { changed: async () => JSON.stringify(await metadata()) !== initial,
    close: () => {} };
}

function processGroupRunning(group) {
  const result = spawnSync("ps", ["-eo", "pgid=,stat="], { encoding: "utf8", timeout: 5000 });
  if (result.status !== 0) throw Error("Cannot verify process-group termination");
  return result.stdout.split("\n").some(line => {
    const [id, state] = line.trim().split(/\s+/u);
    return Number(id) === group && state && !state.startsWith("Z");
  });
}
export async function waitForProcessGroup(group, { probe = processGroupRunning, pause = delay, attempts = 50 } = {}) {
  for (let attempt = 0; attempt < attempts; attempt++) {
    try { if (!probe(group)) return true; } catch { /* Unknown is not terminated. */ }
    await pause(100);
  }
  return false;
}
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
      const owner = { pid: process.pid, started: Date.now() };
      // PID death cannot prove a crashed Windows job owner retired its work.
      // Only the live owner with its receipt may release these leases.
      if (process.platform === "win32") owner.windowsRetirementRequired = true;
      if (process.platform !== "win32" && process.env.MESH_VALIDATION_PROCESS_GROUP_OWNER) {
        const probe = spawnSync("ps", ["-o", "pgid=", "-p", String(process.pid)], { encoding: "utf8" });
        const group = Number(probe.stdout?.trim());
        if (probe.status === 0 && Number.isInteger(group) && group > 1) owner.group = group;
        else owner.quarantined = "Cannot establish nested process group";
      }
      await writeFile(path.join(directory, "owner.json"), JSON.stringify(owner));
      const release = async () => rm(directory, { recursive: true, force: true });
      release.quarantine = async reason => writeFile(path.join(directory, "owner.json"), JSON.stringify({ ...owner, quarantined: reason }));
      return release;
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
      try {
        const owner = JSON.parse(await readFile(path.join(directory, "owner.json"), "utf8"));
        try { process.kill(owner.pid, 0); }
        catch (probe) {
          if (probe.code === "ESRCH" && !owner.quarantined && !owner.windowsRetirementRequired) {
            // One reaper rechecks ownership, so a second waiter cannot remove a new lock.
            const reaper = `${directory}.reaper`;
            let acquired = false;
            try {
              await mkdir(reaper); acquired = true;
              const current = JSON.parse(await readFile(path.join(directory, "owner.json"), "utf8"));
              try { process.kill(current.pid, 0); }
              catch (check) {
                if (check.code === "ESRCH" && !current.quarantined && !current.windowsRetirementRequired &&
                    (!current.group || !processGroupRunning(current.group)))
                  await rm(directory, { recursive: true, force: true });
              }
            } catch (failure) { if (!["EEXIST", "ENOENT"].includes(failure.code)) throw failure; }
            finally { if (acquired) await rm(reaper, { recursive: true, force: true }); }
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
// The inherited marker names a live supervisor ancestor, not an arbitrary PID.
// Validate that relationship again before signalling; stale markers cannot grant
// authority over a re-used PID belonging to another process tree.
function supervisorAncestor(owner) {
  if (!Number.isSafeInteger(owner) || owner <= 1 || owner === process.pid) return false;
  let ancestor = process.ppid;
  for (let depth = 0; ancestor > 1 && depth < 64; depth++) {
    if (ancestor === owner) return true;
    const result = spawnSync("ps", ["-o", "ppid=", "-p", String(ancestor)], { encoding: "utf8", timeout: 5000 });
    if (result.status !== 0) return false;
    ancestor = Number(result.stdout.trim());
  }
  return false;
}
export async function execute(command, { cwd, env, logPath, signal, createLogStream = createWriteStream, onOutput = chunk => process.stdout.write(chunk), detached = true, notifyOwnerOnAbort = false }) {
  signal?.throwIfAborted();
  if (process.platform === "win32") {
    const { executeWindows } = await import("./windows-process-owner.mjs");
    return executeWindows(executableCommand(command, env), { cwd, env, logPath, signal, createLogStream, onOutput });
  }
  if (process.platform !== "win32" && !detached && !env.MESH_VALIDATION_PROCESS_GROUP_OWNER)
    throw Error("Non-detached execution requires an existing managed process-group owner");
  return new Promise(resolve => {
    const output = logPath ? createLogStream(logPath) : new Writable({ write(_chunk, _encoding, done) { done(); } });
    let error;
    let failureKind;
    let cleanupUnverified = false;
    const argv = executableCommand(command, env);
    // Only the outer stage owns a POSIX process group and its kill timer.
    // Nested stage commands join that group so escalation cannot orphan a
    // detached grandchild when the outer timer kills an inner supervisor.
    const nestedGroup = process.platform !== "win32" && !!env.MESH_VALIDATION_PROCESS_GROUP_OWNER;
    const childEnv = process.platform === "win32" ? env : {
      ...env,
      MESH_VALIDATION_PROCESS_GROUP_OWNER: env.MESH_VALIDATION_PROCESS_GROUP_OWNER || String(process.pid),
    };
    const child = spawn(argv[0], argv.slice(1), { cwd, env: childEnv, shell: false, windowsHide: true, detached: process.platform !== "win32" && !nestedGroup && detached });
    const forward = chunk => { output.write(chunk); onOutput(chunk); };
    child.stdout?.on("data", forward); child.stderr?.on("data", forward);
    output.on("error", failure => { error = failure.message; failureKind = "failed"; abort(); });
    child.on("error", failure => { error = failure.message; failureKind = ["ENOENT", "EACCES"].includes(failure.code) ? "unavailable" : "failed"; });
    let cleanup;
    const abort = () => {
      if (cleanup) return;
      if (!child.pid) { cleanup = Promise.resolve(); return; }
      if (nestedGroup) {
        // The owner signals and escalates the whole group. Keep this stage's
        // leases until it kills us too: a closed command leader does not prove
        // ignored-stdio descendants have exited. Dead-owner locks are reaped
        // by the next waiter after the group's processes have stopped.
        cleanup = new Promise(() => {});
        if (!signal?.aborted || notifyOwnerOnAbort) {
          // A local log failure must also notify the outer CLI owner; otherwise
          // nobody would start its group escalation and this lease would hang.
          try {
            const owner = Number(env.MESH_VALIDATION_PROCESS_GROUP_OWNER);
            if (!supervisorAncestor(owner)) throw Error("Process-group owner is not a live supervisor ancestor");
            process.kill(owner, "SIGTERM");
          }
          catch {
            cleanupUnverified = true;
            error ||= "Outer process-group owner is unavailable";
            cleanup = Promise.resolve();
          }
        }
        child.kill("SIGTERM");
      } else {
        try { process.kill(-child.pid, "SIGTERM"); } catch { child.kill(); }
        // The leader may exit while descendants with independent stdio remain.
        // Keep the group escalation and resource lease until cleanup completes.
        cleanup = new Promise(done => setTimeout(async () => {
          try { process.kill(-child.pid, "SIGKILL"); } catch { /* Already exited. */ }
          cleanupUnverified = !await waitForProcessGroup(child.pid);
          if (cleanupUnverified) error = "Process-group termination could not be verified";
          done();
        }, 2000));
      }
    };
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
    child.on("close", async (code, childSignal) => {
      // Only this owner created the child's group. Nested commands share a
      // live supervisor/sibling group; its final outer stage owns retirement.
      if (!cleanup && process.platform !== "win32" && !nestedGroup && detached && child.pid) {
        try {
          if (processGroupRunning(child.pid)) {
            error ||= "Command leader exited while owned descendants remained runnable";
            failureKind ||= "failed";
            abort();
          }
        } catch {
          error ||= "Command descendant retirement could not be verified";
          failureKind ||= "failed";
          abort();
        }
      }
      await new Promise(done => output.end(failure => {
        if (failure) { error ||= failure.message; failureKind ||= "failed"; abort(); }
        done();
      }));
      await cleanup;
      signal?.removeEventListener("abort", abort);
      resolve({ code: signal?.aborted ? 130 : error ? 1 : Number.isInteger(code) && code >= 0 ? code : 1, error, failureKind, cleanupUnverified, exitCode: code, signal: childSignal });
    });
  });
}

export async function runStage({ root, cwd = root, name, command, env = process.env, artifacts = [], context = {}, resource, profile = "all", inputs, fresh = false, signal, createLogStream, log = message => console.log(`[validation:${name}] ${message}`) }) {
  if (!/^[\w:.-]+$/u.test(name) || !command?.length) throw Error("A stage needs a safe name and command");
  if (!["all", "shared"].includes(profile)) throw Error("Unknown validation environment profile");
  // Cargo owns the repository's Rust thread budget. Preserve caller overrides
  // (including serial sanitizer/Windows runs) without injecting a competing
  // default into aggregate stages and every Cargo child they launch.
  env = { ...env, VITEST_MAX_WORKERS: env.VITEST_MAX_WORKERS || "2" };
  root = path.resolve(root); cwd = path.resolve(cwd);
  const started = Date.now();
  let initial;
  try { initial = source(root); } catch { initial = { dirty: true }; }
  const declared = inputs === undefined ? undefined : await inputIdentity(root, inputs);
  const frontendEnvironment = await frontendEnvironmentIdentity(root);
  const frontendUnchanged = async () => JSON.stringify(await frontendEnvironmentIdentity(root)) === JSON.stringify(frontendEnvironment);
  const identity = { schema: 2, source: declared ? undefined : initial.head, inputs: declared, cwd: path.relative(root, cwd), name, command, context, platform: process.platform, arch: process.arch,
    node: process.version, executable: process.execPath, environment: environmentIdentity(env, profile), frontendEnvironment, profile, artifacts };
  // Executable versions invalidate cached results after toolchain upgrades.
  const tools = () => Object.fromEntries(["cargo", "rustc", "npm"].map(tool => {
    const argv = executableCommand([tool, "--version"], env);
    const probe = spawnSync(argv[0], argv.slice(1), { env, encoding: "utf8", timeout: 10_000, shell: false });
    return [tool, `${probe.status}:${probe.stdout || ""}`];
  }));
  identity.tools = tools();
  const configuration = [path.join(env.CARGO_HOME || path.join(homedir(), ".cargo"), "config.toml"),
    path.join(env.CARGO_HOME || path.join(homedir(), ".cargo"), "config"),
    env.npm_config_userconfig || env.NPM_CONFIG_USERCONFIG || path.join(homedir(), ".npmrc"),
    path.join(root, "frontend/node_modules/.package-lock.json")];
  const configurationIdentity = () => Promise.all(configuration.map(async filename => {
    try { return [filename, await digest(filename)]; } catch (error) { if (error.code !== "ENOENT") throw error; return [filename, null]; }
  }));
  identity.configuration = await configurationIdentity();
  const runtimeUnchanged = async () => JSON.stringify(await configurationIdentity()) === JSON.stringify(identity.configuration) &&
    JSON.stringify(tools()) === JSON.stringify(identity.tools);
  const key = hash(JSON.stringify(identity));
  const directory = path.join(root, "target/validation");
  const receipt = path.join(directory, "receipts", `${key}.json`);
  const reusable = !initial.dirty && !env.CI && !fresh && env.MESH_VALIDATION_FRESH !== "1";
  const release = await lock(path.join(directory, "locks", key), { log, signal });
  let releaseResource;
  let cleanupUnverified = false;
  let observation;
  let delivered;
  try {
    observation = await observeInputs(root, configuration);
    if (reusable) {
      try {
        const record = JSON.parse(await readFile(receipt, "utf8"));
        if (record.schema === 1 && record.key === key && record.code === 0 && Date.now() >= record.finished && Date.now() - record.finished < 86_400_000 &&
            await digest(record.logPath) === record.logDigest && JSON.stringify(await artifactDigests(cwd, artifacts)) === JSON.stringify(record.artifacts)) {
          const current = source(root);
          if (!current.dirty && current.head === initial.head && await runtimeUnchanged() && !await observation.changed() && JSON.stringify(await frontendEnvironmentIdentity(root)) === JSON.stringify(frontendEnvironment) && (!declared || JSON.stringify(await inputIdentity(root, inputs)) === JSON.stringify(declared))) {
            const reused = { ...record, source: initial.head, reusedFromSource: record.source, reused: true, receipt,
              savedExecutionMs: record.executionMs ?? record.elapsedMs - record.queuedMs,
              executionMs: 0, queuedMs: Date.now() - started, elapsedMs: Date.now() - started, finished: Date.now() };
            await recordAttempt(directory, reused);
            log(`reused successful evidence (avoided ${Math.round(reused.savedExecutionMs / 1000)}s of prior execution; wall-clock saving unmeasured) from ${record.source}: ${receipt}`);
            signal?.throwIfAborted();
            delivered = reused;
            return delivered;
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
    if (!await runtimeUnchanged() || await observation.changed()) throw Error("Stage inputs changed while waiting for validation (tool/configuration identity or observed edit)");
    // A new attempt supersedes previous proof even if it fails or is cancelled.
    await rm(receipt, { force: true });
    const runId = `${Date.now()}-${randomUUID()}`;
    const logPath = path.join(directory, "logs", `${name}-${runId}.log`);
    await mkdir(path.dirname(logPath), { recursive: true });
    const queuedMs = Date.now() - started;
    log(`started after ${Math.round(queuedMs / 1000)}s queue; log ${logPath}`);
    const timer = setInterval(() => log(`running for ${Math.round((Date.now() - started - queuedMs) / 1000)}s; log ${logPath}`), 30_000);
    let result;
    try { result = await execute(command, { cwd, env, logPath, signal, createLogStream }); }
    finally { clearInterval(timer); }
    cleanupUnverified = result.cleanupUnverified;
    if (cleanupUnverified) {
      const reason = result.error || "Descendant termination was not verified";
      await release.quarantine(reason);
      await releaseResource?.quarantine(reason);
      log(`resource locks quarantined: ${reason}; verify descendant termination before manual recovery`);
    }
    if (result.code === 0 && await observation.changed())
      result = { code: 1, error: "Source changed during validation (source/configuration metadata)", failureKind: "invalidated" };
    if (result.code === 0 && !await runtimeUnchanged())
      result = { code: 1, error: "Stage inputs changed during validation (tool/configuration identity)", failureKind: "invalidated" };
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
    const record = { schema: 1, key, name, platform: process.platform, arch: process.arch, source: initial.head, inputs: declared, reused: false, code: result.code, error: result.error, failureKind: result.failureKind, cleanupUnverified, artifacts: measured, logPath, logDigest: await digest(logPath), elapsedMs: Date.now() - started, executionMs: Date.now() - started - queuedMs, queuedMs, finished: Date.now() };
    const cancelledDuringFinalization = async () => {
      if (record.code !== 0 || !signal?.aborted) return false;
      record.code = 130; record.error = "Validation cancelled during finalization";
      record.finished = Date.now(); record.elapsedMs = record.finished - started;
      await rm(receipt, { force: true });
      return true;
    };
    if (result.code === 0 && !initial.dirty && !signal?.aborted) {
      await mkdir(path.dirname(receipt), { recursive: true });
      const temporary = `${receipt}.${randomUUID()}.tmp`;
      await writeFile(temporary, `${JSON.stringify(record, null, 2)}\n`); await rename(temporary, receipt);
    }
    // Resample after asynchronous log/artifact/receipt I/O. Metadata detects an
    // ordinary reverted edit even when its final content hash is unchanged.
    await new Promise(resolve => setImmediate(resolve));
    const finalSource = source(root);
    if (record.code === 0 && (await observation.changed() || !await frontendUnchanged() || !initial.dirty &&
        (finalSource.dirty || finalSource.head !== initial.head || !await runtimeUnchanged()))) {
      record.code = 1; record.failureKind = "invalidated";
      record.error = "Stage inputs changed during validation finalization";
      await rm(receipt, { force: true });
    }
    await cancelledDuringFinalization();
    await recordAttempt(directory, record);
    await new Promise(resolve => setImmediate(resolve));
    if (record.code === 0 && (await observation.changed() || !await frontendUnchanged())) {
      record.code = 1; record.failureKind = "invalidated";
      record.error = "Stage inputs changed during validation finalization";
      await rm(receipt, { force: true });
      await recordAttempt(directory, record);
    }
    log(`${record.code === 0 ? "passed" : "failed"} in ${Math.round(record.elapsedMs / 1000)}s${record.error ? `: ${record.error}` : ""}`);
    if (await cancelledDuringFinalization()) {
      await recordAttempt(directory, record);
      log("cancelled during finalization; successful receipt removed");
    }
    delivered = { ...record, reused: false, receipt };
    return delivered;
  } finally {
    observation?.close();
    if (!cleanupUnverified) { await releaseResource?.(); await release(); }
    // The returned object is shared with this finalizer: cancellation while
    // asynchronous lease release runs must not deliver a successful verdict.
    if (delivered?.code === 0) {
      const current = source(root);
      const invalidated = await observation.changed() || !await frontendUnchanged() ||
        !initial.dirty && (current.dirty || current.head !== initial.head);
      if (signal?.aborted || invalidated) {
        delivered.code = signal?.aborted ? 130 : 1; delivered.reused = false;
        delivered.failureKind = signal?.aborted ? undefined : "invalidated";
        delivered.error = signal?.aborted ? "Validation cancelled during finalization" : "Stage inputs changed during validation finalization";
        delivered.finished = Date.now(); delivered.elapsedMs = delivered.finished - started;
        await rm(receipt, { force: true });
        await recordAttempt(directory, delivered);
      }
    }
  }
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
