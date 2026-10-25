import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { updaterConfig, collectUpdater, updaterName } from "../release/updater.mjs";

test("updater signing is opt-in, paired and restricted to trusted release sources", () => {
  assert.equal(updaterConfig({}).bundle.createUpdaterArtifacts, false);
  const env = { MESH_TALK_UPDATER_PUBLIC_KEY: "public", TAURI_SIGNING_PRIVATE_KEY: "private", GITHUB_REF: "refs/heads/main" };
  assert.equal(updaterConfig(env).bundle.createUpdaterArtifacts, true);
  assert.throws(() => updaterConfig({ ...env, GITHUB_REF: "refs/heads/feature" }), /trusted/);
  assert.throws(() => updaterConfig({ MESH_TALK_UPDATER_PUBLIC_KEY: "public" }), /both/);
  assert.throws(() => updaterConfig({ TAURI_SIGNING_PRIVATE_KEY: "private" }), /both/);
});

test("collects one native updater and signs its full Nightly identity without exposing keys in arguments", () => {
  const root = mkdtempSync(join(tmpdir(), "mesh-update-"));
  try {
    const bundle = join(root, "bundle");
    mkdirSync(join(bundle, "macos"), { recursive: true });
    writeFileSync(join(bundle, "macos", "mesh-talk.app.tar.gz"), "app bundle");
    const calls = [];
    const name = collectUpdater({ bundle, output: join(root, "release"), tag: "v0.4.4-nightly.42.1", target: "aarch64-apple-darwin", sign: (file, version) => { calls.push({ file, version }); writeFileSync(file + ".sig", "signature"); } });
    assert.equal(name, "mesh-talk-update_v0.4.4-nightly.42.1_macos_arm64.app.tar.gz");
    assert.equal(readFileSync(join(root, "release", name), "utf8"), "app bundle");
    assert.equal(calls[0].version, "0.4.4-nightly.42.1");
    assert.throws(() => updaterName("v0.4.4-beta.1", "aarch64-apple-darwin"), /identity/);
    assert.throws(() => updaterName("v0.4.4", "unsupported"), /target/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});


test("the installed CLI produces a real signature bound to the logical Nightly identity", () => {
  const root = mkdtempSync(join(tmpdir(), "mesh-update-sign-"));
  const cli = new URL("../../frontend/node_modules/@tauri-apps/cli/tauri.js", import.meta.url);
  try {
    const key = join(root, "throwaway.key");
    execFileSync(process.execPath, [fileURLToPath(cli), "signer", "generate", "--ci", "-p", "", "-w", key], { stdio: ["ignore", "pipe", "pipe"] });
    const bundle = join(root, "bundle");
    mkdirSync(join(bundle, "appimage"), { recursive: true });
    writeFileSync(join(bundle, "appimage", "mesh-talk.AppImage"), "fixture");
    const output = join(root, "release");
    const name = collectUpdater({ bundle, output, tag: "v0.4.4-nightly.42.2", target: "x86_64-unknown-linux-gnu", sign: (file, version) => {
      execFileSync(process.execPath, [fileURLToPath(cli), "signer", "sign", "--app-version", version, file], { env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY: readFileSync(key, "utf8"), TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "" }, stdio: ["ignore", "pipe", "pipe"] });
    } });
    const signature = Buffer.from(readFileSync(join(output, name + ".sig"), "utf8"), "base64").toString("utf8");
    assert.match(signature, /trusted comment:.*\tversion:0\.4\.4-nightly\.42\.2/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
