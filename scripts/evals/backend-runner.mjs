import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { createHash } from "node:crypto";

export const BACKEND_SUITES = {
  two_node_cli: ["two_cli_nodes_exchange_a_dm"],
  persistent_history: ["message_history_survives_restart"],
  post_office_offline: ["offline_dm_delivered_via_post_office"],
  channel_and_file_cli: ["two_cli_nodes_exchange_a_channel_message", "two_cli_nodes_transfer_a_file"],
};

export function validateBackendOutput(output, expected) {
  const successful = [...output.matchAll(/^test (\S+) \.\.\. ok\s*$/gm)].map(match => match[1]);
  assert.equal(successful.length, expected.length, "Missing, duplicate or zero executed backend scenarios");
  for (const name of expected) assert.equal(successful.filter(value => value === name).length, 1, `Missing successful scenario: ${name}`);
  const summaries = [...output.matchAll(/test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out/g)];
  assert.equal(summaries.length, 1, "Missing unique Cargo test summary");
  assert.equal(Number(summaries[0][1]), expected.length, "Incorrect scenario count");
  for (const count of summaries[0].slice(2)) assert.equal(Number(count), 0, "Failed, ignored, measured or filtered backend test");
  return successful;
}

function runCargo(target) {
  return new Promise((resolveRun, reject) => {
    const child = spawn("cargo", ["test", "--locked", "-p", "mesh-talk-core", "--test", target, "--", "--ignored", "--test-threads=1", "--nocapture"], { env: { ...process.env, CARGO_TERM_COLOR: "never" }, stdio: ["ignore", "pipe", "pipe"], detached: process.platform !== "win32" });
    let output = "", bytes = 0, failure, forcedKill;
    const stopTree = reason => {
      if (failure) return;
      failure = reason;
      if (!Number.isInteger(child.pid)) return;
      if (process.platform === "win32") {
        // Exact owned PID, including integration test children. Killing Cargo
        // alone can otherwise leave real networking processes running.
        const killer = spawn("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore" });
        killer.on("error", () => child.kill());
        killer.on("close", code => { if (code !== 0) child.kill(); });
      } else {
        try { process.kill(-child.pid, "SIGTERM"); } catch { child.kill(); }
        forcedKill = setTimeout(() => { try { process.kill(-child.pid, "SIGKILL"); } catch { /* Already reaped. */ } }, 2000);
      }
    };
    const timeout = setTimeout(() => stopTree(`Backend suite timeout: ${target}`), 20 * 60 * 1000);
    for (const stream of [child.stdout, child.stderr]) stream.on("data", chunk => {
      bytes += chunk.length;
      if (bytes > 16 * 1024 * 1024) { stopTree("Backend output exceeded evidence limit"); return; }
      output += chunk.toString();
    });
    child.once("error", error => { clearTimeout(timeout); clearTimeout(forcedKill); reject(error); });
    child.once("close", (code, signal) => { clearTimeout(timeout); clearTimeout(forcedKill); resolveRun({ output, code, signal, failure }); });
  });
}

export async function evaluateBackendSuites(runSuite, recordSuite) {
  const failures = [];
  for (const [target, expected] of Object.entries(BACKEND_SUITES)) {
    const started = Date.now();
    const record = { passed: false };
    try {
      const result = await runSuite(target);
      Object.assign(record, result);
      assert.ok(!result.failure, result.failure);
      assert.equal(result.code, 0, `Backend suite failed: ${target}`);
      record.scenarios = validateBackendOutput(result.output, expected);
      record.passed = true;
    } catch (error) {
      record.failure = String(error.message);
      failures.push(`${target}: ${record.failure}`);
    }
    record.elapsedMs = Date.now() - started;
    // Independent fixtures must all run, but no later pass can erase a failure.
    // Failure to preserve evidence remains fatal rather than an incomplete pass.
    await recordSuite(target, record);
  }
  return failures;
}

async function main() {
  const sourceSha = execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
  assert.match(sourceSha, /^[0-9a-f]{40}$/);
  if (process.env.EVAL_SOURCE_SHA) assert.equal(sourceSha, process.env.EVAL_SOURCE_SHA, "Runner checkout differs from requested revision");
  const root = resolve(process.env.EVAL_OUTPUT_DIR ?? "target/evals/backend");
  await mkdir(root, { recursive: true });
  const report = { schema: 1, sourceSha, platform: process.platform, native: true, mocked: false, startedAt: new Date().toISOString(), suites: {}, kdf: "fast-test-kdf via existing self-dev-dependency; release binary unaffected" };
  try {
    const failures = await evaluateBackendSuites(runCargo, async (target, result) => {
      // The integration fixtures use generated throwaway accounts. Still redact
      // recognizable secrets before publishing diagnostics as public artifacts.
      const log = (result.output ?? result.failure).replace(/(--password\s+|password[=:]\s*)\S+/gi, "$1[REDACTED]");
      const file = `${target}.log`;
      await writeFile(join(root, file), log);
      const evidenceDigest = createHash("sha256").update(log).digest("hex");
      report.suites[target] = { passed: result.passed, elapsedMs: result.elapsedMs, evidence: file, evidenceDigest, exitCode: result.code ?? null, signal: result.signal ?? null, failure: result.failure, scenarios: result.scenarios ?? [] };
      console.log(`${target}: ${result.passed ? `${result.scenarios.length} real scenarios passed` : `FAILED; inspect ${file}`}`);
    });
    if (failures.length) {
      report.failures = failures;
      throw new Error(failures.join("; "));
    }
  } catch (error) {
    report.failure = String(error.message); process.exitCode = 1;
    console.error(report.failure);
  } finally {
    report.finishedAt = new Date().toISOString();
    await writeFile(join(root, "report.json"), JSON.stringify(report, null, 2));
  }
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main();
