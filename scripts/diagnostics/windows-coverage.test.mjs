import assert from "node:assert/strict";
import test from "node:test";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import {
  versions,
  command,
  consumerFailurePolicy,
  criticalRecoveryProbeEnabled,
  criticalRecoveryProbeCases,
  runCriticalRecoveryProbe,
  inventory,
  verifyPartition,
  verifyExecution,
  validateLcov,
  verifyIdentity,
  verifyTargets,
  validateProfilePath,
  verifyLayout,
} from "../ci/windows-coverage.mjs";

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

const listing = (selected) => ({
  "test-count": 5,
  "rust-suites": {
    lib: {
      "package-name": "pkg",
      "package-id": "pkg-id",
      "binary-id": "lib",
      "binary-name": "pkg",
      kind: "lib",
      status: "listed",
      testcases: Object.fromEntries(
        ["a", "b", "c", "d", "ignored"].map((name) => [
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

test("four official partitions retain ignored rows and execute every runnable case exactly once", () => {
  const names = ["a", "b", "c", "d"];
  const all = inventory(listing(names));
  const shards = names.map((name) => inventory(listing([name])));
  assert.equal(all.length, 5);
  assert.equal(all.filter((row) => row.ignored).length, 1);
  verifyPartition(all, shards);
  for (const missing of [2, 3])
    assert.throws(
      () =>
        verifyPartition(
          all,
          shards.filter((_, index) => index !== missing),
        ),
      /partitions/,
    );
  assert.throws(
    () => verifyPartition(all, [shards[0], shards[1], shards[2], shards[2]]),
    /partition/,
  );
  assert.throws(
    () => verifyPartition(all, [...shards.slice(0, 3), inventory(listing([]))]),
    /partition/,
  );
  assert.throws(
    () =>
      verifyPartition(all, [
        ...shards.slice(0, 3),
        inventory(listing(["d", "ignored"])),
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
  const target = `${process.cwd()}/target`;
  const metadata = (binary) => ({
    "rust-build-meta": {
      "target-directory": target,
      "build-directory": target,
    },
    "rust-binaries": { lib: { "binary-path": binary } },
  });
  verifyLayout(metadata(`${target}/debug/deps/unit`));
  const example = metadata(`${target}/debug/examples/example`);
  example["rust-binaries"].lib.kind = "example";
  verifyLayout(example);
  for (const binary of [
    `${target}/custom-build/debug/deps/unit`,
    `${target}/release/deps/unit`,
    "relative/debug/deps/unit",
  ])
    assert.throws(() => verifyLayout(metadata(binary)), /layout/);
  const separate = metadata(`${target}/debug/deps/unit`);
  separate["rust-build-meta"]["build-directory"] = `${target}/separate`;
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

test("the producer enables the exact critical recovery cohort and preserves its failure log", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/windows-workspace.yml", import.meta.url),
    "utf8",
  );
  const producer = workflow.split("  tests:\n")[0];
  assert.match(producer, /MESH_WINDOWS_CRITICAL_RECOVERY_PROBE: '1'/u);
  assert.match(
    producer,
    /name: Preserve early critical recovery diagnostic\n\s+if: always\(\)/u,
  );
  assert.match(
    producer,
    /windows-critical-recovery-probe-\$\{\{ github.sha \}\}-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}/u,
  );
});

test("critical recovery cohort retains each known regression exactly once", () => {
  assert.deepEqual(criticalRecoveryProbeCases, [
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
});

test("critical recovery probe defaults off and requires the complete exact runnable cohort", async () => {
  for (const env of [{}, { MESH_WINDOWS_CRITICAL_RECOVERY_PROBE: "0" }])
    assert.equal(criticalRecoveryProbeEnabled(env), false);
  assert.equal(
    criticalRecoveryProbeEnabled({ MESH_WINDOWS_CRITICAL_RECOVERY_PROBE: "1" }),
    true,
  );
  for (const value of ["", "true", "2"])
    assert.throws(
      () =>
        criticalRecoveryProbeEnabled({
          MESH_WINDOWS_CRITICAL_RECOVERY_PROBE: value,
        }),
      /must be 0 or 1/u,
    );
  let calls = 0;
  const run = async () => {
    calls++;
    throw new Error("must not run");
  };
  await runCriticalRecoveryProbe({ env: {}, full: [] }, run);
  const rows = criticalRecoveryProbeCases.map((name) => ({
    eventName: `mesh-talk-core::mesh_talk_core$${name}`,
    selected: true,
    ignored: false,
  }));
  for (const full of [
    [],
    rows.slice(1),
    [rows[0], rows[0], ...rows.slice(2)],
    rows.map((row, i) => (i ? row : { ...row, selected: false })),
    rows.map((row, i) => (i ? row : { ...row, ignored: true })),
    rows.map((row, i) =>
      i ? row : { ...row, eventName: "other::other$unrelated" },
    ),
  ]) {
    await assert.rejects(
      runCriticalRecoveryProbe(
        { env: { MESH_WINDOWS_CRITICAL_RECOVERY_PROBE: "1" }, full },
        run,
      ),
      /requires all|one of each|runnable/u,
    );
  }
  assert.equal(calls, 0);
});

for (const outcome of [
  "pass",
  "nonzero",
  "malformed",
  "failed-receipt",
  "missing-receipt",
  "duplicate-receipt",
]) {
  test(`critical recovery probe ${outcome} preserves real subprocess evidence without coverage acceptance`, async (t) => {
    const temporary = mkdtempSync(path.join(os.tmpdir(), "mesh-gossip-probe-"));
    t.after(() => rmSync(temporary, { recursive: true, force: true }));
    const rows = criticalRecoveryProbeCases.map((name) => ({
      eventName: `mesh-talk-core::mesh_talk_core$${name}`,
      selected: true,
      ignored: false,
    }));
    const suite = {
      crate: "mesh-talk-core",
      test_binary: "mesh_talk_core",
      kind: "lib",
    };
    let executed = outcome === "missing-receipt" ? rows.slice(0, -1) : rows;
    if (outcome === "duplicate-receipt") executed = [...rows, rows[0]];
    const failure = ["nonzero", "failed-receipt"].includes(outcome);
    const raw =
      outcome === "malformed"
        ? "not-json\n"
        : [
            { type: "suite", event: "started", nextest: suite },
            ...executed.flatMap((row, index) => [
              { type: "test", event: "started", name: row.eventName },
              {
                type: "test",
                event: failure && index === rows.length - 1 ? "failed" : "ok",
                name: row.eventName,
              },
            ]),
            {
              type: "suite",
              event: failure ? "failed" : "ok",
              failed: failure ? 1 : 0,
              nextest: suite,
            },
          ]
            .map(JSON.stringify)
            .join("\n") + "\n";
    let profiles;
    const run = async (args, options) => {
      assert.deepEqual(args.slice(0, 2), ["nextest", "run"]);
      assert(
        args.includes("sealed-binaries.json") &&
          args.includes("sealed-metadata.json") &&
          args.includes("sealed-target"),
      );
      assert(
        args.includes(
          `package(=mesh-talk-core) & binary(=mesh_talk_core) & (${criticalRecoveryProbeCases.map((name) => `test(=${name})`).join(" | ")})`,
        ),
      );
      assert.equal(args[args.indexOf("--retries") + 1], "0");
      assert.equal(args[args.indexOf("--test-threads") + 1], "1");
      assert(args.includes("--no-fail-fast") && args.includes("--no-tests"));
      assert(!args.includes("--partition") && !args.includes("--lcov"));
      profiles = path.dirname(options.env.LLVM_PROFILE_FILE);
      assert.equal(path.dirname(profiles), temporary);
      const program =
        "const fs=require('node:fs');const path=require('node:path');fs.writeFileSync(path.join(path.dirname(process.env.LLVM_PROFILE_FILE),'fixture.profraw'),'isolated');process.stdout.write(process.argv[1]);process.stderr.write('gossip diagnostic stderr\\n');process.exitCode=Number(process.argv[2]);";
      return command(["-e", program, raw, outcome === "nonzero" ? "7" : "0"], {
        ...options,
        binary: process.execPath,
      });
    };
    const pending = runCriticalRecoveryProbe(
      {
        full: rows,
        reuse: [
          "--binaries-metadata",
          "sealed-binaries.json",
          "--cargo-metadata",
          "sealed-metadata.json",
          "--target-dir-remap",
          "sealed-target",
        ],
        identity: { sourceSha: "fixture-source", runId: "1", runAttempt: "1" },
        archiveSha256: "a".repeat(64),
        env: {
          ...process.env,
          RUNNER_TEMP: temporary,
          MESH_WINDOWS_CRITICAL_RECOVERY_PROBE: "1",
        },
      },
      run,
    );
    if (outcome === "pass") await pending;
    else
      await assert.rejects(
        pending,
        outcome === "nonzero"
          ? /failed: 7/u
          : outcome === "malformed"
            ? /JSON/u
            : /execution/iu,
      );
    const directory = path.join(temporary, "windows-critical-recovery-probe");
    assert.equal(
      readFileSync(path.join(directory, "execution.jsonl"), "utf8"),
      raw,
    );
    assert.match(
      readFileSync(path.join(directory, "execution.log"), "utf8"),
      /gossip diagnostic stderr/u,
    );
    const closed = readFileSync(path.join(directory, "progress.jsonl"), "utf8")
      .trim()
      .split("\n")
      .map(JSON.parse)
      .at(-1);
    assert.equal(closed.event, "closed");
    assert.equal(closed.code, outcome === "nonzero" ? 7 : 0);
    assert.equal(
      existsSync(path.join(directory, "proof.json")),
      outcome === "pass",
    );
    assert.equal(
      existsSync(profiles),
      outcome === "nonzero",
      "unknown command retirement must conservatively retain isolated profiles",
    );
    assert(!readdirSync(directory).includes("receipt.json"));
    assert(
      !readdirSync(directory).some(
        (name) => name.endsWith(".lcov") || name.endsWith(".profraw"),
      ),
    );
  });
}
