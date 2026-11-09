/** Main-to-dev promotion uses the existing full source acceptance owner. */
import { execFileSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { execute } from "../validation/stage.mjs";

export async function verifyMainPromotion({
  root,
  base,
  signal,
  run,
  env = process.env,
  now = Date.now,
} = {}) {
  signal?.throwIfAborted();
  if (base !== "origin/dev") return false;
  const git = (...args) =>
    execFileSync("git", args, {
      cwd: root,
      encoding: "utf8",
      timeout: 10000,
      maxBuffer: 32 * 1024 * 1024,
      env: { ...env, GIT_NO_REPLACE_OBJECTS: "1", GIT_OPTIONAL_LOCKS: "0" },
    }).trim();
  const sha = git("rev-parse", "--verify", "HEAD");
  const main = git("rev-parse", "--verify", "refs/remotes/origin/main");
  if (sha !== main) return false;
  const clean = () => {
    if (git("status", "--porcelain", "--untracked-files=normal"))
      throw Error("Main promotion requires a clean committed source");
    if (
      git("rev-parse", "--verify", "HEAD") !== sha ||
      git("rev-parse", "--verify", "refs/remotes/origin/main") !== sha
    )
      throw Error("Main promotion source changed");
  };
  clean();
  if (env.GH_REPO || (env.GH_HOST && env.GH_HOST !== "github.com"))
    throw Error("Main promotion rejects GitHub repository/host overrides");
  const logs = await mkdtemp(join(tmpdir(), "mesh-main-promotion-"));
  let retainLogs = false;
  const deadline = now() + 120000;
  const controller = new AbortController();
  const abort = () => controller.abort(signal.reason);
  signal?.addEventListener("abort", abort, { once: true });
  const timer = setTimeout(
    () => controller.abort(Error("Main promotion proof deadline exceeded")),
    120000,
  );
  if (signal?.aborted) abort();
  let index = 0;
  const invoke =
    run ??
    (async (command, { signal: childSignal }) => {
      let stdout = "",
        bytes = 0;
      const logPath = join(logs, `${index++}.log`);
      const result = await execute(command, {
        cwd: root,
        env,
        signal: childSignal,
        logPath,
        onOutput(chunk, channel) {
          bytes += Buffer.byteLength(chunk);
          if (bytes > 4 * 1024 * 1024)
            controller.abort(
              Error("Main promotion proof output exceeded budget"),
            );
          else if (channel === "stdout") stdout += chunk;
        },
      });
      if (result.code !== 0 || result.error || result.cleanupUnverified) {
        retainLogs = true;
        throw Error(
          `Main promotion proof command failed: ${result.error ?? result.code}; diagnostics: ${logPath}`,
        );
      }
      return stdout.trim();
    });
  const withinBudget = () => {
    if (now() >= deadline)
      controller.abort(Error("Main promotion proof deadline exceeded"));
    controller.signal.throwIfAborted();
  };
  const read = async (command) => {
    withinBudget();
    const result = await invoke(command, { signal: controller.signal });
    withinBudget();
    return result;
  };
  try {
    const repo = await read([
      "gh",
      "repo",
      "view",
      "--json",
      "nameWithOwner",
      "--jq",
      ".nameWithOwner",
    ]);
    if (!/^[\w.-]+\/[\w.-]+$/u.test(repo))
      throw Error("Invalid main promotion repository identity");
    const remote = () =>
      read([
        "gh",
        "api",
        `repos/${repo}/git/ref/heads/main`,
        "--jq",
        ".object.sha",
      ]);
    if ((await remote()) !== sha)
      throw Error("Main promotion is not the current remote main source");
    await read([
      process.execPath,
      fileURLToPath(
        new URL("../release/check-source-checks.mjs", import.meta.url),
      ),
      repo,
      sha,
    ]);
    if ((await remote()) !== sha)
      throw Error("Remote main changed during promotion verification");
    clean();
    withinBudget();
    return true;
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener("abort", abort);
    if (!retainLogs) await rm(logs, { recursive: true, force: true });
  }
}
