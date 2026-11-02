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

test("native Host isolation preserves default targets, accumulated coverage and real failure", (t) => {
  const root = mkdtempSync(join(tmpdir(), "mesh-native-host-coverage-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const workflow = readFileSync(
    new URL("../../.github/workflows/ci.yml", import.meta.url),
    "utf8",
  );
  assert.match(workflow, /cargo-llvm-cov@0\.8\.7,cargo-nextest@0\.9\.148/u);
  const commands = workflow.split("\n").map((line) => line.trim());
  const hostCommands = commands.filter((line) =>
    line.startsWith("cargo llvm-cov nextest "),
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
  ]);
  const rest = [
    "llvm-cov",
    "--no-report",
    "--locked",
    "--workspace",
    "--exclude",
    "mesh-talk",
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
  writeFileSync(join(root, ".config/nextest.toml"), config);
  writeFileSync(
    join(root, "Cargo.toml"),
    '[workspace]\nmembers=["host","core"]\nresolver="2"\n',
  );
  for (const [directory, name] of [
    ["host", "mesh-talk"],
    ["core", "mesh-talk-core"],
  ]) {
    for (const path of ["src", "tests"])
      mkdirSync(join(root, directory, path), { recursive: true });
    writeFileSync(
      join(root, directory, "Cargo.toml"),
      `[package]\nname="${name}"\nversion="0.1.0"\nedition="2024"\n`,
    );
    const crate = name.replaceAll("-", "_");
    writeFileSync(
      join(root, directory, "src/lib.rs"),
      `#[inline(never)] pub fn ${directory}_unit() -> u8 { 1 }\n#[inline(never)] pub fn ${directory}_integration() -> u8 { 2 }\n#[test] fn unit() { assert_eq!(${directory}_unit(), 1); ${directory === "host" ? 'assert!(std::env::var_os("MESH_FAIL_HOST").is_none(), "host failure control");' : 'if let Some(path) = std::env::var_os("MESH_REST_MARKER") { std::fs::write(path, "unexpected rest execution").unwrap(); }'} }\n#[test] #[ignore] fn default_ignored() { panic!("must remain ignored"); }\n`,
    );
    writeFileSync(
      join(root, directory, "tests/default.rs"),
      `#[test] fn integration() { assert_eq!(${crate}::${directory}_integration(), 2); }\n`,
    );
    writeFileSync(
      join(root, directory, "src/main.rs"),
      `#[inline(never)] fn ${directory}_binary() -> u8 { 3 }\nfn main() {}\n#[test] fn binary() { assert_eq!(${directory}_binary(), 3); }\n`,
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
  assert.match(hostOutput, /3 tests run: 3 passed, 1 skipped/u);
  const restOutput = run(rest);
  assert.equal(
    (restOutput.match(/test result: ok\. 1 passed; 0 failed;/gu) ?? []).length,
    3,
  );
  assert.match(restOutput, /1 passed; 0 failed; 1 ignored/u);
  run(["llvm-cov", "report", "--lcov", "--output-path", "lcov.info"]);
  const coverage = readFileSync(join(root, "lcov.info"), "utf8");
  for (const directory of ["host", "core"])
    for (const target of ["unit", "integration", "binary"]) {
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
  for (const timing of ["0", "1"]) {
    rmSync(join(root, "lcov.info"));
    const marker = join(root, `unexpected-rest-${timing}`);
    const failedHost = spawnSync(
      "bash",
      ["-e", "-o", "pipefail", "-c", block],
      {
        cwd: root,
        env: {
          ...env,
          MESH_FAIL_HOST: "1",
          MESH_REST_MARKER: marker,
          MESH_TALK_TEST_TIMING: timing,
          RUNNER_TEMP: root,
        },
        encoding: "utf8",
        timeout: 120_000,
      },
    );
    assert.equal(
      failedHost.status,
      100,
      `${failedHost.error ?? ""}\n${failedHost.stdout}\n${failedHost.stderr}`,
    );
    assert.match(
      failedHost.stdout + failedHost.stderr,
      /host failure control/u,
    );
    assert.equal(
      existsSync(marker),
      false,
      "failed Host must stop before the rest partition",
    );
    assert.equal(
      existsSync(join(root, "lcov.info")),
      false,
      "failed Host must stop before report",
    );
    if (timing === "1")
      assert.match(
        readFileSync(join(root, "mesh-talk-test-timing.log"), "utf8"),
        /host failure control/u,
      );
    // Restore the report only for the next independent failure-control iteration.
    writeFileSync(join(root, "lcov.info"), "previous report");
  }
  const emptyHost = run([...host, "-E", "test(=absent_case)"], {}, 4);
  assert.match(emptyHost, /no tests/u);
});
