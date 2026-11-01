import assert from "node:assert/strict";
import test from "node:test";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { command, inventory, verifyPartition, verifyExecution, validateLcov, verifyIdentity, verifyTargets, validateProfilePath, verifyLayout } from "../ci/windows-coverage.mjs";

function commandFixture(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), "windows-coverage-evidence-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return { binary: process.execPath, receipt: path.join(root, "execution"), progress: path.join(root, "progress.jsonl"), phase: "execution" };
}

test("cancelled commands retain live output and pre-child phase without accepting partial execution", { timeout: 10000 }, async t => {
  const options = commandFixture(t);
  const controller = new AbortController();
  t.after(() => controller.abort());
  const started = JSON.stringify({ type: "test", event: "started", name: "pkg::pkg$a" });
  const program = `
    const fs = require('node:fs');
    const phase = JSON.parse(fs.readFileSync(${JSON.stringify(options.progress)}, 'utf8').split('\\n')[0]);
    if (phase.event !== 'started' || phase.phase !== 'execution') process.exit(9);
    process.stdout.write(${JSON.stringify(`${started}\n`)});
    process.stderr.write('still-running\\n');
    setInterval(() => {}, 1000);
  `;
  const completion = command(["-e", program], { ...options, signal: controller.signal });
  const rejected = assert.rejects(completion, /interrupted/u);
  const deadline = performance.now() + 5000;
  while (readFileSync(`${options.receipt}.log`, "utf8") !== "still-running\n") {
    assert.ok(performance.now() < deadline, "Child did not publish its live output");
    await delay(10);
  }
  assert.equal(readFileSync(`${options.receipt}.jsonl`, "utf8"), `${started}\n`);
  const before = readFileSync(options.progress, "utf8").trim().split("\n").map(JSON.parse);
  assert.deepEqual(before.map(row => row.event), ["started", "spawned"]);
  assert.throws(() => verifyExecution(`${started}\n`, inventory(listing(["a"]))), /execution/u);
  controller.abort();
  await rejected;
  const after = readFileSync(options.progress, "utf8").trim().split("\n").map(JSON.parse);
  assert.equal(after.at(-1).event, "closed");
  assert.equal(after.at(-1).failed, true);
  assert.equal(after.at(-1).stdoutBytes, Buffer.byteLength(`${started}\n`));
  assert.equal(readFileSync(`${options.receipt}.log`, "utf8"), "still-running\n");
  assert.throws(() => process.kill(before[1].pid, 0));
});

test("nonzero exits and spawn failures retain evidence and reject before coverage acceptance", async t => {
  const options = commandFixture(t);
  await assert.rejects(command(["-e", "process.stdout.write('partial\\n'); process.stderr.write('failed\\n'); process.exitCode = 7;"], options), /failed: 7/u);
  assert.equal(readFileSync(`${options.receipt}.jsonl`, "utf8"), "partial\n");
  assert.equal(readFileSync(`${options.receipt}.log`, "utf8"), "failed\n");
  assert.equal(JSON.parse(readFileSync(options.progress, "utf8").trim().split("\n").at(-1)).code, 7);
  await assert.rejects(command([], { ...options, binary: path.join(path.dirname(options.receipt), "missing-executable") }), /ENOENT/u);
  assert.equal(JSON.parse(readFileSync(options.progress, "utf8").trim().split("\n").at(-1)).failed, true);
  assert.equal(existsSync(path.join(path.dirname(options.receipt), "receipt.json")), false);
});

test("successful streaming preserves exact Unicode output and closes receipt files", async t => {
  const options = commandFixture(t);
  const text = "retained ✓\n";
  const result = await command(["-e", `const b = Buffer.from(${JSON.stringify(text)}); process.stdout.write(b.subarray(0, 10)); setTimeout(() => process.stdout.write(b.subarray(10)), 10);`], options);
  assert.equal(result, text);
  assert.equal(readFileSync(`${options.receipt}.jsonl`, "utf8"), text);
  const closed = JSON.parse(readFileSync(options.progress, "utf8").trim().split("\n").at(-1));
  assert.equal(closed.failed, false);
  assert.equal(closed.code, 0);
  // In particular, Windows must be able to retire output after handles close.
  rmSync(`${options.receipt}.jsonl`);
});

test("output overflow is rejected before receipt writes and cannot grow during cancellation", { timeout: 10000 }, async t => {
  const options = commandFixture(t);
  await assert.rejects(command(["-e", `
    process.on('SIGTERM', () => process.stdout.write('later-output'));
    process.stdout.write(Buffer.alloc(256));
    setInterval(() => process.stdout.write('more-output'), 10);
  `], { ...options, maxOutputBytes: 64 }), /output exceeded/u);
  assert.equal(readFileSync(`${options.receipt}.jsonl`).length, 0);
  const closed = JSON.parse(readFileSync(options.progress, "utf8").trim().split("\n").at(-1));
  assert.equal(closed.failed, true);
  assert.equal(closed.stdoutBytes, 0);
});

const listing = selected => ({ "test-count": 3, "rust-suites": {
  lib: { "package-name": "pkg", "package-id": "pkg-id", "binary-id": "lib", "binary-name": "pkg", kind: "lib", status: "listed", testcases: Object.fromEntries(["a", "b", "ignored"].map(name => [name, { kind: "test", ignored: name === "ignored", "filter-match": { status: selected.includes(name) ? "matches" : "mismatch", reason: name === "ignored" ? "ignored" : "partition" } }])) },
} });

test("official listings retain ignored rows and partition every runnable case exactly once", () => {
  const all = inventory(listing(["a", "b"]));
  assert.equal(all.length, 3);
  assert.equal(all.filter(row => row.ignored).length, 1);
  verifyPartition(all, [inventory(listing(["a"])), inventory(listing(["b"]))]);
  assert.throws(() => verifyPartition(all, [inventory(listing(["a"])), inventory(listing(["a"]))]), /partition/);
  assert.throws(() => verifyPartition(all, [inventory(listing(["a"])), inventory(listing([]))]), /partition/);
  assert.throws(() => inventory({ ...listing([]), "test-count": 4 }), /count/);
});

test("execution receipts reject missing, duplicate, unexpected, failed and ignored runnable cases", () => {
  const expected = inventory(listing(["a"]));
  const ok = [
    { type: "suite", event: "started", nextest: { crate: "pkg", test_binary: "pkg", kind: "lib" } },
    { type: "test", event: "started", name: "pkg::pkg$a" },
    { type: "test", event: "ok", name: "pkg::pkg$a" },
    { type: "suite", event: "ok", failed: 0, nextest: { crate: "pkg", test_binary: "pkg", kind: "lib" } },
  ].map(event => JSON.stringify(event)).join("\n");
  verifyExecution(ok, expected);
  for (const text of ["", `${ok}\n${ok}`, ok.replace("$a", "$b"), ok.replace('"ok"', '"failed"'), ok.replace('"ok"', '"ignored"'), ok.split("\n").slice(0, -1).join("\n")]) assert.throws(() => verifyExecution(text, expected), /execution/);
});

test("partial LCOV requires source rows, valid measured lines and positive coverage", () => {
  validateLcov("SF:/source/lib.rs\nDA:1,1\nDA:2,0\nend_of_record\n");
  for (const text of ["", "SF:/source/lib.rs\nDA:1,0\nend_of_record\n", "SF:/source/lib.rs\nDA:1,-1\nend_of_record\n", "SF:/source/lib.rs\nDA:1,1\n"]) assert.throws(() => validateLcov(text), /coverage/);
});

test("consumer identity rejects source, toolchain, lock, root, feature/profile and run drift", () => {
  const original = { sourceSha: "a".repeat(40), root: "/work", rustc: "rustc same", lockSha256: "x", runId: "123", runAttempt: "1", buildArgs: ["--workspace", "--locked"], features: ["feature-a"] };
  verifyIdentity(original, structuredClone(original));
  for (const field of Object.keys(original)) assert.throws(() => verifyIdentity(original, { ...original, [field]: "changed" }), /identity/);
});

test("default Cargo test targets cannot silently disappear from the archive", () => {
  const metadata = { workspace_members: ["pkg-id"], resolve: { nodes: [{ id: "pkg-id", features: [] }] }, packages: [{ id: "pkg-id", targets: [{ name: "pkg", kind: ["rlib", "cdylib"], test: true }, { name: "disabled", kind: ["example"], test: false }] }] };
  verifyTargets(listing(["a", "b"]), metadata);
  assert.throws(() => verifyTargets({ "rust-suites": {} }, metadata), /target/);
});


test("instrumented listing cannot emit profiles into source package directories", () => {
  validateProfilePath(`${process.cwd()}/target/coverage-%p-%8m.profraw`, process.cwd());
  for (const file of [undefined, "default.profraw", `${process.cwd()}/crates/default.profraw`, `${process.cwd()}/target-other/default.profraw`]) assert.throws(() => validateProfilePath(file, process.cwd()), /LLVM_PROFILE_FILE/);
});


test("archive layout is anchored exactly to the metadata v1 debug/deps directory", () => {
  const target = `${process.cwd()}/target`;
  const metadata = binary => ({ "rust-build-meta": { "target-directory": target, "build-directory": target }, "rust-binaries": { lib: { "binary-path": binary } } });
  verifyLayout(metadata(`${target}/debug/deps/unit`));
  const example = metadata(`${target}/debug/examples/example`);
  example["rust-binaries"].lib.kind = "example";
  verifyLayout(example);
  for (const binary of [`${target}/custom-build/debug/deps/unit`, `${target}/release/deps/unit`, "relative/debug/deps/unit"]) assert.throws(() => verifyLayout(metadata(binary)), /layout/);
  const separate = metadata(`${target}/debug/deps/unit`);
  separate["rust-build-meta"]["build-directory"] = `${target}/separate`;
  assert.throws(() => verifyLayout(separate), /separate build/);
});


test("legacy Windows cache compatibility overrides stay outside runtime execution", () => {
  const source = readFileSync(new URL("../../.github/workflows/windows-workspace.yml", import.meta.url), "utf8");
  const globals = source.split("\njobs:")[0];
  assert.match(globals, /CARGO_TERM_COLOR: never/u);
  assert.match(globals, /RUST_TEST_THREADS: '1'/u);
  const cache = source.split(/^      - /mu).find(step => step.startsWith("uses: Swatinem/rust-cache@v2\n"));
  assert.ok(cache);
  assert.match(cache, /shared-key: workspace-checks-checks/u);
  assert.match(cache, /env:\n          CARGO_TERM_COLOR: always\n          RUST_TEST_THREADS: ''/u);
  const runtime = source.replace(cache, "");
  assert.doesNotMatch(runtime, /CARGO_TERM_COLOR: always|RUST_TEST_THREADS: ''/u);
  assert.match(runtime, /cargo test --workspace --doc --locked/u);
});
