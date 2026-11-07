// Requires the pinned nextest/llvm-cov tools and matching llvm-tools-preview.
// Runs outside the portable Health glob; every Git commit is inside this temp fixture.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
  copyFileSync,
  readdirSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

test(
  "real pinned archive CLI preserves serial shards, coverage, Cargo runtime and clean source",
  { timeout: 120000 },
  (t) => {
    const temporary = mkdtempSync(path.join(tmpdir(), "mesh-windows-archive-"));
    t.after(() => rmSync(temporary, { recursive: true, force: true }));
    const root = path.join(temporary, "source");
    for (const directory of [
      "src",
      "tests",
      "examples",
      ".cargo",
      ".config",
      "empty-hooks",
    ])
      mkdirSync(path.join(root, directory), { recursive: true });
    const write = (file, text) => writeFileSync(path.join(root, file), text);
    write(".gitignore", "/target/\n");
    write(".cargo/config.toml", "[build]\njobs = 1\n");
    write(
      ".config/nextest.toml",
      "[profile.default]\ntest-threads = 1\nretries = 0\nfail-fast = false\n",
    );
    write(
      "Cargo.toml",
      '[package]\nname="archive-fixture"\nversion="0.0.0"\nedition="2024"\n[workspace]\n[[example]]\nname="default-example"\ntest=true\n',
    );
    write(
      "src/lib.rs",
      `
/// \`\`\`
/// assert_eq!(archive_fixture::branch(true), 7);
/// \`\`\`
pub fn branch(first: bool) -> u8 { assert!(std::env::var_os("MESH_ARCHIVE_FAIL_CASE").is_none(), "intentional archive failure"); if first { 7 } else { 9 } }
#[cfg(test)] mod tests {
  #[test] fn first() { println!("PHASE {{\\\"successful\\\":true}}"); assert_eq!(crate::branch(true), 7); }
  #[test] fn second() { assert_eq!(crate::branch(false), 9); }
  #[test] #[ignore] fn ignored() { panic!("must stay ignored"); }
}
`,
    );
    const shardCases = Array.from(
      { length: 64 },
      (_, index) =>
        `#[test] fn partition_case_${index}() { assert_eq!(crate::branch(${index % 2 === 0}), ${index % 2 === 0 ? 7 : 9}); }`,
    ).join("\n");
    write(
      "src/lib.rs",
      readFileSync(path.join(root, "src/lib.rs"), "utf8") +
        `\n#[cfg(test)] mod partition_cases { ${shardCases} }\n`,
    );
    write(
      "examples/default-example.rs",
      "fn main() {}\n#[test] fn example_body() { assert_eq!(archive_fixture::branch(true), 7); }\n",
    );
    write("src/main.rs", 'fn main() { println!("helper"); }\n');
    write(
      "tests/runtime.rs",
      `
#[test] fn cargo_runtime_contract() {
 assert_eq!(std::env::current_dir().unwrap(), std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()));
 for binary in [std::env::var("CARGO_BIN_EXE_archive-fixture").unwrap(), env!("CARGO_BIN_EXE_archive-fixture").to_owned()] {
   assert!(std::process::Command::new(binary).output().unwrap().status.success());
 }
}
`,
    );
    copyFileSync(
      fileURLToPath(new URL("./windows-coverage.mjs", import.meta.url)),
      path.join(root, "windows-coverage.mjs"),
    );
    const baseEnv = {
      ...process.env,
      CARGO_NET_OFFLINE: "true",
      RUST_TEST_THREADS: "1",
      CARGO_TERM_COLOR: "never",
      CARGO_PROFILE_DEV_DEBUG: "line-tables-only",
      CARGO_PROFILE_TEST_DEBUG: "line-tables-only",
    };
    // Fixture compilation must not inherit an outer workspace's instrumentation paths.
    for (const name of Object.keys(baseEnv))
      if (
        name.startsWith("CARGO_LLVM_COV") ||
        name.startsWith("__CARGO_LLVM_COV") ||
        [
          "RUSTC_WRAPPER",
          "RUSTC_WORKSPACE_WRAPPER",
          "RUSTFLAGS",
          "CARGO_ENCODED_RUSTFLAGS",
          "CARGO_TARGET_DIR",
          "LLVM_PROFILE_FILE",
          "CARGO_BUILD_TARGET",
        ].includes(name)
      )
        delete baseEnv[name];
    delete baseEnv.MESH_WINDOWS_FAIL_FAST;
    let latestStderr = "";
    const run = (binary, args, env = baseEnv, expectedStatus = 0) => {
      const result = spawnSync(binary, args, {
        cwd: root,
        encoding: "utf8",
        env,
        timeout: 60000,
        maxBuffer: 16 * 1024 * 1024,
      });
      latestStderr = result.stderr ?? "";
      assert.equal(
        result.status,
        expectedStatus,
        `${binary} ${args.join(" ")}\n${result.stdout}\n${result.stderr}\n${result.error ?? ""}`,
      );
      return result.stdout.trim();
    };
    run("cargo", ["generate-lockfile", "--offline"]);
    run("cargo", ["test", "--doc", "--offline"]);
    run("git", ["init", "--quiet"]);
    run("git", ["add", "."]);
    run("git", [
      "-c",
      "user.name=CI Fixture",
      "-c",
      "user.email=fixture@example.invalid",
      "-c",
      "core.hooksPath=empty-hooks",
      "-c",
      "commit.gpgsign=false",
      "commit",
      "--quiet",
      "-m",
      "test: isolated archive fixture",
    ]);
    const env = {
      ...baseEnv,
      GITHUB_SHA: run("git", ["rev-parse", "HEAD"]),
      GITHUB_RUN_ID: "1",
      GITHUB_RUN_ATTEMPT: "1",
      MESH_TALK_TEST_TIMING: "1",
      MESH_ARCHIVE_NODE: process.execPath.replaceAll("\\", "/"),
    };
    const artifact = path.join(temporary, "archive");
    const phase = (
      args,
      environment,
      expectedStatus = 0,
      expectedFailure = /Consumer identity differs from producer/,
    ) => {
      // Script paths and argv stay in environment variables; no shell interpolation.
      const scoped = { ...environment };
      args.forEach((arg, index) => {
        scoped[`MESH_ARCHIVE_ARG_${index}`] = arg;
      });
      const script =
        'eval "$(cargo llvm-cov show-env --sh)"\n"$MESH_ARCHIVE_NODE" windows-coverage.mjs ' +
        args.map((_, index) => `"$MESH_ARCHIVE_ARG_${index}"`).join(" ");
      run("bash", ["-euc", script], scoped, expectedStatus);
      if (expectedStatus !== 0) {
        assert.match(latestStderr, expectedFailure);
        return;
      }
      if (args[0] === "consume")
        assert.doesNotMatch(
          latestStderr,
          /^\s*(?:Compiling|Checking) /mu,
          "consumer compiled instead of reusing archive",
        );
      assert.equal(
        run("git", ["status", "--porcelain", "--untracked-files=normal"]),
        "",
        "instrumented listing dirtied source",
      );
      assert.ok(
        !readdirSync(root).some((name) => /^default.*\.profraw$/u.test(name)),
      );
    };
    phase(["produce", artifact], env);
    renameSync(
      path.join(root, "target"),
      path.join(temporary, "producer-target"),
    );
    phase(
      ["consume", "1", artifact, path.join(temporary, "rejected-profile")],
      { ...env, CARGO_PROFILE_TEST_DEBUG: "full" },
      1,
    );
    for (const failFast of ["0", "1"]) {
      const output = path.join(temporary, `failed-results-${failFast}`);
      phase(
        ["consume", "1", artifact, output],
        {
          ...env,
          MESH_ARCHIVE_FAIL_CASE: "1",
          MESH_WINDOWS_FAIL_FAST: failFast,
        },
        1,
        /failed: 100/u,
      );
      const events = readFileSync(path.join(output, "execution.jsonl"), "utf8")
        .trim()
        .split("\n")
        .map(JSON.parse);
      const observed = JSON.parse(
        readFileSync(path.join(output, "expected-inventory.json"), "utf8"),
      );
      const expected = observed.filter((row) => row.selected);
      const selected = new Set(expected.map((row) => row.eventName));
      const isSelectedStart = (event) =>
        event.type === "test" &&
        event.event === "started" &&
        selected.has(event.name);
      const starts = events.filter(isSelectedStart);
      // Nextest also reports a started/ignored pair for an ignored inventory row.
      // Such a row is not runnable; all other starts must belong to this partition.
      for (const event of events.filter(
        (event) => event.type === "test" && event.event === "started",
      )) {
        const row = observed.find((row) => row.eventName === event.name);
        assert(
          row && (row.selected || row.ignored),
          "unexpected test started outside the selected partition",
        );
      }
      assert(
        events.some(
          (event) =>
            event.type === "test" &&
            event.event === "failed" &&
            selected.has(event.name),
        ),
      );
      if (failFast === "0") {
        assert.deepEqual(
          starts.map((event) => event.name).sort(),
          [...selected].sort(),
          "actual CI must collect every selected case exactly once after failure",
        );
        const firstFailure = events.findIndex(
          (event) =>
            event.type === "test" &&
            event.event === "failed" &&
            selected.has(event.name),
        );
        assert(
          events.slice(firstFailure + 1).some(isSelectedStart),
          "a later selected case must actually execute after the first failure",
        );
        const completed = events.filter(
          (event) =>
            event.type === "test" &&
            ["ok", "failed"].includes(event.event) &&
            selected.has(event.name),
        );
        assert.deepEqual(
          completed.map((event) => event.name).sort(),
          [...selected].sort(),
          "every selected case must reach a terminal result even when the receipt rejects failure",
        );
      } else {
        assert.equal(
          new Set(starts.map((event) => event.name)).size,
          starts.length,
          "selected cases cannot restart after failure",
        );
        assert(
          starts.length < expected.length,
          "explicit fail-fast diagnostics must stop after their first failure",
        );
        const firstFailure = events.findIndex(
          (event) =>
            event.type === "test" &&
            event.event === "failed" &&
            selected.has(event.name),
        );
        assert(
          !events.slice(firstFailure + 1).some(isSelectedStart),
          "no later runnable case may start after the first failed case",
        );
      }
      assert(
        !readdirSync(output).includes("receipt.json"),
        "failed or incomplete execution cannot publish acceptance",
      );
      renameSync(
        path.join(root, "target"),
        path.join(temporary, `failed-consumer-${failFast}-target`),
      );
    }
    const freshCargoHome = path.join(temporary, "fresh-cargo-home");
    mkdirSync(freshCargoHome);
    const receipts = [],
      logs = [];
    for (const shard of [1, 2, 3, 4, 5, 6, 7, 8]) {
      const output = path.join(temporary, `results-${shard}`);
      phase(["consume", String(shard), artifact, output], {
        ...env,
        CARGO_HOME: freshCargoHome,
        MESH_WINDOWS_FAIL_FAST: "0",
      });
      receipts.push(
        JSON.parse(readFileSync(path.join(output, "receipt.json"), "utf8")),
      );
      logs.push(readFileSync(path.join(output, "execution.log"), "utf8"));
      renameSync(
        path.join(root, "target"),
        path.join(temporary, `consumer-${shard}-target`),
      );
    }
    assert.equal(
      receipts.reduce((total, receipt) => total + receipt.executedTests, 0),
      68,
    );
    assert.ok(
      receipts.every(
        (receipt) =>
          receipt.totalTests === 69 && receipt.coverage.coveredLines > 0,
      ),
    );
    assert.ok(
      receipts.every(
        (receipt) => receipt.archiveSha256 === receipts[0].archiveSha256,
      ),
    );
    assert.ok(
      logs.some((log) => log.includes('PHASE {"successful":true}')),
      "successful timing payload was lost",
    );
  },
);
