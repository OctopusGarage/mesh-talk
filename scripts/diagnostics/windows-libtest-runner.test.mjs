import assert from "node:assert/strict";
import test from "node:test";
import { parseInventory, plan, verifyPartition } from "../validation/windows-libtest-runner.mjs";

test("unknown tests stay serial; only reviewed full module paths become concurrent", () => {
  const names = ["ratchet::state::tests::roundtrip", "node::tests::network", "future::test"];
  const result = plan(names);
  assert.deepEqual(result.concurrent, [names[0]]);
  assert.deepEqual(result.serial, names.slice(1));
  assert.equal(result.concurrentArgs.at(-1), "--test-threads=2");
  assert.equal(result.serialArgs.at(-1), "--test-threads=1");
});

test("inventory parsing is fail closed and retains ignored tests and benchmarks", () => {
  assert.deepEqual(parseInventory("a::test: test\nb::bench: benchmark\n"), ["a::test", "b::bench"]);
  assert.deepEqual(parseInventory(""), []);
  for (const input of ["broken", "a: test\na: test", "a: test\n1 test, 0 benchmarks", "a: mystery"]) {
    assert.throws(() => parseInventory(input), /inventory/i);
  }
});

test("real selection must equal expected partition, with neither omissions nor duplicates", () => {
  const result = plan(["dm::tests::roundtrip", "node::tests::network"]);
  verifyPartition(result, result.concurrent, result.serial);
  assert.throws(() => verifyPartition(result, [], result.serial), /partition/);
  assert.throws(() => verifyPartition(result, result.concurrent, [...result.serial, ...result.concurrent]), /partition/);
  assert.throws(() => verifyPartition(result, [...result.concurrent, "new"], result.serial), /partition/);
});

test("serial substring exclusions cannot silently drop nested path lookalikes", () => {
  assert.throws(() => plan(["dm::tests::roundtrip", "wrapper::dm::tests::roundtrip"]), /ambiguous/);
});

test("unreviewed harnesses remain entirely serial, including crypto-shaped names", () => {
  const names = ["dm::tests::first", "unknown::new", "bench"];
  const result = plan(names, false);
  assert.deepEqual(result.concurrent, []);
  assert.deepEqual(result.serial, names);
  assert.deepEqual(result.serialArgs, ["--test-threads=1"]);
});

test("exact wait fixture selectors reject future substring collisions", () => {
  const selected = "node::privacy_runtime::tests::manual_presence_publication_has_one_deadline_for_multiple_stalled_devices";
  assert.deepEqual(plan([selected, "node::privacy_runtime::tests::tight_timing"]).concurrent, [selected]);
  assert.throws(() => plan([selected, `${selected}_new_variant`]), /ambiguous/);
});

import { mkdtempSync, mkdirSync, writeFileSync, rmSync, readdirSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

test("Cargo runner prototype preserves runtime contract, concurrent group, ignored tests and failure", t => {
  const root = mkdtempSync(path.join(tmpdir(), "mesh-windows-schedule-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(path.join(root, "src"));
  writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname="mesh-talk-core"\nversion="0.1.0"\nedition="2024"\n');
  writeFileSync(path.join(root, "src/lib.rs"), `
#[cfg(test)] mod dm { pub mod tests {
    static BARRIER: std::sync::Barrier = std::sync::Barrier::new(2);
    #[test] fn first() { BARRIER.wait(); }
    #[test] fn second() { BARRIER.wait(); }
}}
#[cfg(test)] mod unknown {
    #[test] fn serial() {
        assert_eq!(std::env::current_dir().unwrap(), std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()));
        assert!(std::env::var_os("CARGO").is_some());
        assert!(std::env::var_os("MESH_FIXTURE_FAIL").is_none());
    }
    #[test] #[ignore] fn ignored() { panic!("must remain ignored"); }
}
`);
  const runner = fileURLToPath(new URL("../validation/windows-libtest-runner.mjs", import.meta.url));
  const args = ["test", "--config", `target.'cfg(all())'.runner=${JSON.stringify([process.execPath, runner])}`];
  const options = { cwd: root, encoding: "utf8", timeout: 30000, env: { ...process.env, RUST_TEST_THREADS: "1", GITHUB_SHA: "a".repeat(40), MESH_WINDOWS_TEST_REPORT_DIR: path.join(root, "reports") } };
  const result = spawnSync("cargo", args, options);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.match(result.stdout, /4 inventoried; 2 at 2 threads; 2 serial/);
  assert.match(result.stdout, /1 passed; 0 failed; 1 ignored/);
  const reports = readdirSync(path.join(root, "reports"));
  assert.equal(reports.length, 1);
  const report = JSON.parse(readFileSync(path.join(root, "reports", reports[0]), "utf8"));
  assert.equal(report.sourceSha, "a".repeat(40));
  assert.match(report.rustc, /rustc/);
  assert.match(report.runnerSha256, /^[a-f0-9]{64}$/);
  assert.equal(report.all.length, 4);
  assert.deepEqual(report.runs.map(run => run.group), ["concurrent", "serial"]);
  assert.ok(report.runs.every(run => run.status === 0 && run.seconds >= 0));
  const failure = spawnSync("cargo", args, { ...options, env: { ...options.env, MESH_FIXTURE_FAIL: "1" } });
  assert.notEqual(failure.status, 0);
  assert.match(failure.stdout, /unknown::serial.*FAILED/);
});
