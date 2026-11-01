/** Windows command ownership. No target is launched outside the helper's job. */
import { spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { tmpdir } from "node:os";
import path from "node:path";
import { Writable } from "node:stream";
import { fileURLToPath } from "node:url";

const helper = fileURLToPath(new URL("./windows-process-owner.ps1", import.meta.url));
const source = fileURLToPath(new URL("./windows-process-owner.cs", import.meta.url));
const literal = value => `'${value.replaceAll("'", "''")}'`;

function environmentEntries(env) {
  // Match Node's case-insensitive Windows environment selection. libuv also
  // inherits these system variables when omitted from an explicit env object.
  const entries = new Map();
  for (const name of Object.keys(env).sort()) {
    if (env[name] !== undefined && !entries.has(name.toUpperCase()))
      entries.set(name.toUpperCase(), [name, String(env[name])]);
  }
  for (const name of ["HOMEDRIVE", "HOMEPATH", "LOGONSERVER", "PATH", "SYSTEMDRIVE", "SYSTEMROOT", "TEMP", "USERDOMAIN", "USERNAME", "USERPROFILE", "WINDIR"]) {
    if (!entries.has(name) && process.env[name] !== undefined) entries.set(name, [name, process.env[name]]);
  }
  return [...entries.values()];
}

async function executable(program, cwd, entries) {
  // Preserve libuv's .com/.exe search rather than adding cmd.exe or PATHEXT.
  const directories = /[\\/:]/u.test(program) ? [""] : [
    ...(process.env.NoDefaultCurrentDirectoryInExePath === undefined ? [""] : []),
    ...(entries.find(([name]) => name.toUpperCase() === "PATH")?.[1] || "").split(";").filter(Boolean).map(value => value.replace(/^"|"$/gu, "")),
  ];
  const basename = path.win32.basename(program);
  const dot = basename.indexOf(".");
  const names = dot >= 0 && dot < basename.length - 1 ? [program] : [];
  names.push(`${program.endsWith(".") ? program : `${program}.`}com`, `${program.endsWith(".") ? program : `${program}.`}exe`);
  for (const directory of directories) for (const name of names) {
    const candidate = path.resolve(cwd, directory, name);
    try { if (!(await stat(candidate)).isDirectory()) return candidate; }
    catch (error) { if (!["ENOENT", "ENOTDIR", "EACCES"].includes(error.code)) throw error; }
  }
  throw Object.assign(Error("Command executable is unavailable"), { code: "ENOENT" });
}

export async function executeWindows(command, { cwd = process.cwd(), env = process.env, logPath, signal, createLogStream = createWriteStream, onOutput = chunk => process.stdout.write(chunk), spawnOwner = spawn }) {
  signal?.throwIfAborted();
  // CreateProcessW and environment blocks are NUL-terminated. Reject rather
  // than silently executing a truncated command or environment value.
  for (const value of [...command, cwd, ...Object.entries(env).flat()]) {
    if (value !== undefined && String(value).includes("\0"))
      throw Object.assign(TypeError("Windows command inputs cannot contain NUL"), { code: "ERR_INVALID_ARG_VALUE" });
  }
  const entries = environmentEntries(env);
  let app;
  try { app = await executable(command[0], cwd, entries); }
  catch (error) {
    const result = { code: 1, exitCode: null, error: error.message, failureKind: ["ENOENT", "EACCES"].includes(error.code) ? "unavailable" : "failed", cleanupUnverified: false, forcedRetirement: false };
    // runStage hashes the declared log for failed launches too. Preserve that
    // artifact contract even when resolution proves no helper can be admitted.
    if (logPath) {
      const output = createLogStream(logPath);
      await new Promise(resolve => {
        const failed = failure => { result.error = failure.message; result.failureKind = "failed"; resolve(); };
        output.on("error", failed);
        output.end(failure => { if (failure) failed(failure); else resolve(); });
      });
    }
    return result;
  }
  signal?.throwIfAborted();
  const directory = await mkdtemp(path.join(tmpdir(), "mesh-command-owner-"));
  try {
    signal?.throwIfAborted();
    const receipt = path.join(directory, "retired.json");
    const nonce = randomUUID();
    const output = logPath ? createLogStream(logPath) : new Writable({ write(_chunk, _encoding, done) { done(); } });
    let failure, helperFailure, abortTimer;
    let ownerClosed = false;
    const powershell = path.join(process.env.SystemRoot || "C:/Windows", "System32/WindowsPowerShell/v1.0/powershell.exe");
    const loader = `& ([ScriptBlock]::Create([IO.File]::ReadAllText(${literal(helper)}))) -Source ${literal(source)}`;
    const child = spawnOwner(powershell, ["-NoProfile", "-NonInteractive", "-EncodedCommand", Buffer.from(loader, "utf16le").toString("base64")], { cwd, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
    const abort = () => {
      if (abortTimer || ownerClosed) return;
      child.stdin.write("abort\n");
      // A wedged/dead helper cannot certify retirement. Killing its retained
      // handle closes its sole job handle, but never substitutes for a receipt.
      abortTimer = setTimeout(() => child.kill(), 7000);
    };
    const forward = chunk => { output.write(chunk); onOutput(chunk); };
    child.stdout.on("data", forward); child.stderr.on("data", forward);
    output.on("error", error => { failure ||= error.message; abort(); });
    child.on("error", error => { helperFailure = error; });
    child.stdin.on("error", () => { /* Missing retirement receipt fails closed. */ });
    const closed = new Promise(resolve => child.on("close", (code, helperSignal) => resolve({ code, helperSignal })));
    child.stdin.write(`${JSON.stringify({ app, args: command.slice(1).map(String), environment: entries.map(([name, value]) => `${name}=${value}`), cwd, receipt, nonce })}\n`);
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
    const status = await closed;
    ownerClosed = true;
    clearTimeout(abortTimer);
    signal?.removeEventListener("abort", abort);
    await new Promise(resolve => output.end(error => { failure ||= error?.message; resolve(); }));
    let record;
    try { record = JSON.parse(await readFile(receipt, "utf8")); } catch { /* Unknown retirement retains the owner's leases. */ }
    const result = record?.schema === 1 && record.nonce === nonce ? record.result : undefined;
    const verified = result?.retired === true;
    const error = failure || helperFailure?.message || result?.error ||
      (!verified ? "Windows command retirement receipt is missing or unverified" :
        status.code !== 0 ? "Windows command owner failed" : undefined);
    return { code: signal?.aborted ? 130 : error ? 1 : result.exitCode, exitCode: result?.exitCode ?? null,
      error, failureKind: error ? "failed" : undefined, forcedRetirement: verified && result.forcedRetirement === true, cleanupUnverified: !verified && !!child.pid, signal: status.helperSignal };
  } finally { await rm(directory, { recursive: true, force: true }); }
}
