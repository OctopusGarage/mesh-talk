import assert from "node:assert/strict";
import test from "node:test";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  mkdirSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import { createHash } from "node:crypto";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import {
  versions,
  windowsPartitions,
  command,
  consumerFailurePolicy,
  inventory,
  verifyPartition,
  verifyExecution,
  validateLcov,
  verifyIdentity,
  verifyTargets,
  validateProfilePath,
  verifyLayout,
  archiveProfile,
  macDebugSymbolPaths,
  validateMacEnvironment,
  collectMacRemaining,
} from "../ci/windows-coverage.mjs";

test("Windows execution partitions keep all eight native jobs directly required", () => {
  assert.deepEqual(windowsPartitions, [1, 2, 3, 4, 5, 6, 7, 8]);
  const workflow = readFileSync(
    new URL("../../.github/workflows/windows-workspace.yml", import.meta.url),
    "utf8",
  );
  assert.match(workflow, /name: Windows workspace tests .*\/8\)/u);
  assert.match(workflow, /shard: \[1, 2, 3, 4, 5, 6, 7, 8\]/u);
  assert.doesNotMatch(workflow, /\n  qualify:|\n  acceptance:/u);
});

test("nextest security minimum and every CI installer follow the version owner", () => {
  // GHSA-vc25-qg58-w57c: portable recording parsing is patched in 0.9.150.
  const minimum = [0, 9, 150];
  const current = versions.nextest.split(".").map(Number);
  assert.equal(current.length, minimum.length);
  assert(current.every(Number.isSafeInteger));
  const changed = current.findIndex((part, index) => part !== minimum[index]);
  assert(
    changed < 0 || current[changed] > minimum[changed],
    "nextest must include the recording parser security fix",
  );
  const config = readFileSync(
    new URL("../../.config/nextest.toml", import.meta.url),
    "utf8",
  );
  assert.equal(
    config.match(/^nextest-version = "([^"]+)"$/mu)?.[1],
    versions.nextest,
  );
  for (const file of ["ci.yml", "windows-workspace.yml", "mutants.yml"]) {
    const source = readFileSync(
      new URL(`../../.github/workflows/${file}`, import.meta.url),
      "utf8",
    );
    const pins = [...source.matchAll(/cargo-nextest@([0-9.]+)/gu)].map(
      (match) => match[1],
    );
    assert(pins.length > 0, `${file} must install the owned nextest version`);
    assert(
      pins.every((pin) => pin === versions.nextest),
      `${file} nextest installer drift`,
    );
  }
});

test("actual Windows consumer collects all selected cases after a failure", () => {
  assert.equal(consumerFailurePolicy({}), "--no-fail-fast");
  assert.equal(
    consumerFailurePolicy({ GITHUB_ACTIONS: "true" }),
    "--no-fail-fast",
  );
  assert.equal(
    consumerFailurePolicy({ MESH_WINDOWS_FAIL_FAST: "0" }),
    "--no-fail-fast",
  );
  assert.equal(
    consumerFailurePolicy({ MESH_WINDOWS_FAIL_FAST: "1" }),
    "--fail-fast",
  );
  for (const value of ["", "true", "2"])
    assert.throws(
      () => consumerFailurePolicy({ MESH_WINDOWS_FAIL_FAST: value }),
      /must be 0 or 1/u,
    );
  const workflow = readFileSync(
    new URL("../../.github/workflows/windows-workspace.yml", import.meta.url),
    "utf8",
  );
  assert.equal(workflow.match(/MESH_WINDOWS_FAIL_FAST:/gu)?.length, 1);
  const consumer = workflow
    .split(
      "      - name: Run validated serial shard and export partial coverage\n",
    )[1]
    ?.split("      - name:")[0];
  assert.match(consumer, /MESH_WINDOWS_FAIL_FAST: '0'/u);
  assert.match(workflow, /fail-fast: false/u);
});

function commandFixture(t) {
  const root = mkdtempSync(
    path.join(os.tmpdir(), "windows-coverage-evidence-"),
  );
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return {
    binary: process.execPath,
    receipt: path.join(root, "execution"),
    progress: path.join(root, "progress.jsonl"),
    phase: "execution",
  };
}

test(
  "cancelled commands retain live output and pre-child phase without accepting partial execution",
  { timeout: 10000 },
  async (t) => {
    const options = commandFixture(t);
    const controller = new AbortController();
    t.after(() => controller.abort());
    const started = JSON.stringify({
      type: "test",
      event: "started",
      name: "pkg::pkg$a",
    });
    const program = `
    const fs = require('node:fs');
    const phase = JSON.parse(fs.readFileSync(${JSON.stringify(options.progress)}, 'utf8').split('\\n')[0]);
    if (phase.event !== 'started' || phase.phase !== 'execution') process.exit(9);
    process.stdout.write(${JSON.stringify(`${started}\n`)});
    process.stderr.write('still-running\\n');
    setInterval(() => {}, 1000);
  `;
    const completion = command(["-e", program], {
      ...options,
      signal: controller.signal,
    });
    const rejected = assert.rejects(completion, /interrupted/u);
    const deadline = performance.now() + 5000;
    while (
      readFileSync(`${options.receipt}.log`, "utf8") !== "still-running\n"
    ) {
      assert.ok(
        performance.now() < deadline,
        "Child did not publish its live output",
      );
      await delay(10);
    }
    assert.equal(
      readFileSync(`${options.receipt}.jsonl`, "utf8"),
      `${started}\n`,
    );
    const before = readFileSync(options.progress, "utf8")
      .trim()
      .split("\n")
      .map(JSON.parse);
    assert.deepEqual(
      before.map((row) => row.event),
      ["started", "spawned"],
    );
    assert.throws(
      () => verifyExecution(`${started}\n`, inventory(listing(["a"]))),
      /execution/u,
    );
    controller.abort();
    await rejected;
    const after = readFileSync(options.progress, "utf8")
      .trim()
      .split("\n")
      .map(JSON.parse);
    assert.equal(after.at(-1).event, "closed");
    assert.equal(after.at(-1).failed, true);
    assert.equal(after.at(-1).stdoutBytes, Buffer.byteLength(`${started}\n`));
    assert.equal(
      readFileSync(`${options.receipt}.log`, "utf8"),
      "still-running\n",
    );
    assert.throws(() => process.kill(before[1].pid, 0));
  },
);

test("nonzero exits and spawn failures retain evidence and reject before coverage acceptance", async (t) => {
  const options = commandFixture(t);
  await assert.rejects(
    command(
      [
        "-e",
        "process.stdout.write('partial\\n'); process.stderr.write('failed\\n'); process.exitCode = 7;",
      ],
      options,
    ),
    /failed: 7/u,
  );
  assert.equal(readFileSync(`${options.receipt}.jsonl`, "utf8"), "partial\n");
  assert.equal(readFileSync(`${options.receipt}.log`, "utf8"), "failed\n");
  assert.equal(
    JSON.parse(readFileSync(options.progress, "utf8").trim().split("\n").at(-1))
      .code,
    7,
  );
  await assert.rejects(
    command([], {
      ...options,
      binary: path.join(path.dirname(options.receipt), "missing-executable"),
    }),
    /ENOENT/u,
  );
  assert.equal(
    JSON.parse(readFileSync(options.progress, "utf8").trim().split("\n").at(-1))
      .failed,
    true,
  );
  assert.equal(
    existsSync(path.join(path.dirname(options.receipt), "receipt.json")),
    false,
  );
});

test("successful streaming preserves exact Unicode output and closes receipt files", async (t) => {
  const options = commandFixture(t);
  const text = "retained ✓\n";
  const result = await command(
    [
      "-e",
      `const b = Buffer.from(${JSON.stringify(text)}); process.stdout.write(b.subarray(0, 10)); setTimeout(() => process.stdout.write(b.subarray(10)), 10);`,
    ],
    options,
  );
  assert.equal(result, text);
  assert.equal(readFileSync(`${options.receipt}.jsonl`, "utf8"), text);
  const closed = JSON.parse(
    readFileSync(options.progress, "utf8").trim().split("\n").at(-1),
  );
  assert.equal(closed.failed, false);
  assert.equal(closed.code, 0);
  // In particular, Windows must be able to retire output after handles close.
  rmSync(`${options.receipt}.jsonl`);
});

test(
  "output overflow is rejected before receipt writes and cannot grow during cancellation",
  { timeout: 10000 },
  async (t) => {
    const options = commandFixture(t);
    await assert.rejects(
      command(
        [
          "-e",
          `
    process.on('SIGTERM', () => process.stdout.write('later-output'));
    process.stdout.write(Buffer.alloc(256));
    setInterval(() => process.stdout.write('more-output'), 10);
  `,
        ],
        { ...options, maxOutputBytes: 64 },
      ),
      /output exceeded/u,
    );
    assert.equal(readFileSync(`${options.receipt}.jsonl`).length, 0);
    const closed = JSON.parse(
      readFileSync(options.progress, "utf8").trim().split("\n").at(-1),
    );
    assert.equal(closed.failed, true);
    assert.equal(closed.stdoutBytes, 0);
  },
);

const listing = (selected, names = ["a", "b", "c", "d"]) => ({
  "test-count": names.length + 1,
  "rust-suites": {
    lib: {
      "package-name": "pkg",
      "package-id": "pkg-id",
      "binary-id": "lib",
      "binary-name": "pkg",
      kind: "lib",
      status: "listed",
      testcases: Object.fromEntries(
        [...names, "ignored"].map((name) => [
          name,
          {
            kind: "test",
            ignored: name === "ignored",
            "filter-match": {
              status: selected.includes(name) ? "matches" : "mismatch",
              reason: name === "ignored" ? "ignored" : "partition",
            },
          },
        ]),
      ),
    },
  },
});

test("eight official partitions retain ignored rows and execute every runnable case exactly once", () => {
  const names = ["a", "b", "c", "d", "e", "f", "g", "h"];
  const all = inventory(listing(names, names));
  const shards = names.map((name) => inventory(listing([name], names)));
  assert.equal(all.length, 9);
  assert.equal(all.filter((row) => row.ignored).length, 1);
  verifyPartition(all, shards);
  for (const missing of [0, 7])
    assert.throws(
      () =>
        verifyPartition(
          all,
          shards.filter((_, index) => index !== missing),
        ),
      /partitions/,
    );
  assert.throws(
    () => verifyPartition(all, [...shards.slice(0, 7), shards[0]]),
    /partition/,
  );
  assert.throws(
    () =>
      verifyPartition(all, [
        ...shards.slice(0, 7),
        inventory(listing([], names)),
      ]),
    /partition/,
  );
  assert.throws(
    () =>
      verifyPartition(all, [
        ...shards.slice(0, 7),
        inventory(listing(["h", "ignored"], names)),
      ]),
    /ignored/,
  );
  assert.throws(() => inventory({ ...listing([]), "test-count": 6 }), /count/);
});

test("execution receipts reject missing, duplicate, unexpected, failed and ignored runnable cases", () => {
  const expected = inventory(listing(["a"]));
  const ok = [
    {
      type: "suite",
      event: "started",
      nextest: { crate: "pkg", test_binary: "pkg", kind: "lib" },
    },
    { type: "test", event: "started", name: "pkg::pkg$a" },
    { type: "test", event: "ok", name: "pkg::pkg$a" },
    {
      type: "suite",
      event: "ok",
      failed: 0,
      nextest: { crate: "pkg", test_binary: "pkg", kind: "lib" },
    },
  ]
    .map((event) => JSON.stringify(event))
    .join("\n");
  verifyExecution(ok, expected);
  for (const text of [
    "",
    `${ok}\n${ok}`,
    ok.replace("$a", "$b"),
    ok.replace('"ok"', '"failed"'),
    ok.replace('"ok"', '"ignored"'),
    ok.split("\n").slice(0, -1).join("\n"),
  ])
    assert.throws(() => verifyExecution(text, expected), /execution/);
});

test("partial LCOV requires source rows, valid measured lines and positive coverage", () => {
  validateLcov("SF:/source/lib.rs\nDA:1,1\nDA:2,0\nend_of_record\n");
  for (const text of [
    "",
    "SF:/source/lib.rs\nDA:1,0\nend_of_record\n",
    "SF:/source/lib.rs\nDA:1,-1\nend_of_record\n",
    "SF:/source/lib.rs\nDA:1,1\n",
  ])
    assert.throws(() => validateLcov(text), /coverage/);
});

test("consumer identity rejects source, toolchain, lock, root, feature/profile and run drift", () => {
  const original = {
    sourceSha: "a".repeat(40),
    root: "/work",
    rustc: "rustc same",
    lockSha256: "x",
    runId: "123",
    runAttempt: "1",
    buildArgs: ["--workspace", "--locked"],
    features: ["feature-a"],
  };
  verifyIdentity(original, structuredClone(original));
  for (const field of Object.keys(original))
    assert.throws(
      () => verifyIdentity(original, { ...original, [field]: "changed" }),
      /identity/,
    );
});

test("default Cargo test targets cannot silently disappear from the archive", () => {
  const metadata = {
    workspace_members: ["pkg-id"],
    resolve: { nodes: [{ id: "pkg-id", features: [] }] },
    packages: [
      {
        id: "pkg-id",
        targets: [
          { name: "pkg", kind: ["rlib", "cdylib"], test: true },
          { name: "disabled", kind: ["example"], test: false },
        ],
      },
    ],
  };
  verifyTargets(listing(["a", "b"]), metadata);
  assert.throws(() => verifyTargets({ "rust-suites": {} }, metadata), /target/);
});

test("instrumented listing cannot emit profiles into source package directories", () => {
  validateProfilePath(
    `${process.cwd()}/target/coverage-%p-%8m.profraw`,
    process.cwd(),
  );
  for (const file of [
    undefined,
    "default.profraw",
    `${process.cwd()}/crates/default.profraw`,
    `${process.cwd()}/target-other/default.profraw`,
  ])
    assert.throws(
      () => validateProfilePath(file, process.cwd()),
      /LLVM_PROFILE_FILE/,
    );
});

test("archive layout is anchored exactly to the metadata v1 debug/deps directory", () => {
  const target = path.join(process.cwd(), "target");
  const metadata = (binary) => ({
    "rust-build-meta": {
      "target-directory": target,
      "build-directory": target,
    },
    "rust-binaries": { lib: { "binary-path": binary } },
  });
  verifyLayout(metadata(path.join(target, "debug", "deps", "unit")));
  const example = metadata(path.join(target, "debug", "examples", "example"));
  example["rust-binaries"].lib.kind = "example";
  verifyLayout(example);
  for (const binary of [
    path.join(target, "custom-build", "debug", "deps", "unit"),
    path.join(target, "release", "deps", "unit"),
    "relative/debug/deps/unit",
  ])
    assert.throws(() => verifyLayout(metadata(binary)), /layout/);
  const separate = metadata(path.join(target, "debug", "deps", "unit"));
  separate["rust-build-meta"]["build-directory"] = path.join(target, "separate");
  assert.throws(() => verifyLayout(separate), /separate build/);
});

test("legacy Windows cache compatibility overrides stay outside runtime execution", () => {
  const source = readFileSync(
    new URL("../../.github/workflows/windows-workspace.yml", import.meta.url),
    "utf8",
  );
  const globals = source.split("\njobs:")[0];
  assert.match(globals, /CARGO_TERM_COLOR: never/u);
  assert.match(globals, /RUST_TEST_THREADS: '1'/u);
  const cache = source
    .split(/^      - /mu)
    .find((step) => step.startsWith("uses: Swatinem/rust-cache@v2\n"));
  assert.ok(cache);
  assert.match(cache, /shared-key: workspace-checks-checks/u);
  assert.match(
    cache,
    /env:\n          CARGO_TERM_COLOR: always\n          RUST_TEST_THREADS: ''/u,
  );
  const runtime = source.replace(cache, "");
  assert.doesNotMatch(
    runtime,
    /CARGO_TERM_COLOR: always|RUST_TEST_THREADS: ''/u,
  );
  assert.match(runtime, /cargo test --workspace --doc --locked/u);
});

test("macOS remaining profile is explicit and leaves Windows defaults intact", () => {
  assert.deepEqual(archiveProfile().buildArgs, ["--workspace", "--locked"]);
  const mac = archiveProfile("macos-remaining");
  assert.deepEqual(mac.buildArgs, [
    "--workspace",
    "--exclude",
    "mesh-talk",
    "--locked",
  ]);
  assert.deepEqual(mac.partitions, [1, 2]);
  assert.equal(mac.testThreads, 4);
  assert.equal(mac.instrumented, false);
  assert.throws(() => archiveProfile("other"), /profile/);
});

test("uninstrumented macOS archive rejects inherited compilation and target overrides", () => {
  const root = process.cwd();
  const env = {
    CARGO_BUILD_JOBS: "2",
    CARGO_INCREMENTAL: "0",
    CARGO_PROFILE_DEV_DEBUG: "line-tables-only",
    CARGO_PROFILE_TEST_DEBUG: "line-tables-only",
    CARGO_TARGET_DIR: path.join(root, "target"),
  };
  validateMacEnvironment(env, root);
  for (const key of [
    "LLVM_PROFILE_FILE",
    "CARGO_LLVM_COV_SHOW_ENV",
    "__CARGO_LLVM_COV_RUSTFLAGS",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_TARGET",
    "CARGO_BUILD_BUILD_DIR",
    "CARGO_TARGET_AARCH64_APPLE_DARWIN_RUSTFLAGS",
  ]) {
    assert.throws(
      () => validateMacEnvironment({ ...env, [key]: "inherited" }, root),
      /environment|override/,
    );
  }
  for (const [key, value] of [
    ["CARGO_TARGET_DIR", path.join(root, "other")],
    ["CARGO_BUILD_JOBS", "4"],
    ["CARGO_PROFILE_TEST_DEBUG", "full"],
  ])
    assert.throws(
      () => validateMacEnvironment({ ...env, [key]: value }, root),
      /environment|target|jobs|debug/,
    );
});

test("remaining target cohort excludes exactly Host and automatically includes new workspace packages", () => {
  const packages = ["host", "remaining", "new"].map((id, index) => ({
    id,
    name: index === 0 ? "mesh-talk" : id,
    targets: [{ name: id, kind: ["lib"], test: true }],
  }));
  const metadata = {
    workspace_members: packages.map((pkg) => pkg.id),
    packages,
    resolve: { nodes: packages.map((pkg) => ({ id: pkg.id, features: [] })) },
  };
  const suite = (id) => ({ "package-id": id, kind: "lib", "binary-name": id });
  const listing = {
    "rust-suites": { remaining: suite("remaining"), new: suite("new") },
  };
  verifyTargets(listing, metadata, "macos-remaining");
  assert.throws(
    () =>
      verifyTargets(
        { "rust-suites": { remaining: suite("remaining") } },
        metadata,
        "macos-remaining",
      ),
    /target/,
  );
  assert.throws(
    () =>
      verifyTargets(
        { "rust-suites": { ...listing["rust-suites"], host: suite("host") } },
        metadata,
        "macos-remaining",
      ),
    /target/,
  );
  assert.throws(
    () =>
      verifyTargets(
        listing,
        {
          ...metadata,
          packages: packages.slice(1),
          workspace_members: ["remaining", "new"],
        },
        "macos-remaining",
      ),
    /Host/,
  );
});

test("layout must match the original checkout target, not just internally consistent binary paths", () => {
  const foreign = `${process.cwd()}/foreign/target`;
  assert.throws(
    () =>
      verifyLayout(
        {
          "rust-build-meta": { "target-directory": foreign },
          "rust-binaries": {
            lib: { "binary-path": `${foreign}/debug/deps/unit` },
          },
        },
        process.cwd(),
      ),
    /target/,
  );
});

test("pure Mac collector anchors producer authority and rejects missing, duplicate, stale and partial receipts", async (t) => {
  const root = mkdtempSync(path.join(os.tmpdir(), "mac-collector-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const hash = (value) => createHash("sha256").update(value).digest("hex");
  const put = (directory, name, value) => {
    const text = JSON.stringify(value);
    writeFileSync(path.join(directory, name), text);
    return hash(text);
  };
  const all = inventory(listing(["a", "b", "c", "d"]));
  const shards = [
    ["a", "b"],
    ["c", "d"],
  ].map((names) => inventory(listing(names)));
  const inventoryFiles = Object.fromEntries(
    [all, ...shards].map((rows, index) => [
      `inventory-${index}.json`,
      put(root, `inventory-${index}.json`, rows),
    ]),
  );
  const identity = {
    archiveProfile: "macos-remaining",
    sourceSha: "a".repeat(40),
    runId: "1",
    runAttempt: "2",
    platform: "darwin",
    arch: "arm64",
    root: "/darwin/source",
    rustc: "rustc native",
    cargo: "cargo native",
    cargoConfigSha256: hash("cargo config"),
    nextestConfigSha256: hash("nextest config"),
    buildArgs: archiveProfile("macos-remaining").buildArgs,
  };
  const archiveSha256 = hash("archive bytes not present on collector");
  const manifest = {
    schema: 1,
    identity,
    archiveSha256,
    inventoryFiles,
    featureResolutionSha256: put(root, "feature-resolution.json", {
      nodes: [],
    }),
    testCount: all.length,
    runnableCount: 4,
    scheduling: {
      partitions: 2,
      partition: "hash",
      testThreads: 4,
      retries: 0,
      ignored: "default",
    },
  };
  const manifestSha256 = put(root, "manifest.json", manifest);
  const outputs = shards.map((rows, index) => {
    const output = path.join(root, `result-${index}`);
    mkdirSync(output);
    const selectedInventorySha256 = put(
      output,
      "selected-inventory.json",
      rows,
    );
    const events = rows
      .filter((row) => row.selected)
      .flatMap((row) =>
        ["started", "ok"].map((event) =>
          JSON.stringify({ type: "test", event, name: row.eventName }),
        ),
      )
      .join("\n");
    writeFileSync(path.join(output, "execution.jsonl"), events);
    put(output, "receipt.json", {
      schema: 1,
      identity,
      sourceSha: identity.sourceSha,
      runId: "1",
      runAttempt: "2",
      shard: index + 1,
      archiveSha256,
      manifestSha256,
      inventoryFiles,
      featureResolutionSha256: manifest.featureResolutionSha256,
      selectedInventorySha256,
      executionSha256: hash(events),
      executedTests: 2,
      totalTests: all.length,
      seconds: 0.1,
    });
    return output;
  });
  const trusted = {
    manifestSha256,
    archiveSha256,
    sourceSha: identity.sourceSha,
    runId: "1",
    runAttempt: "2",
    jobResults: {
      native: "success",
      producer: "success",
      consumers: "success",
    },
  };
  assert.equal(
    (await collectMacRemaining(root, outputs, trusted)).executedTests,
    4,
  );
  for (const field of [
    "manifestSha256",
    "archiveSha256",
    "sourceSha",
    "runId",
    "runAttempt",
  ])
    await assert.rejects(
      collectMacRemaining(root, outputs, {
        ...trusted,
        [field]: field.endsWith("Sha256") ? hash("wrong") : "wrong",
      }),
    );
  for (const status of ["failure", "cancelled", "skipped", ""]) {
    await assert.rejects(
      collectMacRemaining(root, outputs, {
        ...trusted,
        jobResults: { ...trusted.jobResults, native: status },
      }),
      /job/,
    );
    await assert.rejects(
      collectMacRemaining(root, outputs, {
        ...trusted,
        jobResults: { ...trusted.jobResults, consumers: status },
      }),
      /job/,
    );
  }
  await assert.rejects(
    collectMacRemaining(root, [outputs[0]], trusted),
    /receipt/,
  );
  await assert.rejects(
    collectMacRemaining(root, [outputs[0], outputs[0]], trusted),
    /shard/,
  );
  const receipt = JSON.parse(
    readFileSync(path.join(outputs[0], "receipt.json"), "utf8"),
  );
  for (const changed of [
    { ...receipt, identity: { ...identity, rustc: "changed" } },
    { ...receipt, coverageSha256: hash("fake LCOV") },
    { ...receipt, executedTests: 1 },
    { ...receipt, featureResolutionSha256: hash("wrong") },
    { ...receipt, manifestSha256: hash("wrong") },
  ]) {
    put(outputs[0], "receipt.json", changed);
    await assert.rejects(collectMacRemaining(root, outputs, trusted));
  }
  put(outputs[0], "receipt.json", receipt);
  const eventsFile = path.join(outputs[0], "execution.jsonl");
  const events = readFileSync(eventsFile, "utf8");
  writeFileSync(eventsFile, events.split("\n").slice(0, -1).join("\n"));
  await assert.rejects(
    collectMacRemaining(root, outputs, trusted),
    /execution/,
  );
  put(outputs[0], "receipt.json", {
    ...receipt,
    executionSha256: hash(readFileSync(eventsFile)),
  });
  await assert.rejects(
    collectMacRemaining(root, outputs, trusted),
    /execution/,
  );
});

test("Mac debug bundles follow exactly selected executable metadata, including embedded CLI helpers", () => {
  const root = process.cwd();
  const target = path.join(root, "target");
  const binaries = {
    "rust-build-meta": {
      "target-directory": target,
      "non-test-binaries": { pkg: [{ kind: "bin-exe", path: "debug/helper" }] },
    },
    "rust-binaries": { lib: { "binary-path": path.join(target, "debug", "deps", "unit") } },
  };
  assert.deepEqual(macDebugSymbolPaths(binaries, root), [
    path.join(target, "debug", "deps", "unit"),
    path.join(target, "debug", "helper"),
  ]);
  binaries["rust-build-meta"]["non-test-binaries"].pkg[0].path = "../foreign";
  assert.throws(() => macDebugSymbolPaths(binaries, root), /target/);
});
