/** Owning sidecars select acceptance evidence; existing native validator owns its meaning. */
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { validateEvidence } from "../diagnostics/hidden-contacts-report.mjs";

export const scenarioSidecars = [
  "frontend/validation.json",
  "src-tauri/validation.json",
  "crates/mesh-talk-core/validation.json",
];
const safePath = (value) =>
  typeof value === "string" &&
  /^[\w./-]+$/u.test(value) &&
  !value.startsWith("/") &&
  !value.startsWith("-") &&
  !value.split("/").includes("..");
export function selectRequirements(files, rules) {
  const browser = new Set(),
    native = new Set(),
    owners = [],
    ids = new Set();
  for (const rule of rules) {
    if (
      !rule ||
      Object.keys(rule).some(
        (key) => !["id", "paths", "browser", "native"].includes(key),
      ) ||
      !/^[a-z][a-z0-9-]*$/u.test(rule.id) ||
      ids.has(rule.id) ||
      !Array.isArray(rule.paths) ||
      !rule.paths.length ||
      !rule.paths.every(safePath)
    )
      throw Error("Invalid PR scenario contract");
    ids.add(rule.id);
    if (
      rule.browser &&
      (!Array.isArray(rule.browser) ||
        !rule.browser.every(
          (spec) => safePath(spec) && /^e2e\/.+\.spec\.ts$/u.test(spec),
        ))
    )
      throw Error(`Invalid browser requirement for ${rule.id}`);
    if (
      rule.native &&
      (!Array.isArray(rule.native) ||
        !rule.native.every((platform) =>
          ["host", "darwin", "win32", "linux"].includes(platform),
        ))
    )
      throw Error(`Invalid native requirement for ${rule.id}`);
    if (
      !files.some((file) =>
        rule.paths.some((pattern) =>
          pattern.endsWith("/") ? file.startsWith(pattern) : file === pattern,
        ),
      )
    )
      continue;
    owners.push(rule.id);
    for (const spec of rule.browser ?? []) browser.add(spec);
    for (const platform of rule.native ?? [])
      native.add(platform === "host" ? process.platform : platform);
  }
  return { browser: [...browser].sort(), native: [...native].sort(), owners };
}
export function requiredBrowserSpecs(selected, requirements) {
  return [...new Set([...selected, ...requirements.browser])];
}
export async function loadRequirements(files, root = process.cwd()) {
  const rules = [];
  for (const sidecar of scenarioSidecars) {
    const data = JSON.parse(await readFile(resolve(root, sidecar), "utf8"));
    if (data.schema !== 1 || !Array.isArray(data.prScenarios ?? []))
      throw Error(`Invalid scenario sidecar: ${sidecar}`);
    rules.push(...(data.prScenarios ?? []));
  }
  return selectRequirements(files, rules);
}
export async function verifyNativeReports(
  platforms,
  reports,
  sourceSha,
  { read = (filename) => readFile(filename, "utf8") } = {},
) {
  if (!platforms.length) return;
  const valid = new Set();
  for (const filename of reports) {
    const report = JSON.parse(await read(filename));
    if (!platforms.includes(report.platform)) continue;
    if (report.cleanSource !== true)
      throw Error(
        `Invalid native evidence ${filename}: run from an unchanged clean committed source`,
      );
    if (report.buildVerified !== true)
      throw Error(
        `Invalid native evidence ${filename}: rebuild using node scripts/validation/native-build.mjs`,
      );
    const errors = await validateEvidence(report, dirname(resolve(filename)), {
      sourceSha,
      platform: report.platform,
    });
    if (errors.length)
      throw Error(`Invalid native evidence ${filename}: ${errors.join("; ")}`);
    valid.add(report.platform);
  }
  for (const platform of platforms)
    if (!valid.has(platform))
      throw Error(
        `Missing native evidence for ${platform} at ${sourceSha}. Run the native desktop evaluation and set MESH_PR_NATIVE_REPORTS to a JSON array of report.json paths. An unavailable desktop is a blocker.`,
      );
}
export function committedFiles(root, base, source) {
  if (
    ![base, source].every((value) =>
      /^(?:[a-f0-9]{40}|[a-f0-9]{64})$/u.test(value),
    )
  )
    throw Error("Expected resolved comparison commits");
  return execFileSync(
    "git",
    ["diff", "--name-only", "-z", "--no-renames", base, source],
    { cwd: root, encoding: "utf8" },
  )
    .split("\0")
    .filter(Boolean);
}
export async function verifyRequiredNative(
  requirements,
  source,
  env = process.env,
) {
  const reports = JSON.parse(env.MESH_PR_NATIVE_REPORTS ?? "[]");
  if (
    !Array.isArray(reports) ||
    !reports.every(
      (filename) => typeof filename === "string" && filename.length > 0,
    )
  )
    throw Error("MESH_PR_NATIVE_REPORTS must be a JSON array of report paths");
  await verifyNativeReports(requirements.native, reports, source);
}
async function main() {
  const [mode, base = "origin/main"] = process.argv.slice(2);
  if (!["plan", "verify"].includes(mode) || base.startsWith("-"))
    throw Error("Usage: pr-scenarios.mjs plan|verify [base]");
  const git = (args) => execFileSync("git", args, { encoding: "utf8" }).trim();
  const source = git(["rev-parse", "HEAD"]);
  const mergeBase = git(["merge-base", source, base]);
  const files = committedFiles(process.cwd(), mergeBase, source);
  const requirements = await loadRequirements(files);
  if (mode === "plan") {
    console.log(JSON.stringify(requirements));
    return;
  }
  await verifyRequiredNative(requirements, source);
}
if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
)
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
