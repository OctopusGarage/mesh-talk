/** Explicit dependency closure for trusted local deterministic frontend stages. */
import { createHash } from "node:crypto";
import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import { spawnSync } from "node:child_process";

export const frontendInputs = [
  "frontend/", "tools/help/", "docs/help/", "site/market/",
  "scripts/", "Cargo.toml", "package.json", "package-lock.json", ".gitignore", ".gitattributes",
  ".prettierignore", ".prettierrc", ".prettierrc.json", ".npmrc",
];

export function inputGroup(name, stage) {
  if (name !== "frontend") throw Error(`Unknown stage input group: ${name}`);
  if (!["frontend-format", "frontend-lint", "frontend-types", "frontend-build", "frontend-tests", "frontend-coverage"].includes(stage))
    throw Error(`Input reuse is not registered for stage: ${stage}`);
  return frontendInputs;
}

export async function frontendEnvironmentIdentity(root) {
  let names = [];
  try { names = (await readdir(path.join(root, "frontend"))).filter(name => /^\.env(?:\.|$)/u.test(name)).sort(); }
  catch (error) { if (error.code !== "ENOENT") throw error; }
  return Promise.all(names.map(async name => [name,
    createHash("sha256").update(await readFile(path.join(root, "frontend", name))).digest("hex")]));
}

export async function inputIdentity(root, inputs) {
  if (!Array.isArray(inputs) || !inputs.length || inputs.some(value =>
    typeof value !== "string" || !value || value.startsWith("/") ||
    value.startsWith("-") || value.split("/").includes("..") ||
    /[\\\x00-\x1f\x7f:*?\[\]{}]/u.test(value)))
    throw Error("Stage inputs must be nonempty literal repository paths");
  const paths = [...new Set(inputs)].sort();
  // Index entries include paths, modes and blob IDs. The caller permits reusable
  // receipts only in a clean worktree. Directory additions/deletions change this
  // list, including when an optional input previously had no tracked files.
  const result = spawnSync("git", ["-C", root, "ls-files", "--stage", "-z", "--",
    ...paths.map(value => `:(literal)${value.replace(/\/$/u, "")}`)],
  { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 });
  if (result.status !== 0) throw Error(result.stderr || "Stage input discovery failed");
  // Always include implementation bytes even when called from an external test
  // fixture or an older checkout lacking these files in its Git index.
  const implementation = await Promise.all(["stage.mjs", "stage-inputs.mjs"].map(
    name => readFile(new URL(name, import.meta.url))));
  const hash = createHash("sha256").update(JSON.stringify(paths)).update(result.stdout);
  for (const bytes of implementation) hash.update(bytes);
  // Vite reads ignored .env files as well as tracked sources. Their additions,
  // removal and bytes must invalidate deterministic frontend receipts too.
  if (paths.includes("frontend/")) {
    hash.update(JSON.stringify(await frontendEnvironmentIdentity(root)));
  }
  return { paths, digest: hash.digest("hex") };
}
