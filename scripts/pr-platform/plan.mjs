/** Explicit affected-test selection shared by Linux and Windows guests. */
import { realpathSync } from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
export function parseFlags(argv) {
  const flags = { base: "origin/main", coreFilters: [], coreTests: [], hostFilters: [], uiSpecs: [], planOnly: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--plan") { flags.planOnly = true; continue; }
    const key = { "--base": "base", "--core-filter": "coreFilters", "--core-test": "coreTests", "--host-filter": "hostFilters" }[arg];
    if (key) {
      const value = argv[++i];
      if (!value || value.startsWith("-") || /[\x00-\x1f\x7f]/u.test(value)) throw Error(`${arg} needs a nonempty literal value`);
      if (key === "coreTests" && !/^[A-Za-z0-9_-]+$/u.test(value)) throw Error("--core-test requires an exact Cargo integration target name");
      if (key === "base") flags.base = value;
      else flags[key].push(value);
    } else {
      if (!/^e2e\/[\w./-]+\.spec\.ts$/u.test(arg) || arg.split("/").includes("..")) throw Error(`Unknown option or unsafe Playwright spec: ${arg}`);
      flags.uiSpecs.push(arg);
    }
  }
  for (const key of ["coreFilters", "coreTests", "hostFilters", "uiSpecs"]) flags[key] = [...new Set(flags[key])];
  return flags;
}

const workflowDiagnostics = [
  "scripts/diagnostics/pr-platform.test.mjs",
  "scripts/diagnostics/create-pr.test.mjs",
  "scripts/diagnostics/pr-preflight-awake.test.mjs",
  "scripts/diagnostics/source-identity.test.mjs",
  "scripts/diagnostics/linux-target.test.mjs",
  "scripts/diagnostics/pr-preflight-output.test.mjs",
];

export function buildPlan(flags, changedFiles) {
  const sharedRust = changedFiles.some(file => /^(Cargo\.(toml|lock)|\.cargo\/)/u.test(file));
  if ((sharedRust || changedFiles.some(file => /^crates\/mesh-talk-core\/(?:src\/|tests\/|Cargo\.toml$|build\.rs$)/u.test(file))) && !flags.coreFilters.length && !flags.coreTests.length) throw Error("Backend changes require explicit --core-filter or --core-test selectors; no full-suite fallback is allowed.");
  if ((sharedRust || changedFiles.some(file => /^src-tauri\/(?:src\/|tests\/|Cargo\.toml$|build\.rs$)/u.test(file))) && !flags.hostFilters.length) throw Error("Host changes require explicit --host-filter selectors; no full-suite fallback is allowed.");
  if (changedFiles.some(file => file.startsWith("frontend/")) && !flags.uiSpecs.length) throw Error("Frontend changes require at least one affected Playwright spec (e2e/*.spec.ts); frontend unit coverage runs once in the local PR preflight.");
  return {
    schema: 1,
    coreFilters: flags.coreFilters,
    coreTests: flags.coreTests,
    hostFilters: flags.hostFilters,
    uiSpecs: flags.uiSpecs,
    frontendBuild: flags.hostFilters.length > 0,
    nodeDiagnostics: changedFiles.some(file => file.startsWith("scripts/pr-platform/") ||
      ["scripts/pr-preflight.sh", "scripts/create-pr.sh", "scripts/evals/source-identity.mjs", ...workflowDiagnostics].includes(file))
      ? [...workflowDiagnostics] : [],
  };
}

export function hasSelectedTests(plan) {
  return !!(plan.frontendBuild || plan.coreFilters.length || plan.coreTests.length || plan.uiSpecs.length || plan.nodeDiagnostics.length);
}

export function validateWindowsRoot(value) {
  const root = value.replaceAll("\\", "/").replace(/\/$/u, "");
  if (!/^D:\/[A-Za-z0-9_./-]+$/iu.test(root) || root.split("/").some(part => part === ".." || part === ".")) throw Error("Windows root must be a safe absolute D:/ path, for example D:/MeshTalk/PRPreflight");
  return `D:${root.slice(2)}`;
}

export function validatePlan(plan) {
  if (plan.schema !== 1 || !/^[a-f0-9]{40,64}$/u.test(plan.sourceSha)) throw Error("Invalid committed source SHA in platform plan");
  for (const key of ["coreFilters", "coreTests", "hostFilters", "uiSpecs", "nodeDiagnostics"]) if (!Array.isArray(plan[key])) throw Error(`Invalid plan field: ${key}`);
  for (const key of ["frontendBuild"]) if (typeof plan[key] !== "boolean") throw Error(`Invalid plan field: ${key}`);
  if (plan.nodeDiagnostics.length && JSON.stringify(plan.nodeDiagnostics) !== JSON.stringify(workflowDiagnostics)) throw Error("Invalid workflow diagnostic selection: expected the fixed test files");
  // Validate guest input again: archive/JSON transport must not broaden selection.
  parseFlags([
    ...plan.coreFilters.flatMap(filter => ["--core-filter", filter]),
    ...plan.coreTests.flatMap(target => ["--core-test", target]),
    ...plan.hostFilters.flatMap(filter => ["--host-filter", filter]),
    ...plan.uiSpecs,
  ]);
}


/** Reuse configured registry/mirror state only when its physical home is on D:. */
export function selectWindowsCargoHome(canonical, fallback) {
  const physical = typeof canonical === "string" ? canonical.replaceAll("\\", "/").replace(/^\/\/\?\//u, "") : "";
  if (/^D:\//iu.test(physical) && !/[\x00-\x1f\x7f]/u.test(physical) && !physical.split("/").some(part => part === "." || part === "..")) return `D:${physical.slice(2)}`;
  return validateWindowsRoot(fallback);
}

// Invoked from the extracted committed source before the guest profile changes.
// Only the selected path is returned; registry/proxy/credential contents stay private.
if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  if (process.argv[2] !== "--windows-cargo-home" || !process.argv[3]) throw Error("Usage: plan.mjs --windows-cargo-home D:/fallback/cargo");
  let canonical;
  try { canonical = realpathSync(process.env.CARGO_HOME || path.join(process.env.USERPROFILE || homedir(), ".cargo")); } catch { /* Missing home uses the isolated D: fallback. */ }
  console.log(selectWindowsCargoHome(canonical, process.argv[3]));
}
