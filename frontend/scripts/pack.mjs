#!/usr/bin/env node
import { createHash } from "node:crypto";
import { readFile, lstat, mkdir, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, resolve, sep } from "node:path";
import { zipSync } from "fflate";
import { parsePack, referencedPackImages } from "../src/lib/pack.ts";

async function build(folder, output) {
  const source = resolve(folder);
  if (
    (await lstat(source)).isSymbolicLink() ||
    (await lstat(join(source, "manifest.json"))).isSymbolicLink()
  )
    throw new Error("Symlinks are not allowed in pack sources");
  const manifestBytes = await readFile(join(source, "manifest.json"));
  const manifest = JSON.parse(manifestBytes.toString("utf8"));
  const files = { "manifest.json": new Uint8Array(manifestBytes) };
  try {
    const credits = join(source, "credits.json");
    const stat = await lstat(credits);
    if (stat.isSymbolicLink() || !stat.isFile())
      throw new Error("credits.json must be a regular file");
    files["credits.json"] = new Uint8Array(await readFile(credits));
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  for (const path of referencedPackImages(manifest)) {
    if (
      typeof path !== "string" ||
      isAbsolute(path) ||
      !/^images\/[a-zA-Z0-9_./-]+\.(png|jpe?g|webp|gif)$/.test(path) ||
      path.split("/").some((part) => !part || part === "." || part === "..")
    ) {
      throw new Error(`Invalid image path: ${path}`);
    }
    const file = join(source, path);
    if (!resolve(file).startsWith(`${source}${sep}`))
      throw new Error(`Image escapes source folder: ${path}`);
    const parts = path.split("/");
    let parent = source;
    for (const part of parts) {
      parent = join(parent, part);
      if ((await lstat(parent)).isSymbolicLink())
        throw new Error(`Symlinks are not allowed: ${path}`);
    }
    files[path] = new Uint8Array(await readFile(file));
  }
  const bytes = zipSync(files, { level: 6 });
  const pack = parsePack(bytes);
  const destination = resolve(output ?? `${pack.id}.zip`);
  await mkdir(dirname(destination), { recursive: true });
  await writeFile(destination, bytes);
  report(pack, bytes, destination);
}

function report(pack, bytes, file) {
  const count =
    pack.kind === "avatar"
      ? pack.avatars.length
      : pack.kind === "sticker"
        ? pack.stickers.length
        : (pack.wallpapers?.length ?? Number(!!pack.wallpaper));
  console.log(`${pack.name} (${pack.kind}, v${pack.version}, ${count} images)`);
  console.log(`ZIP: ${file} (${bytes.length} bytes)`);
  console.log(`SHA-256: ${createHash("sha256").update(bytes).digest("hex")}`);
  console.log("Structure, sizes, paths, and image signatures passed.");
  console.log(
    "This check does not scan for malware, rights, or offensive imagery.",
  );
}

async function check(file) {
  const bytes = new Uint8Array(await readFile(file));
  report(parsePack(bytes), bytes, resolve(file));
}

const [command, input, output] = process.argv.slice(2);
if (
  !input ||
  !["build", "check"].includes(command) ||
  (command === "check" && output)
) {
  console.error("Usage: npm run pack:build -- <source-folder> [output.zip]");
  console.error("       npm run pack:check -- <pack.zip>");
  process.exitCode = 2;
} else {
  try {
    if (command === "build") await build(input, output);
    else await check(input);
  } catch (error) {
    console.error(
      `Pack ${command} failed: ${error instanceof Error ? error.message : error}`,
    );
    process.exitCode = 1;
  }
}
