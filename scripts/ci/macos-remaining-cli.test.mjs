// Native pinned-tool qualification; intentionally outside portable diagnostics.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  cpSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { collectMacRemaining } from "./windows-coverage.mjs";

const runner = fileURLToPath(
  new URL("./windows-coverage.mjs", import.meta.url),
);
const hash = (text) => createHash("sha256").update(text).digest("hex");
test(
  "real macOS archive excludes Host and executes complete offline remaining targets at the original root",
  { timeout: 120000 },
  async (t) => {
    assert.equal(
      process.platform,
      "darwin",
      "Run this qualification in the native macOS producer",
    );
    const temporary = realpathSync(
      mkdtempSync(path.join(tmpdir(), "mesh-mac-archive-")),
    );
    t.after(() => rmSync(temporary, { recursive: true, force: true }));
    const root = path.join(temporary, "source");
    const write = (file, text) => writeFileSync(path.join(root, file), text);
    for (const directory of [
      ".cargo",
      ".config",
      "empty-hooks",
      "host/src",
      "core/src",
      "core/tests",
      "core/examples",
      "extra/src",
    ])
      mkdirSync(path.join(root, directory), { recursive: true });
    write(".gitignore", "/target/\n");
    write(
      ".cargo/config.toml",
      '[build]\njobs=4\n[env]\nRUST_TEST_THREADS="4"\n',
    );
    write(
      ".config/nextest.toml",
      'nextest-version="0.9.150"\n[profile.default]\nslow-timeout={period="60s",terminate-after=15,grace-period="5s"}\n',
    );
    write(
      "Cargo.toml",
      '[workspace]\nmembers=["host","core","extra"]\nresolver="2"\n',
    );
    write(
      "host/Cargo.toml",
      '[package]\nname="mesh-talk"\nversion="0.0.0"\nedition="2024"\nbuild="build.rs"\n',
    );
    write(
      "host/src/lib.rs",
      '#[test] fn host_must_not_run() { panic!("Host selected"); }\n',
    );
    write(
      "host/build.rs",
      'fn main() { panic!("Host must not compile or need frontend"); }\n',
    );
    write(
      "core/Cargo.toml",
      '[package]\nname="remaining-core"\nversion="0.0.0"\nedition="2024"\n[[example]]\nname="default-example"\ntest=true\n[features]\ndefault=["enabled"]\nenabled=[]\n[[test]]\nname="enabled"\nrequired-features=["enabled"]\n[[test]]\nname="disabled"\nrequired-features=["disabled"]\n',
    );
    const body =
      'assert!(cfg!(debug_assertions)); assert!(std::panic::catch_unwind(|| { std::hint::black_box(u8::MAX) + std::hint::black_box(1) }).is_err()); assert_eq!(std::env::var("RUST_TEST_THREADS").unwrap(), "4"); assert!(std::env::var_os("LLVM_PROFILE_FILE").is_none()); assert!(std::env::var_os("MESH_MAC_FAIL").is_none(), "intentional remaining failure");';
    write(
      "core/src/lib.rs",
      `${Array.from({ length: 16 }, (_, index) => `#[test] fn case_${index}() { ${body} }`).join("\n")}\n#[test] #[ignore] fn ignored() { panic!("must stay ignored"); }\n`,
    );
    write(
      "core/src/main.rs",
      `fn main() { println!("embedded helper"); }\n#[test] fn binary() { ${body} }\n`,
    );
    write(
      "core/tests/enabled.rs",
      `#[test] fn embedded_cli() { ${body} for binary in [env!("CARGO_BIN_EXE_remaining-core").to_owned(), std::env::var("CARGO_BIN_EXE_remaining-core").unwrap()] { assert!(std::process::Command::new(binary).output().unwrap().status.success()); } }\n`,
    );
    write(
      "core/tests/disabled.rs",
      'compile_error!("disabled target must not compile");\n',
    );
    write(
      "core/examples/default-example.rs",
      `fn main() {}\n#[test] fn example() { ${body} }\n`,
    );
    write(
      "extra/Cargo.toml",
      '[package]\nname="new-remaining"\nversion="0.0.0"\nedition="2024"\n',
    );
    write(
      "extra/src/lib.rs",
      `#[test] fn new_workspace_package() { ${body} }\n`,
    );
    copyFileSync(runner, path.join(root, "windows-coverage.mjs"));
    const env = {
      ...process.env,
      CARGO_NET_OFFLINE: "true",
      CARGO_TERM_COLOR: "never",
      CARGO_BUILD_JOBS: "2",
      CARGO_INCREMENTAL: "0",
      CARGO_PROFILE_DEV_DEBUG: "line-tables-only",
      CARGO_PROFILE_TEST_DEBUG: "line-tables-only",
      CARGO_TARGET_DIR: path.join(root, "target"),
      RUST_TEST_THREADS: "4",
    };
    for (const key of Object.keys(env))
      if (
        /^(?:__)?CARGO_LLVM_COV/u.test(key) ||
        [
          "LLVM_PROFILE_FILE",
          "RUSTFLAGS",
          "CARGO_ENCODED_RUSTFLAGS",
          "RUSTC",
          "RUSTC_WRAPPER",
          "RUSTC_WORKSPACE_WRAPPER",
          "CARGO_BUILD_TARGET",
        ].includes(key)
      )
        delete env[key];
    const run = (binary, args, environment = env, expected = 0, cwd = root) => {
      const result = spawnSync(binary, args, {
        cwd,
        env: environment,
        encoding: "utf8",
        timeout: 60000,
        maxBuffer: 16 * 1024 * 1024,
      });
      assert.equal(
        result.error,
        undefined,
        `${result.error}\n${result.stderr}`,
      );
      assert.equal(
        result.status,
        expected,
        `${binary} ${args.join(" ")}\n${result.stdout}\n${result.stderr}`,
      );
      return result;
    };
    run("cargo", ["generate-lockfile", "--offline"]);
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
      "test: isolated Mac archive",
    ]);
    Object.assign(env, {
      GITHUB_SHA: run("git", ["rev-parse", "HEAD"]).stdout.trim(),
      GITHUB_RUN_ID: "10",
      GITHUB_RUN_ATTEMPT: "2",
    });
    const artifact = path.join(temporary, "artifact");
    const githubOutput = path.join(temporary, "github-output");
    writeFileSync(githubOutput, "parent output\n");
    env.GITHUB_OUTPUT = githubOutput;
    const producerStarted = performance.now();
    const produced = run(process.execPath, [
      "--input-type=module",
      "-e",
      `import { produce } from './windows-coverage.mjs'; console.log('OUTPUTS:' + JSON.stringify(await produce(process.argv[1], { profile: 'macos-remaining' })));`,
      artifact,
    ]);
    const trustedOutputs = JSON.parse(produced.stdout.split("OUTPUTS:")[1]);
    assert.equal(
      readFileSync(githubOutput, "utf8"),
      "parent output\n",
      "imported API must not write parent step outputs",
    );
    const manifestFile = path.join(artifact, "manifest.json");
    const originalManifest = readFileSync(manifestFile, "utf8");
    const manifest = JSON.parse(originalManifest);
    console.log(
      JSON.stringify({
        producerSeconds: (performance.now() - producerStarted) / 1000,
        archiveBytes: statSync(path.join(artifact, "workspace.tar.zst")).size,
        debugSymbolBytes: manifest.debugSymbols.reduce(
          (total, row) =>
            total +
            statSync(
              path.join(
                root,
                "target",
                `${row.binary}.dSYM/Contents/Resources/DWARF`,
                path.basename(row.binary),
              ),
            ).size,
          0,
        ),
      }),
    );
    assert.equal(manifest.scheduling.testThreads, 4);
    assert.equal(manifest.scheduling.partitions, 2);
    assert.equal(manifest.testCount, 21);
    assert.equal(manifest.runnableCount, 20);
    assert.equal(manifest.identity.llvmCov, "");
    renameSync(
      path.join(root, "target"),
      path.join(temporary, "producer-target"),
    );
    const cli = (
      shard,
      output,
      environment = env,
      trust = trustedOutputs,
      expected = 0,
      cwd = root,
    ) =>
      run(
        process.execPath,
        [
          path.join(root, "windows-coverage.mjs"),
          "consume-macos-remaining",
          String(shard),
          artifact,
          output,
          trust.manifestSha256,
          trust.archiveSha256,
        ],
        environment,
        expected,
        cwd,
      );
    let rejectedIndex = 0;
    const reject = (
      environment = env,
      trust = trustedOutputs,
      pattern = /identity|digest|environment|target|jobs/u,
      cwd = root,
    ) => {
      const output = path.join(temporary, `rejected-${rejectedIndex++}`);
      const result = cli(1, output, environment, trust, 1, cwd);
      assert.match(result.stderr, pattern);
      assert.equal(existsSync(path.join(output, "receipt.json")), false);
      assert.equal(
        existsSync(path.join(root, "target")),
        false,
        "identity failure must precede extraction",
      );
    };
    reject(env, { ...trustedOutputs, manifestSha256: hash("wrong") });
    reject(env, { ...trustedOutputs, archiveSha256: hash("wrong") });
    reject({ ...env, GITHUB_RUN_ATTEMPT: "3" });
    reject({ ...env, GITHUB_SHA: "f".repeat(40) }, trustedOutputs, /source/u);
    reject(
      { ...env, CARGO_PROFILE_TEST_DEBUG: "full" },
      trustedOutputs,
      /debug/u,
    );
    reject({
      ...env,
      LLVM_PROFILE_FILE: path.join(root, "target", "bad.profraw"),
    });
    for (const field of [
      "rustc",
      "cargoConfigSha256",
      "nextestConfigSha256",
      "root",
      "archiveProfile",
      "featureResolutionSha256",
    ]) {
      const changed = structuredClone(manifest);
      // Feature graph is checked after extraction; identity fields before it.
      if (field === "featureResolutionSha256") continue;
      changed.identity[field] = "changed";
      const text = JSON.stringify(changed);
      writeFileSync(manifestFile, text);
      reject(env, { ...trustedOutputs, manifestSha256: hash(text) });
    }
    writeFileSync(manifestFile, originalManifest);
    const relocated = path.join(temporary, "relocated");
    cpSync(root, relocated, { recursive: true });
    reject(
      { ...env, CARGO_TARGET_DIR: path.join(relocated, "target") },
      trustedOutputs,
      /identity/u,
      relocated,
    );
    const failed = path.join(temporary, "failed");
    const failure = cli(
      1,
      failed,
      { ...env, MESH_MAC_FAIL: "1", RUST_BACKTRACE: "1" },
      trustedOutputs,
      1,
    );
    assert.match(failure.stderr, /failed: 100/u);
    const failureLog = readFileSync(path.join(failed, "execution.log"), "utf8");
    assert.match(failureLog, /stack backtrace:/u);
    assert.match(
      failureLog,
      /\n\s+\d+: (?:remaining_core::)?case_\d+\n\s+at (?:\.\/)?src\/lib\.rs:\d+/u,
      `archive-only consumer lost application stack line tables:\n${failureLog}`,
    );
    assert.equal(existsSync(path.join(failed, "receipt.json")), false);
    const selected = JSON.parse(
      readFileSync(path.join(failed, "expected-inventory.json"), "utf8"),
    )
      .filter((row) => row.selected)
      .map((row) => row.eventName)
      .sort();
    const failedEvents = readFileSync(
      path.join(failed, "execution.jsonl"),
      "utf8",
    )
      .trim()
      .split("\n")
      .map(JSON.parse);
    assert.deepEqual(
      failedEvents
        .filter(
          (event) =>
            event.type === "test" && ["ok", "failed"].includes(event.event),
        )
        .map((event) => event.name)
        .sort(),
      selected,
      "no-fail-fast must finish every selected real case after failure",
    );
    // Negative control separates actual stack line tables from the panic header.
    const debugRow = manifest.debugSymbols.find((row) =>
      /debug\/deps\/remaining_core-/u.test(row.binary),
    );
    assert.ok(debugRow);
    const executable = path.join(root, "target", debugRow.binary);
    const bundle = `${executable}.dSYM`;
    const hiddenBundle = path.join(temporary, "hidden.dSYM");
    renameSync(bundle, hiddenBundle);
    const missingSymbols = run(
      executable,
      ["case_11", "--exact", "--nocapture"],
      { ...env, MESH_MAC_FAIL: "1", RUST_BACKTRACE: "1" },
      101,
      path.join(root, "core"),
    );
    assert.match(missingSymbols.stderr, /panicked at core\/src\/lib.rs:\d+/u);
    assert.doesNotMatch(
      missingSymbols.stderr.split("stack backtrace:")[1],
      /at (?:\.\/)?src\/lib.rs:\d+/u,
    );
    renameSync(hiddenBundle, bundle);
    renameSync(
      path.join(root, "target"),
      path.join(temporary, "failed-target"),
    );
    const freshHome = path.join(temporary, "fresh-cargo-home");
    mkdirSync(freshHome);
    const results = [];
    for (const shard of [1, 2]) {
      const output = path.join(temporary, `result-${shard}`);
      const result = cli(shard, output, { ...env, CARGO_HOME: freshHome });
      assert.doesNotMatch(
        result.stderr,
        /^\s*(?:Compiling|Checking) /mu,
        "consumer must never compile",
      );
      assert.equal(existsSync(path.join(output, "coverage.lcov")), false);
      const receipt = JSON.parse(
        readFileSync(path.join(output, "receipt.json"), "utf8"),
      );
      assert.equal("coverage" in receipt, false);
      results.push(output);
      renameSync(
        path.join(root, "target"),
        path.join(temporary, `consumer-target-${shard}`),
      );
    }
    // The collector needs no executable archive or Darwin checkout.
    const archive = path.join(artifact, "workspace.tar.zst");
    renameSync(archive, path.join(temporary, "binary-archive.tar.zst"));
    const collected = await collectMacRemaining(artifact, results, {
      ...trustedOutputs,
      sourceSha: env.GITHUB_SHA,
      runId: env.GITHUB_RUN_ID,
      runAttempt: env.GITHUB_RUN_ATTEMPT,
      jobResults: {
        native: "success",
        producer: "success",
        consumers: "success",
      },
    });
    assert.equal(collected.executedTests, 20);
    assert.equal(
      run("git", [
        "status",
        "--porcelain",
        "--untracked-files=normal",
      ]).stdout.trim(),
      "",
    );
    const exportedArtifact = path.join(temporary, "exported-artifact");
    run(process.execPath, [
      path.join(root, "windows-coverage.mjs"),
      "produce-macos-remaining",
      exportedArtifact,
      "--github-output",
    ]);
    const exportedManifest = readFileSync(
      path.join(exportedArtifact, "manifest.json"),
    );
    const exported = readFileSync(githubOutput, "utf8")
      .split("\n")
      .slice(1)
      .filter(Boolean)
      .map((line) => line.split("="));
    assert.deepEqual(Object.keys(Object.fromEntries(exported)).sort(), [
      "archiveSha256",
      "manifestSha256",
      "rustVersion",
    ]);
    assert.equal(
      Object.fromEntries(exported).manifestSha256,
      hash(exportedManifest),
    );
    assert.equal(
      Object.fromEntries(exported).archiveSha256,
      hash(readFileSync(path.join(exportedArtifact, "workspace.tar.zst"))),
    );
    const beforeProducerFailure = readFileSync(githubOutput, "utf8");
    write(
      "extra/src/lib.rs",
      'compile_error!("actual producer compiler failure control");\n',
    );
    run("git", ["add", "extra/src/lib.rs"]);
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
      "test: failing remaining compile",
    ]);
    const failedArtifact = path.join(temporary, "failed-artifact");
    const producerFailure = run(
      process.execPath,
      [
        path.join(root, "windows-coverage.mjs"),
        "produce-macos-remaining",
        failedArtifact,
        "--github-output",
      ],
      { ...env, GITHUB_SHA: run("git", ["rev-parse", "HEAD"]).stdout.trim() },
      1,
    );
    assert.match(
      producerFailure.stderr,
      /actual producer compiler failure control/u,
    );
    assert.equal(existsSync(path.join(failedArtifact, "manifest.json")), false);
    assert.equal(
      readFileSync(githubOutput, "utf8"),
      beforeProducerFailure,
      "failed producer cannot export acceptance outputs",
    );
  },
);
