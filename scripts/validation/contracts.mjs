/** Registered development contracts shared by selection and structural checks. */
import path from "node:path";
import { readFile, realpath, stat } from "node:fs/promises";
import { rustPackages } from "../pr-platform/rust-packages.mjs";

export const contractFiles = [
  "scripts/validation/validation.json",
  "frontend/validation.json",
  ...rustPackages.map(({ directory }) => `${directory}/validation.json`),
];

/** Read a regular repository file without following a symlink outside root. */
export async function readRepositoryFile(root, file) {
  if (
    typeof file !== "string" ||
    !file ||
    path.isAbsolute(file) ||
    file.split("/").includes("..") ||
    /[\\\x00-\x1f\x7f]/u.test(file)
  )
    throw Error(`Unsafe repository path: ${file}`);
  const base = await realpath(root);
  const resolved = await realpath(path.resolve(base, file));
  const relative = path.relative(base, resolved);
  if (
    !relative ||
    relative === ".." ||
    relative.startsWith(`..${path.sep}`) ||
    path.isAbsolute(relative)
  )
    throw Error(`Repository path escapes root: ${file}`);
  if (!(await stat(resolved)).isFile())
    throw Error(`Not a regular file: ${file}`);
  return readFile(resolved, "utf8");
}

export async function loadContracts(root) {
  const contracts = [];
  for (const file of contractFiles) {
    try {
      const parsed = JSON.parse(await readRepositoryFile(root, file));
      if (parsed.schema !== 1 || !Array.isArray(parsed.contracts))
        throw Error("expected schema 1 and a contracts array");
      contracts.push(...parsed.contracts);
    } catch (error) {
      throw Error(`Invalid validation contract: ${file}: ${error.message}`);
    }
  }
  return contracts;
}
