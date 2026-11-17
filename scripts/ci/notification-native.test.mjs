import assert from "node:assert/strict";
import { existsSync, mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import path from "node:path";
import os from "node:os";
import { command } from "./windows-coverage.mjs";
import test from "node:test";
import { pathToFileURL } from "node:url";
const owner = new URL("./notification-native.mjs", import.meta.url);
const load = async () => existsSync(owner) ? import(owner) : {};
const root = path.resolve("/fixture");
const target = path.join(root, "target/llvm-cov-target");
const packageDirectory = path.join(root, "src-tauri");
const metadata = () => ({ "rust-build-meta": { "target-directory": target }, "rust-binaries": { "mesh-talk": { "binary-name": "mesh_talk", kind: "lib", "package-id": `path+${pathToFileURL(packageDirectory).href}#mesh-talk@0.4.6`, "binary-path": path.join(target, "debug/deps/mesh_talk-abcdef") } } });

test("native notification control selects only the exact current instrumented Host library", async () => {
  const { selectNotificationBinary } = await load();
  assert.equal(typeof selectNotificationBinary, "function");
  const select = value => selectNotificationBinary(value, { target, packageDirectory, version: "0.4.6" });
  assert.equal(select(metadata()), metadata()["rust-binaries"]["mesh-talk"]["binary-path"]);
  for (const change of [m => { m["rust-binaries"] = {}; }, m => { m["rust-binaries"].duplicate = m["rust-binaries"]["mesh-talk"]; }, m => { m["rust-binaries"]["mesh-talk"].kind = "bin"; }, m => { m["rust-binaries"]["mesh-talk"]["package-id"] += "-wrong"; }, m => { m["rust-binaries"]["mesh-talk"]["binary-path"] = path.join(root, "unrelated"); }, m => { m["rust-build-meta"]["target-directory"] = root; }]) {
    const value = metadata(); change(value); assert.throws(() => select(value));
  }
});

test("native notification selection accepts Cargo v2 through the shared build layout owner", async () => {
  const { selectNotificationBinary } = await load();
  const value = metadata();
  const binary = value["rust-binaries"]["mesh-talk"];
  binary["binary-path"] = path.join(target, "debug/build/mesh-talk/18a5cfde97c079fc/out/mesh_talk-18a5cfde97c079fc");
  const select = () => selectNotificationBinary(value, { target, packageDirectory, version: "0.4.6" });
  assert.equal(select(), binary["binary-path"]);
  for (const invalid of ["debug/build/mesh-talk/not-a-hash/out/mesh_talk-abcdef", "debug/build/mesh-talk/18a5cfde97c079fc/out/unrelated-abcdef", "debug/build/mesh-talk/18a5cfde97c079fc/other/mesh_talk-abcdef", "../outside/mesh_talk-abcdef"]) {
    binary["binary-path"] = path.join(target, invalid);
    assert.throws(select);
  }
});

test("native notification control requires one actual passing case, never an empty selection", async () => {
  const { verifyNotificationExecution, nativeCase } = await load();
  assert.equal(typeof verifyNotificationExecution, "function");
  const accepted = `test ${nativeCase} ... ok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 8s\n`;
  verifyNotificationExecution(accepted);
  for (const text of [accepted.replace("1 passed", "0 passed"), accepted.replace("0 ignored", "1 ignored"), accepted.replace("0 failed", "1 failed"), accepted.replace(nativeCase, "another_case")]) assert.throws(() => verifyNotificationExecution(text));
});

test("native notification CI never inherits authorization intent or runs on a user desktop", async () => {
  const { notificationControlEnvironment } = await load();
  assert.equal(typeof notificationControlEnvironment, "function");
  const env = { GITHUB_ACTIONS: "true", RUNNER_ENVIRONMENT: "github-hosted", RUNNER_OS: "macOS", MESH_NOTIFICATION_NATIVE_REQUEST: "1" };
  const actual = notificationControlEnvironment(env, "darwin");
  assert.equal(actual.MESH_NOTIFICATION_NATIVE_REQUEST, "0");
  assert.equal(actual.MESH_NOTIFICATION_NATIVE_EXPECT, "unconfigured");
  assert.equal(actual.MESH_NOTIFICATION_NATIVE_DELIVERY, "absent");
  for (const change of [{ GITHUB_ACTIONS: "false" }, { RUNNER_ENVIRONMENT: "self-hosted" }, { RUNNER_OS: "Linux" }]) assert.throws(() => notificationControlEnvironment({ ...env, ...change }, "darwin"));
  assert.throws(() => notificationControlEnvironment(env, "linux"));
});

// This fixture qualifies metadata, signing and Rust harness execution only.
// Its Rust program never calls a native notification or authorization API.
test("real Mach-O bundle protocol preserves actual passing and failing ignored-test outcomes", { skip: process.platform !== "darwin", timeout: 60000 }, async t => {
  const { selectNotificationBinary, runBundledNotificationControl } = await import(owner);
  const fixture = mkdtempSync(path.join(os.tmpdir(), "mesh-notification-protocol-"));
  t.after(() => rmSync(fixture, { recursive: true, force: true }));
  mkdirSync(path.join(fixture, "src-tauri/src"), { recursive: true });
  writeFileSync(path.join(fixture, "Cargo.toml"), '[workspace]\nmembers=["src-tauri"]\nresolver="2"\n');
  writeFileSync(path.join(fixture, "src-tauri/Cargo.toml"), '[package]\nname="mesh-talk"\nversion="0.0.0"\nedition="2024"\n');
  writeFileSync(path.join(fixture, "src-tauri/src/lib.rs"), 'mod notifications { mod tests { #[test] #[ignore] fn mac_packaged_permission_and_delivery() { assert!(std::env::var_os("MESH_PROTOCOL_FAIL").is_none(), "intentional protocol failure"); } } }\n');
  const env = { ...process.env, CARGO_TARGET_DIR: path.join(fixture, "target") };
  for (const key of Object.keys(env)) if (key.startsWith("CARGO_LLVM_COV") || key.startsWith("__CARGO_LLVM_COV") || ["RUSTFLAGS", "LLVM_PROFILE_FILE", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"].includes(key)) delete env[key];
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 45000);
  t.after(() => clearTimeout(timer));
  const cwd = process.cwd();
  // command's owner inherits cwd; manifest/target paths keep all work private.
  const run = (binary, args, childEnv = env) => command(args, { binary, env: childEnv, signal: controller.signal, maxOutputBytes: 1024 * 1024 });
  const metadata = JSON.parse(await run("cargo", ["nextest", "list", "--manifest-path", path.join(fixture, "Cargo.toml"), "-p", "mesh-talk", "--lib", "--message-format", "json", "--list-type", "binaries-only"]));
  assert.equal(process.cwd(), cwd);
  const binary = selectNotificationBinary(metadata, { target: env.CARGO_TARGET_DIR, packageDirectory: path.join(fixture, "src-tauri"), version: "0.0.0" });
  await runBundledNotificationControl(binary, path.join(fixture, "passing"), run, env);
  await assert.rejects(runBundledNotificationControl(binary, path.join(fixture, "failing"), run, { ...env, MESH_PROTOCOL_FAIL: "1" }), /failed: 101/u);
});

// Real profile accumulation through the copied/signed binary, without any OS
// notification calls. Hosted native qualification must have matching LLVM tools.
test("signed relocated control contributes real counters to the original instrumented binary", { skip: process.platform !== "darwin", timeout: 60000 }, async t => {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 45000);
  t.after(() => clearTimeout(timer));
  const run = (binary, args, env = process.env) => command(args, { binary, env, signal: controller.signal, maxOutputBytes: 1024 * 1024 });
  const sysroot = (await run("rustc", ["--print", "sysroot"])).trim();
  const version = await run("rustc", ["-vV"]);
  const host = version.split("\n").find(line => line.startsWith("host: ")).slice(6).trim();
  const tools = path.join(sysroot, "lib/rustlib", host, "bin");
  const llvmCov = process.env.LLVM_COV || path.join(tools, "llvm-cov");
  const llvmProfdata = process.env.LLVM_PROFDATA || path.join(tools, "llvm-profdata");
  if (!existsSync(llvmCov) || !existsSync(llvmProfdata)) {
    assert.ok(!process.env.CI && !process.env.GITHUB_ACTIONS, "Hosted counter qualification requires matching LLVM tools");
    t.skip("local matching LLVM tools unavailable; actual hosted native qualification remains mandatory");
    return;
  }
  const llvmVersion = version.split("\n").find(line => line.startsWith("LLVM version: ")).slice(14).trim();
  for (const binary of [llvmCov, llvmProfdata]) assert.ok((await run(binary, ["--version"])).includes(`LLVM version ${llvmVersion}`), "Native counter tools must match rustc's LLVM version");
  const { selectNotificationBinary, runBundledNotificationControl } = await import(owner);
  const fixture = mkdtempSync(path.join(os.tmpdir(), "mesh-notification-counters-"));
  t.after(() => rmSync(fixture, { recursive: true, force: true }));
  mkdirSync(path.join(fixture, "src-tauri/src"), { recursive: true });
  writeFileSync(path.join(fixture, "Cargo.toml"), '[workspace]\nmembers=["src-tauri"]\nresolver="2"\n');
  writeFileSync(path.join(fixture, "src-tauri/Cargo.toml"), '[package]\nname="mesh-talk"\nversion="0.0.0"\nedition="2024"\n');
  writeFileSync(path.join(fixture, "src-tauri/src/lib.rs"), '#[inline(never)] pub fn native_bundle_control_sentinel() -> u64 { std::hint::black_box(37) }\nmod notifications { mod tests { #[test] #[ignore] fn mac_packaged_permission_and_delivery() { assert_eq!(super::super::native_bundle_control_sentinel(), 37); } } }\n');
  const target = path.join(fixture, "target");
  mkdirSync(path.join(fixture, "setup"));
  const env = { ...process.env, CARGO_TARGET_DIR: target, RUSTFLAGS: "-Cinstrument-coverage", LLVM_PROFILE_FILE: path.join(fixture, "setup/%p-%m.profraw") };
  for (const key of Object.keys(env)) if (key.startsWith("CARGO_LLVM_COV") || key.startsWith("__CARGO_LLVM_COV") || ["CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"].includes(key)) delete env[key];
  const metadata = JSON.parse(await run("cargo", ["nextest", "list", "--manifest-path", path.join(fixture, "Cargo.toml"), "-p", "mesh-talk", "--lib", "--message-format", "json", "--list-type", "binaries-only"], env));
  const binary = selectNotificationBinary(metadata, { target, packageDirectory: path.join(fixture, "src-tauri"), version: "0.0.0" });
  const { readdirSync } = await import("node:fs");
  const report = async name => {
    const directory = path.join(fixture, name);
    const raw = readdirSync(directory).filter(file => file.endsWith(".profraw")).map(file => path.join(directory, file));
    assert.ok(raw.length > 0 && raw.length <= 8, "Missing or excessive native profiles");
    const merged = path.join(directory, "control.profdata");
    await run(llvmProfdata, ["merge", "--sparse", ...raw, "-o", merged]);
    return run(llvmCov, ["export", "--format=lcov", `--instr-profile=${merged}`, binary]);
  };
  for (const name of ["negative", "positive"]) mkdirSync(path.join(fixture, name));
  await run(binary, ["--list"], { ...env, LLVM_PROFILE_FILE: path.join(fixture, "negative/%p-%m.profraw") });
  assert.match(await report("negative"), /^FNDA:0,.*native_bundle_control_sentinel/mu);
  await runBundledNotificationControl(binary, path.join(fixture, "bundle"), run, { ...env, LLVM_PROFILE_FILE: path.join(fixture, "positive/%p-%m.profraw") });
  assert.match(await report("positive"), /^FNDA:[1-9]\d*,.*native_bundle_control_sentinel/mu, "Relocated signed control did not accumulate genuine library counters");
});
