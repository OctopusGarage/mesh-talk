// Preserve signed APT configuration; this changes one runner-image mirror only.
import {
  lstat,
  readFile,
  readdir,
  open,
  rename,
  unlink,
} from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";

function archiveUri(uri) {
  if (!uri.toLowerCase().includes("azure.archive.ubuntu.com")) return uri;
  let parsed;
  try {
    parsed = new URL(uri);
  } catch {
    throw new Error("Unsupported Azure APT URI");
  }
  if (parsed.hostname !== "azure.archive.ubuntu.com") return uri;
  const exact = /^https?:\/\/azure\.archive\.ubuntu\.com\/ubuntu(\/?)$/.exec(
    uri,
  );
  if (!exact) throw new Error("Unsupported Azure APT URI");
  return `https://archive.ubuntu.com/ubuntu${exact[1]}`;
}

export function rewriteUbuntuAptSource(text, deb822) {
  let uriField = false;
  return text
    .split(/(?<=\n)/u)
    .map((line) => {
      const ending = /[\r\n]*$/.exec(line)[0];
      const body = line.slice(0, line.length - ending.length);
      if (!body.trim()) {
        uriField = false;
        return line;
      }
      if (/^[ \t]*#/.test(body)) return line;
      if (!deb822) {
        const row =
          /^([ \t]*deb(?:-src)?[ \t]+(?:\[[^\]\r\n]*\][ \t]+)?)(\S+)(.*)$/.exec(
            body,
          );
        if (!row) {
          if (
            /^[ \t]*deb(?:-src)?\b/.test(body) &&
            body.includes("azure.archive.ubuntu.com")
          ) {
            throw new Error("Unsupported Azure APT source line");
          }
          return line;
        }
        return `${row[1]}${archiveUri(row[2])}${row[3]}${ending}`;
      }
      const field = /^([A-Za-z][A-Za-z0-9-]*:[ \t]*)(.*)$/.exec(body);
      if (field) {
        uriField = field[1].split(":")[0].toLowerCase() === "uris";
        return uriField
          ? `${field[1]}${field[2].replace(/\S+/g, archiveUri)}${ending}`
          : line;
      }
      return uriField && /^[ \t]/.test(body)
        ? body.replace(/\S+/g, archiveUri) + ending
        : line;
    })
    .join("");
}

async function regularFile(path) {
  try {
    const info = await lstat(path);
    if (!info.isFile()) throw new Error("APT source must be a regular file");
    return info;
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw error;
  }
}

export async function prepareUbuntuAptSources({
  root = "/",
  env = process.env,
  platform = process.platform,
} = {}) {
  if (
    platform !== "linux" ||
    env.GITHUB_ACTIONS !== "true" ||
    env.RUNNER_ENVIRONMENT !== "github-hosted" ||
    env.RUNNER_OS !== "Linux"
  ) {
    throw new Error(
      "APT source preparation requires a disposable GitHub-hosted Linux runner",
    );
  }
  const release = await readFile(join(root, "etc/os-release"), "utf8");
  if (!/^ID=(?:ubuntu|"ubuntu"|'ubuntu')[ \t]*$/m.test(release)) {
    throw new Error("APT source preparation requires Ubuntu");
  }
  const apt = join(root, "etc/apt");
  const files = [join(apt, "sources.list")];
  const directory = join(apt, "sources.list.d");
  try {
    const info = await lstat(directory);
    if (!info.isDirectory())
      throw new Error("APT sources directory must not be a symlink");
    files.push(
      ...(await readdir(directory))
        .filter((name) => /\.(?:sources|list)$/.test(name))
        .sort()
        .map((name) => join(directory, name)),
    );
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const changes = [];
  // Validate every candidate before replacing any source file.
  for (const path of files) {
    const info = await regularFile(path);
    if (!info) continue;
    const before = await readFile(path, "utf8");
    const after = rewriteUbuntuAptSource(before, path.endsWith(".sources"));
    if (after !== before) changes.push({ path, info, after });
  }
  for (const { path, info, after } of changes) {
    const temporary = join(dirname(path), `.mesh-talk-apt-${randomUUID()}`);
    try {
      const file = await open(temporary, "wx", info.mode & 0o7777);
      try {
        await file.writeFile(after, "utf8");
        await file.chown(info.uid, info.gid);
        await file.chmod(info.mode & 0o7777);
        await file.sync();
      } finally {
        await file.close();
      }
      await rename(temporary, path);
    } finally {
      await unlink(temporary).catch((error) => {
        if (error.code !== "ENOENT") throw error;
      });
    }
  }
  return changes.length;
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  if (process.argv.length !== 2)
    throw new Error("Usage: ubuntu-apt-source.mjs");
  console.log(
    `Prepared official Ubuntu archive in ${await prepareUbuntuAptSources()} APT source file(s)`,
  );
}
