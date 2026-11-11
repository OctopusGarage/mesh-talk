import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const packageApp = fileURLToPath(new URL("../release/package-app.sh", import.meta.url));
const packageSource = readFileSync(packageApp, "utf8");
const workflow = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");

test("release artifact steps delegate policy with target passed as data", () => {
  for (const [name, mode] of [["Collect build artifacts", "collect"], ["Generate SHA256 checksums", "checksums"], ["Package and sign artifacts", "package"]]) {
    const step = workflow.split(/^      - /m).find(value => value.startsWith(`name: ${name}\n`));
    assert.ok(step, name);
    assert.ok(step.includes(`run: bash scripts/release/package-app.sh ${mode}`));
    assert.doesNotMatch(step, /run: \|/);
    if (mode === "package") assert.match(step, /TARGET: \$\{\{ matrix\.target \}\}/);
  }
  assert.doesNotMatch(packageSource, /\$\{\{/);
});

test("manual packages exclude updater payloads while the signed manifest binds standalone roles", { skip: process.platform === "win32" }, () => {
  for (const [target, platform, extensions] of [
    ["aarch64-apple-darwin", "macos_arm64", ["dmg"]],
    ["x86_64-apple-darwin", "macos_x86_64", ["dmg"]],
    ["aarch64-pc-windows-msvc", "windows_arm64", ["exe", "msi"]],
    ["x86_64-pc-windows-msvc", "windows_x86_64", ["exe", "msi"]],
    ["aarch64-unknown-linux-gnu", "linux_aarch64", ["AppImage", "deb", "rpm"]],
    ["x86_64-unknown-linux-gnu", "linux_x86_64", ["AppImage", "deb", "rpm"]],
  ]) {
    const root = mkdtempSync(join(tmpdir(), "mesh-talk-lean-package-"));
    try {
      mkdirSync(join(root, "release"));
      const files = new Map(extensions.map((extension) => [`installer with spaces.${extension}`, Buffer.from(`native ${extension}`)]));
      const suffix = platform.startsWith("macos_") ? "app.tar.gz" : platform.startsWith("windows_") ? "nsis.exe" : "AppImage";
      const full = `mesh-talk-update_v0.1.5_${platform}.${suffix}`;
      const raw = full.replace("mesh-talk-update_", "mesh-talk-update-raw_");
      const standalone = [full, `${full}.sig`, ...(platform.startsWith("linux_") ? [] :
        [`${full}.delta.json`, `${full}.delta.json.sig`, raw, `${raw}.sig`, `mesh-talk-patch_v0.1.5_${platform}_from-v0.1.4.zst`])];
      for (const name of standalone) files.set(name, Buffer.from(`standalone ${name}`));
      for (const [name, bytes] of files) writeFileSync(join(root, "release", name), bytes);
      execFileSync("bash", [packageApp, "checksums"], { cwd: root, stdio: "pipe" });
      // Replace only the external signing service; exercise the real helper.
      mkdirSync(join(root, "scripts/release"), { recursive: true });
      writeFileSync(join(root, "scripts/release/sign-blob.mjs"), 'import {readFileSync,writeFileSync} from "node:fs"; const [zip,bundle]=process.argv.slice(2); if(!readFileSync(zip).length) process.exit(1); writeFileSync(bundle,zip);');
      const packageEnv = { ...process.env, TARGET: target, ARTIFACT_TAG: "v0.1.5" };
      execFileSync("bash", [packageApp, "package"], { cwd: root, env: packageEnv, stdio: "pipe" });
      const archive = join(root, "release_zips", `mesh-talk_v0.1.5_${platform}.zip`);
      const members = execFileSync("unzip", ["-Z1", archive], { encoding: "utf8" }).trim().split(/\r?\n/);
      assert.deepEqual(members.sort(), [...extensions.map((extension) => `release/installer with spaces.${extension}`), "release/SHA256SUMS", "release/UPDATER_SHA256SUMS"].sort(), platform);
      const manifest = execFileSync("unzip", ["-p", archive, "release/SHA256SUMS"], { encoding: "utf8" });
      assert.equal(manifest.trim().split(/\r?\n/).length, extensions.length, "manual installer checksums must remain self-contained");
      const updaterManifest = execFileSync("unzip", ["-p", archive, "release/UPDATER_SHA256SUMS"], { encoding: "utf8" });
      const manual = join(root, "manual");
      mkdirSync(manual);
      execFileSync("unzip", ["-q", archive, "-d", manual]);
      execFileSync("shasum", ["-a", "256", "-c", "SHA256SUMS"], { cwd: join(manual, "release"), stdio: "pipe" });
      for (const name of standalone) {
        assert.deepEqual(readFileSync(join(root, "release_zips", name)), files.get(name));
        assert.ok(updaterManifest.includes(`${createHash("sha256").update(files.get(name)).digest("hex")}  ./${name}`), name);
      }
      execFileSync("zip", [archive, `release/${full}`], { cwd: root, stdio: "pipe" });
      execFileSync("bash", [packageApp, "package"], { cwd: root, env: packageEnv, stdio: "pipe" });
      assert.deepEqual(execFileSync("unzip", ["-Z1", archive], { encoding: "utf8" }).trim().split(/\r?\n/).sort(), members.sort(), "repackaging must discard stale ZIP entries");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }
});

test("native release inspection uses the shared pinned toolchain and a real NSIS health regression", () => {
  for (const job of ["build-release", "verify-release", "publish-release"]) {
    const body = workflow.split(`  ${job}:\n`)[1]?.split(/^  [\w-]+:\n/m)[0];
    assert.ok(body, job);
    assert.match(body, /python scripts\/release\/sevenzip\.py install/, job);
    assert.doesNotMatch(body, /apt-get install[^\n]*\b7zip\b/, "Ubuntu's package must not select the release decoder");
  }
  const health = readFileSync(new URL("../../.github/workflows/check-health.yml", import.meta.url), "utf8");
  const toolchain = health.split("      - name: Install pinned native inspection toolchain\n")[1]?.split("      - ")[0];
  assert.ok(toolchain, "all three portable platforms must execute the tool bootstrap before merge");
  assert.match(toolchain, /if:.*steps\.scope\.outputs\.full == 'true'/);
  assert.match(toolchain, /python scripts\/release\/sevenzip\.py install/);
  const native = health.split("      - name: Native release inspection regression\n")[1]?.split("      - ")[0];
  assert.ok(native, "portable health must exercise a genuine plugin installer");
  assert.match(native, /if:.*steps\.scope\.outputs\.full == 'true'.*runner\.os == 'Linux'/);
  assert.match(native, /apt-get install[^\n]*\bnsis\b/);
  const smoke = health.split("      - name: Verify native NSIS payload equivalence\n")[1]?.split("      - ")[0];
  assert.match(smoke || "", /if:.*steps\.scope\.outputs\.full == 'true'.*runner\.os == 'Linux'/);
  assert.match(smoke || "", /python scripts\/release\/test_native_nsis\.py -v/);
});

test("Linux release builds retain the Ubuntu 22.04 runtime baseline across architectures", () => {
  assert.equal(workflow.match(/runner: (\S+)\n\s+target: x86_64-unknown-linux-gnu/)?.[1], "ubuntu-22.04");
  assert.equal(workflow.match(/runner: (\S+)\n\s+target: aarch64-unknown-linux-gnu/)?.[1], "ubuntu-22.04-arm");
  assert.ok(/key: \$\{\{ matrix\.target \}\}-\$\{\{ matrix\.runner \}\}-cargo-/.test(workflow),
    "compiled build scripts and native objects must not cross runner baselines");
});

test("release and inspection APT preparation stops before install after source or index failure", { skip: process.platform === "win32" }, () => {
  const root = mkdtempSync(join(tmpdir(), "mesh-talk-release-apt-"));
  try {
    const bin = join(root, "bin");
    const calls = join(root, "calls.log");
    mkdirSync(bin);
    writeFileSync(join(bin, "sudo"), `#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$APT_CALLS"
if [[ "$1" == --preserve-env=* ]]; then
  exit "$APT_PREPARE_STATUS"
fi
if [[ "$1 $2" == 'apt-get update' ]]; then
  # APT can return success after a transient index failure unless told otherwise.
  if [[ "$3" == '--error-on=any' ]]; then exit "$APT_UPDATE_STATUS"; fi
  exit 0
fi
if [[ "$1 $2" == 'apt-get install' ]]; then exit 0; fi
exit 91
`, { mode: 0o755 });
    const health = readFileSync(new URL("../../.github/workflows/check-health.yml", import.meta.url), "utf8");
    for (const [source, job, name] of [
      [workflow, "build-release", "Install Tauri system dependencies (Linux)"],
      [workflow, "verify-release", "Install installer metadata tools"],
      [workflow, "publish-release", "Install installer metadata tools"],
      [health, "platform", "Native release inspection regression"],
    ]) {
      const body = source.split(`  ${job}:\n`)[1]?.split(/^  [\w-]+:\n/m)[0];
      const step = body?.split(/^      - /m).find(value => value.startsWith(`name: ${name}\n`));
      assert.ok(step, job);
      const block = step.split("        run: ")[1];
      assert.ok(block?.startsWith("|\n"), job);
      const commands = block.slice(2).replace(/^          /gm, "");
      const run = (prepareStatus, updateStatus) => {
        writeFileSync(calls, "");
        const result = spawnSync("bash", ["-e", "-o", "pipefail", "-c", commands], {
          cwd: fileURLToPath(new URL("../../", import.meta.url)),
          env: { ...process.env, PATH: `${bin}${delimiter}${process.env.PATH}`, APT_CALLS: calls, APT_PREPARE_STATUS: String(prepareStatus), APT_UPDATE_STATUS: String(updateStatus) },
          encoding: "utf8",
        });
        assert.ifError(result.error);
        return { status: result.status, calls: readFileSync(calls, "utf8") };
      };
      const indexFailure = run(0, 100);
      assert.equal(indexFailure.status, 100, `${job}: a partial index failure must fail preparation`);
      assert.doesNotMatch(indexFailure.calls, /apt-get install/);
      const sourceFailure = run(19, 0);
      assert.equal(sourceFailure.status, 19, `${job}: source preparation failure must propagate`);
      assert.doesNotMatch(sourceFailure.calls, /apt-get (?:update|install)/);
      const success = run(0, 0);
      assert.equal(success.status, 0, job);
      const lines = success.calls.trim().split("\n");
      assert.equal(lines.length, 3, job);
      assert.match(lines[0], /^--preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,RUNNER_OS .* scripts\/ci\/ubuntu-apt-source\.mjs$/);
      assert.equal(lines[1], "apt-get update --error-on=any");
      assert.match(lines[2], /^apt-get install -y /);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("release matrix builds native Windows and Linux ARM64 packages", () => {
  assert.match(workflow, /runner: windows-11-(?:vs2026-)?arm[\s\S]*?target: aarch64-pc-windows-msvc/);
  assert.match(workflow, /runner: ubuntu-22\.04-arm[\s\S]*?target: aarch64-unknown-linux-gnu/);
  assert.match(packageSource, /aarch64-pc-windows-msvc\)[\s\S]*?ARCH="arm64"/);
});

test("Windows ARM64 installs a pinned x64 Cosign binary for emulated signing", () => {
  assert.match(workflow, /name: Install cosign\n\s+if: matrix\.target != 'aarch64-pc-windows-msvc'/);
  assert.match(workflow, /name: Install Cosign for Windows ARM64\n\s+if: matrix\.target == 'aarch64-pc-windows-msvc'/);
  assert.match(workflow, /cosign-windows-amd64\.exe/);
  assert.match(workflow, /9b85a88ebff2d9dd30ff4984a6f61f2cedc232dd87d81fa7f2ff3c0ed96c241c/);
});

for (const runner of ["Windows", "Linux", "macOS"]) {
  test(`${runner} checksum step hashes real files and preserves filenames with spaces`, () => {
    const root = mkdtempSync(join(tmpdir(), "mesh-talk-checksums-"));
    try {
      const release = join(root, "release");
      mkdirSync(release);
      const files = new Map([["installer with spaces.exe", Buffer.from("binary installer\0data")], ["package.msi", Buffer.from("second installer")]]);
      for (const [name, bytes] of files) writeFileSync(join(release, name), bytes);
      for (let attempt = 0; attempt < 2; attempt++) {
        execFileSync("bash", [packageApp, "checksums"], { cwd: root, env: { ...process.env, RUNNER_OS: runner } });
        const lines = readFileSync(join(release, "SHA256SUMS"), "utf8").trim().split(/\r?\n/);
        assert.equal(lines.length, files.size, "checksums must not include SHA256SUMS itself");
        const names = new Set();
        for (const line of lines) {
          const match = line.match(/^([a-f\d]{64}) [ *](?:\.\/)?(.+)$/i);
          assert.ok(match, `checksum entry must contain a SHA-256 digest and full filename: ${line}`);
          const bytes = files.get(match[2]);
          assert.ok(bytes, "checksum must reference an actual installer");
          assert.ok(!names.has(match[2]), "each installer must appear exactly once");
          names.add(match[2]);
          assert.equal(match[1].toLowerCase(), createHash("sha256").update(bytes).digest("hex"));
        }
      }
      // A partial hasher failure must propagate even if another file was hashed.
      const bin = join(root, "bin");
      mkdirSync(bin);
      const digest = createHash("sha256").update(files.get("installer with spaces.exe")).digest("hex");
      writeFileSync(join(bin, "sha256sum"), `#!/usr/bin/env bash\nif [[ "$1" == '-c' ]]; then exit 0; fi\nfor file in "$@"; do\n  [[ "$file" != */package.msi ]] || exit 23\n  printf '%s  %s\\n' '${digest}' "$file"\ndone\n`, { mode: 0o755 });
      assert.throws(() => execFileSync("bash", [packageApp, "checksums"], { cwd: root, env: { ...process.env, RUNNER_OS: runner, PATH: `${bin}${delimiter}${process.env.PATH}` } }), "a partial checksum failure must stop packaging/signing");
      for (const name of files.keys()) rmSync(join(release, name));
      assert.throws(() => execFileSync("bash", [packageApp, "checksums"], { cwd: root, env: { ...process.env, RUNNER_OS: runner } }), "empty releases must fail before packaging/signing");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test("platform uploads remain drafts until all release artifacts are verified", () => {
  const uploads = workflow.split(/^      - /m).filter((step) => /uses: softprops\/action-gh-release@/.test(step));
  assert.ok(uploads.length > 0, "release upload action must be present");
  for (const upload of uploads) {
    const inputs = upload.match(/^        with:\n((?:^          .*\n)+)/m)?.[1];
    assert.ok(inputs, "release upload must declare its inputs");
    assert.match(inputs, /^          draft: true\s*$/m, "platform upload must not publish an incomplete release");
  }
});


test("Nightly runs gate main before the shared matrix and publish verified prereleases", () => {
  assert.match(workflow, /schedule:\n\s+- cron:/);
  assert.match(workflow, /publish_nightly:/);
  assert.match(workflow, /group: .*nightly/);
  const preflight = workflow.split("  preflight:\n")[1].split("  build-release:\n")[0];
  assert.match(preflight, /check-source-checks\.mjs/);
  assert.match(preflight, /checks: read/);
  assert.match(preflight, /security-events: read/);
  assert.match(workflow, /MESH_TALK_BUILD_TAG: .*outputs.nightly == 'true' \|\| startsWith\(github.ref, 'refs\/tags\/'\)\).*outputs.tag \|\| format\('v\{0\}-dev'/);
  const verify = workflow.split("  verify-release:\n")[1].split("  resolve-publication:\n")[0];
  assert.match(verify, /prepare-nightly\.mjs/);
  assert.match(verify, /target_commitish: \$\{\{ github.sha \}\}/);
  assert.match(verify, /prerelease: \$\{\{ needs.preflight.outputs.nightly == 'true' \}\}/);
  assert.match(verify, /make_latest: false/);
  assert.ok(verify.indexOf("release-state.mjs record") < verify.indexOf("release-state.mjs publish"));
});


test("Nightly preflight waits within a bounded job while publication retains immediate checks", () => {
  const preflight = workflow.split("  preflight:\n")[1].split("  build-release:\n")[0];
  assert.match(preflight, /timeout-minutes: 250/);
  assert.match(preflight, /check-source-checks\.mjs "\$GITHUB_REPOSITORY" "\$GITHUB_SHA" --wait/);
  const publication = workflow.split("  preflight:\n")[1].split("  build-release:\n")[1];
  assert.doesNotMatch(publication, /check-source-checks\.mjs[^\n]*--wait/);
});

test("both release archive roles use the bounded signing owner before mandatory verification", () => {
  for (const name of ["Package and sign artifacts", "Package and sign post-office node"]) {
    const step = workflow.split(/^      - /m).find((value) => value.startsWith(`name: ${name}\n`));
    assert.ok(step, name);
    assert.match(name === "Package and sign artifacts" ? packageSource : step, /node scripts\/release\/sign-blob\.mjs/);
    assert.doesNotMatch(step, /cosign sign-blob/);
  }
  const verifier = readFileSync(new URL("../release/verify-release.sh", import.meta.url), "utf8");
  assert.match(verifier, /cosign verify-blob --bundle/);
  assert.match(verifier, /--certificate-identity/);
});


test("Stable mutation commands cannot run after a newly unsuccessful source check", () => {
  const steps = workflow.split("  publish-release:\n")[1].split(/^      - /m).slice(1);
  const image = steps.find(step => step.startsWith("name: Recheck source immediately before container publication\n"));
  const publication = steps.find(step => step.startsWith("name: Publish only the verified draft\n"));
  assert.ok(image && publication);
  const commands = step => {
    const run = step.split("        run: ")[1];
    return run.startsWith("|\n") ? run.slice(2).replace(/^          /gm, "") : run.trim();
  };
  const temporary = mkdtempSync(join(tmpdir(), "mesh-stable-source-"));
  try {
    const fixture = join(temporary, "source-check.mjs");
    const source = new URL("../release/check-source-checks.mjs", import.meta.url).href;
    const policy = new URL("../ci/required-checks.mjs", import.meta.url).href;
    writeFileSync(fixture, `import {requireSourceChecks} from ${JSON.stringify(source)};\nimport {releaseSourceChecks,releaseSourceWorkflows} from ${JSON.stringify(policy)};\nconst paths=[...new Set(Object.values(releaseSourceWorkflows))];const provenance={repo:"owner/repo",sha:"a".repeat(40),runs:paths.map((path,i)=>({path,id:i+1,run_attempt:1,check_suite_id:i+1,repository:{full_name:"owner/repo"},head_repository:{full_name:"owner/repo"},head_sha:"a".repeat(40),head_branch:"main",event:"push",status:"completed",conclusion:"success"}))};const checks=releaseSourceChecks.map((name,id)=>({name,id,check_suite:{id:paths.indexOf(releaseSourceWorkflows[name])+1},app:{slug:"github-actions"},status:"completed",conclusion:"success"}));\nif(process.env.NEW_CHECK_STATE!=="success") checks.push({...checks[0],id:999,...JSON.parse(process.env.NEW_CHECK_STATE)});\nrequireSourceChecks(checks,[],provenance);\n`);
    const prelude = 'node() { if [ "$1" = scripts/release/check-source-checks.mjs ]; then "$REAL_NODE" "$CHECK_FIXTURE"; else printf "PUBLIC_MUTATION\\n"; fi; }\n';
    for (const script of [commands(image) + '\nprintf "PUBLIC_MUTATION\\n"', commands(publication)]) {
      const run = (script, state) => spawnSync("bash", ["-e", "-c", prelude + script], { encoding: "utf8", env: { ...process.env, REAL_NODE: process.execPath, CHECK_FIXTURE: fixture, NEW_CHECK_STATE: state, SOURCE_REPO: "fixture/repo", SOURCE_SHA: "a".repeat(40), RELEASE_TAG: "v1.2.3" } });
      const healthy = run(script, "success");
      assert.equal(healthy.status, 0, healthy.stderr);
      assert.match(healthy.stdout, /PUBLIC_MUTATION/);
      for (const state of [{ conclusion: "failure" }, { status: "queued", conclusion: null }, { status: "in_progress", conclusion: null }]) {
        const blocked = run(script, JSON.stringify(state));
        assert.notEqual(blocked.status, 0);
        assert.doesNotMatch(blocked.stdout, /PUBLIC_MUTATION/);
        // Negative control: removing the owning source check permits the mutation.
        const old = run(script.replace(/^node scripts\/release\/check-source-checks\.mjs.*$/m, ""), JSON.stringify(state));
        assert.equal(old.status, 0, old.stderr);
        assert.match(old.stdout, /PUBLIC_MUTATION/);
      }
    }
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
});


test("macOS release linking discards local symbols before native packaging", () => {
  const producer = workflow.split("  build-release:")[1].split("  verify-release:")[0];
  for (const target of ["AARCH64_APPLE_DARWIN", "X86_64_APPLE_DARWIN"]) {
    assert.ok(producer.includes(`CARGO_TARGET_${target}_RUSTFLAGS: "-C link-arg=-Wl,-x"`));
  }
  assert.doesNotMatch(producer, /CARGO_TARGET_.*(?:WINDOWS|LINUX).*link-arg=-Wl,-x/);
});


test("macOS local-symbol stripping preserves executable output and native signature", { skip: process.platform !== "darwin", timeout: 30000 }, (t) => {
  const root = mkdtempSync(join(tmpdir(), "mesh-release-symbols-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const source = join(root, "probe.rs");
  writeFileSync(source, '#[inline(never)] fn local_probe(x: u64) -> u64 { std::hint::black_box(x) + 1 }\nfn main() { println!("{}", local_probe(41)); }\n');
  for (const stripped of [false, true]) {
    const binary = join(root, stripped ? "stripped" : "baseline");
    execFileSync("rustc", ["--crate-name", "symbol_probe", "-C", "opt-level=3", ...(stripped ? ["-C", "link-arg=-Wl,-x"] : []), source, "-o", binary]);
    assert.equal(execFileSync(binary, [], { encoding: "utf8" }).trim(), "42");
    const symbols = execFileSync("nm", [binary], { encoding: "utf8" });
    if (stripped) assert.doesNotMatch(symbols, / [tbs] /);
    else assert.match(symbols, /local_probe/);
    execFileSync("codesign", ["--force", "--sign", "-", binary]);
    execFileSync("codesign", ["--verify", "--strict", binary]);
  }
});
