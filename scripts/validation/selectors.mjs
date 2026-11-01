#!/usr/bin/env node
/** Compile one workspace test configuration, then inspect libtest's real names. */
import path from "node:path";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile } from "node:fs/promises";
import { runCommand, requireCommandStdout } from "../pr-platform/guest.mjs";
import { parseFlags } from "../pr-platform/plan.mjs";
import { selectedRustPackages } from "../pr-platform/rust-packages.mjs";

export async function validateSelectors(plan, { cwd = process.cwd(), run = runCommand } = {}) {
  const results = [];
  async function checked(program, args) {
    const result = await run(program, args, { cwd, timeoutMs: 1_800_000 });
    if (result.code !== 0 || result.timedOut || result.error || result.cancelled || result.cleanupUnverified) throw Error(`Test inventory failed: ${result.error || result.output.slice(-3000)}`);
    return requireCommandStdout(result);
  }
  const packages = selectedRustPackages(plan);
  const libraries = packages.filter(pkg => plan[pkg.filters]?.length);
  if (libraries.length) {
    const output = await checked("cargo", ["test", "--locked", ...libraries.flatMap(pkg => ["-p", pkg.package]), "--lib", "--no-run", "--message-format=json"]);
    const executables = new Map();
    for (const line of output.split("\n")) {
      try { const item = JSON.parse(line); if (item.reason === "compiler-artifact" && item.executable) executables.set(item.target.name, item.executable); }
      catch { /* Cargo diagnostics need not be JSON artifacts. */ }
    }
    for (const { kind, library: name, filters: key } of libraries) {
      const filters = plan[key];
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
  for (const pkg of packages) {
    for (const target of pkg.targets ? plan[pkg.targets] : []) {
      const listed = await checked("cargo", ["test", "--locked", "-p", pkg.package, "--test", target, "--", "--list"]);
      const count = [...listed.matchAll(/^(.+): test\r?$/gmu)].length;
      if (!count) throw Error(`Integration selector matches zero tests: ${target}`);
      results.push({ kind: `${pkg.kind}-test`, target, count });
    }
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
