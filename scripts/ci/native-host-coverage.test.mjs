// Requires the pinned native CI coverage/Nextest tools and matching LLVM tools.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
const helper = fileURLToPath(
  new URL("./workspace-coverage.sh", import.meta.url),
);
const helperSource = readFileSync(helper, "utf8");
import test from "node:test";
import { versions } from "./windows-coverage.mjs";

test("native workspace coverage bounds both complete partitions with Nextest", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/ci.yml", import.meta.url),
    "utf8",
  );
  const commands = `${workflow}\n${helperSource}`
    .split("\n")
    .map((line) =>
      line.trim().replace(/^run_phase (?:host|remaining|report) /u, ""),
    );
  const nextest = commands.filter((line) =>
    line.startsWith("cargo llvm-cov nextest "),
  );
  assert.equal(
    nextest.length,
    2,
    "the shared Host command and Linux-only instrumented remainder are explicit",
  );
  const flags =
    "--no-tests fail --no-fail-fast --retries 0 --user-config-file none --profile default --success-output immediate --test-threads 4";
  const host = `cargo llvm-cov nextest --no-report --locked -p mesh-talk ${flags}`;
  const rest = `cargo llvm-cov nextest --no-report --locked --workspace --exclude mesh-talk ${flags}`;
  assert.equal(nextest.filter((line) => line === host).length, 1);
  assert.equal(nextest.filter((line) => line === rest).length, 1);
  assert.equal(
    commands.filter(
      (line) =>
        line ===
        `cargo nextest run --locked --workspace --exclude mesh-talk ${flags}`,
    ).length,
    1,
  );
  assert.ok(
    !commands.some((line) =>
      line.startsWith(
        "cargo llvm-cov --no-report --locked --workspace --exclude mesh-talk",
      ),
    ),
    "raw Cargo cannot enforce Nextest case timeouts",
  );
  const config = readFileSync(
    new URL("../../.config/nextest.toml", import.meta.url),
    "utf8",
  );
  assert.match(
    config,
    /package\(=mesh-talk-core\) & test\(=node::node::tests::linked_device_catches_up_incoming_dm_after_reconnect\)/u,
  );
  assert.match(
    config,
    /slow-timeout = \{ period = "30s", terminate-after = 10, grace-period = "5s" \}/u,
  );
  assert.ok(config.includes(`nextest-version = "${versions.nextest}"`));
  assert.ok(commands.includes("run: cargo test --workspace --doc --locked"));
});

test("ordinary process hang policy preserves heavy-case margin and dedicated exceptions", () => {
  const config = readFileSync(
    new URL("../../.config/nextest.toml", import.meta.url),
    "utf8",
  );
  const section = config.split("[profile.default]\n")[1]?.split(/^\[/mu)[0];
  assert(section, "ordinary profile must explicitly bound stuck processes");
  const policy = section.match(
    /slow-timeout = \{ period = "(\d+)s", terminate-after = (\d+), grace-period = "5s" \}/u,
  );
  assert(policy, "ordinary process watchdog must fail and terminate");
  assert.equal(Number(policy[1]), 60);
  assert.equal(Number(policy[2]), 15);
  assert.equal(Number(policy[1]) * Number(policy[2]), 900);
  assert.match(config, /heavy.*400s/u);
  assert.match(
    config,
    /period = "15s", terminate-after = 4, grace-period = "5s"/u,
  );
  assert.match(
    config,
    /period = "30s", terminate-after = 10, grace-period = "5s"/u,
  );
  assert.match(
    config.split("[profile.mutants]\n")[1],
    /period = "60s", terminate-after = 1, grace-period = "5s"/u,
  );
  assert.doesNotMatch(config, /on-timeout\s*=\s*"pass"/u);
});

test("native Host isolation preserves default targets, accumulated coverage and real failure", (t) => {
  const root = mkdtempSync(join(tmpdir(), "mesh-native-host-coverage-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const workflow = readFileSync(
    new URL("../../.github/workflows/ci.yml", import.meta.url),
    "utf8",
  );
  assert.ok(
    workflow.includes(
      `cargo-llvm-cov@${versions.llvmCov},cargo-nextest@${versions.nextest}`,
    ),
  );
  const commands = `${workflow}\n${helperSource}`
    .split("\n")
    .map((line) =>
      line.trim().replace(/^run_phase (?:host|remaining|report) /u, ""),
    );
  const hostCommands = commands.filter(
    (line) =>
      line.startsWith("cargo llvm-cov nextest ") &&
      line.includes(" -p mesh-talk "),
  );
  assert.equal(hostCommands.length, 1);
  const host = hostCommands[0].split(/\s+/u).slice(1);
  assert.deepEqual(host, [
    "llvm-cov",
    "nextest",
    "--no-report",
    "--locked",
    "-p",
    "mesh-talk",
    "--no-tests",
    "fail",
    "--no-fail-fast",
    "--retries",
    "0",
    "--user-config-file",
    "none",
    "--profile",
    "default",
    "--success-output",
    "immediate",
    "--test-threads",
    "4",
  ]);
  const rest = [
    "llvm-cov",
    "nextest",
    "--no-report",
    "--locked",
    "--workspace",
    "--exclude",
    "mesh-talk",
    "--no-tests",
    "fail",
    "--no-fail-fast",
    "--retries",
    "0",
    "--user-config-file",
    "none",
    "--profile",
    "default",
    "--success-output",
    "immediate",
    "--test-threads",
    "4",
  ];
  assert.equal(
    commands.filter((line) => line === `cargo ${rest.join(" ")}`).length,
    1,
  );
  assert.equal(
    commands.filter(
      (line) => line === "cargo llvm-cov report --lcov --output-path lcov.info",
    ).length,
    1,
  );
  assert.match(workflow, /NEXTEST_TEST_THREADS: 4/u);
  const config = readFileSync(
    new URL("../../.config/nextest.toml", import.meta.url),
    "utf8",
  );
  assert.match(
    config,
    /test\(=owner_commands::tests::registered_channel_metadata_preserves_versions_and_owner_admission\)/u,
  );
  assert.match(
    config,
    /slow-timeout = \{ period = "15s", terminate-after = 4, grace-period = "5s" \}/u,
  );
  mkdirSync(join(root, ".config"));
  assert.match(
    config,
    /package\(=mesh-talk-core\) & test\(=node::node::tests::linked_device_catches_up_incoming_dm_after_reconnect\)/u,
  );
  assert.match(
    config,
    /slow-timeout = \{ period = "30s", terminate-after = 10, grace-period = "5s" \}/u,
  );
  writeFileSync(join(root, ".config/nextest.toml"), config);
  writeFileSync(
    join(root, "Cargo.toml"),
    '[workspace]\nmembers=["host","core"]\nresolver="2"\n',
  );
  for (const [directory, name] of [
    ["host", "mesh-talk"],
    ["core", "mesh-talk-core"],
  ]) {
    for (const path of ["src", "tests", "examples"])
      mkdirSync(join(root, directory, path), { recursive: true });
    writeFileSync(
      join(root, directory, "Cargo.toml"),
      `[package]\nname="${name}"\nversion="0.1.0"\nedition="2024"\n\n[[example]]\nname="default"\ntest=true\n`,
    );
    const crate = name.replaceAll("-", "_");
    writeFileSync(
      join(root, directory, "src/lib.rs"),
      `#[inline(never)] pub fn ${directory}_unit() -> u8 { 1 }\n#[inline(never)] pub fn ${directory}_example() -> u8 { 4 }\n#[inline(never)] pub fn ${directory}_integration() -> u8 { ${directory === "core" ? 'if let Some(path) = std::env::var_os("MESH_AFTER_TIMEOUT_MARKER") { std::fs::write(path, "continued after timeout").unwrap(); }' : ""} 2 }\n#[test] fn unit() { assert_eq!(${directory}_unit(), 1); ${directory === "host" ? 'assert!(std::env::var_os("MESH_FAIL_HOST").is_none(), "host failure control");' : 'if let Some(path) = std::env::var_os("MESH_REST_MARKER") { std::fs::write(path, "rest execution").unwrap(); } assert!(std::env::var_os("MESH_FAIL_CORE").is_none(), "core failure control"); if let Some(path) = std::env::var_os("MESH_PARK_CORE") { std::fs::write(path, "park entered").unwrap(); loop { std::thread::park(); } }'} }\n#[test] #[ignore] fn default_ignored() { panic!("must remain ignored"); }\n`,
    );
    writeFileSync(
      join(root, directory, "tests/default.rs"),
      `#[test] fn integration() { assert_eq!(${crate}::${directory}_integration(), 2); }\n`,
    );
    writeFileSync(
      join(root, directory, "src/main.rs"),
      `#[inline(never)] fn ${directory}_binary() -> u8 { 3 }\nfn main() {}\n#[test] fn binary() { assert_eq!(${directory}_binary(), 3); }\n`,
    );
    writeFileSync(
      join(root, directory, "examples/default.rs"),
      `fn main() {}\n#[test] fn example() { assert_eq!(${crate}::${directory}_example(), 4); }\n`,
    );
  }
  const env = {
    ...process.env,
    CARGO_TARGET_DIR: join(root, "target"),
    CARGO_LLVM_COV_TARGET_DIR: join(root, "coverage-target"),
    CARGO_TERM_COLOR: "never",
  };
  delete env.LLVM_PROFILE_FILE;
  const run = (args, extra = {}, expected = 0) => {
    const result = spawnSync("cargo", args, {
      cwd: root,
      env: { ...env, ...extra },
      encoding: "utf8",
      timeout: 120_000,
      maxBuffer: 8 * 1024 * 1024,
    });
    assert.equal(
      result.status,
      expected,
      `${args.join(" ")}\n${result.error ?? ""}\n${result.stdout}\n${result.stderr}`,
    );
    return result.stdout + result.stderr;
  };
  run(["generate-lockfile", "--offline"]);
  const hostOutput = run(host);
  assert.match(hostOutput, /4 tests run: 4 passed, 1 skipped/u);
  const restOutput = run(rest);
  assert.match(restOutput, /4 tests run: 4 passed, 1 skipped/u);
  run(["llvm-cov", "report", "--lcov", "--output-path", "lcov.info"]);
  // llvm-cov intentionally excludes examples/ and tests/ from its default report.
  // Each example therefore calls its own library sentinel, so a real hit proves
  // execution without changing the production coverage denominator.
  const coverage = readFileSync(join(root, "lcov.info"), "utf8");
  for (const directory of ["host", "core"])
    for (const target of ["unit", "integration", "binary", "example"]) {
      assert.match(
        coverage,
        new RegExp(`^FNDA:[1-9]\\d*,[^\\n]*${directory}_${target}`, "m"),
        `${directory} ${target} coverage must accumulate`,
      );
    }
  assert.match(
    workflow,
    /bash \.\.\/scripts\/ci\/workspace-coverage\.sh macos/u,
  );
  assert.match(
    workflow,
    /bash \.\.\/scripts\/ci\/workspace-coverage\.sh linux/u,
  );
  const blockMac = "macos";
  // Execute the actual macOS block in a clean coverage cohort. Core's real
  // default targets must run, but must not require another instrumented build.
  run(["llvm-cov", "clean", "--workspace"]);
  const macMarker = join(root, "mac-core-executed");
  const macPass = spawnSync("bash", [helper, blockMac], {
    cwd: root,
    env: {
      ...env,
      MESH_REST_MARKER: macMarker,
      MESH_TALK_TEST_TIMING: "0",
      RUNNER_TEMP: root,
    },
    encoding: "utf8",
    timeout: 120_000,
  });
  assert.equal(macPass.status, 0, macPass.stdout + macPass.stderr);
  assert.equal(
    (macPass.stdout + macPass.stderr).match(
      /4 tests run: 4 passed, 1 skipped/gu,
    )?.length,
    2,
  );
  assert.equal(readFileSync(macMarker, "utf8"), "rest execution");
  const macCoverage = readFileSync(join(root, "lcov.info"), "utf8");
  assert.match(macCoverage, /^FNDA:[1-9]\d*,[^\n]*host_unit/mu);
  assert.doesNotMatch(
    macCoverage,
    /^FNDA:[1-9]\d*,[^\n]*core_/mu,
    "macOS must execute Core normally while Linux/Windows own its full instrumented coverage",
  );
  const linuxBlock = "linux";
  for (const block of [blockMac, linuxBlock]) {
    rmSync(join(root, "lcov.info"), { recursive: true, force: true });
    writeFileSync(join(root, "lcov.info"), "previous report");
    for (const failure of ["HOST", "CORE", "BOTH"]) {
      for (const timing of ["0", "1"]) {
        rmSync(join(root, "lcov.info"));
        const marker = join(
          root,
          `rest-${block === blockMac ? "mac" : "linux"}-${failure}-${timing}`,
        );
        const failedPartition = spawnSync("bash", [helper, block], {
          cwd: root,
          env: {
            ...env,
            ...(failure === "BOTH"
              ? { MESH_FAIL_HOST: "1", MESH_FAIL_CORE: "1" }
              : { [`MESH_FAIL_${failure}`]: "1" }),
            MESH_REST_MARKER: marker,
            MESH_TALK_TEST_TIMING: timing,
            RUNNER_TEMP: root,
          },
          encoding: "utf8",
          timeout: 120_000,
        });
        assert.equal(
          failedPartition.status,
          100,
          `${failedPartition.error ?? ""}\n${failedPartition.stdout}\n${failedPartition.stderr}`,
        );
        const diagnostic = failedPartition.stdout + failedPartition.stderr;
        if (process.platform === "darwin" && block === blockMac) {
          assert.match(diagnostic, /native-resource phase=host/u);
          assert.match(diagnostic, /maximum resident set size/u);
          assert.match(diagnostic, /hw\.memsize:/u);
          assert.match(diagnostic, /vm\.swapusage:/u);
          if (failure === "CORE")
            assert.match(diagnostic, /native-resource phase=remaining/u);
          else
            assert.doesNotMatch(diagnostic, /native-resource phase=remaining/u);
        } else {
          assert.doesNotMatch(diagnostic, /native-resource phase=/u);
        }
        assert.match(
          failedPartition.stdout + failedPartition.stderr,
          new RegExp(
            `${failure === "BOTH" ? "host" : failure.toLowerCase()} failure control`,
            "u",
          ),
        );
        assert.equal(
          existsSync(marker),
          failure === "CORE",
          "only a passing Host may admit the remaining workspace partition",
        );
        if (failure === "BOTH")
          assert.doesNotMatch(
            failedPartition.stdout + failedPartition.stderr,
            /core failure control/u,
            "a failed Host must stop before starting another compilation/test partition",
          );
        assert.equal(
          existsSync(join(root, "lcov.info")),
          false,
          "either failed partition must stop before report",
        );
        if (timing === "1")
          assert.match(
            readFileSync(join(root, "mesh-talk-test-timing.log"), "utf8"),
            new RegExp(
              `${failure === "BOTH" ? "host" : failure.toLowerCase()} failure control`,
              "u",
            ),
          );
        // Restore the report only for the next independent failure-control iteration.
        writeFileSync(join(root, "lcov.info"), "previous report");
      }
    }
    for (const timing of ["0", "1"]) {
      rmSync(join(root, "lcov.info"), { recursive: true, force: true });
      // A real report destination failure must fail even after both suites pass.
      mkdirSync(join(root, "lcov.info"));
      const reportFailure = spawnSync("bash", [helper, block], {
        cwd: root,
        env: { ...env, MESH_TALK_TEST_TIMING: timing, RUNNER_TEMP: root },
        encoding: "utf8",
        timeout: 120_000,
      });
      assert.equal(reportFailure.error, undefined);
      assert.notEqual(
        reportFailure.status,
        0,
        reportFailure.stdout + reportFailure.stderr,
      );
      assert.equal(
        (reportFailure.stdout + reportFailure.stderr).match(
          /4 tests run: 4 passed, 1 skipped/gu,
        )?.length,
        2,
        "report failure must remain nonzero after both complete partitions pass",
      );
    }
  }
  rmSync(join(root, "lcov.info"), { recursive: true, force: true });
  const emptyHost = run([...host, "-E", "test(=absent_case)"], {}, 4);
  assert.match(emptyHost, /no tests/u);
  const emptyRest = run([...rest, "-E", "test(=absent_case)"], {}, 4);
  assert.match(emptyRest, /no tests/u);

  // Parse the actual source default, accelerating ONLY its period in this
  // isolated fixture. Keep all fifteen periods and the real grace/exception
  // policy; a one-period hand-written override would not prove the default.
  const defaultTimeout =
    'slow-timeout = { period = "60s", terminate-after = 15, grace-period = "5s" }';
  assert.equal(config.split(defaultTimeout).length, 2);
  const accelerated = config.replace(
    defaultTimeout,
    defaultTimeout.replace('period = "60s"', 'period = "100ms"'),
  );
  assert.match(
    accelerated,
    /period = "100ms", terminate-after = 15, grace-period = "5s"/u,
  );
  writeFileSync(
    join(root, ".config/nextest.toml"),
    `${accelerated}\n[[profile.default.overrides]]\nfilter = 'package(=mesh-talk-core) & test(=unit)'\npriority = 100\n`,
  );
  const parkEntered = join(root, "park-entered");
  const afterTimeout = join(root, "after-timeout");
  const serialRest = [...rest];
  serialRest[serialRest.indexOf("--test-threads") + 1] = "1";
  const timedOut = run(
    serialRest,
    {
      MESH_PARK_CORE: parkEntered,
      MESH_AFTER_TIMEOUT_MARKER: afterTimeout,
    },
    100,
  );
  assert.match(timedOut, /TIMEOUT|timed out/iu);
  assert.equal(readFileSync(parkEntered, "utf8"), "park entered");
  assert.equal(readFileSync(afterTimeout, "utf8"), "continued after timeout");
});
