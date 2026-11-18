// No-prompt macOS coverage control; never used by the desktop application.
import assert from "node:assert/strict";
import { copyFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { command, verifyLayout } from "./windows-coverage.mjs";
import { readVersion } from "../release/version.mjs";

export const nativeCase = "notifications::tests::mac_packaged_permission_and_delivery";
const identifier = "com.mesh-talk.notification-validation";
export function selectNotificationBinary(metadata, { target, packageDirectory, version }) {
  assert.equal(metadata["rust-build-meta"]?.["target-directory"], target, "Wrong coverage target");
  const binaries = Object.values(metadata["rust-binaries"] ?? {});
  assert.equal(binaries.length, 1, "Require exactly one Host library binary");
  const [binary] = binaries;
  assert.equal(binary.kind, "lib");
  assert.equal(binary["binary-name"], "mesh_talk");
  assert.equal(binary["package-id"], `path+${pathToFileURL(packageDirectory).href}#mesh-talk@${version}`);
  verifyLayout(metadata);
  assert.match(path.basename(binary["binary-path"]), /^mesh_talk-[a-zA-Z0-9]+$/u, "Wrong native Host executable");
  return binary["binary-path"];
}
export function verifyNotificationExecution(output) {
  assert.ok(output.includes(`test ${nativeCase} ... ok`), "Native notification case did not pass");
  assert.match(output, /test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;/u, "Empty or incomplete native notification execution");
}
export function notificationControlEnvironment(env, platform) {
  assert.equal(platform, "darwin", "Native control requires macOS");
  assert.equal(env.GITHUB_ACTIONS, "true", "Native control requires GitHub Actions");
  assert.equal(env.RUNNER_ENVIRONMENT, "github-hosted", "Native control requires a disposable hosted runner");
  assert.equal(env.RUNNER_OS, "macOS");
  return { ...env, MESH_NOTIFICATION_NATIVE_REQUEST: "0", MESH_NOTIFICATION_NATIVE_EXPECT: "unconfigured", MESH_NOTIFICATION_NATIVE_DELIVERY: "reference" };
}
// Kept separate for an actual Mach-O/bundle protocol fixture without native APIs.
export async function runBundledNotificationControl(binary, directory, run, env) {
  const app = path.join(directory, "NotificationControl.app");
  await mkdir(path.join(app, "Contents/MacOS"), { recursive: true });
  const executable = path.join(app, "Contents/MacOS/notification-control");
  await copyFile(binary, executable);
  await writeFile(path.join(app, "Contents/Info.plist"), `<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>${identifier}</string><key>CFBundleExecutable</key><string>notification-control</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleVersion</key><string>1</string></dict></plist>`);
  await run("codesign", ["--force", "--sign", "-", "--identifier", identifier, app]);
  await run("codesign", ["--verify", "--strict", app]);
  const output = await run(executable, ["--exact", nativeCase, "--ignored", "--nocapture"], env);
  verifyNotificationExecution(output);
  return output;
}
async function main() {
  const env = notificationControlEnvironment(process.env, process.platform);
  const root = path.resolve("..");
  const target = path.join(root, "target/llvm-cov-target");
  assert.equal(env.CARGO_LLVM_COV_TARGET_DIR, target, "Source the current llvm-cov environment first");
  assert.ok(path.isAbsolute(env.LLVM_PROFILE_FILE ?? "") && !path.relative(target, env.LLVM_PROFILE_FILE).startsWith(".."), "Profiles must accumulate in the current coverage target");
  const controller = new AbortController();
  const directory = await mkdtemp(path.join(env.RUNNER_TEMP, "mesh-notification-control-"));
  const timer = setTimeout(() => controller.abort(), 120000);
  const run = (binary, args, childEnv = env) => command(args, { binary, env: childEnv, signal: controller.signal, maxOutputBytes: 4 * 1024 * 1024 });
  try {
    assert.equal((await run("git", ["rev-parse", "HEAD"])).trim(), env.GITHUB_SHA, "Native control source changed");
    assert.equal((await run("git", ["status", "--porcelain", "--untracked-files=no"])).trim(), "", "Native control tracked source is dirty");
    const metadata = JSON.parse(await run("cargo", ["nextest", "list", "--locked", "-p", "mesh-talk", "--lib", "--message-format", "json", "--list-type", "binaries-only", "--user-config-file", "none", "--profile", "default"]));
    const binary = selectNotificationBinary(metadata, { target, packageDirectory: path.join(root, "src-tauri"), version: readVersion(root) });
    const libdir = metadata["rust-build-meta"]?.platforms?.host?.libdir;
    assert.equal(libdir?.status, "available", "Missing native Rust dynamic library path");
    const childEnv = { ...env, DYLD_FALLBACK_LIBRARY_PATH: [path.dirname(binary), path.join(target, "debug"), libdir.path].join(":") };
    const output = await runBundledNotificationControl(binary, directory, run, childEnv);
    assert.equal((await run("git", ["status", "--porcelain", "--untracked-files=no"])).trim(), "", "Native control source changed during execution");
    console.log(output);
    controller.signal.throwIfAborted();
    await rm(directory, { recursive: true });
    controller.signal.throwIfAborted();
    await writeFile(path.join(env.RUNNER_TEMP, "notification-native.json"), JSON.stringify({ sourceSha: env.GITHUB_SHA, case: nativeCase, status: "passed", authorizationRequested: false, output }, null, 2));
  } catch (error) {
    // Failed/uncertain child cleanup cannot authorize deletion of its bundle.
    // The disposable hosted VM owns final retirement; never mask the first error.
    try { await writeFile(path.join(env.RUNNER_TEMP, "notification-native.json"), JSON.stringify({ sourceSha: env.GITHUB_SHA, case: nativeCase, status: "failed", retainedBundleDirectory: directory, aborted: controller.signal.aborted }, null, 2)); } catch { /* Preserve the original failure. */ }
    throw error;
  } finally {
    clearTimeout(timer);
  }
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main().catch(error => { console.error(error); process.exitCode = 1; });
