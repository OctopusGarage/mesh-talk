// Linux VM-only exit-crash evidence. Never substitutes for the test exit status.
import { execute } from "../validation/stage.mjs";
import { createHash, randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import {
  mkdir,
  open,
  readFile,
  writeFile,
  realpath,
  rename,
  lstat,
  readdir,
  rm,
} from "node:fs/promises";
import { join, resolve, sep, basename } from "node:path";
import { fileURLToPath } from "node:url";

export const limits = Object.freeze({
  imageBytes: 2 * 1024 ** 3,
  coreBytes: 256 * 1024 ** 2,
  cores: 8,
  textBytes: 1024 ** 2,
  perCoreMs: 30_000,
  totalMs: 120_000,
});
export function decodeCoreName(name) {
  const match = /^(!.+)\.([1-9][0-9]*)\.core$/u.exec(name);
  if (!match || name.includes("/") || name.includes("\0"))
    throw Error("Invalid encoded core name");
  return { executable: match[1].replaceAll("!", "/"), pid: match[2] };
}
export async function validateExecutable(name, workspace) {
  const decoded = decodeCoreName(name);
  const owner = await realpath(
    join(workspace, "target/llvm-cov-target/debug/deps"),
  );
  const executable = await realpath(decoded.executable);
  if (
    !executable.startsWith(owner + sep) ||
    !(await lstat(executable)).isFile() ||
    !((await lstat(executable)).mode & 0o111)
  )
    throw Error("Core executable is outside the exact coverage owner");
  return { ...decoded, executable };
}
export function boundedCapture(max = limits.textBytes) {
  const chunks = [];
  let bytes = 0;
  let truncated = false;
  return {
    append(chunk) {
      const data = Buffer.from(chunk);
      const available = Math.max(0, max - bytes);
      if (data.length > available) truncated = true;
      if (available) chunks.push(data.subarray(0, available));
      bytes += Math.min(available, data.length);
    },
    result() {
      return { output: Buffer.concat(chunks).toString(), truncated };
    },
  };
}
async function command(argv, { ms = 30_000, expected = 0 } = {}) {
  const controller = new AbortController();
  const capture = boundedCapture();
  const timer = setTimeout(() => controller.abort(), ms);
  try {
    const result = await execute(argv, {
      cwd: process.cwd(),
      env: { ...process.env, DEBUGINFOD_URLS: "", LC_ALL: "C" },
      signal: controller.signal,
      onOutput: (chunk) => capture.append(chunk),
    });
    const evidence = { ...result, ...capture.result() };
    if (
      result.cleanupUnverified !== false ||
      result.error ||
      controller.signal.aborted ||
      (expected !== null && result.code !== expected)
    )
      throw Object.assign(Error(`Command failed: ${argv[0]}`), { evidence });
    return evidence;
  } finally {
    clearTimeout(timer);
  }
}
async function digest(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}
function paths() {
  if (!process.env.RUNNER_TEMP) throw Error("RUNNER_TEMP is required");
  const root = resolve(process.env.RUNNER_TEMP, "native-crash-diagnostics");
  return {
    root,
    state: join(root, "state.json"),
    image: join(root, "cores.ext4"),
    mount: join(root, "cores"),
    reports: join(root, "reports"),
  };
}
export async function save(p, state, { write = writeFile } = {}) {
  // Publish complete recovery authority; interruption cannot truncate the prior receipt.
  const candidate = `${p.state}.${randomUUID()}.tmp`;
  try {
    await write(candidate, JSON.stringify(state, null, 2));
    await rename(candidate, p.state);
  } finally {
    await rm(candidate, { force: true });
  }
}
async function metadata(workspace) {
  return {
    source: process.env.GITHUB_SHA,
    run: process.env.GITHUB_RUN_ID,
    attempt: process.env.GITHUB_RUN_ATTEMPT,
    workspace,
    node: process.version,
    platform: process.platform,
    kernel: (await command(["uname", "-a"])).output.trim(),
    tools: Object.fromEntries(
      await Promise.all(
        [
          ["gdb", "--version"],
          ["rustc", "-vV"],
          ["cargo", "--version"],
          ["cargo", "llvm-cov", "--version"],
          ["cargo", "nextest", "--version"],
        ].map(async (argv) => [
          argv.slice(0, -1).join(" "),
          (await command(argv)).output.trim(),
        ]),
      ),
    ),
    os: await readFile("/etc/os-release", "utf8"),
  };
}
async function restore(p, state) {
  const failures = [];
  if (state.patternChanged) {
    try {
      await command([
        "sudo",
        "sysctl",
        "-w",
        `kernel.core_pattern=${state.originalPattern}`,
      ]);
      state.patternChanged = false;
      await save(p, state);
    } catch (error) {
      failures.push(error.message);
    }
  }
  try {
    const probe = await command(
      ["findmnt", "--mountpoint", p.mount, "-n", "-o", "SOURCE"],
      { expected: null },
    );
    if (probe.code === 0) {
      if (!/^\/dev\/loop[0-9]+$/u.test(probe.output.trim()))
        throw Error("Unexpected crash mount owner");
      const backing = await command([
        "sudo",
        "losetup",
        "--noheadings",
        "--raw",
        "--output",
        "BACK-FILE",
        probe.output.trim(),
      ]);
      if ((await realpath(backing.output.trim())) !== (await realpath(p.image)))
        throw Error("Loop device does not own the crash image");
      state.mounted = true;
    } else if (probe.code === 1) state.mounted = false;
    else throw Error("Crash mount state is unknown");
  } catch (error) {
    state.mounted = true;
    failures.push(error.message);
  }
  if (state.mounted && failures.length === 0) {
    try {
      await command(["sudo", "umount", p.mount]);
      state.mounted = false;
      await save(p, state);
    } catch (error) {
      failures.push(error.message);
    }
  }
  if (!state.mounted) await rm(p.image, { force: true });
  if (failures.length)
    throw Error(
      `Crash diagnostic cleanup unverified; retain state/image: ${failures.join("; ")}`,
    );
}
async function setup(p) {
  await mkdir(p.reports, { recursive: true });
  try {
    await lstat(p.state);
    throw Error("Existing crash diagnostic state must be collected first");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const state = {
    originalPattern: (
      await readFile("/proc/sys/kernel/core_pattern", "utf8")
    ).trim(),
    mounted: false,
    patternChanged: false,
    workspace: await realpath(process.env.GITHUB_WORKSPACE || process.cwd()),
  };
  await save(p, state);
  try {
    await mkdir(p.mount, { recursive: true });
    const image = await open(p.image, "wx");
    try {
      await image.truncate(limits.imageBytes);
    } finally {
      await image.close();
    }
    await command(["sudo", "mkfs.ext4", "-q", "-F", p.image]);
    await command([
      "sudo",
      "mount",
      "-o",
      "loop,nodev,nosuid,noexec",
      p.image,
      p.mount,
    ]);
    state.mounted = true;
    await save(p, state);
    await command([
      "sudo",
      "chown",
      `${process.getuid()}:${process.getgid()}`,
      p.mount,
    ]);
    const pattern = `${p.mount}/%E.%p.core`;
    if (Buffer.byteLength(pattern) >= 128)
      throw Error("core_pattern exceeds kernel limit");
    state.patternChanged = true;
    await save(p, state);
    await command(["sudo", "sysctl", "-w", `kernel.core_pattern=${pattern}`]);
    await writeFile(
      join(p.reports, "metadata.json"),
      JSON.stringify(
        { ...(await metadata(state.workspace)), limits, corePattern: pattern },
        null,
        2,
      ),
    );
  } catch (error) {
    await restore(p, state);
    throw error;
  }
}
export function coreOutcome(result, possiblyTruncated = false) {
  if (result.truncated) return "text-truncated";
  if (
    possiblyTruncated ||
    /core file may be truncated|segment extending past end|truncated core/iu.test(
      result.output,
    )
  )
    return "core-truncated";
  if (
    /core file may not match|not a core dump|exec file is newer than core file/iu.test(
      result.output,
    )
  )
    return "core-mismatch";
  if (
    !/Core was generated by/u.test(result.output) ||
    !/Program terminated with signal SIG[A-Z0-9]+/u.test(result.output) ||
    !/^#0\s/mu.test(result.output)
  )
    return "core-unconfirmed";
  return "symbolized";
}
async function symbolize(p, state, name, ms) {
  const core = join(p.mount, name);
  const report = { name, outcome: "invalid" };
  try {
    const stat = await lstat(core);
    if (!stat.isFile() || stat.isSymbolicLink())
      throw Error("Core is not a regular file");
    Object.assign(report, await validateExecutable(name, state.workspace), {
      coreBytes: stat.size,
      possiblyTruncated: stat.size >= limits.coreBytes,
    });
    report.binarySha256 = await digest(report.executable);
    const result = await command(
      [
        "gdb",
        "-nx",
        "-nh",
        "-batch",
        "-iex",
        "set auto-load off",
        "-iex",
        "set debuginfod enabled off",
        "-ex",
        "set pagination off",
        "-iex",
        "set print frame-arguments none",
        "-iex",
        "set print entry-values no",
        "-ex",
        "info files",
        "-ex",
        "info sharedlibrary",
        "-ex",
        "thread apply all bt 64",
        report.executable,
        core,
      ],
      { ms },
    );
    Object.assign(report, result, {
      outcome: coreOutcome(result, report.possiblyTruncated),
    });
  } catch (error) {
    Object.assign(report, { error: error.message, ...error.evidence });
  } finally {
    await rm(core, { force: true });
  }
  await writeFile(
    join(
      p.reports,
      `${createHash("sha256").update(basename(name)).digest("hex")}.json`,
    ),
    JSON.stringify(report, null, 2),
  );
  return report;
}
async function collect(p) {
  let state;
  try {
    state = JSON.parse(await readFile(p.state, "utf8"));
  } catch (error) {
    if (error.code === "ENOENT") {
      console.warn("No Linux crash setup state; no evidence collected");
      return;
    }
    throw error;
  }
  const reports = [];
  let omittedCount = 0;
  const started = Date.now();
  try {
    const names = (await readdir(p.mount))
      .filter((name) => name !== "lost+found")
      .sort();
    for (const name of names) {
      const remaining = limits.totalMs - (Date.now() - started);
      if (reports.length < limits.cores && remaining > 0)
        reports.push(
          await symbolize(
            p,
            state,
            name,
            Math.min(limits.perCoreMs, remaining),
          ),
        );
      else {
        omittedCount++;
        await rm(join(p.mount, name), { force: true });
      }
    }
    if (!names.length) console.warn("No native cores found");
    await writeFile(
      join(p.reports, "summary.json"),
      JSON.stringify({ reports, omittedCount }, null, 2),
    );
  } finally {
    await restore(p, state);
  }
  if (omittedCount || reports.some((report) => report.outcome !== "symbolized"))
    throw Error(
      "Native crash evidence is incomplete; inspect diagnostic reports",
    );
  await rm(p.state);
}
async function selftest(p) {
  const state = JSON.parse(await readFile(p.state, "utf8"));
  const directory = join(state.workspace, "target/llvm-cov-target/debug/deps");
  await mkdir(directory, { recursive: true });
  const executable = join(directory, "linux-exit-crash-control");
  const source = join(p.root, "exit-crash-control.c");
  await writeFile(
    source,
    '#include <signal.h>\n#include <stdlib.h>\n#include <stdio.h>\n__attribute__((noinline)) void diagnostic_exit_crash(void) { raise(SIGSEGV); }\nint main(void) { atexit(diagnostic_exit_crash); puts("body passed"); fflush(stdout); return 0; }\n',
  );
  try {
    await command(["cc", "-g", "-O0", source, "-o", executable]);
    const result = await command([executable], { expected: null });
    if (result.signal !== "SIGSEGV")
      throw Error(
        `Selftest must exit SIGSEGV, observed ${JSON.stringify(result)}`,
      );
    const names = (await readdir(p.mount)).filter((name) => {
      try {
        return decodeCoreName(name).executable === executable;
      } catch {
        return false;
      }
    });
    if (names.length !== 1)
      throw Error(`Expected one actual selftest core, found ${names.length}`);
    const report = await symbolize(p, state, names[0], limits.perCoreMs);
    if (
      report.outcome !== "symbolized" ||
      !/diagnostic_exit_crash/u.test(report.output) ||
      !/SIGSEGV/u.test(report.output)
    )
      throw Error("Selftest core must symbolize the actual exit crash");
  } finally {
    await rm(source, { force: true });
    await rm(executable, { force: true });
  }
}
if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  try {
    if (process.platform !== "linux") throw Error("Linux VM required");
    const p = paths();
    if (process.argv[2] === "setup") await setup(p);
    else if (process.argv[2] === "collect") await collect(p);
    else if (process.argv[2] === "selftest") await selftest(p);
    else throw Error("Expected setup, selftest or collect");
  } catch (error) {
    console.error(error);
    process.exitCode = 1;
  }
}
