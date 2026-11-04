#!/usr/bin/env node
// Official nextest archives, fixed source paths, serial hash shards and receipts.
// Pinned LLVM coverage uses v1 target/debug/{deps,examples}; see upstream #520.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, closeSync, createReadStream, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, realpathSync, rmSync, writeFileSync, writeSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const versions = Object.freeze({ nextest: "0.9.150", llvmCov: "0.8.7" });
export const windowsPartitions = Object.freeze([1, 2, 3, 4]);
const buildArgs = ["--workspace", "--locked"];
const nextestArgs = ["--profile", "default", "--user-config-file", "none"];
const canonical = value => JSON.stringify(value);
const digest = data => createHash("sha256").update(data).digest("hex");
const readJson = file => JSON.parse(readFileSync(file, "utf8"));
const writeJson = (file, value) => writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
async function fileHash(file) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest("hex");
}
export async function command(args, { env = process.env, receipt, progress, phase = "command", liveStdout = false, binary = "cargo", signal, maxOutputBytes = 128 * 1024 * 1024 } = {}) {
  assert.ok(Number.isSafeInteger(maxOutputBytes) && maxOutputBytes > 0 && maxOutputBytes <= 128 * 1024 * 1024, "Invalid CI command output limit");
  const record = event => {
    if (progress) appendFileSync(progress, `${JSON.stringify({ at: new Date().toISOString(), phase, ...event })}\n`);
  };
  const files = [];
  try {
    if (receipt) {
      files.push(openSync(`${receipt}.jsonl`, "w"));
      files.push(openSync(`${receipt}.log`, "w"));
    }
    // Publish the phase before starting the child; cancellation must not erase
    // whether a shard was listing tests, running them, or reporting coverage.
    record({ event: "started", binary, args });
    signal?.throwIfAborted();
    return await new Promise((resolve, reject) => {
      const child = spawn(binary, args, { env, windowsHide: true, detached: process.platform !== "win32", stdio: ["ignore", "pipe", "pipe"] });
      const output = [];
      const bytes = [0, 0];
      let failure, stopping = false, outputRejected = false;
      let cleanup = Promise.resolve();
      const stop = error => {
        failure ??= error;
        if (stopping || !child.pid) return;
        stopping = true;
        if (process.platform === "win32") {
          cleanup = new Promise(done => {
            const killer = spawn("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], { windowsHide: true, stdio: "ignore" });
            killer.on("error", error => { failure = error; child.kill(); });
            killer.on("close", code => {
              if (code !== 0) { failure = new Error("Windows child-tree termination could not be verified"); child.kill(); }
              done();
            });
          });
        } else {
          try { process.kill(-child.pid, "SIGTERM"); } catch (error) { if (error.code !== "ESRCH") failure = error; }
          // As in the validation stage owner, finish group cleanup even when
          // the leader exits before a descendant with independent stdio.
          cleanup = new Promise(done => setTimeout(() => {
            try { process.kill(-child.pid, "SIGKILL"); } catch (error) { if (error.code !== "ESRCH") failure = error; }
            done();
          }, 2000));
        }
      };
      const interrupted = () => stop(new Error("CI command interrupted"));
      process.on("SIGINT", interrupted);
      process.on("SIGTERM", interrupted);
      signal?.addEventListener("abort", interrupted, { once: true });
      if (signal?.aborted) interrupted();
      child.on("spawn", () => {
        try { record({ event: "spawned", pid: child.pid }); } catch (error) { stop(error); }
      });
      const capture = (index, chunk) => {
        if (outputRejected) return;
        try {
          assert.ok(chunk.length <= maxOutputBytes - bytes[index], "CI command output exceeded its byte limit");
          if (files[index] !== undefined) {
            let written = 0;
            while (written < chunk.length) {
              const count = writeSync(files[index], chunk, written, chunk.length - written);
              assert.ok(count > 0, "CI command receipt write made no progress");
              written += count;
            }
          }
          if (index === 1) process.stderr.write(chunk);
          else if (liveStdout) process.stdout.write(chunk);
          bytes[index] += chunk.length;
          if (index === 0) output.push(chunk);
        } catch (error) { outputRejected = true; stop(error); }
      };
      child.stdout.on("data", chunk => capture(0, chunk));
      child.stderr.on("data", chunk => capture(1, chunk));
      child.on("error", stop);
      child.on("close", async (code, childSignal) => {
        await cleanup;
        process.off("SIGINT", interrupted);
        process.off("SIGTERM", interrupted);
        signal?.removeEventListener("abort", interrupted);
        try {
          record({ event: "closed", code, signal: childSignal, stdoutBytes: bytes[0], stderrBytes: bytes[1], failed: !!failure || code !== 0 });
          if (failure) throw failure;
          assert.equal(code, 0, `${binary} ${args.join(" ")} failed: ${code}/${childSignal}`);
          resolve(Buffer.concat(output).toString("utf8"));
        } catch (error) { reject(error); }
      });
    });
  } finally {
    for (const file of files) closeSync(file);
  }
}
function tool(binary, args) {
  const result = spawnSync(binary, args, { encoding: "utf8" });
  assert.ok(!result.error && result.status === 0, `${binary} ${args.join(" ")} failed`);
  return result.stdout.trim();
}

export function inventory(listing) {
  const rows = [];
  assert.ok(listing["rust-suites"] && Number.isInteger(listing["test-count"]), "Invalid nextest inventory");
  for (const [binaryId, suite] of Object.entries(listing["rust-suites"])) {
    assert.equal(suite.status, "listed", `Unlisted test binary: ${binaryId}`);
    for (const [name, test] of Object.entries(suite.testcases)) {
      assert.equal(test.kind, "test", "Unsupported benchmark/custom test kind");
      assert.equal(typeof test.ignored, "boolean", "Missing ignored marker");
      assert.ok(["matches", "mismatch"].includes(test["filter-match"]?.status), "Missing filter result");
      rows.push({ id: `${binaryId}::${name}`, eventName: `${suite["package-name"]}::${suite["binary-name"]}$${name}`, ignored: test.ignored, selected: test["filter-match"].status === "matches" });
    }
  }
  rows.sort((a, b) => a.id.localeCompare(b.id, "en"));
  assert.equal(rows.length, listing["test-count"], "nextest inventory count mismatch");
  assert.equal(new Set(rows.map(row => row.id)).size, rows.length, "Duplicate test inventory");
  assert.equal(new Set(rows.map(row => row.eventName)).size, rows.length, "Ambiguous event identity");
  return rows;
}

export function verifyPartition(all, shards) {
  assert.equal(shards.length, windowsPartitions.length, "Expected four partitions");
  assert.ok(all.length && all.some(row => !row.ignored), "Empty full partition inventory");
  assert.ok(all.every(row => row.selected === !row.ignored), "Full partition has hidden filters or changed ignored behavior");
  const stable = rows => rows.map(({ selected: _selected, ...rest }) => rest);
  const expected = all.filter(row => row.selected).map(row => row.id).sort();
  const actual = [];
  for (const shard of shards) {
    assert.equal(canonical(stable(shard)), canonical(stable(all)), "partition inventory drift");
    assert.ok(shard.some(row => row.selected), "Empty runnable partition");
    assert.ok(shard.every(row => !row.ignored || !row.selected), "partition executes ignored tests");
    actual.push(...shard.filter(row => row.selected).map(row => row.id));
  }
  assert.equal(canonical(actual.sort()), canonical(expected), "partition omission or duplicate execution");
}

export function verifyExecution(text, expected) {
  const passed = [], started = [];
  const suites = new Map();
  const selected = new Set(expected.filter(row => row.selected).map(row => row.eventName));
  for (const line of text.split(/\r?\n/u).filter(Boolean)) {
    const event = JSON.parse(line);
    assert.ok(["suite", "test"].includes(event.type), "Unknown execution receipt type");
    if (event.type === "suite") {
      assert.ok(event.nextest?.crate && event.nextest?.test_binary && event.nextest?.kind, "Missing execution suite identity");
      const key = canonical(event.nextest);
      assert.ok(["started", "ok"].includes(event.event), "Unsuccessful execution suite");
      if (event.event === "started") suites.set(key, (suites.get(key) ?? 0) + 1);
      else {
        assert.equal(event.failed, 0, "Failed execution suite");
        assert.ok(suites.get(key) > 0, "Execution suite finished without starting");
        suites.set(key, suites.get(key) - 1);
      }
    } else {
      assert.ok(["started", "ok", "ignored"].includes(event.event), "Unexpected execution event");
      assert.ok(!(event.event === "ignored" && selected.has(event.name)), "Selected execution was ignored");
      if (event.event === "started" && selected.has(event.name)) started.push(event.name);
      if (event.event === "ok") passed.push(event.name);
    }
    assert.ok(event.event !== "failed", "Failed execution receipt");
  }
  assert.ok([...suites.values()].every(open => open === 0), "Unfinished execution suite");
  assert.equal(canonical(started.sort()), canonical([...selected].sort()), "execution receipt missing or duplicate starts");
  assert.equal(canonical(passed.sort()), canonical([...selected].sort()), "execution receipt missing, duplicate or unexpected tests");
}

export function validateLcov(text) {
  const blocks = text.trim().split("end_of_record");
  assert.equal(blocks.pop().trim(), "", "coverage record is incomplete");
  let measured = 0, positive = 0;
  for (const block of blocks) {
    assert.match(block, /^SF:.+$/mu, "coverage source is missing");
    for (const line of block.split(/\r?\n/u).filter(line => line.startsWith("DA:"))) {
      const match = /^DA:([1-9]\d*),(\d+)(?:,[a-fA-F0-9]+)?$/u.exec(line);
      assert.ok(match, "Invalid coverage line");
      measured++; positive += Number(match[2]) > 0;
    }
  }
  assert.ok(measured > 0 && positive > 0, "coverage contains no executed measured lines");
  return { measuredLines: measured, coveredLines: positive };
}

export function verifyIdentity(expected, actual) {
  assert.deepEqual(actual, expected, "Consumer identity differs from producer");
}

export function verifyTargets(listing, metadata) {
  const featureSets = new Map(metadata.resolve.nodes.map(node => [node.id, new Set(node.features)]));
  const expected = [];
  for (const pkg of metadata.packages.filter(pkg => metadata.workspace_members.includes(pkg.id))) {
    for (const target of pkg.targets) {
      if (!target.test || target.kind.includes("custom-build")) continue;
      if ((target["required-features"] ?? []).some(feature => !featureSets.get(pkg.id)?.has(feature))) continue;
      const kind = target.kind.some(kind => ["lib", "rlib", "cdylib", "staticlib", "proc-macro"].includes(kind)) ? "lib" : target.kind[0];
      expected.push(`${pkg.id}|${kind}|${target.name}`);
    }
  }
  const actual = Object.values(listing["rust-suites"]).map(suite => `${suite["package-id"]}|${suite.kind}|${suite["binary-name"]}`);
  assert.deepEqual(actual.sort(), expected.sort(), "Cargo default test target omitted or added in nextest archive");
}

export function validateProfilePath(profile, root) {
  assert.ok(typeof profile === "string" && path.isAbsolute(profile), "LLVM_PROFILE_FILE must be absolute");
  const relative = path.relative(path.join(root, "target"), profile);
  assert.ok(relative && !relative.startsWith("..") && !path.isAbsolute(relative), "LLVM_PROFILE_FILE must remain under workspace/target");
}

async function identity(evidence = () => undefined) {
  const root = process.cwd();
  validateProfilePath(process.env.LLVM_PROFILE_FILE, root);
  const sha = tool("git", ["rev-parse", "HEAD"]);
  assert.match(sha, /^[a-f0-9]{40}$/u);
  assert.equal(sha, process.env.GITHUB_SHA, "Checkout differs from requested source");
  assert.equal(tool("git", ["status", "--porcelain", "--untracked-files=normal"]), "", "Source must be clean");
  assert.match(process.env.GITHUB_RUN_ID ?? "", /^[1-9]\d*$/u);
  assert.match(process.env.GITHUB_RUN_ATTEMPT ?? "", /^[1-9]\d*$/u);
  const nextest = await command(["nextest", "--version"], evidence("identity-nextest"));
  const llvmCov = await command(["llvm-cov", "--version"], evidence("identity-llvm-cov"));
  const [nextestName, nextestVersion] = nextest.trim().split(/\s+/u);
  assert.equal(nextestName, "cargo-nextest");
  assert.equal(nextestVersion, versions.nextest);
  assert.equal(llvmCov.trim(), `cargo-llvm-cov ${versions.llvmCov}`);
  assert.equal(process.env.CARGO_LLVM_COV_SHOW_ENV, "1", "Source cargo llvm-cov show-env --sh before running this command");
  assert.equal(path.resolve(process.env.CARGO_LLVM_COV_TARGET_DIR ?? ""), path.join(root, "target"), "Coverage target must be workspace/target");
  return {
    sourceSha: sha, root, platform: process.platform, arch: process.arch,
    rustc: tool(process.env.RUSTC || "rustc", ["-Vv"]), cargo: tool("cargo", ["-V"]),
    nextest: nextest.trim(), llvmCov: llvmCov.trim(), buildArgs, cargoProfile: "test", features: "Cargo workspace defaults; no feature flags",
    profileEnvironment: Object.fromEntries(Object.entries(process.env).filter(([name]) => /^CARGO_PROFILE_(?:DEV|TEST)_/u.test(name)).sort(([a], [b]) => a.localeCompare(b, "en"))),
    lockSha256: await fileHash("Cargo.lock"), cargoConfigSha256: await fileHash(".cargo/config.toml"), nextestConfigSha256: await fileHash(".config/nextest.toml"),
    runnerSha256: await fileHash(fileURLToPath(import.meta.url)),
    runId: process.env.GITHUB_RUN_ID, runAttempt: process.env.GITHUB_RUN_ATTEMPT,
  };
}
async function list(args, options) { return JSON.parse(await command(["nextest", "list", ...nextestArgs, ...args, "--message-format", "json"], options)); }
export function verifyLayout(binaries) {
  const metadata = binaries["rust-build-meta"];
  assert.ok(typeof metadata?.["target-directory"] === "string" && path.isAbsolute(metadata["target-directory"]), "Missing absolute archive target directory");
  const normalize = value => process.platform === "win32" ? path.resolve(value).toLowerCase() : path.resolve(value);
  const target = normalize(metadata["target-directory"]);
  assert.ok(!metadata["build-directory"] || normalize(metadata["build-directory"]) === target, "Unsupported separate build directory (upstream cargo-llvm-cov #520)");
  for (const binary of Object.values(binaries["rust-binaries"])) {
    const expected = normalize(path.join(target, "debug", binary.kind === "example" ? "examples" : "deps"));
    assert.ok(path.isAbsolute(binary["binary-path"]) && normalize(path.dirname(binary["binary-path"])) === expected, "Unsupported build layout/profile (upstream cargo-llvm-cov #520); expected v1 target/debug/deps or example target/debug/examples");
  }
}

export const criticalRecoveryProbeCases = Object.freeze([
  "node::group_chain_tests::background_group_gossip_reaches_end_of_chain_without_sender_or_roster_changes",
  "node::channels::account_self_control::tests::reference_modern_leave_request_overtakes_the_executors_held_requester_read",
  "node::channels::account_self_control::tests::reference_modern_leave_keeps_left_over_old_member_head_and_reopen",
  "node::forwarding::tests::forwarding_to_group_delivers_ordered_text_and_independent_merged_file",
  "node::node::tests::group_reception_failed_rollback_retries_without_new_traffic",
  "postoffice::maintenance::tests::actual_role_gc_supplies_free_pages_for_physical_progress",
  "node::account_documents::tests::custody_channel_setup_waits_for_post_create_authenticated_inventory",
  "node::account_documents::tests::custody_channel_setup_waits_for_same_lane_dm_recall_baseline",
  "node::account_documents::tests::custody_channel_setup_does_not_wait_for_independent_directory",
]);
export function criticalRecoveryProbeEnabled(env = process.env) {
  const value = env.MESH_WINDOWS_CRITICAL_RECOVERY_PROBE;
  assert.ok(
    value === undefined || value === "0" || value === "1",
    "MESH_WINDOWS_CRITICAL_RECOVERY_PROBE must be 0 or 1",
  );
  return value === "1";
}

// Diagnostic only: no consumer receipt or LCOV output. The full four-shard
// acceptance remains mandatory even after this same-binary probe passes.
export async function runCriticalRecoveryProbe(
  { full, reuse, identity: sourceIdentity, archiveSha256, env = process.env },
  run = command,
) {
  if (!criticalRecoveryProbeEnabled(env)) return;
  const expectedNames = criticalRecoveryProbeCases.map(
    (name) => `mesh-talk-core::mesh_talk_core$${name}`,
  );
  const matches = full.filter((row) => expectedNames.includes(row.eventName));
  assert.equal(
    matches.length,
    expectedNames.length,
    `Critical recovery probe requires all ${expectedNames.length} archived cases`,
  );
  for (const name of expectedNames) {
    assert.equal(
      matches.filter((row) => row.eventName === name).length,
      1,
      "Critical recovery probe requires one of each exact case",
    );
  }
  assert.ok(
    matches.every((row) => row.selected && !row.ignored),
    "Critical recovery probe cases must be runnable",
  );
  assert.ok(
    path.isAbsolute(env.RUNNER_TEMP ?? ""),
    "Critical recovery probe requires absolute runner temp",
  );
  assert.match(
    archiveSha256 ?? "",
    /^[a-f0-9]{64}$/u,
    "Critical recovery probe requires sealed archive digest",
  );
  const relative = path.relative(
    realpathSync(process.cwd()),
    realpathSync(env.RUNNER_TEMP),
  );
  assert.ok(
    relative === ".." ||
      relative.startsWith(`..${path.sep}`) ||
      path.isAbsolute(relative),
    "Critical recovery profiles must remain outside the workspace/cache",
  );
  const directory = path.join(
    env.RUNNER_TEMP,
    "windows-critical-recovery-probe",
  );
  assert.ok(
    !existsSync(directory),
    "Critical recovery probe output already exists",
  );
  mkdirSync(directory);
  const profiles = mkdtempSync(
    path.join(env.RUNNER_TEMP, "windows-critical-recovery-profraw-"),
  );
  writeJson(path.join(directory, "input.json"), {
    identity: sourceIdentity,
    archiveSha256,
    selected: matches,
  });
  let retired = false;
  try {
    const raw = await run(
      [
        "nextest",
        "run",
        ...nextestArgs,
        ...reuse,
        "-E",
        `package(=mesh-talk-core) & binary(=mesh_talk_core) & (${criticalRecoveryProbeCases.map((name) => `test(=${name})`).join(" | ")})`,
        "--test-threads",
        "1",
        "--retries",
        "0",
        "--no-fail-fast",
        "--no-tests",
        "fail",
        "--message-format",
        "libtest-json-plus",
        "--message-format-version",
        "0.1",
      ],
      {
        env: {
          ...env,
          CARGO_NET_OFFLINE: "true",
          RUST_TEST_THREADS: "1",
          NEXTEST_EXPERIMENTAL_LIBTEST_JSON: "1",
          LLVM_PROFILE_FILE: path.join(profiles, "%m-%p.profraw"),
        },
        receipt: path.join(directory, "execution"),
        progress: path.join(directory, "progress.jsonl"),
        phase: "early-critical-recovery-probe",
        liveStdout: true,
        maxOutputBytes: 8 * 1024 * 1024,
      },
    );
    retired = true;
    verifyExecution(raw, matches);
    writeJson(path.join(directory, "proof.json"), {
      identity: sourceIdentity,
      archiveSha256,
      selected: matches,
      executionSha256: digest(raw),
      diagnosticOnly: true,
    });
  } finally {
    // A rejected command may indicate unverified process-tree cleanup. Never
    // delete its profile directory while ownership is uncertain; runner teardown
    // owns that temporary residue. Raw logs remain available on every outcome.
    if (retired) rmSync(profiles, { recursive: true });
  }
}
export async function produce(directory) {
  criticalRecoveryProbeEnabled();
  const before = await identity();
  const archive = path.join(directory, "workspace.tar.zst");
  assert.ok(!existsSync(archive), "Archive already exists");
  mkdirSync(directory, { recursive: true });
  await command(["nextest", "archive", ...nextestArgs, ...buildArgs, "--archive-file", archive]);
  const probe = path.join(directory, "inventory-extract");
  mkdirSync(probe, { recursive: true });
  const binaries = await list(["--archive-file", archive, "--extract-to", probe, "--list-type", "binaries-only"]);
  verifyLayout(binaries);
  const binaryFile = path.join(directory, "producer-binaries.json");
  writeJson(binaryFile, binaries);
  const metadataFile = path.join(probe, "target", "nextest", "cargo-metadata.json");
  const metadata = readJson(metadataFile);
  const reuse = ["--binaries-metadata", binaryFile, "--cargo-metadata", metadataFile, "--target-dir-remap", path.join(probe, "target")];
  const listings = [await list(reuse)];
  for (const shard of windowsPartitions) listings.push(await list([...reuse, "--partition", `hash:${shard}/${windowsPartitions.length}`]));
  verifyTargets(listings[0], metadata);
  const inventories = listings.map(inventory);
  verifyPartition(inventories[0], inventories.slice(1));
  const files = {};
  for (let index = 0; index < inventories.length; index++) {
    const name = `inventory-${index}.json`;
    writeJson(path.join(directory, name), inventories[index]);
    files[name] = await fileHash(path.join(directory, name));
  }
  const featureResolutionSha256 = digest(canonical(metadata.resolve));
  verifyIdentity(before, await identity());
  const archiveSha256 = await fileHash(archive);
  writeJson(path.join(directory, "manifest.json"), { schema: 1, identity: before, archiveSha256, inventoryFiles: files, featureResolutionSha256, testCount: inventories[0].length, runnableCount: inventories[0].filter(row => row.selected).length, scheduling: { partitions: windowsPartitions.length, partition: "hash", testThreads: 1, retries: 0, ignored: "default" } });
  await runCriticalRecoveryProbe({ full: inventories[0], reuse, identity: before, archiveSha256 });
  rmSync(probe, { recursive: true });
  rmSync(binaryFile);
  console.log(`Archived ${inventories[0].length} tests; ${inventories[0].filter(row => row.selected).length} runnable across ${windowsPartitions.length} verified serial shards.`);
}

export function consumerFailurePolicy(env = process.env) {
  const value = env.MESH_WINDOWS_FAIL_FAST;
  assert.ok(value === undefined || value === "0" || value === "1", "MESH_WINDOWS_FAIL_FAST must be 0 or 1");
  return value === "1" ? "--fail-fast" : "--no-fail-fast";
}

export async function consume(shard, directory, output) {
  assert.ok(windowsPartitions.includes(shard), "shard must be an integer from 1 through 4");
  mkdirSync(output, { recursive: true });
  const progress = path.join(output, "progress.jsonl");
  assert.ok(!existsSync(progress) && !existsSync(path.join(output, "receipt.json")), "Consumer output already contains a prior attempt");
  const evidence = phase => ({ progress, phase, receipt: path.join(output, phase) });
  const manifest = readJson(path.join(directory, "manifest.json"));
  assert.equal(manifest.schema, 1, "Unsupported archive manifest");
  verifyIdentity(manifest.identity, await identity(evidence));
  assert.deepEqual(manifest.scheduling, { partitions: windowsPartitions.length, partition: "hash", testThreads: 1, retries: 0, ignored: "default" });
  const archive = path.join(directory, "workspace.tar.zst");
  assert.equal(await fileHash(archive), manifest.archiveSha256, "Archive digest mismatch");
  const expected = [];
  for (let index = 0; index <= windowsPartitions.length; index++) {
    const name = `inventory-${index}.json`;
    assert.equal(await fileHash(path.join(directory, name)), manifest.inventoryFiles[name], "Inventory digest mismatch");
    expected.push(readJson(path.join(directory, name)));
  }
  verifyPartition(expected[0], expected.slice(1));
  // Never reuse producer build outputs accidentally: this proves archive-only execution.
  assert.ok(!existsSync(path.join(process.cwd(), "target", "debug")), "Consumer target contains prior compilation outputs");
  writeJson(path.join(output, "expected-inventory.json"), expected[shard]);
  writeJson(path.join(output, "input-identity.json"), { sourceSha: manifest.identity.sourceSha, runId: manifest.identity.runId, runAttempt: manifest.identity.runAttempt, shard, archiveSha256: manifest.archiveSha256 });
  const binaries = await list(["--archive-file", archive, "--extract-to", process.cwd(), "--list-type", "binaries-only"], evidence("extract"));
  verifyLayout(binaries);
  const binaryFile = path.join(output, "consumer-binaries.json");
  writeJson(binaryFile, binaries);
  const metadataFile = path.resolve("target/nextest/cargo-metadata.json");
  const metadata = readJson(metadataFile);
  assert.equal(digest(canonical(metadata.resolve)), manifest.featureResolutionSha256, "Archived feature resolution changed");
  const reuse = ["--binaries-metadata", binaryFile, "--cargo-metadata", metadataFile, "--target-dir-remap", path.resolve("target")];
  const listings = [await list(reuse, evidence("inventory-all"))];
  for (const index of windowsPartitions) listings.push(await list([...reuse, "--partition", `hash:${index}/${windowsPartitions.length}`], evidence(`inventory-${index}`)));
  verifyTargets(listings[0], metadata);
  const observed = listings.map(inventory);
  verifyPartition(observed[0], observed.slice(1));
  assert.deepEqual(observed, expected, "Consumer partition differs from producer inventory");
  writeJson(path.join(output, "selected-inventory.json"), observed[shard]);
  const started = performance.now();
  const events = await command(["nextest", "run", ...nextestArgs, ...reuse, "--partition", `hash:${shard}/${windowsPartitions.length}`, "--test-threads", "1", "--retries", "0", consumerFailurePolicy(), "--no-tests", "fail", ...(process.env.MESH_TALK_TEST_TIMING === "1" ? ["--success-output", "final"] : []), "--message-format", "libtest-json-plus", "--message-format-version", "0.1"], {
    env: { ...process.env, CARGO_NET_OFFLINE: "true", RUST_TEST_THREADS: "1", NEXTEST_EXPERIMENTAL_LIBTEST_JSON: "1" },
    ...evidence("execution"), liveStdout: true,
  });
  verifyExecution(events, observed[shard]);
  const coverageFile = path.join(output, "coverage.lcov");
  await command(["llvm-cov", "report", "--lcov", "--output-path", coverageFile], { env: { ...process.env, CARGO_NET_OFFLINE: "true" }, ...evidence("coverage") });
  const coverage = validateLcov(readFileSync(coverageFile, "utf8"));
  verifyIdentity(manifest.identity, await identity(phase => evidence(`final-${phase}`)));
  writeJson(path.join(output, "receipt.json"), { schema: 1, sourceSha: manifest.identity.sourceSha, runId: manifest.identity.runId, runAttempt: manifest.identity.runAttempt, shard, archiveSha256: manifest.archiveSha256, manifestSha256: await fileHash(path.join(directory, "manifest.json")), coverageSha256: await fileHash(coverageFile), executionSha256: await fileHash(path.join(output, "execution.jsonl")), seconds: (performance.now() - started) / 1000, executedTests: observed[shard].filter(row => row.selected).length, totalTests: observed[0].length, coverage });
  console.log(`Shard ${shard}/${windowsPartitions.length} passed exact inventory and coverage validation.`);
}

async function main([mode, first, second, third, ...extra]) {
  assert.equal(extra.length, 0, "Unexpected arguments");
  if (mode === "produce" && first && !second) await produce(path.resolve(first));
  else if (mode === "consume" && first && second && third) await consume(Number(first), path.resolve(second), path.resolve(third));
  else throw new Error("Usage: windows-coverage.mjs produce ARTIFACT_DIR | consume SHARD ARTIFACT_DIR OUTPUT_DIR");
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main(process.argv.slice(2)).catch(error => { console.error(error.message); process.exitCode = 1; });
