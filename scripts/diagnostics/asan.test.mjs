import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { pathToFileURL, fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, mkdirSync, mkdtempSync, renameSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { reservedCohorts } from "../ci/asan.mjs";
import { versions } from "../ci/windows-coverage.mjs";

const owner = new URL("../ci/asan.mjs", import.meta.url);
const hash = value => createHash("sha256").update(value).digest("hex");
const identity = { platform: "linux", architecture: "x64", sourceSha: "a".repeat(40), sourceTree: "b".repeat(40), runId: "123", runAttempt: "2", toolchain: "nightly-2026-11-02", rustcSha256: "c".repeat(64), cargoSha256: "d".repeat(64), nextest: versions.nextest, inputsSha256: "e".repeat(64), target: "x86_64-unknown-linux-gnu", rustflags: "-Zsanitizer=address", rustdocflags: "-Zsanitizer=address", asanOptions: "detect_leaks=1", testThreads: "1", profileEnvironment: {} };
const normal = Array.from({ length: 9 }, (_, index) => ({ id: `core::case_${index}`, eventName: `core::core$case_${index}`, ignored: index === 8, selected: index !== 8 }));
const reservedRows = reservedCohorts.flat().map(name => ({ id: `core::${name}`, eventName: `core::core$${name}`, ignored: false, selected: true }));
const full = [...normal, ...reservedRows];
const events = rows => {
  const nextest = { crate: "core", test_binary: "core", kind: "lib" };
  return [ { type: "suite", event: "started", nextest }, ...rows.filter(row => row.selected).flatMap(row => [{ type: "test", event: "started", name: row.eventName }, { type: "test", event: "ok", name: row.eventName }]), { type: "suite", event: "ok", failed: 0, nextest } ].map(JSON.stringify).join("\n") + "\n";
};
function bundles() {
  return [1, 2, 3, 4, 5, 6, 7, 8].map(shard => {
    const selected = full.map((row, index) => ({ ...row, selected: shard <= 6 ? index < 8 && index % 6 === shard - 1 : reservedCohorts[shard - 7].some(name => row.eventName === `core::core$${name}`) }));
    const execution = events(selected);
    return { receipt: { schema: 1, shard, identity: structuredClone(identity), fullSha256: hash(JSON.stringify(full)), selectedSha256: hash(JSON.stringify(selected)), executionSha256: hash(execution), executedTests: selected.filter(row => row.selected).length }, full: structuredClone(full), selected, execution };
  });
}
const load = () => import(owner);

test("ASan has a substantive eight-partition evidence owner", () => assert(existsSync(owner), "missing ASan full-union owner"));

test("nightly cohort uses the installed release manifest date, not rustc's previous commit date", async () => {
  const { nightlyFromManifest } = await load();
  assert.equal(nightlyFromManifest('manifest-version = "2"\ndate = "2026-11-02"\n[pkg.rust]\nversion="1.100.0-nightly (abc 2026-10-31)"\n'), "nightly-2026-11-02");
  for (const input of ['[pkg.rust]\ndate="2026-11-02"', 'date="bad"', 'date="2026-13-01"', 'date="2026-02-30"', 'date="2026-11-02"\ndate="2026-11-03"']) assert.throws(() => nightlyFromManifest(input), /manifest date/u);
});

test("ASan reserves two cohorts and hashes the remaining inventory over six runners", async () => {
  const { partitionArgs } = await load();
  for (const mode of ["list", "run"]) {
    for (let shard = 1; shard <= 6; shard++) {
      const args = partitionArgs(mode, [], shard);
      assert.equal(args[args.indexOf("--partition") + 1], `hash:${shard}/6`);
      assert.match(args[args.indexOf("-E") + 1], /^not \(test\(=/u);
    }
    for (const shard of [7, 8]) {
      const args = partitionArgs(mode, [], shard);
      assert.ok(!args.includes("--partition"));
      assert.ok(args.includes("-E"));
    }
    const full = partitionArgs(mode, []);
    assert.ok(!full.includes("-E") && !full.includes("--partition"));
  }
});

test("ASan reserved inventory rejects missing, duplicate, renamed and ignored configured tests", async () => {
  const { verifyReservedInventory } = await load();
  assert.doesNotThrow(() => verifyReservedInventory(full));
  for (const rows of [full.slice(0, -1), [...full, reservedRows[0]], full.map(row => row === reservedRows[0] ? { ...row, eventName: row.eventName + "_renamed" } : row), full.map(row => row === reservedRows[0] ? { ...row, ignored: true, selected: false } : row)]) {
    assert.throws(() => verifyReservedInventory(rows), /ASan reserved test/u);
  }
});

test("ASan union proves all selected cases once, with default ignored cases excluded", async () => {
  const { verifyAsanUnion } = await load();
  assert.deepEqual(verifyAsanUnion(bundles(), identity), { totalTests: 13, executedTests: 12, ignoredTests: 1, shards: 8 });
});

test("ASan union rejects missing, duplicate, empty and overlapping partitions", async () => {
  const { verifyAsanUnion } = await load();
  const cases = [bundles().slice(1), [...bundles(), bundles()[0]], bundles().map((value, index) => index === 3 ? bundles()[0] : value)];
  for (const value of cases) assert.throws(() => verifyAsanUnion(value, identity));
  const empty = bundles();
  empty[0].selected.forEach(row => { row.selected = false; });
  empty[0].receipt.selectedSha256 = hash(JSON.stringify(empty[0].selected));
  assert.throws(() => verifyAsanUnion(empty, identity), /Empty runnable partition/u);
  const overlap = bundles(); overlap[3].selected = structuredClone(overlap[0].selected); overlap[3].receipt.selectedSha256 = hash(JSON.stringify(overlap[3].selected));
  assert.throws(() => verifyAsanUnion(overlap, identity), /omission or duplicate/u);
});

test("ASan union rejects changed ignored/default coverage and cross-run/toolchain/config identities", async () => {
  const { verifyAsanUnion } = await load();
  for (const key of Object.keys(identity)) {
    const value = bundles();
    value[2].receipt.identity[key] = typeof identity[key] === "object" ? { changed: "1" } : `${identity[key]}changed`;
    assert.throws(() => verifyAsanUnion(value, identity), /identity/u, key);
  }
  const hidden = bundles(); hidden[0].full[0].selected = false; hidden[0].receipt.fullSha256 = hash(JSON.stringify(hidden[0].full));
  assert.throws(() => verifyAsanUnion(hidden, identity));
  const ignored = bundles(); ignored[0].selected[8].selected = true; ignored[0].receipt.selectedSha256 = hash(JSON.stringify(ignored[0].selected));
  assert.throws(() => verifyAsanUnion(ignored, identity), /ignored tests/u);
});

test("ASan union rejects failed, retried, skipped, truncated and altered execution receipts", async () => {
  const { verifyAsanUnion } = await load();
  for (const transform of [
    text => text.replace('"event":"ok","name"', '"event":"failed","name"'),
    text => text + text,
    text => text.replace('"event":"ok","name"', '"event":"ignored","name"'),
    text => text.split("\n").slice(0, 2).join("\n"),
  ]) {
    const value = bundles(); value[0].execution = transform(value[0].execution); value[0].receipt.executionSha256 = hash(value[0].execution);
    assert.throws(() => verifyAsanUnion(value, identity));
  }
  const altered = bundles(); altered[0].execution += " ";
  assert.throws(() => verifyAsanUnion(altered, identity), /digest/u);
});

test("ASan workflow retains required name as strict full-union check with one archive and eight independently instrumented runners", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/sanitizers.yml", import.meta.url), "utf8");
  assert.match(workflow, /shard: \[1, 2, 3, 4, 5, 6, 7, 8\]/u);
  assert.match(workflow, /fail-fast: false/u);
  assert.equal(workflow.match(/name: ASan \+ LeakSanitizer \(core\)$/gmu)?.length, 1);
  assert.match(workflow, /needs: \[cohort, shards, scope\]/u);
  assert.match(workflow, /if: always\(\)/u);
  assert.match(workflow, /node scripts\/ci\/asan.mjs verify /u);
  assert.match(workflow, /node scripts\/ci\/asan.mjs smoke /u);
  assert.match(workflow, /MESH_ASAN_SHARDS_RESULT: \$\{\{ needs.shards.result \}\}/u);
  assert.ok(workflow.includes(`cargo-nextest@${versions.nextest}`));
  assert.match(workflow, /name: asan-\$\{\{ github.sha \}\}-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}-shard-\$\{\{ matrix.shard \}\}/u);
  assert.match(workflow, /pattern: asan-\$\{\{ github.sha \}\}-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}-shard-\*/u);
  const storage = workflow.split(/^      - /mu).find(step => step.startsWith("name: Preserve runner storage measurements\n"));
  assert.match(storage, /uses: actions\/upload-artifact@/u);
  assert.match(storage, /name: runner-storage-/u);
  // Only this auxiliary transport may fail; compilation, controls, execution,
  // source-bound archives and the complete union remain mandatory.
  const acceptance = workflow.replace(storage, storage.replace(/^        continue-on-error: true\n/mu, ""));
  assert.doesNotMatch(acceptance, /continue-on-error|timeout-minutes|--exclude|--skip/u);
  const config = readFileSync(new URL("../../.config/nextest-asan.toml", import.meta.url), "utf8");
  assert.equal(config.match(/^nextest-version = "([^"]+)"$/mu)?.[1], versions.nextest);
  assert.match(config, /test-threads = 1/u); assert.match(config, /retries = 0/u);
  assert.doesNotMatch(config, /terminate-after|overrides|default-filter/u);
});


const smokeNames = ["smoke-clean.jsonl", "smoke-clean.log", "smoke-intentional_asan_error.jsonl", "smoke-intentional_asan_error.log"];
function smokeFixture(directory) {
  const nextest = { crate: "mesh-asan-smoke", test_binary: "mesh_asan_smoke", kind: "lib" };
  const raw = (name, outcome) => [
    { type: "suite", event: "started", nextest },
    { type: "test", event: "started", name: `mesh-asan-smoke::mesh_asan_smoke$${name}` },
    { type: "test", event: outcome, name: `mesh-asan-smoke::mesh_asan_smoke$${name}`, ...(outcome === "failed" ? { stdout: "ERROR: AddressSanitizer: heap-use-after-free\n" } : {}) },
    { type: "suite", event: outcome, passed: outcome === "ok" ? 1 : 0, failed: outcome === "failed" ? 1 : 0, ignored: 0, nextest },
  ].map(JSON.stringify).join("\n") + "\n";
  mkdirSync(directory, { recursive: true });
  writeFileSync(path.join(directory, smokeNames[0]), raw("clean", "ok"));
  writeFileSync(path.join(directory, smokeNames[1]), "clean diagnostic\n");
  writeFileSync(path.join(directory, smokeNames[2]), raw("intentional_asan_error", "failed"));
  writeFileSync(path.join(directory, smokeNames[3]), "actual failed test diagnostic\n");
  resealSmoke(directory);
}
function resealSmoke(directory) {
  const files = Object.fromEntries(smokeNames.map(name => [name, hash(readFileSync(path.join(directory, name)))]));
  writeFileSync(path.join(directory, "smoke.json"), JSON.stringify({ schema: 1, identity, files }));
}
function temporary(t) {
  const directory = mkdtempSync(path.join(os.tmpdir(), "asan-proof-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return directory;
}

test("ASan smoke collector verifies raw channels and actual one-pass/one-error outcomes", async t => {
  const { verifySmoke } = await load();
  const root = temporary(t); smokeFixture(root);
  assert.doesNotThrow(() => verifySmoke(root, identity));
  for (const name of smokeNames) {
    const before = readFileSync(path.join(root, name));
    writeFileSync(path.join(root, name), "altered");
    assert.throws(() => verifySmoke(root, identity), /digest/u);
    rmSync(path.join(root, name));
    assert.throws(() => verifySmoke(root, identity));
    writeFileSync(path.join(root, name), before);
  }
  for (const [name, transform] of [
    [smokeNames[0], text => text.replace('"event":"ok","name"', '"event":"ignored","name"')],
    [smokeNames[2], text => text.replace('AddressSanitizer', 'ordinary-error')],
    [smokeNames[2], text => text.replace('"event":"failed","name"', '"event":"ok","name"')],
    [smokeNames[2], text => text + text],
  ]) {
    smokeFixture(root);
    writeFileSync(path.join(root, name), transform(readFileSync(path.join(root, name), "utf8")));
    resealSmoke(root);
    assert.throws(() => verifySmoke(root, identity), /smoke|execution|Selected|Unexpected/iu);
  }
  writeFileSync(path.join(root, "smoke.json"), JSON.stringify({ schema: 1, identity, cleanPassed: true, errorRejected: true, marker: "ERROR: AddressSanitizer: heap-use-after-free" }));
  assert.throws(() => verifySmoke(root, identity), /smoke/iu);
});

test("ASan artifact collector binds source, run, attempt and the receipt's actual folder shard", async t => {
  const { artifactName, collectAsan } = await load();
  const root = temporary(t);
  const expectedName = shard => `asan-${identity.sourceSha}-${identity.runId}-${identity.runAttempt}-shard-${shard}`;
  for (const bundle of bundles()) {
    assert.equal(artifactName(identity, bundle.receipt.shard), expectedName(bundle.receipt.shard));
    const directory = path.join(root, expectedName(bundle.receipt.shard));
    smokeFixture(directory);
    for (const [name, value] of [["receipt", bundle.receipt], ["full", bundle.full], ["selected", bundle.selected]]) writeFileSync(path.join(directory, `${name}.json`), JSON.stringify(value));
    writeFileSync(path.join(directory, "execution.jsonl"), bundle.execution);
  }
  assert.deepEqual(collectAsan(root, identity), { totalTests: 13, executedTests: 12, ignoredTests: 1, shards: 8 });
  const original = path.join(root, expectedName(1));
  for (const wrong of [expectedName(1).replace("-2-shard-", "-1-shard-"), "asan-renamed-shard-1"]) {
    const renamed = path.join(root, wrong); renameSync(original, renamed);
    assert.throws(() => collectAsan(root, identity), /artifacts/u);
    renameSync(renamed, original);
  }
  // A permutation preserves the old global [1,2,3,4] union but violates the
  // folder-to-receipt binding; each individual artifact must reject it.
  for (const [folder, shard] of [[1,2],[2,1]]) {
    const file = path.join(root, expectedName(folder), "receipt.json");
    const receipt = JSON.parse(readFileSync(file)); receipt.shard = shard;
    writeFileSync(file, JSON.stringify(receipt));
  }
  assert.throws(() => collectAsan(root, identity), /folder/u);
});

test("ASan compiles once and binds every consumer to the current producer archive", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/sanitizers.yml", import.meta.url), "utf8");
  const producer = workflow.split("  cohort:\n")[1].split("  shards:\n")[0];
  const consumers = workflow.split("  shards:\n")[1].split("  asan:\n")[0];
  assert.match(producer, /node scripts\/ci\/asan.mjs build tmp\/asan-build/u);
  assert.ok(producer.indexOf("node --test scripts/ci/asan-cli.test.mjs") < producer.indexOf("node scripts/ci/asan.mjs build"), "actual archive protocol must reject regressions before the large compilation");
  const native = readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8");
  assert.doesNotMatch(native, /scripts\/ci\/asan-cli\.test\.mjs/u, "the non-instrumented ASan protocol belongs to its required producer, not both native jobs");
  assert.equal(workflow.match(/node --test scripts\/ci\/asan-cli\.test\.mjs/gu)?.length, 1);
  assert.match(producer, /archive_sha256: \$\{\{ steps.build.outputs.archive_sha256 \}\}/u);
  assert.match(producer, /manifest_sha256: \$\{\{ steps.build.outputs.manifest_sha256 \}\}/u);
  assert.match(producer, /artifact_id: \$\{\{ steps.archive.outputs.artifact-id \}\}/u);
  assert.match(producer, /name: Upload this attempt's compiled ASan archive\n        id: archive\n/u);
  assert.match(producer, /compression-level: 0/u);
  assert.match(producer, /retention-days: 1/u);
  assert.doesNotMatch(consumers, /Swatinem\/rust-cache|runner-storage.mjs|cargo (?:build|test)/u);
  assert.match(producer, /asan-build-\$\{\{ github.sha \}\}-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}/u);
  assert.match(consumers, /artifact-ids: \$\{\{ needs.cohort.outputs.artifact_id \}\}/u);
  assert.doesNotMatch(consumers, /name: asan-build-/u, "consumers must use the producer's immutable ID instead of a second name lookup");
  for (const part of [consumers, workflow.split("  asan:\n")[1]]) {
    assert.match(part, /MESH_ASAN_ARCHIVE_SHA256: \$\{\{ needs.cohort.outputs.archive_sha256 \}\}/u);
    assert.match(part, /MESH_ASAN_MANIFEST_SHA256: \$\{\{ needs.cohort.outputs.manifest_sha256 \}\}/u);
  }
  assert.match(workflow, /MESH_ASAN_BUILD_RESULT: \$\{\{ needs.cohort.result \}\}/u);
});


test("ASan cohort CLI binds every Rust probe to the declared dated toolchain before exporting outputs", t => {
  const root = temporary(t);
  const pin = identity.toolchain;
  const bin = path.join(root, "toolchain/bin");
  const manifest = path.join(root, "toolchain/lib/rustlib/multirust-channel-manifest.toml");
  mkdirSync(bin, { recursive: true }); mkdirSync(path.dirname(manifest), { recursive: true });
  const probes = path.join(root, "probes.jsonl");
  const outputs = path.join(root, "outputs");
  const fake = path.join(root, "rustup.cjs");
  writeFileSync(fake, String.raw`const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.FIXTURE_PROBES, JSON.stringify(args) + '\n');
if (process.env.FIXTURE_TOOL_FAILURE === (args[0] === 'which' ? 'which' : args[2])) process.exitCode = 9;
else if (args[0] === 'which') console.log(process.env.FIXTURE_RUSTC);
else if (args[0] === 'run') console.log(args[2] + ' exact compiler fixture');
else process.exitCode = 9;
`);
  const preload = path.join(root, "preload.cjs");
  writeFileSync(preload, `const cp = require('node:child_process'); const spawn = cp.spawn;
cp.spawn = function(binary, args, options) { return binary === 'rustup' ? spawn(process.execPath, [process.env.FIXTURE_RUSTUP, ...args], options) : spawn(binary, args, options); };
require('node:module').syncBuiltinESMExports();`);
  const run = (overrides = {}) => {
    rmSync(outputs, { force: true }); rmSync(probes, { force: true });
    const env = { ...process.env, NODE_OPTIONS: `--import=${pathToFileURL(preload).href}`, MESH_ASAN_TOOLCHAIN: pin, GITHUB_OUTPUT: outputs, FIXTURE_RUSTUP: fake, FIXTURE_PROBES: probes, FIXTURE_RUSTC: path.join(bin, "rustc"), ...overrides };
    for (const key of Object.keys(env)) if (env[key] === undefined) delete env[key];
    return spawnSync(process.execPath, [fileURLToPath(owner), "cohort"], { env, encoding: "utf8", timeout: 15000 });
  };
  writeFileSync(manifest, `date="${pin.slice(8)}"\n[pkg.rust]\n`);
  const good = run();
  assert.equal(good.status, 0, good.stderr);
  assert.deepEqual(readFileSync(probes, "utf8").trim().split("\n").map(JSON.parse), [["which", "--toolchain", pin, "rustc"], ["run", pin, "rustc", "-vV"], ["run", pin, "cargo", "-vV"]]);
  assert.equal(readFileSync(outputs, "utf8"), `toolchain=${pin}\nrustc=${hash("rustc exact compiler fixture")}\ncargo=${hash("cargo exact compiler fixture")}\n`);
  for (const overrides of [{ MESH_ASAN_TOOLCHAIN: undefined }, { MESH_ASAN_TOOLCHAIN: "nightly" }, { MESH_ASAN_TOOLCHAIN: "nightly-2026-02-30" }, ... ["which", "rustc", "cargo"].map(FIXTURE_TOOL_FAILURE => ({ FIXTURE_TOOL_FAILURE })), { GITHUB_OUTPUT: undefined }]) {
    const failed = run(overrides); assert.notEqual(failed.status, 0, failed.stdout); assert.ok(!existsSync(outputs), "failure must not publish a cohort");
  }
  writeFileSync(manifest, 'date="2026-11-03"\n');
  assert.notEqual(run().status, 0); assert.ok(!existsSync(outputs));
  rmSync(manifest);
  assert.notEqual(run().status, 0); assert.ok(!existsSync(outputs));
});

test("ASan producer installs the declared reviewed nightly once before strict cohort validation and cache restore", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/sanitizers.yml", import.meta.url), "utf8");
  assert.match(workflow, /^  MESH_ASAN_TOOLCHAIN: nightly-\d{4}-\d{2}-\d{2}$/mu);
  const producer = workflow.split("  cohort:\n")[1].split("  shards:\n")[0];
  assert.equal(producer.match(/uses: dtolnay\/rust-toolchain@nightly/gu)?.length, 1);
  assert.match(producer, /toolchain: \$\{\{ env.MESH_ASAN_TOOLCHAIN \}\}\n          components: rust-src/u);
  assert.ok(producer.indexOf("components: rust-src") < producer.indexOf("asan.mjs cohort"));
  assert.ok(producer.indexOf("asan.mjs cohort") < producer.indexOf("Swatinem/rust-cache"));
  assert.match(producer, /shared-key: sanitizers-shards/u);
  for (const [key, value] of [["RUSTFLAGS", "-Zsanitizer=address"], ["RUSTDOCFLAGS", "-Zsanitizer=address"], ["ASAN_OPTIONS", "detect_leaks=1"], ["RUST_TEST_THREADS", "1"]]) assert.ok(workflow.includes(`  ${key}: "${value}"`));
});
