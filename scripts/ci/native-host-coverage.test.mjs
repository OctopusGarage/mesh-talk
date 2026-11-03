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
import test from "node:test";
import { versions } from "./windows-coverage.mjs";

test("native workspace coverage bounds both complete partitions with Nextest", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/ci.yml", import.meta.url),
    "utf8",
  );
  const commands = workflow.split("\n").map((line) => line.trim());
  const nextest = commands.filter((line) =>
    line.startsWith("cargo llvm-cov nextest "),
  );
  assert.equal(
    nextest.length,
    4,
    "both OS steps must bound Host and remaining workspace tests",
  );
  const flags =
    "--no-tests fail --no-fail-fast --retries 0 --user-config-file none --profile default --success-output immediate --test-threads 4";
  const host = `cargo llvm-cov nextest --no-report --locked -p mesh-talk ${flags}`;
  const rest = `cargo llvm-cov nextest --no-report --locked --workspace --exclude mesh-talk ${flags}`;
  assert.equal(nextest.filter((line) => line === host).length, 2);
  assert.equal(nextest.filter((line) => line === rest).length, 2);
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
  const commands = workflow.split("\n").map((line) => line.trim());
  const hostCommands = commands.filter(
    (line) =>
      line.startsWith("cargo llvm-cov nextest ") &&
      line.includes(" -p mesh-talk "),
  );
  assert.equal(hostCommands.length, 2);
  assert.equal(hostCommands[0], hostCommands[1]);
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
    2,
  );
  assert.equal(
    commands.filter(
      (line) => line === "cargo llvm-cov report --lcov --output-path lcov.info",
    ).length,
    2,
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
      `#[inline(never)] pub fn ${directory}_unit() -> u8 { 1 }\n#[inline(never)] pub fn ${directory}_integration() -> u8 { ${directory === "core" ? 'if let Some(path) = std::env::var_os("MESH_AFTER_TIMEOUT_MARKER") { std::fs::write(path, "continued after timeout").unwrap(); }' : ""} 2 }\n#[test] fn unit() { assert_eq!(${directory}_unit(), 1); ${directory === "host" ? 'assert!(std::env::var_os("MESH_FAIL_HOST").is_none(), "host failure control");' : 'if let Some(path) = std::env::var_os("MESH_REST_MARKER") { std::fs::write(path, "rest execution").unwrap(); } assert!(std::env::var_os("MESH_FAIL_CORE").is_none(), "core failure control"); if let Some(path) = std::env::var_os("MESH_PARK_CORE") { std::fs::write(path, "park entered").unwrap(); loop { std::thread::park(); } }'} }\n#[test] #[ignore] fn default_ignored() { panic!("must remain ignored"); }\n`,
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
      `#[inline(never)] fn ${directory}_example() -> u8 { 4 }\nfn main() {}\n#[test] fn example() { assert_eq!(${directory}_example(), 4); }\n`,
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
  const coverage = readFileSync(join(root, "lcov.info"), "utf8");
  for (const directory of ["host", "core"])
    for (const target of ["unit", "integration", "binary", "example"]) {
      assert.match(
        coverage,
        new RegExp(`^FNDA:[1-9]\\d*,[^\\n]*${directory}_${target}`, "m"),
        `${directory} ${target} coverage must accumulate`,
      );
    }
  const step = workflow
    .split(/^      - /m)
    .find((value) => value.startsWith("name: Test + coverage (macOS)"));
  const block = step
    .split("        run: |\n")[1]
    .split("          # llvm-cov runs")[0]
    .replace(/^          /gm, "");
  for (const failure of ["HOST", "CORE"]) {
    for (const timing of ["0", "1"]) {
      rmSync(join(root, "lcov.info"));
      const marker = join(root, `rest-${failure}-${timing}`);
      const failedPartition = spawnSync(
        "bash",
        ["-e", "-o", "pipefail", "-c", block],
        {
          cwd: root,
          env: {
            ...env,
            [`MESH_FAIL_${failure}`]: "1",
            MESH_REST_MARKER: marker,
            MESH_TALK_TEST_TIMING: timing,
            RUNNER_TEMP: root,
          },
          encoding: "utf8",
          timeout: 120_000,
        },
      );
      assert.equal(
        failedPartition.status,
        100,
        `${failedPartition.error ?? ""}\n${failedPartition.stdout}\n${failedPartition.stderr}`,
      );
      assert.match(
        failedPartition.stdout + failedPartition.stderr,
        new RegExp(`${failure.toLowerCase()} failure control`, "u"),
      );
      assert.equal(
        existsSync(marker),
        failure === "CORE",
        "only a successful Host may admit the rest partition",
      );
      assert.equal(
        existsSync(join(root, "lcov.info")),
        false,
        "either failed partition must stop before report",
      );
      if (timing === "1")
        assert.match(
          readFileSync(join(root, "mesh-talk-test-timing.log"), "utf8"),
          new RegExp(`${failure.toLowerCase()} failure control`, "u"),
        );
      // Restore the report only for the next independent failure-control iteration.
      writeFileSync(join(root, "lcov.info"), "previous report");
    }
  }
  const emptyHost = run([...host, "-E", "test(=absent_case)"], {}, 4);
  assert.match(emptyHost, /no tests/u);
  const emptyRest = run([...rest, "-E", "test(=absent_case)"], {}, 4);
  assert.match(emptyRest, /no tests/u);

  // Use an isolated short fault backstop, never change the production 300s
  // reconnect policy. Real Nextest must kill the parked test and keep collecting.
  writeFileSync(
    join(root, ".config/nextest.toml"),
    `${config}\n[[profile.default.overrides]]\nfilter = 'package(=mesh-talk-core) & test(=unit)'\nslow-timeout = { period = "1s", terminate-after = 1, grace-period = "100ms" }\npriority = 100\n`,
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
