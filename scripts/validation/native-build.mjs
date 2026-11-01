#!/usr/bin/env node
/** Bind native evaluation executables and bundled frontend bytes to clean source. */
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { lstat, mkdir, readFile, readdir, realpath, rename, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { runCommand, requireCommandStdout } from "../pr-platform/guest.mjs";

const hash = value => createHash("sha256").update(value).digest("hex");
export const nativeBuildManifest = root => path.resolve(root, "target/validation/native-build.json");
export function nativeSourceIdentity(root = process.cwd()) {
  const git = args => execFileSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true }).trim();
  return { sourceSha: git(["rev-parse", "HEAD"]), cleanSource: git(["status", "--porcelain"]) === "" };
}
function artifactPaths(root) {
  const suffix = process.platform === "win32" ? ".exe" : "";
  return {
    app: path.resolve(root, `target/release/examples/native-contact-eval${suffix}`),
    node: path.resolve(root, `target/release/mesh-talk-node${suffix}`),
    productionApp: path.resolve(root, `target/release/mesh-talk${suffix}`),
    frontend: path.resolve(root, "frontend/dist"),
  };
}

/** Directory names and bytes both participate; symbolic tree entries are rejected. */
export async function nativeArtifactDigest(filename) {
  const info = await lstat(filename);
  if (info.isFile()) return hash(await readFile(filename));
  if (!info.isDirectory()) throw Error(`Unsupported native artifact: ${filename}`);
  const entries = [];
  for (const name of (await readdir(filename)).sort()) entries.push([name, await nativeArtifactDigest(path.join(filename, name))]);
  return hash(JSON.stringify(entries));
}
async function artifactsAt(paths) {
  return Object.fromEntries(await Promise.all(Object.entries(paths).map(async ([key, filename]) => {
    const canonical = await realpath(filename);
    return [key, { path: canonical, sha256: await nativeArtifactDigest(canonical) }];
  })));
}
function requireSource(root, expected) {
  const current = nativeSourceIdentity(root);
  if (!current.cleanSource) throw Error(expected ? "Source changed during native build: working tree is dirty" : "Native build requires clean committed source");
  if (!/^[a-f0-9]{40,64}$/u.test(current.sourceSha) || (expected && current.sourceSha !== expected)) throw Error("Source changed during native build");
  return current.sourceSha;
}

async function requireCargoExecutables(output, required, paths, root) {
  const records = [];
  for (const line of output.split("\n")) {
    try {
      const record = JSON.parse(line);
      if (record.reason === "compiler-artifact" && record.executable) records.push(record);
    } catch { /* Cargo may interleave plain diagnostics with its JSON messages. */ }
  }
  const suffix = process.platform === "win32" ? ".exe" : "";
  for (const [key, kind] of Object.entries(required)) {
    const name = path.basename(paths[key], suffix);
    const matches = records.filter(record => record.target?.name === name && record.target.kind?.includes(kind));
    if (!matches.length) throw Error(`Cargo did not report the required ${kind} executable artifact: ${name}`);
    const expected = await realpath(paths[key]);
    for (const record of matches) {
      if (await realpath(path.resolve(root, record.executable)) !== expected) {
        throw Error(`Cargo executable ${name} was emitted outside the expected target/release layout; remove CARGO_BUILD_TARGET or Cargo build.target configuration for native evaluation`);
      }
    }
  }
}

export async function buildNative({ root = process.cwd(), env = process.env, run = runCommand, log = console.log } = {}) {
  root = path.resolve(root);
  const manifestFile = nativeBuildManifest(root);
  // Failed attempts supersede earlier proof, even when source stayed at the same SHA.
  await rm(manifestFile, { force: true });
  const sourceSha = requireSource(root);
  if (env.EVAL_SOURCE_SHA && env.EVAL_SOURCE_SHA !== sourceSha) throw Error("Requested native build source SHA differs from HEAD");
  const paths = artifactPaths(root);
  const commands = [
    { program: "npm", args: ["run", "build"], cwd: path.join(root, "frontend") },
    { program: "cargo", args: ["build", "--release", "--locked", "--message-format=json", "--target-dir", "target", "-p", "mesh-talk", "--bin", "mesh-talk", "-p", "mesh-talk-core", "--bin", "mesh-talk-node"], cwd: root, executables: { productionApp: "bin", node: "bin" } },
    { program: "cargo", args: ["build", "--release", "--locked", "--message-format=json", "--target-dir", "target", "-p", "mesh-talk", "--example", "native-contact-eval"], cwd: root, executables: { app: "example" } },
  ];
  for (const command of commands) {
    log(`[native-build] ${command.program} ${command.args.join(" ")}`);
    const result = await run(command.program, command.args, { cwd: command.cwd, env, timeoutMs: 1_800_000 });
    if (result.output) log(result.output);
    if (result.code !== 0 || result.timedOut || result.error || result.cancelled || result.cleanupUnverified) throw Error(`Native build failed: ${result.error || result.output || (result.timedOut ? "command timed out" : `exit ${result.code}`)}`);
    if (command.executables) await requireCargoExecutables(requireCommandStdout(result), command.executables, paths, root);
    requireSource(root, sourceSha);
  }
  const artifacts = await artifactsAt(paths);
  requireSource(root, sourceSha);
  const manifest = { schema: 1, sourceSha, platform: process.platform, architecture: process.arch, builtAt: new Date().toISOString(), artifacts };
  await mkdir(path.dirname(manifestFile), { recursive: true });
  const temporary = `${manifestFile}.${process.pid}.tmp`;
  await writeFile(temporary, `${JSON.stringify(manifest, null, 2)}\n`);
  await rename(temporary, manifestFile);
  log(`[native-build] verified build manifest: ${manifestFile}`);
  return manifest;
}

/** Development runs may proceed on false; publication must require buildVerified. */
export async function verifyNativeBuild({ root = process.cwd(), sourceSha, appBinary, nodeBinary, expectedManifestDigest, manifestFile = nativeBuildManifest(root) } = {}) {
  root = path.resolve(root);
  try {
    const current = nativeSourceIdentity(root);
    if (!current.cleanSource || (sourceSha && sourceSha !== current.sourceSha)) throw Error("Native proof source must match clean current HEAD");
    const bytes = await readFile(manifestFile);
    if (expectedManifestDigest && hash(bytes) !== expectedManifestDigest) throw Error("Native build manifest changed during evaluation");
    const manifest = JSON.parse(bytes);
    if (manifest.schema !== 1 || manifest.sourceSha !== current.sourceSha) throw Error("Native build proof source SHA differs from current source");
    if (manifest.platform !== process.platform || manifest.architecture !== process.arch) throw Error("Native build proof platform or architecture differs from this host");
    const paths = artifactPaths(root);
    paths.app = appBinary ?? paths.app;
    paths.node = nodeBinary ?? paths.node;
    const artifacts = await artifactsAt(paths);
    for (const [kind, actual] of Object.entries(artifacts)) {
      const expected = manifest.artifacts?.[kind];
      if (expected?.path !== actual.path) throw Error(`Native ${kind} executable/artifact path differs from build proof`);
      if (expected.sha256 !== actual.sha256) throw Error(`Native ${kind} artifact digest differs from build proof`);
    }
    const final = nativeSourceIdentity(root);
    if (!final.cleanSource || final.sourceSha !== current.sourceSha) throw Error("Native proof source changed during verification");
    return { buildVerified: true, manifestFile: path.resolve(manifestFile), manifestDigest: hash(bytes), sourceSha: manifest.sourceSha, platform: manifest.platform, architecture: manifest.architecture, artifacts };
  } catch (error) {
    return { buildVerified: false, manifestFile: path.resolve(manifestFile), error: error.message };
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  if (process.argv.length !== 2) throw Error("Usage: node scripts/validation/native-build.mjs");
  buildNative().catch(error => { console.error(error.message); process.exitCode = 1; });
}
