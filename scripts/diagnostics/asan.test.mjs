import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, mkdirSync, mkdtempSync, renameSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { versions } from "../ci/windows-coverage.mjs";

const owner = new URL("../ci/asan.mjs", import.meta.url);
const hash = value => createHash("sha256").update(value).digest("hex");
const identity = { sourceSha: "a".repeat(40), sourceTree: "b".repeat(40), runId: "123", runAttempt: "2", toolchain: "nightly-2026-11-02", rustcSha256: "c".repeat(64), cargoSha256: "d".repeat(64), nextest: versions.nextest, inputsSha256: "e".repeat(64), target: "x86_64-unknown-linux-gnu", rustflags: "-Zsanitizer=address", rustdocflags: "-Zsanitizer=address", asanOptions: "detect_leaks=1", testThreads: "1", profileEnvironment: {} };
const full = Array.from({ length: 5 }, (_, index) => ({ id: `core::case_${index}`, eventName: `core::core$case_${index}`, ignored: index === 4, selected: index !== 4 }));
const events = rows => {
  const nextest = { crate: "core", test_binary: "core", kind: "lib" };
  return [ { type: "suite", event: "started", nextest }, ...rows.filter(row => row.selected).flatMap(row => [{ type: "test", event: "started", name: row.eventName }, { type: "test", event: "ok", name: row.eventName }]), { type: "suite", event: "ok", failed: 0, nextest } ].map(JSON.stringify).join("\n") + "\n";
};
function bundles() {
  return [1, 2, 3, 4].map(shard => {
    const selected = full.map((row, index) => ({ ...row, selected: index === shard - 1 }));
    const execution = events(selected);
    return { receipt: { schema: 1, shard, identity: structuredClone(identity), fullSha256: hash(JSON.stringify(full)), selectedSha256: hash(JSON.stringify(selected)), executionSha256: hash(execution), executedTests: 1 }, full: structuredClone(full), selected, execution };
  });
}
const load = () => import(owner);

test("ASan has a substantive four-partition evidence owner", () => assert(existsSync(owner), "missing ASan full-union owner"));

test("nightly cohort uses the installed release manifest date, not rustc's previous commit date", async () => {
  const { nightlyFromManifest } = await load();
  assert.equal(nightlyFromManifest('manifest-version = "2"\ndate = "2026-11-02"\n[pkg.rust]\nversion="1.100.0-nightly (abc 2026-10-31)"\n'), "nightly-2026-11-02");
  for (const input of ['[pkg.rust]\ndate="2026-11-02"', 'date="bad"', 'date="2026-13-01"', 'date="2026-02-30"', 'date="2026-11-02"\ndate="2026-11-03"']) assert.throws(() => nightlyFromManifest(input), /manifest date/u);
});

test("ASan union proves all selected cases once, with default ignored cases excluded", async () => {
  const { verifyAsanUnion } = await load();
  assert.deepEqual(verifyAsanUnion(bundles(), identity), { totalTests: 5, executedTests: 4, ignoredTests: 1, shards: 4 });
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
  const ignored = bundles(); ignored[0].selected[4].selected = true; ignored[0].receipt.selectedSha256 = hash(JSON.stringify(ignored[0].selected));
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

test("ASan workflow retains required name as strict full-union check with four independently instrumented runners", () => {
  const workflow = readFileSync(new URL("../../.github/workflows/sanitizers.yml", import.meta.url), "utf8");
  assert.match(workflow, /shard: \[1, 2, 3, 4\]/u);
  assert.match(workflow, /fail-fast: false/u);
  assert.equal(workflow.match(/name: ASan \+ LeakSanitizer \(core\)$/gmu)?.length, 1);
  assert.match(workflow, /needs: \[cohort, shards\]/u);
  assert.match(workflow, /if: always\(\)/u);
  assert.match(workflow, /node scripts\/ci\/asan.mjs verify /u);
  assert.match(workflow, /node scripts\/ci\/asan.mjs smoke /u);
  assert.match(workflow, /MESH_ASAN_SHARDS_RESULT: \$\{\{ needs.shards.result \}\}/u);
  assert.ok(workflow.includes(`cargo-nextest@${versions.nextest}`));
  assert.match(workflow, /name: asan-\$\{\{ github.sha \}\}-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}-shard-\$\{\{ matrix.shard \}\}/u);
  assert.match(workflow, /pattern: asan-\$\{\{ github.sha \}\}-\$\{\{ github.run_id \}\}-\$\{\{ github.run_attempt \}\}-shard-\*/u);
  assert.doesNotMatch(workflow, /continue-on-error|timeout-minutes|--exclude|--skip/u);
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
  assert.deepEqual(collectAsan(root, identity), { totalTests: 5, executedTests: 4, ignoredTests: 1, shards: 4 });
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
