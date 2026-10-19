#!/usr/bin/env node
/** Compile one workspace test configuration, then inspect libtest's real names. */
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile } from "node:fs/promises";
import { runCommand } from "../pr-platform/guest.mjs";
import { parseFlags } from "../pr-platform/plan.mjs";

export async function validateSelectors(plan, { cwd = process.cwd(), run = runCommand } = {}) {
  const results = [];
  async function checked(program, args) {
    const result = await run(program, args, { cwd, timeoutMs: 1_800_000, detached: false });
    if (result.code !== 0 || result.timedOut) throw Error(`Test inventory failed: ${result.error || result.output.slice(-3000)}`);
    return result.output;
  }
  if (plan.coreFilters.length || plan.hostFilters.length) {
    const output = await checked("cargo", ["test", "--locked", "--workspace", "--lib", "--no-run", "--message-format=json"]);
    const executables = new Map();
    for (const line of output.split("\n")) {
      try { const item = JSON.parse(line); if (item.reason === "compiler-artifact" && item.executable) executables.set(item.target.name, item.executable); }
      catch { /* Cargo diagnostics need not be JSON artifacts. */ }
    }
    for (const [kind, name, filters] of [["core", "mesh_talk_core", plan.coreFilters], ["host", "mesh_talk", plan.hostFilters]]) {
      if (!filters.length) continue;
      if (!executables.has(name)) throw Error(`Missing ${kind} test executable`);
      const listed = await checked(executables.get(name), ["--list"]);
      const names = [...listed.matchAll(/^(.+): test\r?$/gmu)].map(match => match[1]);
      for (const filter of filters) {
        const matched = names.filter(name => name.includes(filter));
        if (!matched.length) throw Error(`Selector matches zero ${kind} tests on ${process.platform}: ${filter}. Choose a test name or module available on the preparing platform.`);
        results.push({ kind, filter, count: matched.length });
      }
    }
  }
  for (const target of plan.coreTests) {
    const listed = await checked("cargo", ["test", "--locked", "-p", "mesh-talk-core", "--test", target, "--", "--list"]);
    const count = [...listed.matchAll(/^(.+): test\r?$/gmu)].length;
    if (!count) throw Error(`Integration selector matches zero tests: ${target}`);
    results.push({ kind: "core-test", target, count });
  }
  return results;
}
async function main() {
  const selected = await validateSelectors(parseFlags(process.argv.slice(2)));
  await mkdir("target/validation", { recursive: true });
  await writeFile("target/validation/selector-inventory.json", JSON.stringify(selected, null, 2));
  for (const result of selected) console.log(`${result.kind}: ${result.filter || result.target}: ${result.count} tests available`);
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main().catch(error => { console.error(error.message); process.exitCode = 1; });
