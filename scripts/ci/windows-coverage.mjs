#!/usr/bin/env node
// Official nextest archives, fixed source paths, serial hash shards and receipts.
// Pinned LLVM coverage uses v1 target/debug/{deps,examples}; see upstream #520.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  appendFileSync,
  closeSync,
  createReadStream,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
  writeSync,
} from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const versions = Object.freeze({ nextest: "0.9.150", llvmCov: "0.8.7" });
export const windowsPartitions = Object.freeze([1, 2, 3, 4, 5, 6, 7, 8]);
export function archiveProfile(name = "windows") {
  assert.ok(
    ["windows", "macos-remaining"].includes(name),
    "Unsupported archive profile",
  );
  const instrumented = name === "windows";
  return {
    name,
    instrumented,
    buildArgs: instrumented
      ? ["--workspace", "--locked"]
      : ["--workspace", "--exclude", "mesh-talk", "--locked"],
    partitions: instrumented ? windowsPartitions : [1, 2],
    testThreads: instrumented ? 1 : 4,
  };
}
export function validateMacEnvironment(env, root) {
  for (const name of Object.keys(env)) {
    assert.ok(
      !(
        /^(?:__)?CARGO_LLVM_COV/u.test(name) ||
        /^(?:LLVM_PROFILE_FILE|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|RUSTC|CARGO_BUILD_TARGET|CARGO_BUILD_TARGET_DIR|CARGO_BUILD_BUILD_DIR|CARGO_BUILD_RUSTFLAGS|CARGO_BUILD_RUSTC|CARGO_BUILD_RUSTC_WRAPPER|CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER|CARGO_TARGET_.*_(?:RUSTFLAGS|LINKER|RUNNER))$/u.test(
          name,
        )
      ),
      `Unsupported macOS archive environment override: ${name}`,
    );
  }
  assert.equal(
    env.CARGO_BUILD_JOBS,
    "2",
    "macOS archive compiler jobs must be 2",
  );
  assert.equal(
    env.CARGO_INCREMENTAL,
    "0",
    "macOS archive environment requires CARGO_INCREMENTAL=0",
  );
  for (const profile of ["DEV", "TEST"])
    assert.equal(
      env[`CARGO_PROFILE_${profile}_DEBUG`],
      "line-tables-only",
      "macOS archive debug environment must be line-tables-only",
    );
  if (env.CARGO_TARGET_DIR !== undefined)
    assert.equal(
      env.CARGO_TARGET_DIR,
      path.join(root, "target"),
      "macOS target override must equal workspace/target",
    );
}
const scheduling = (profile) => ({
  partitions: profile.partitions.length,
  partition: "hash",
  testThreads: profile.testThreads,
  retries: 0,
  ignored: "default",
});
const nextestArgs = ["--profile", "default", "--user-config-file", "none"];
const canonical = (value) => JSON.stringify(value);
const digest = (data) => createHash("sha256").update(data).digest("hex");
const readJson = (file) => JSON.parse(readFileSync(file, "utf8"));
const writeJson = (file, value) =>
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
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

export function verifyPartition(all, shards, expectedPartitions = windowsPartitions.length) {
  assert.ok(Number.isSafeInteger(expectedPartitions) && expectedPartitions > 0, "Invalid expected partition count");
  assert.equal(shards.length, expectedPartitions, "Unexpected number of partitions");
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

export function verifyTargets(listing, metadata, profileName = "windows") {
  const profile = archiveProfile(profileName);
  const workspace = metadata.packages.filter((pkg) =>
    metadata.workspace_members.includes(pkg.id),
  );
  if (!profile.instrumented)
    assert.equal(
      workspace.filter((pkg) => pkg.name === "mesh-talk").length,
      1,
      "Remaining cohort requires exactly one Host package",
    );
  const featureSets = new Map(
    metadata.resolve.nodes.map((node) => [node.id, new Set(node.features)]),
  );
  const expected = [];
  for (const pkg of workspace.filter(
    (pkg) => profile.instrumented || pkg.name !== "mesh-talk",
  )) {
    for (const target of pkg.targets) {
      if (!target.test || target.kind.includes("custom-build")) continue;
      if (
        (target["required-features"] ?? []).some(
          (feature) => !featureSets.get(pkg.id)?.has(feature),
        )
      )
        continue;
      const kind = target.kind.some((kind) =>
        ["lib", "rlib", "cdylib", "staticlib", "proc-macro"].includes(kind),
      )
        ? "lib"
        : target.kind[0];
      expected.push(`${pkg.id}|${kind}|${target.name}`);
    }
  }
  const actual = Object.values(listing["rust-suites"]).map(
    (suite) => `${suite["package-id"]}|${suite.kind}|${suite["binary-name"]}`,
  );
  assert.deepEqual(
    actual.sort(),
    expected.sort(),
    "Cargo default test target omitted or added in nextest archive",
  );
}

export function validateProfilePath(profile, root) {
  assert.ok(typeof profile === "string" && path.isAbsolute(profile), "LLVM_PROFILE_FILE must be absolute");
  const relative = path.relative(path.join(root, "target"), profile);
  assert.ok(relative && !relative.startsWith("..") && !path.isAbsolute(relative), "LLVM_PROFILE_FILE must remain under workspace/target");
}

async function identity(
  evidence = () => undefined,
  profile = archiveProfile(),
) {
  const root = process.cwd();
  if (profile.instrumented)
    validateProfilePath(process.env.LLVM_PROFILE_FILE, root);
  else {
    assert.equal(process.platform, "darwin", "macos-remaining requires Darwin");
    validateMacEnvironment(process.env, root);
  }
  const sha = tool("git", ["rev-parse", "HEAD"]);
  assert.match(sha, /^[a-f0-9]{40}$/u);
  assert.equal(
    sha,
    process.env.GITHUB_SHA,
    "Checkout differs from requested source",
  );
  assert.equal(
    tool("git", ["status", "--porcelain", "--untracked-files=normal"]),
    "",
    "Source must be clean",
  );
  assert.match(process.env.GITHUB_RUN_ID ?? "", /^[1-9]\d*$/u);
  assert.match(process.env.GITHUB_RUN_ATTEMPT ?? "", /^[1-9]\d*$/u);
  const nextest = await command(
    ["nextest", "--version"],
    evidence("identity-nextest"),
  );
  const llvmCov = profile.instrumented
    ? await command(["llvm-cov", "--version"], evidence("identity-llvm-cov"))
    : "";
  const [nextestName, nextestVersion] = nextest.trim().split(/\s+/u);
  assert.equal(nextestName, "cargo-nextest");
  assert.equal(nextestVersion, versions.nextest);
  if (profile.instrumented) {
    assert.equal(llvmCov.trim(), `cargo-llvm-cov ${versions.llvmCov}`);
    assert.equal(
      process.env.CARGO_LLVM_COV_SHOW_ENV,
      "1",
      "Source cargo llvm-cov show-env --sh before running this command",
    );
    assert.equal(
      path.resolve(process.env.CARGO_LLVM_COV_TARGET_DIR ?? ""),
      path.join(root, "target"),
      "Coverage target must be workspace/target",
    );
  }
  return {
    sourceSha: sha,
    root,
    platform: process.platform,
    arch: process.arch,
    rustc: tool(process.env.RUSTC || "rustc", ["-Vv"]),
    cargo: tool("cargo", ["-V"]),
    nextest: nextest.trim(),
    llvmCov: llvmCov.trim(),
    buildArgs: profile.buildArgs,
    ...(!profile.instrumented
      ? {
          archiveProfile: profile.name,
          targetDirectory: path.join(root, "target"),
          compilerJobs: process.env.CARGO_BUILD_JOBS,
          incremental: process.env.CARGO_INCREMENTAL,
        }
      : {}),
    cargoProfile: "test",
    features: "Cargo workspace defaults; no feature flags",
    profileEnvironment: Object.fromEntries(
      Object.entries(process.env)
        .filter(([name]) => /^CARGO_PROFILE_(?:DEV|TEST)_/u.test(name))
        .sort(([a], [b]) => a.localeCompare(b, "en")),
    ),
    lockSha256: await fileHash("Cargo.lock"),
    cargoConfigSha256: await fileHash(".cargo/config.toml"),
    nextestConfigSha256: await fileHash(".config/nextest.toml"),
    runnerSha256: await fileHash(fileURLToPath(import.meta.url)),
    runId: process.env.GITHUB_RUN_ID,
    runAttempt: process.env.GITHUB_RUN_ATTEMPT,
  };
}
async function list(args, options) {
  return JSON.parse(
    await command(
      ["nextest", "list", ...nextestArgs, ...args, "--message-format", "json"],
      options,
    ),
  );
}
export function verifyLayout(binaries, root) {
  const metadata = binaries["rust-build-meta"];
  assert.ok(
    typeof metadata?.["target-directory"] === "string" &&
      path.isAbsolute(metadata["target-directory"]),
    "Missing absolute archive target directory",
  );
  const normalize = (value) =>
    process.platform === "win32"
      ? path.resolve(value).toLowerCase()
      : path.resolve(value);
  const target = normalize(metadata["target-directory"]);
  if (root !== undefined)
    assert.equal(
      target,
      normalize(path.join(root, "target")),
      "Archive target directory differs from checkout target",
    );
  assert.ok(
    !metadata["build-directory"] ||
      normalize(metadata["build-directory"]) === target,
    "Unsupported separate build directory (upstream cargo-llvm-cov #520)",
  );
  for (const binary of Object.values(binaries["rust-binaries"])) {
    const expected = normalize(
      path.join(
        target,
        "debug",
        binary.kind === "example" ? "examples" : "deps",
      ),
    );
    assert.ok(
      path.isAbsolute(binary["binary-path"]) &&
        normalize(path.dirname(binary["binary-path"])) === expected,
      "Unsupported build layout/profile (upstream cargo-llvm-cov #520); expected v1 target/debug/deps or example target/debug/examples",
    );
  }
}

// Darwin's default unpacked line tables live in compiler object files. Keep only
// the selected runtime binaries' dSYMs, using official Nextest archive extras.
export function macDebugSymbolPaths(binaries, root) {
  verifyLayout(binaries, root);
  const target = path.join(root, "target");
  const selected = Object.values(binaries["rust-binaries"]).map(
    (binary) => binary["binary-path"],
  );
  for (const values of Object.values(
    binaries["rust-build-meta"]["non-test-binaries"] ?? {},
  ))
    for (const binary of values)
      selected.push(path.resolve(target, binary.path));
  for (const binary of selected) {
    const relative = path.relative(target, binary);
    assert.ok(
      relative && !relative.startsWith("..") && !path.isAbsolute(relative),
      "Debug executable must remain under checkout target",
    );
  }
  return [...new Set(selected)].sort();
}
async function verifyMacDebugSymbols(
  binaries,
  root,
  expected,
  extractedRoot = root,
) {
  const target = path.join(extractedRoot, "target");
  assert.deepEqual(
    expected.map((row) => row.binary),
    macDebugSymbolPaths(binaries, root).map((binary) =>
      path.relative(path.join(root, "target"), binary),
    ),
    "Archived debug symbol inventory differs",
  );
  for (const row of expected) {
    const bundle = path.join(target, `${row.binary}.dSYM`);
    assert.equal(
      await fileHash(
        path.join(
          bundle,
          "Contents",
          "Resources",
          "DWARF",
          path.basename(row.binary),
        ),
      ),
      row.dwarfSha256,
      "Archived debug line tables differ",
    );
    assert.equal(
      await fileHash(path.join(bundle, "Contents", "Info.plist")),
      row.plistSha256,
      "Archived debug symbol metadata differs",
    );
  }
}

export async function produce(
  directory,
  { profile: profileName = "windows" } = {},
) {
  const profile = archiveProfile(profileName);
  const before = await identity(undefined, profile);
  const archive = path.join(directory, "workspace.tar.zst");
  assert.ok(!existsSync(archive), "Archive already exists");
  mkdirSync(directory, { recursive: true });
  let debugSymbols,
    archiveConfigArgs = [];
  if (!profile.instrumented) {
    const binaries = await list([
      ...profile.buildArgs,
      "--list-type",
      "binaries-only",
    ]);
    debugSymbols = [];
    for (const binary of macDebugSymbolPaths(binaries, process.cwd())) {
      const bundle = `${binary}.dSYM`;
      await command([binary, "-o", bundle], { binary: "dsymutil" });
      debugSymbols.push({
        binary: path.relative(path.resolve("target"), binary),
        dwarfSha256: await fileHash(
          path.join(
            bundle,
            "Contents",
            "Resources",
            "DWARF",
            path.basename(binary),
          ),
        ),
        plistSha256: await fileHash(
          path.join(bundle, "Contents", "Info.plist"),
        ),
      });
    }
    const configFile = path.join(directory, "archive-debug-symbols.toml");
    writeFileSync(
      configFile,
      `[profile.default.archive]\ninclude = [\n${debugSymbols.map((row) => `  { path = ${JSON.stringify(`${row.binary}.dSYM`)}, relative-to = "target", on-missing = "error" }`).join(",\n")}\n]\n`,
    );
    archiveConfigArgs = ["--tool-config-file", `mesh-talk:${configFile}`];
  }
  await command([
    "nextest",
    "archive",
    ...nextestArgs,
    ...archiveConfigArgs,
    ...profile.buildArgs,
    "--archive-file",
    archive,
  ]);
  const probe = path.join(directory, "inventory-extract");
  mkdirSync(probe, { recursive: true });
  const binaries = await list([
    "--archive-file",
    archive,
    "--extract-to",
    probe,
    "--list-type",
    "binaries-only",
  ]);
  verifyLayout(binaries, process.cwd());
  if (!profile.instrumented)
    await verifyMacDebugSymbols(binaries, process.cwd(), debugSymbols, probe);
  const binaryFile = path.join(directory, "producer-binaries.json");
  writeJson(binaryFile, binaries);
  const metadataFile = path.join(
    probe,
    "target",
    "nextest",
    "cargo-metadata.json",
  );
  const metadata = readJson(metadataFile);
  const reuse = [
    "--binaries-metadata",
    binaryFile,
    "--cargo-metadata",
    metadataFile,
    "--target-dir-remap",
    path.join(probe, "target"),
  ];
  const listings = [await list(reuse)];
  for (const shard of profile.partitions)
    listings.push(
      await list([
        ...reuse,
        "--partition",
        `hash:${shard}/${profile.partitions.length}`,
      ]),
    );
  verifyTargets(listings[0], metadata, profile.name);
  const inventories = listings.map(inventory);
  verifyPartition(
    inventories[0],
    inventories.slice(1),
    profile.partitions.length,
  );
  const files = {};
  for (let index = 0; index < inventories.length; index++) {
    const name = `inventory-${index}.json`;
    writeJson(path.join(directory, name), inventories[index]);
    files[name] = await fileHash(path.join(directory, name));
  }
  const featureResolutionSha256 = digest(canonical(metadata.resolve));
  if (!profile.instrumented)
    writeJson(
      path.join(directory, "feature-resolution.json"),
      metadata.resolve,
    );
  verifyIdentity(before, await identity(undefined, profile));
  const archiveSha256 = await fileHash(archive);
  writeJson(path.join(directory, "manifest.json"), {
    schema: 1,
    identity: before,
    archiveSha256,
    inventoryFiles: files,
    featureResolutionSha256,
    ...(!profile.instrumented ? { debugSymbols } : {}),
    testCount: inventories[0].length,
    runnableCount: inventories[0].filter((row) => row.selected).length,
    scheduling: scheduling(profile),
  });
  rmSync(probe, { recursive: true });
  rmSync(binaryFile);
  console.log(
    `Archived ${inventories[0].length} tests; ${inventories[0].filter((row) => row.selected).length} runnable across ${profile.partitions.length} verified shards.`,
  );
  return {
    archiveSha256,
    manifestSha256: await fileHash(path.join(directory, "manifest.json")),
    rustVersion: /^rustc ([^ ]+)/u.exec(before.rustc)?.[1],
  };
}

export function consumerFailurePolicy(env = process.env) {
  const value = env.MESH_WINDOWS_FAIL_FAST;
  assert.ok(value === undefined || value === "0" || value === "1", "MESH_WINDOWS_FAIL_FAST must be 0 or 1");
  return value === "1" ? "--fail-fast" : "--no-fail-fast";
}

export async function consume(
  shard,
  directory,
  output,
  { profile: profileName = "windows", manifestSha256, archiveSha256 } = {},
) {
  const profile = archiveProfile(profileName);
  assert.ok(
    profile.partitions.includes(shard),
    `shard must be an integer from 1 through ${profile.partitions.length}`,
  );
  if (!profile.instrumented) {
    requireDigest(manifestSha256);
    requireDigest(archiveSha256);
    assert.equal(
      await fileHash(path.join(directory, "manifest.json")),
      manifestSha256,
      "Trusted manifest digest mismatch",
    );
  }
  mkdirSync(output, { recursive: true });
  const progress = path.join(output, "progress.jsonl");
  assert.ok(
    !existsSync(progress) && !existsSync(path.join(output, "receipt.json")),
    "Consumer output already contains a prior attempt",
  );
  const evidence = (phase) => ({
    progress,
    phase,
    receipt: path.join(output, phase),
  });
  const manifest = readJson(path.join(directory, "manifest.json"));
  assert.equal(manifest.schema, 1, "Unsupported archive manifest");
  verifyIdentity(manifest.identity, await identity(evidence, profile));
  assert.deepEqual(manifest.scheduling, scheduling(profile));
  if (!profile.instrumented)
    assert.equal(
      manifest.archiveSha256,
      archiveSha256,
      "Trusted archive digest mismatch",
    );
  const archive = path.join(directory, "workspace.tar.zst");
  assert.equal(
    await fileHash(archive),
    manifest.archiveSha256,
    "Archive digest mismatch",
  );
  const expected = [];
  for (let index = 0; index <= profile.partitions.length; index++) {
    const name = `inventory-${index}.json`;
    assert.equal(
      await fileHash(path.join(directory, name)),
      manifest.inventoryFiles[name],
      "Inventory digest mismatch",
    );
    expected.push(readJson(path.join(directory, name)));
  }
  verifyPartition(expected[0], expected.slice(1), profile.partitions.length);
  // Never reuse producer build outputs accidentally: this proves archive-only execution.
  assert.ok(
    profile.instrumented
      ? !existsSync(path.join(process.cwd(), "target", "debug"))
      : !existsSync(path.resolve("target")) ||
          readdirSync(path.resolve("target")).length === 0,
    "Consumer target contains prior compilation outputs",
  );
  writeJson(path.join(output, "expected-inventory.json"), expected[shard]);
  writeJson(path.join(output, "input-identity.json"), {
    sourceSha: manifest.identity.sourceSha,
    runId: manifest.identity.runId,
    runAttempt: manifest.identity.runAttempt,
    shard,
    archiveSha256: manifest.archiveSha256,
  });
  const binaries = await list(
    [
      "--archive-file",
      archive,
      "--extract-to",
      process.cwd(),
      "--list-type",
      "binaries-only",
    ],
    evidence("extract"),
  );
  verifyLayout(binaries, process.cwd());
  if (!profile.instrumented)
    await verifyMacDebugSymbols(binaries, process.cwd(), manifest.debugSymbols);
  const binaryFile = path.join(output, "consumer-binaries.json");
  writeJson(binaryFile, binaries);
  const metadataFile = path.resolve("target/nextest/cargo-metadata.json");
  const metadata = readJson(metadataFile);
  assert.equal(
    digest(canonical(metadata.resolve)),
    manifest.featureResolutionSha256,
    "Archived feature resolution changed",
  );
  const reuse = [
    "--binaries-metadata",
    binaryFile,
    "--cargo-metadata",
    metadataFile,
    "--target-dir-remap",
    path.resolve("target"),
  ];
  const listings = [await list(reuse, evidence("inventory-all"))];
  for (const index of profile.partitions)
    listings.push(
      await list(
        [...reuse, "--partition", `hash:${index}/${profile.partitions.length}`],
        evidence(`inventory-${index}`),
      ),
    );
  verifyTargets(listings[0], metadata, profile.name);
  const observed = listings.map(inventory);
  verifyPartition(observed[0], observed.slice(1), profile.partitions.length);
  assert.deepEqual(
    observed,
    expected,
    "Consumer partition differs from producer inventory",
  );
  writeJson(path.join(output, "selected-inventory.json"), observed[shard]);
  const started = performance.now();
  const events = await command(
    [
      "nextest",
      "run",
      ...nextestArgs,
      ...reuse,
      "--partition",
      `hash:${shard}/${profile.partitions.length}`,
      "--test-threads",
      String(profile.testThreads),
      "--retries",
      "0",
      profile.instrumented ? consumerFailurePolicy() : "--no-fail-fast",
      "--no-tests",
      "fail",
      ...(process.env.MESH_TALK_TEST_TIMING === "1"
        ? ["--success-output", "final"]
        : []),
      "--message-format",
      "libtest-json-plus",
      "--message-format-version",
      "0.1",
    ],
    {
      env: {
        ...process.env,
        CARGO_NET_OFFLINE: "true",
        RUST_TEST_THREADS: String(profile.testThreads),
        NEXTEST_EXPERIMENTAL_LIBTEST_JSON: "1",
      },
      ...evidence("execution"),
      liveStdout: true,
    },
  );
  verifyExecution(events, observed[shard]);
  let coverageEvidence = {};
  if (profile.instrumented) {
    const coverageFile = path.join(output, "coverage.lcov");
    await command(
      ["llvm-cov", "report", "--lcov", "--output-path", coverageFile],
      {
        env: { ...process.env, CARGO_NET_OFFLINE: "true" },
        ...evidence("coverage"),
      },
    );
    const coverage = validateLcov(readFileSync(coverageFile, "utf8"));
    coverageEvidence = {
      coverageSha256: await fileHash(coverageFile),
      coverage,
    };
  }
  verifyIdentity(
    manifest.identity,
    await identity((phase) => evidence(`final-${phase}`), profile),
  );
  writeJson(path.join(output, "receipt.json"), {
    schema: 1,
    sourceSha: manifest.identity.sourceSha,
    runId: manifest.identity.runId,
    runAttempt: manifest.identity.runAttempt,
    shard,
    archiveSha256: manifest.archiveSha256,
    manifestSha256: await fileHash(path.join(directory, "manifest.json")),
    ...coverageEvidence,
    ...(!profile.instrumented
      ? {
          identity: manifest.identity,
          featureResolutionSha256: manifest.featureResolutionSha256,
          inventoryFiles: manifest.inventoryFiles,
          selectedInventorySha256: await fileHash(
            path.join(output, "selected-inventory.json"),
          ),
        }
      : {}),
    executionSha256: await fileHash(path.join(output, "execution.jsonl")),
    seconds: (performance.now() - started) / 1000,
    executedTests: observed[shard].filter((row) => row.selected).length,
    totalTests: observed[0].length,
  });
  console.log(
    `Shard ${shard}/${profile.partitions.length} passed exact inventory${profile.instrumented ? " and coverage" : ""} validation.`,
  );
}

// Pure cross-platform collection: producer job outputs are authority, never this
// collector's platform/root or an artifact's self-reported checksum.
export async function collectMacRemaining(directory, outputs, trusted) {
  requireDigest(trusted?.manifestSha256);
  requireDigest(trusted?.archiveSha256);
  assert.deepEqual(
    trusted.jobResults,
    { native: "success", producer: "success", consumers: "success" },
    "All native archive job results must be success",
  );
  assert.match(
    trusted.sourceSha ?? "",
    /^[a-f0-9]{40}$/u,
    "Expected source SHA required",
  );
  assert.match(trusted.runId ?? "", /^[1-9]\d*$/u, "Expected run ID required");
  assert.match(
    trusted.runAttempt ?? "",
    /^[1-9]\d*$/u,
    "Expected attempt required",
  );
  assert.equal(
    await fileHash(path.join(directory, "manifest.json")),
    trusted.manifestSha256,
    "Trusted manifest digest mismatch",
  );
  const manifest = readJson(path.join(directory, "manifest.json"));
  const profile = archiveProfile("macos-remaining");
  assert.equal(manifest.schema, 1, "Unsupported archive manifest");
  assert.equal(
    manifest.identity.archiveProfile,
    profile.name,
    "Wrong archive profile",
  );
  assert.equal(manifest.identity.platform, "darwin", "Wrong producer platform");
  for (const key of ["sourceSha", "runId", "runAttempt"])
    assert.equal(manifest.identity[key], trusted[key], `Stale producer ${key}`);
  assert.equal(
    manifest.archiveSha256,
    trusted.archiveSha256,
    "Trusted archive digest mismatch",
  );
  assert.equal(
    digest(
      canonical(readJson(path.join(directory, "feature-resolution.json"))),
    ),
    manifest.featureResolutionSha256,
    "Producer feature resolution digest mismatch",
  );
  assert.deepEqual(
    manifest.scheduling,
    scheduling(profile),
    "Wrong archive scheduling",
  );
  const expected = [];
  assert.deepEqual(
    Object.keys(manifest.inventoryFiles).sort(),
    ["inventory-0.json", "inventory-1.json", "inventory-2.json"],
    "Unexpected inventory files",
  );
  for (let index = 0; index <= 2; index++) {
    const name = `inventory-${index}.json`;
    assert.equal(
      await fileHash(path.join(directory, name)),
      manifest.inventoryFiles[name],
      "Producer inventory digest mismatch",
    );
    expected.push(readJson(path.join(directory, name)));
  }
  verifyPartition(expected[0], expected.slice(1), 2);
  assert.equal(
    manifest.testCount,
    expected[0].length,
    "Producer total test count mismatch",
  );
  assert.equal(
    manifest.runnableCount,
    expected[0].filter((row) => row.selected).length,
    "Producer runnable count mismatch",
  );
  assert.equal(outputs.length, 2, "Exactly two consumer receipts required");
  const seen = new Set();
  for (const output of outputs) {
    const receipt = readJson(path.join(output, "receipt.json"));
    assert.equal(receipt.schema, 1, "Unsupported consumer receipt");
    assert.ok(
      profile.partitions.includes(receipt.shard) && !seen.has(receipt.shard),
      "Missing or duplicate consumer shard",
    );
    seen.add(receipt.shard);
    assert.ok(
      !("coverage" in receipt) && !("coverageSha256" in receipt),
      "Uninstrumented receipt cannot claim coverage",
    );
    verifyIdentity(manifest.identity, receipt.identity);
    for (const key of ["sourceSha", "runId", "runAttempt"])
      assert.equal(receipt[key], trusted[key], `Stale receipt ${key}`);
    assert.equal(
      receipt.archiveSha256,
      trusted.archiveSha256,
      "Receipt archive digest mismatch",
    );
    assert.equal(
      receipt.manifestSha256,
      trusted.manifestSha256,
      "Receipt manifest digest mismatch",
    );
    assert.equal(
      receipt.featureResolutionSha256,
      manifest.featureResolutionSha256,
      "Receipt feature graph differs",
    );
    assert.deepEqual(
      receipt.inventoryFiles,
      manifest.inventoryFiles,
      "Receipt inventories differ",
    );
    const selectedFile = path.join(output, "selected-inventory.json");
    assert.equal(
      await fileHash(selectedFile),
      receipt.selectedInventorySha256,
      "Selected inventory digest mismatch",
    );
    const selected = readJson(selectedFile);
    assert.deepEqual(
      selected,
      expected[receipt.shard],
      "Selected partition differs",
    );
    assert.equal(
      receipt.executedTests,
      selected.filter((row) => row.selected).length,
      "Execution count differs",
    );
    assert.equal(
      receipt.totalTests,
      expected[0].length,
      "Total inventory count differs",
    );
    assert.ok(
      Number.isFinite(receipt.seconds) && receipt.seconds >= 0,
      "Invalid execution duration",
    );
    const execution = path.join(output, "execution.jsonl");
    assert.equal(
      await fileHash(execution),
      receipt.executionSha256,
      "execution digest mismatch",
    );
    verifyExecution(readFileSync(execution, "utf8"), selected);
  }
  return {
    sourceSha: trusted.sourceSha,
    runId: trusted.runId,
    runAttempt: trusted.runAttempt,
    archiveSha256: trusted.archiveSha256,
    manifestSha256: trusted.manifestSha256,
    executedTests: manifest.runnableCount,
  };
}
function requireDigest(value) {
  assert.match(value ?? "", /^[a-f0-9]{64}$/u, "Trusted digest required");
}

async function main(args) {
  const [mode, ...values] = args;
  if (mode === "produce" && values.length === 1)
    await produce(path.resolve(values[0]));
  else if (mode === "consume" && values.length === 3)
    await consume(
      Number(values[0]),
      path.resolve(values[1]),
      path.resolve(values[2]),
    );
  else if (
    mode === "produce-macos-remaining" &&
    (values.length === 1 ||
      (values.length === 2 && values[1] === "--github-output"))
  ) {
    const result = await produce(path.resolve(values[0]), {
      profile: "macos-remaining",
    });
    if (values.length === 2) {
      assert.ok(
        process.env.GITHUB_OUTPUT,
        "GITHUB_OUTPUT is required for explicit export",
      );
      for (const [key, value] of Object.entries(result)) {
        assert.match(value ?? "", /^[a-zA-Z0-9.+-]+$/u, "Invalid job output");
        appendFileSync(process.env.GITHUB_OUTPUT, `${key}=${value}\n`);
      }
    }
  } else if (mode === "consume-macos-remaining" && values.length === 5) {
    await consume(
      Number(values[0]),
      path.resolve(values[1]),
      path.resolve(values[2]),
      {
        profile: "macos-remaining",
        manifestSha256: values[3],
        archiveSha256: values[4],
      },
    );
  } else if (mode === "collect-macos-remaining" && values.length === 5) {
    const result = await collectMacRemaining(
      path.resolve(values[0]),
      values.slice(1, 3).map((value) => path.resolve(value)),
      {
        manifestSha256: values[3],
        archiveSha256: values[4],
        sourceSha: process.env.GITHUB_SHA,
        runId: process.env.GITHUB_RUN_ID,
        runAttempt: process.env.GITHUB_RUN_ATTEMPT,
        jobResults: {
          native: process.env.MESH_MACOS_NATIVE_RESULT,
          producer: process.env.MESH_MACOS_PRODUCER_RESULT,
          consumers: process.env.MESH_MACOS_CONSUMERS_RESULT,
        },
      },
    );
    console.log(JSON.stringify(result));
  } else
    throw new Error(
      "Usage: windows-coverage.mjs produce ARTIFACT_DIR | consume SHARD ARTIFACT_DIR OUTPUT_DIR | produce-macos-remaining ARTIFACT_DIR [--github-output] | consume-macos-remaining SHARD ARTIFACT_DIR OUTPUT_DIR MANIFEST_SHA256 ARCHIVE_SHA256 | collect-macos-remaining METADATA_DIR RESULT_1_DIR RESULT_2_DIR MANIFEST_SHA256 ARCHIVE_SHA256",
    );
}
if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
)
  main(process.argv.slice(2)).catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
