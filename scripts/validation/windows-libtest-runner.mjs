#!/usr/bin/env node
/** Cargo target runner: retain Cargo's cwd, dynamic loader paths and fixture env.
 * Never use nextest here: process-per-test would defeat in-process fixture locks.
 * Only reviewed crypto and isolated wait fixtures run concurrently; everything else stays
 * serial. Prefix additions require reviewing the module's entire test subtree.
 */
import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";

const concurrentFilters = [
  "ratchet::kdf::tests::", "ratchet::state::tests::", "dm::tests::",
  "channel::crypto::tests::", "channel::sender_key::tests::",
  "identity::keys::tests::", "identity::device::tests::", "identity::account::tests::",
  // These three fixtures own temporary directories, identities and ephemeral
  // loopback listeners. Their long waits can overlap without sharing sockets,
  // files or process globals. Keep the other privacy tests serial: several
  // explicitly assert sub-second responsiveness under contention.
  "node::privacy_runtime::tests::private_probe_refreshes_public_presence_before_its_lease_expires",
  "node::privacy_runtime::tests::routed_handshake_can_finish_after_three_seconds_without_losing_identity_pin",
  "node::privacy_runtime::tests::manual_presence_publication_has_one_deadline_for_multiple_stalled_devices",
];

export function parseInventory(output) {
  const names = [];
  for (const line of output.split(/\r?\n/u).filter(Boolean)) {
    const match = line.match(/^(\S+): (?:test|benchmark)$/u);
    if (!match || names.includes(match[1])) throw new Error(`Invalid libtest inventory row: ${line}`);
    names.push(match[1]);
  }
  return names;
}

export function plan(names, eligible = true) {
  const matches = (name, filter) => filter.endsWith("::") ? name.startsWith(filter) : name === filter;
  const filters = eligible ? concurrentFilters.filter(filter => names.some(name => matches(name, filter))) : [];
  // libtest matches substrings. Refuse a future name that would accidentally
  // join a reviewed module through a nested namespace instead of dropping it.
  if (names.some(name => filters.some(prefix => name.includes(prefix) && !matches(name, prefix)))) {
    throw new Error("ambiguous libtest module substring; review scheduler policy");
  }
  const concurrent = names.filter(name => filters.some(prefix => matches(name, prefix)));
  const serial = names.filter(name => !concurrent.includes(name));
  return {
    concurrent, serial,
    concurrentArgs: [...filters, "--test-threads=2"],
    serialArgs: [...filters.flatMap(prefix => ["--skip", prefix]), "--test-threads=1"],
  };
}

export function verifyPartition(expected, concurrent, serial) {
  const equal = (a, b) => JSON.stringify([...a].sort()) === JSON.stringify([...b].sort());
  if (!equal(expected.concurrent, concurrent) || !equal(expected.serial, serial)) {
    throw new Error("libtest partition mismatch: selected inventory must cover each original test exactly once");
  }
}

function command(binary, args, capture = false) {
  const result = spawnSync(binary, args, {
    stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit",
    encoding: "utf8", maxBuffer: 32 * 1024 * 1024,
    // cwd/env intentionally inherited unchanged from Cargo's target runner.
  });
  if (result.error) throw result.error;
  if (result.signal) throw new Error(`${binary} terminated by ${result.signal}`);
  return result;
}

export function run(binary, extra = []) {
  if (!binary || extra.length) throw new Error("Expected Cargo test executable without extra harness arguments");
  const list = args => {
    const result = command(binary, [...args, "--list", "--format=terse"], true);
    if (result.status !== 0) throw new Error(`libtest inventory failed (${result.status}): ${binary}`);
    return parseInventory(result.stdout);
  };
  const all = list([]);
  const schedule = plan(all, /^mesh_talk_core-[a-z0-9]+(?:\.exe)?$/u.test(path.basename(binary)));
  verifyPartition(schedule, schedule.concurrent.length ? list(schedule.concurrentArgs) : [], list(schedule.serialArgs));
  console.log(`Windows libtest schedule: ${path.basename(binary)}: ${all.length} inventoried; ${schedule.concurrent.length} at 2 threads; ${schedule.serial.length} serial (includes ignored entries)`);
  const report = {
    binary, cwd: process.cwd(), node: process.version, platform: process.platform,
    sourceSha: process.env.GITHUB_SHA ?? null,
    runnerSha256: createHash("sha256").update(readFileSync(fileURLToPath(import.meta.url))).digest("hex"),
    rustc: process.env.MESH_WINDOWS_TEST_REPORT_DIR ? command(process.env.RUSTC || "rustc", ["-Vv"], true).stdout.trim() : null,
    all, schedule, runs: [],
  };
  let status = 0;
  for (const [group, args] of [["concurrent", schedule.concurrentArgs], ["serial", schedule.serialArgs]]) {
    if (!schedule[group].length) continue;
    const started = performance.now();
    const result = command(binary, [...args, ...(process.env.MESH_TALK_TEST_TIMING === "1" ? ["--nocapture"] : [])]);
    report.runs.push({ group, status: result.status, seconds: (performance.now() - started) / 1000 });
    status ||= result.status ?? 1;
  }
  if (process.env.MESH_WINDOWS_TEST_REPORT_DIR) {
    mkdirSync(process.env.MESH_WINDOWS_TEST_REPORT_DIR, { recursive: true });
    writeFileSync(path.join(process.env.MESH_WINDOWS_TEST_REPORT_DIR, `${path.basename(binary)}-${process.pid}.json`), `${JSON.stringify(report, null, 2)}\n`);
  }
  return status;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { process.exitCode = run(process.argv[2], process.argv.slice(3)); }
  catch (error) { console.error(error.message); process.exitCode = 2; }
}
