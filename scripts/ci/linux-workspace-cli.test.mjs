// Actual-tool protocol qualification; Darwin exercises mechanics, never Linux ABI.
import assert from "node:assert/strict";
import test from "node:test";
import * as archive from "./windows-coverage.mjs";

test("Linux profile preserves the full instrumented default cohort and old target", () => {
  const profile = archive.archiveProfile("linux-workspace");
  assert.equal(profile.instrumented, true);
  assert.deepEqual(profile.buildArgs, ["--workspace", "--locked"]);
  assert.deepEqual(profile.partitions, [1, 2]);
  assert.equal(profile.testThreads, 4);
  assert.equal(profile.targetRelative, "target/llvm-cov-target");
  assert.equal(archive.archiveProfile("windows").testThreads, 1);
  assert.equal(archive.archiveProfile("windows").partitions.length, 8);
  assert.equal(archive.archiveProfile("macos-remaining").instrumented, false);
});

test("Linux runtime identity requires the actual hosted Ubuntu ABI cohort", () => {
  const env = {
    GITHUB_ACTIONS: "true",
    RUNNER_ENVIRONMENT: "github-hosted",
    RUNNER_OS: "Linux",
    ImageOS: "ubuntu26",
    ImageVersion: "20261101.1.0",
  };
  const observed = {
    platform: "linux",
    arch: "x64",
    osRelease: 'ID=ubuntu\nVERSION_ID="26.04"\n',
    glibc: "glibc 2.43",
    runtimePackages:
      "libc6\t2.43\nlibgtk-3-0t64\t3.24\nlibwebkit2gtk-4.1-0\t2.50\nlibayatana-appindicator3-1\t0.5\nlibrsvg2-2\t2.60\nlibstdc++6\t15\n",
  };
  const identity = archive.validateLinuxRuntime(env, observed);
  assert.equal(identity.kind, "native-linux");
  assert.equal(identity.observation.imageVersion, env.ImageVersion);
  for (const imageOS of ["ubuntu24", "ubuntu-26.04", "ubuntu28"])
    assert.equal(
      archive.validateLinuxRuntime({ ...env, ImageOS: imageOS }, observed)
        .observation.imageOS,
      imageOS,
    );
  for (const changed of [
    { ...env, ImageOS: "windows26" },
    { ...env, ImageOS: "" },
    { ...env, ImageVersion: "" },
    { ...env, RUNNER_ENVIRONMENT: "self-hosted" },
  ])
    assert.throws(() => archive.validateLinuxRuntime(changed, observed));
  for (const changed of [
    { ...observed, platform: "darwin" },
    { ...observed, osRelease: 'ID=ubuntu\nVERSION_ID="24.04"' },
    { ...observed, glibc: "" },
    { ...observed, runtimePackages: "libc6\t2.43" },
  ])
    assert.throws(() => archive.validateLinuxRuntime(env, changed));
});

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  realpathSync,
  renameSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
const hash = (data) => createHash("sha256").update(data).digest("hex");

test(
  "real Linux-profile archive executes two complete offline shards at embedded paths",
  { timeout: 180000 },
  async (t) => {
    const temporary = realpathSync(
      mkdtempSync(
        path.join(
          tmpdir(),
          "mesh-linux-archive-'$(touch mesh-env-path-injected)-",
        ),
      ),
    );
    t.after(() => rmSync(temporary, { recursive: true, force: true }));
    const root = path.join(temporary, "source");
    for (const directory of [
      ".cargo",
      ".config",
      "empty-hooks",
      "host/src",
      "core/src",
      "core/tests",
      "core/examples",
      "integrations/src",
      "mcp/src",
    ])
      mkdirSync(path.join(root, directory), { recursive: true });
    const write = (file, text) => writeFileSync(path.join(root, file), text);
    write(".gitignore", "/target/\n");
    write(".cargo/config.toml", "[build]\njobs=1\n");
    write(
      ".config/nextest.toml",
      'nextest-version="0.9.150"\n[profile.default]\nretries=0\n',
    );
    write(
      "Cargo.toml",
      '[workspace]\nmembers=["host","core","integrations","mcp"]\nresolver="2"\n',
    );
    for (const [directory, name] of [
      ["host", "mesh-talk"],
      ["core", "mesh-talk-core"],
      ["integrations", "mesh-talk-integrations"],
      ["mcp", "mesh-talk-mcp"],
    ])
      write(
        `${directory}/Cargo.toml`,
        `[package]\nname="${name}"\nversion="0.0.0"\nedition="2024"\n${directory === "core" ? '[[example]]\nname="default-example"\ntest=true\n[features]\ndefault=["enabled"]\nenabled=[]\n[[test]]\nname="runtime"\nrequired-features=["enabled"]\n[[test]]\nname="disabled"\nrequired-features=["disabled"]\n' : ""}`,
      );
    write(
      "host/src/lib.rs",
      "pub fn host_truth() -> u8 { 4 }\n#[test] fn host() { assert_eq!(host_truth(),4); }\n",
    );
    write(
      "core/src/lib.rs",
      `pub fn core_truth(first: bool) -> u8 { if first { 7 } else { 9 } }
pub fn bin_truth() -> &'static str { "ACTUAL_BIN_MARKER" }
#[inline(never)] pub fn application_frame() { assert!(std::env::var_os("MESH_LINUX_FAIL").is_none(), "real application failure"); }
${Array.from({ length: 16 }, (_, i) => `#[test] fn case_${i}() { application_frame(); assert_eq!(core_truth(${i % 2 === 0}),${i % 2 === 0 ? 7 : 9}); println!("PHASE {{\\\"core\\\":true}}"); }`).join("\n")}
#[test] #[ignore] fn ignored() { panic!("ignored case must not run"); }\n`,
    );
    write(
      "core/src/main.rs",
      'fn main() { println!("{}",mesh_talk_core::bin_truth()); }\n#[test] fn bin_case() { assert_eq!(mesh_talk_core::core_truth(true),7); }\n',
    );
    write(
      "core/tests/runtime.rs",
      '#[test] fn embedded_cli() { assert_eq!(std::env::var("RUST_TEST_THREADS").unwrap(),"4"); for binary in [env!("CARGO_BIN_EXE_mesh-talk-core").to_owned(),std::env::var("CARGO_BIN_EXE_mesh-talk-core").unwrap()] { let output=std::process::Command::new(binary).output().unwrap(); assert!(output.status.success()); assert_eq!(String::from_utf8(output.stdout).unwrap().trim(),"ACTUAL_BIN_MARKER"); } }\n',
    );
    write(
      "core/tests/disabled.rs",
      'compile_error!("disabled target must not compile");\n',
    );
    write(
      "core/examples/default-example.rs",
      "fn main() {}\n#[test] fn example() { assert_eq!(mesh_talk_core::core_truth(false),9); }\n",
    );
    for (const directory of ["integrations", "mcp"])
      write(
        `${directory}/src/lib.rs`,
        `pub fn ${directory}_truth() -> u8 { 3 }\n#[test] fn ${directory}() { assert_eq!(${directory}_truth(),3); }\n`,
      );
    copyFileSync(
      fileURLToPath(new URL("./windows-coverage.mjs", import.meta.url)),
      path.join(root, "windows-coverage.mjs"),
    );
    // API-only local fixture identity. Production CLI has no fixture switch.
    write(
      "driver.mjs",
      `import {produce,consume} from './windows-coverage.mjs'; import {writeFileSync} from 'node:fs';
const [mode,artifact,output,trust,shard]=process.argv.slice(2);
const fixtureRuntime=process.platform==='linux'?undefined:{kind:'generic-archive-fixture',platform:process.platform,arch:process.arch,root:process.cwd(),qualification:'not-native-linux'};
const options={profile:'linux-workspace',fixtureRuntime,...(trust?JSON.parse(trust):{})};
try { if(mode==='produce') writeFileSync(output,JSON.stringify(await produce(artifact,options))); else await consume(Number(shard),artifact,output,options); } catch(error) { console.error(error.message); process.exitCode=1; }\n`,
    );
    const env = {
      ...process.env,
      CARGO_NET_OFFLINE: "true",
      CARGO_TERM_COLOR: "never",
      CARGO_PROFILE_DEV_DEBUG: "line-tables-only",
      CARGO_PROFILE_TEST_DEBUG: "line-tables-only",
      MESH_ARCHIVE_NODE: process.execPath,
      MESH_TALK_TEST_TIMING: "1",
    };
    for (const key of Object.keys(env))
      if (
        /^(?:__)?CARGO_LLVM_COV/u.test(key) ||
        [
          "LLVM_PROFILE_FILE",
          "RUSTFLAGS",
          "CARGO_ENCODED_RUSTFLAGS",
          "CARGO_TARGET_DIR",
          "CARGO_BUILD_TARGET",
          "RUSTC_WRAPPER",
          "RUSTC_WORKSPACE_WRAPPER",
          "CI",
          "GITHUB_ACTIONS",
        ].includes(key)
      )
        delete env[key];
    // Linux must qualify the real hosted identity; never strip its admission inputs.
    if (process.platform === "linux") {
      env.CI = process.env.CI;
      env.GITHUB_ACTIONS = process.env.GITHUB_ACTIONS;
    }
    const resultFor = (result, expected, label) => {
      assert.equal(
        result.status,
        expected,
        `${label}\n${result.stdout}\n${result.stderr}\n${result.error ?? ""}`,
      );
      return result;
    };
    const run = (binary, args, environment = env, expected = 0) =>
      resultFor(
        spawnSync(binary, args, {
          cwd: root,
          env: environment,
          encoding: "utf8",
          timeout: 90000,
          maxBuffer: 32 * 1024 * 1024,
        }),
        expected,
        `${binary} ${args.join(" ")}`,
      );
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
      "test: tiny Linux archive",
    ]);
    Object.assign(env, {
      GITHUB_SHA: run("git", ["rev-parse", "HEAD"]).stdout.trim(),
      GITHUB_RUN_ID: "1",
      GITHUB_RUN_ATTEMPT: "1",
    });
    const artifact = path.join(temporary, "archive"),
      trustFile = path.join(temporary, "trust.json"),
      target = path.join(root, "target", "llvm-cov-target");
    const phase = (
      mode,
      output,
      environment = env,
      trust = "",
      shard = "1",
      expected = 0,
    ) => {
      const scoped = {
        ...environment,
        MESH_MODE: mode,
        MESH_ARTIFACT: artifact,
        MESH_OUTPUT: output,
        MESH_TRUST: trust,
        MESH_SHARD: shard,
        CARGO_TARGET_DIR: target,
      };
      // Literal interpreter program and ordinary Node/Cargo argv have distinct launches.
      return resultFor(
        spawnSync(
          "bash",
          [
            "-euc",
            'coverage_environment="$(cargo llvm-cov show-env --sh)"\neval "$coverage_environment"\n"$MESH_ARCHIVE_NODE" driver.mjs "$MESH_MODE" "$MESH_ARTIFACT" "$MESH_OUTPUT" "$MESH_TRUST" "$MESH_SHARD"',
          ],
          {
            cwd: root,
            env: scoped,
            encoding: "utf8",
            timeout: 90000,
            maxBuffer: 32 * 1024 * 1024,
          },
        ),
        expected,
        `bash coverage phase ${mode}`,
      );
    };
    phase("produce", trustFile);
    const trusted = JSON.parse(readFileSync(trustFile, "utf8")),
      manifestFile = path.join(artifact, "manifest.json"),
      original = readFileSync(manifestFile, "utf8"),
      manifest = JSON.parse(original);
    assert.equal(manifest.identity.targetDirectory, target);
    assert.equal(
      manifest.identity.runtime.kind,
      process.platform === "linux" ? "native-linux" : "generic-archive-fixture",
    );
    assert.equal(
      manifest.schema,
      process.platform === "linux" ? 2 : "linux-archive-fixture-v1",
    );
    if (process.platform !== "linux") {
      const result = run(
        process.execPath,
        [
          path.join(root, "windows-coverage.mjs"),
          "consume-linux-workspace",
          "1",
          artifact,
          path.join(temporary, "production-reject"),
          trusted.manifestSha256,
          trusted.archiveSha256,
        ],
        env,
        1,
      );
      assert.match(
        result.stderr,
        /Unsupported archive manifest/u,
        "production CLI must reject generic fixture schema before native execution",
      );
    }
    const full = JSON.parse(
      readFileSync(path.join(artifact, "inventory-0.json"), "utf8"),
    );
    assert.equal(full.filter((row) => row.ignored).length, 1);
    assert.ok(full.every((row) => row.selected === !row.ignored));
    renameSync(target, path.join(temporary, "producer-target"));
    // Derive coverage settings again, with the producer outputs absent, exactly
    // as a separate consumer process does. No producer environment is retained.
    const freshEnvironment = resultFor(
      spawnSync(
        "bash",
        [
          "-euc",
          'coverage_environment="$(cargo llvm-cov show-env --sh)"\neval "$coverage_environment"\nprintf "%s" "$CARGO_LLVM_COV_TARGET_DIR"',
        ],
        {
          cwd: root,
          env: { ...env, CARGO_TARGET_DIR: target },
          encoding: "utf8",
          timeout: 90000,
          maxBuffer: 32 * 1024 * 1024,
        },
      ),
      0,
      "bash fresh coverage environment",
    );
    assert.equal(
      freshEnvironment.stdout,
      target,
      "compare evaluated path, not shell serialization or quoting style",
    );
    assert.equal(
      existsSync(target),
      false,
      "fresh show-env must not create a nonempty runtime target",
    );
    const partialOutput = path.join(temporary, "partial-extraction-evidence");
    mkdirSync(partialOutput);
    let actualStage;
    await assert.rejects(
      () =>
        archive.extractLinuxArchive(
          path.join(artifact, "workspace.tar.zst"),
          root,
          partialOutput,
          {},
          async (args) => {
            actualStage = args[args.indexOf("--extract-to") + 1];
            run("cargo", [
              "nextest",
              "list",
              "--profile",
              "default",
              "--user-config-file",
              "none",
              ...args,
              "--message-format",
              "json",
            ]);
            assert.ok(
              existsSync(
                path.join(actualStage, "target", "debug", "mesh-talk-core"),
              ),
              "negative control must extract the real archived runtime",
            );
            writeFileSync(
              path.join(partialOutput, "extract.log"),
              "real archive extracted; intentional post-extraction listing failure",
            );
            throw new Error("intentional post-extraction listing failure");
          },
        ),
      /intentional post-extraction listing failure/,
    );
    assert.equal(existsSync(actualStage), false);
    assert.equal(existsSync(target), false);
    assert.deepEqual(
      readdirSync(partialOutput),
      ["extract.log"],
      "failed extraction evidence must exclude real runtime bytes",
    );
    const reject = (
      environment = env,
      trust = trusted,
      pattern = /identity|digest|source|debug|fixture/u,
    ) => {
      const output = path.join(temporary, `rejected-${reject.index++}`);
      const result = phase(
        "consume",
        output,
        environment,
        JSON.stringify(trust),
        "1",
        1,
      );
      assert.match(result.stderr, pattern);
      assert.equal(existsSync(path.join(output, "receipt.json")), false);
      assert.equal(existsSync(target), false, "reject before extraction");
    };
    reject.index = 0;
    reject(env, { ...trusted, manifestSha256: hash("wrong") });
    reject(env, { ...trusted, archiveSha256: hash("wrong") });
    reject({ ...env, GITHUB_RUN_ATTEMPT: "2" });
    reject({ ...env, GITHUB_SHA: "f".repeat(40) });
    reject({ ...env, CARGO_PROFILE_TEST_DEBUG: "full" });
    if (process.platform !== "linux") reject({ ...env, CI: "true" });
    for (const field of [
      "rustc",
      "root",
      "runtime",
      "targetDirectory",
      "archiveProfile",
      "cargoConfigSha256",
    ]) {
      const changed = structuredClone(manifest);
      changed.identity[field] = "changed";
      const text = JSON.stringify(changed);
      writeFileSync(manifestFile, text);
      reject(env, { ...trusted, manifestSha256: hash(text) });
    }
    writeFileSync(manifestFile, original);
    const inventoryFile = path.join(artifact, "inventory-1.json"),
      inventoryText = readFileSync(inventoryFile, "utf8");
    writeFileSync(inventoryFile, "[]");
    reject();
    writeFileSync(inventoryFile, inventoryText);
    const archiveFile = path.join(artifact, "workspace.tar.zst"),
      archiveBytes = readFileSync(archiveFile);
    writeFileSync(archiveFile, "corrupt");
    reject();
    writeFileSync(archiveFile, archiveBytes);
    const failed = path.join(temporary, "failed");
    const failure = phase(
      "consume",
      failed,
      {
        ...env,
        MESH_LINUX_FAIL: "1",
        MESH_WINDOWS_FAIL_FAST: "1",
        RUST_BACKTRACE: "1",
      },
      JSON.stringify(trusted),
      "1",
      1,
    );
    assert.match(failure.stderr, /failed: 100/u);
    assert.equal(existsSync(path.join(failed, "receipt.json")), false);
    const failureLog = readFileSync(path.join(failed, "execution.log"), "utf8");
    assert.match(failureLog, /real application failure/u);
    if (process.platform === "linux")
      assert.match(
        failureLog,
        /\n\s+\d+: .*application_frame\n\s+at .*src\/lib.rs:\d+/u,
        "archive must retain actual ELF application stack file:line",
      );
    if (process.platform === "linux") {
      const binaries = JSON.parse(
        readFileSync(path.join(failed, "consumer-binaries.json"), "utf8"),
      );
      const probes = Object.values(binaries["rust-binaries"]).filter(
        (binary) =>
          binary.kind === "lib" && binary["binary-id"] === "mesh-talk-core",
      );
      assert.equal(
        probes.length,
        1,
        "ELF negative probe requires exact Core library target",
      );
      const stripped = path.join(temporary, "stripped-core");
      copyFileSync(probes[0]["binary-path"], stripped);
      run("objcopy", ["--strip-debug", stripped]);
      const negative = spawnSync(
        stripped,
        ["case_0", "--exact", "--nocapture"],
        {
          cwd: path.join(root, "core"),
          env: {
            ...env,
            MESH_LINUX_FAIL: "1",
            RUST_BACKTRACE: "1",
            LLVM_PROFILE_FILE: path.join(temporary, "elf-negative-%p.profraw"),
          },
          encoding: "utf8",
          timeout: 15000,
        },
      );
      assert.equal(negative.status, 101);
      assert.match(negative.stdout, /1 failed/u);
      assert.match(
        negative.stderr,
        /panicked at core\/src\/lib.rs:\d+/u,
        "panic header must survive stripping",
      );
      assert.doesNotMatch(
        negative.stderr.split("stack backtrace:")[1] ?? "",
        /at .*src\/lib.rs:\d+/u,
        "panic header alone must not qualify application stack lines",
      );
    }
    const selected = JSON.parse(
      readFileSync(path.join(failed, "selected-inventory.json"), "utf8"),
    );
    const events = readFileSync(path.join(failed, "execution.jsonl"), "utf8")
      .trim()
      .split("\n")
      .map(JSON.parse);
    assert.deepEqual(
      events
        .filter(
          (event) =>
            event.type === "test" && ["ok", "failed"].includes(event.event),
        )
        .map((event) => event.name)
        .sort(),
      selected
        .filter((row) => row.selected)
        .map((row) => row.eventName)
        .sort(),
    );
    renameSync(target, path.join(temporary, "failed-target"));
    const freshHome = path.join(temporary, "fresh-cargo-home");
    mkdirSync(freshHome);
    const coverages = [],
      executed = [],
      logs = [];
    for (const shard of [1, 2]) {
      const output = path.join(temporary, `result-${shard}`),
        result = phase(
          "consume",
          output,
          { ...env, CARGO_HOME: freshHome },
          JSON.stringify(trusted),
          String(shard),
        );
      assert.doesNotMatch(result.stderr, /^\s*(?:Compiling|Checking) /mu);
      const receipt = JSON.parse(
        readFileSync(path.join(output, "receipt.json"), "utf8"),
      );
      assert.equal(receipt.schema, manifest.schema);
      assert.ok(receipt.coverage.coveredLines > 0);
      const current = JSON.parse(
        readFileSync(path.join(output, "selected-inventory.json"), "utf8"),
      );
      const text = readFileSync(path.join(output, "execution.jsonl"), "utf8");
      archive.verifyExecution(text, current);
      for (const invalid of [
        "",
        text.slice(0, text.lastIndexOf("\n", text.length - 2)),
      ])
        assert.throws(() => archive.verifyExecution(invalid, current));
      logs.push(readFileSync(path.join(output, "execution.log"), "utf8"));
      executed.push(
        ...current.filter((row) => row.selected).map((row) => row.id),
      );
      coverages.push(readFileSync(path.join(output, "coverage.lcov"), "utf8"));
      renameSync(target, path.join(temporary, `consumer-${shard}-target`));
    }
    assert.deepEqual(
      executed.sort(),
      full
        .filter((row) => row.selected)
        .map((row) => row.id)
        .sort(),
    );
    assert.ok(
      logs.some((log) => log.includes('PHASE {"core":true}')),
      "actual successful PHASE payload must survive",
    );
    const coverage = coverages.join("\n");
    for (const name of [
      "host_truth",
      "core_truth",
      "bin_truth",
      "integrations_truth",
      "mcp_truth",
    ])
      assert.match(
        coverage,
        new RegExp(`^FNDA:[1-9]\\d*,[^\\n]*${name}`, "m"),
        `actual executed ${name} missing from coverage union`,
      );
    assert.equal(
      run("git", [
        "status",
        "--porcelain",
        "--untracked-files=normal",
      ]).stdout.trim(),
      "",
    );
    assert.equal(
      existsSync(path.join(root, "mesh-env-path-injected")),
      false,
      "temporary paths must stay literal data",
    );
    t.diagnostic(
      process.platform === "linux"
        ? "Actual Linux ABI/archive/ELF fixture qualified; full workspace still requires hosted acceptance"
        : "Darwin generic instrumentation/archive protocol only; Ubuntu ABI/runtime/ELF stack evidence remains unqualified",
    );
  },
);

test("Linux image observations do not replace authoritative runtime compatibility", () => {
  const expected = {
    archiveProfile: "linux-workspace",
    runtime: {
      kind: "native-linux",
      glibc: "glibc 2.43",
      runtimePackages: "exact package versions",
    },
    runtimeObservation: { imageOS: "ubuntu26", imageVersion: "20261101.186.1" },
  };
  archive.verifyIdentity(expected, {
    ...expected,
    runtimeObservation: {
      imageOS: "ubuntu-26.04",
      imageVersion: "20261102.1.0",
    },
  });
  assert.throws(
    () =>
      archive.verifyIdentity(expected, {
        ...expected,
        runtime: { ...expected.runtime, glibc: "glibc 2.44" },
      }),
    /identity/,
  );
  assert.throws(
    () =>
      archive.verifyIdentity(expected, {
        ...expected,
        runtime: {
          ...expected.runtime,
          runtimePackages: "changed package versions",
        },
      }),
    /identity/,
  );
});

test("partial Linux extraction stays outside evidence and retires only owned staging", async (t) => {
  const root = realpathSync(
    mkdtempSync(path.join(tmpdir(), "mesh-linux-stage-")),
  );
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const output = path.join(root, "evidence"),
    native = path.join(root, "target", "debug"),
    target = path.join(root, "target", "llvm-cov-target");
  mkdirSync(output);
  mkdirSync(native, { recursive: true });
  writeFileSync(path.join(native, "preserved"), "native owner");
  const before = readdirSync(path.dirname(target));
  for (const reason of [
    "partial extraction failure",
    "CI command interrupted",
  ]) {
    let stage;
    await assert.rejects(
      () =>
        archive.extractLinuxArchive(
          "unused.tar.zst",
          root,
          output,
          {},
          async (args) => {
            stage = args[args.indexOf("--extract-to") + 1];
            assert.equal(path.dirname(stage), path.dirname(target));
            const runtime = path.join(stage, "target", "debug");
            mkdirSync(runtime, { recursive: true });
            copyFileSync(
              process.execPath,
              path.join(runtime, "partial-runtime"),
            );
            assert.ok(existsSync(path.join(runtime, "partial-runtime")));
            writeFileSync(path.join(output, "extract.log"), reason);
            throw new Error(reason);
          },
        ),
      new RegExp(reason),
    );
    assert.equal(
      existsSync(stage),
      false,
      "failed/interrupted extraction must retire its owned stage",
    );
    assert.deepEqual(
      readdirSync(output),
      ["extract.log"],
      "failed evidence must contain diagnostics only",
    );
    assert.deepEqual(readdirSync(path.dirname(target)), before);
    assert.equal(
      readFileSync(path.join(native, "preserved"), "utf8"),
      "native owner",
    );
  }
  const alias = path.join(root, "root-alias");
  symlinkSync(root, alias, "dir");
  await assert.rejects(
    () =>
      archive.extractLinuxArchive(
        "unused",
        alias,
        path.join(root, "target"),
        {},
        async () =>
          assert.fail("aliased evidence must reject before extraction"),
      ),
    /overlap/i,
  );
  for (const unsafe of [
    root,
    path.join(root, "target"),
    path.join(root, "target", "evidence"),
  ]) {
    mkdirSync(unsafe, { recursive: true });
    await assert.rejects(
      () =>
        archive.extractLinuxArchive("unused", root, unsafe, {}, async () => {
          assert.fail("unsafe evidence must reject before extraction");
        }),
      /evidence|overlap/i,
    );
  }
});

test("Linux coverage environment failure cannot be masked by eval or admit execution", async (t) => {
  const root = mkdtempSync(path.join(tmpdir(), "mesh-linux-env-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(path.join(root, "scripts/ci"), { recursive: true });
  mkdirSync(path.join(root, "bin"));
  const wrapper = path.join(root, "scripts/ci/linux-workspace.sh");
  copyFileSync(new URL("./linux-workspace.sh", import.meta.url), wrapper);
  writeFileSync(
    path.join(root, "bin/cargo"),
    '#!/usr/bin/env bash\ntouch "$MESH_ENV_CARGO"\nprintf "%s\\n" "export LLVM_PROFILE_FILE=invalid"\nexit 37\n',
    { mode: 0o755 },
  );
  writeFileSync(
    path.join(root, "bin/node"),
    '#!/usr/bin/env bash\ntouch "$MESH_ENV_EXECUTED"\n',
    { mode: 0o755 },
  );
  const marker = path.join(root, "executed"),
    cargoMarker = path.join(root, "cargo-invoked");
  const environment = {
    ...process.env,
    PATH: `${path.join(root, "bin")}:${process.env.PATH}`,
    MESH_ENV_EXECUTED: marker,
    MESH_ENV_CARGO: cargoMarker,
  };
  for (const args of [[], ["unknown"]]) {
    const invalid = spawnSync("bash", [wrapper, ...args], {
      env: environment,
      encoding: "utf8",
    });
    assert.equal(invalid.status, 2);
    assert.match(invalid.stderr, /Usage:/u);
    assert.equal(
      existsSync(cargoMarker),
      false,
      "invalid mode must not invoke Cargo",
    );
    assert.equal(existsSync(marker), false);
  }
  const result = spawnSync(
    "bash",
    [wrapper, "produce", path.join(root, "archive")],
    {
      env: environment,
      encoding: "utf8",
    },
  );
  assert.equal(result.status, 37);
  assert.equal(existsSync(cargoMarker), true);
  assert.equal(existsSync(marker), false);
});
