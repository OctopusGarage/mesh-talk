import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, readdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const targets = {
  "aarch64-apple-darwin": ["macos_arm64", "macos", "app.tar.gz"],
  "x86_64-apple-darwin": ["macos_x86_64", "macos", "app.tar.gz"],
  "aarch64-pc-windows-msvc": ["windows_arm64", "nsis", "nsis.exe"],
  "x86_64-pc-windows-msvc": ["windows_x86_64", "nsis", "nsis.exe"],
  "aarch64-unknown-linux-gnu": ["linux_aarch64", "appimage", "AppImage"],
  "x86_64-unknown-linux-gnu": ["linux_x86_64", "appimage", "AppImage"],
};

export function updaterConfig(env) {
  const pubkey = env.MESH_TALK_UPDATER_PUBLIC_KEY?.trim() || "";
  const privateKey = env.TAURI_SIGNING_PRIVATE_KEY?.trim() || "";
  assert.equal(Boolean(pubkey), Boolean(privateKey), "configure both updater signing keys or neither");
  if (pubkey && env.GITHUB_REF) {
    assert.ok(env.GITHUB_REF === "refs/heads/main" || /^refs\/tags\/v\d+\.\d+\.\d+$/.test(env.GITHUB_REF), "updater signing requires a trusted main or Stable tag source");
  }
  return { bundle: { createUpdaterArtifacts: Boolean(pubkey) }, plugins: { updater: { pubkey, requireSignedVersion: true, windows: { installMode: "passive" } } } };
}

export function updaterName(tag, target) {
  assert.match(tag, /^v\d+\.\d+\.\d+(?:-nightly\.[1-9]\d*\.[1-9]\d*)?$/, "invalid updater build identity");
  assert.ok(Object.hasOwn(targets, target), "unsupported updater target");
  const [platform, , extension] = targets[target];
  return `mesh-talk-update_${tag}_${platform}.${extension}`;
}

export function collectUpdater({ bundle, output, tag, target, sign }) {
  const name = updaterName(tag, target);
  const [, directory, extension] = targets[target];
  const suffix = extension === "nsis.exe" ? ".exe" : `.${extension}`;
  const candidates = readdirSync(join(bundle, directory), { withFileTypes: true }).filter((entry) => entry.isFile() && entry.name.endsWith(suffix));
  assert.equal(candidates.length, 1, "expected one native updater payload");
  mkdirSync(output, { recursive: true });
  const destination = join(output, name);
  copyFileSync(join(bundle, directory, candidates[0].name), destination);
  // The numeric installer version is shared by Nightly builds. Bind the full
  // logical identity separately so a signed older build cannot be relabelled.
  sign(destination, tag.slice(1));
  return name;
}

function main() {
  const config = updaterConfig(process.env);
  if (process.argv[2] === "prepare") {
    mkdirSync("tmp", { recursive: true });
    writeFileSync("tmp/updater-build.json", JSON.stringify(config));
  } else if (process.argv[2] === "collect") {
    if (!config.bundle.createUpdaterArtifacts) return;
    const target = process.argv[3];
    collectUpdater({
      bundle: join("target", target, "release", "bundle"), output: "release", tag: process.env.ARTIFACT_TAG, target,
      sign: (file, version) => execFileSync(process.execPath, [resolve("frontend/node_modules/@tauri-apps/cli/tauri.js"), "signer", "sign", "--app-version", version, file], { stdio: ["ignore", "pipe", "pipe"] }),
    });
  } else { throw new Error("expected prepare or collect"); }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { main(); } catch { console.error("Could not prepare signed updater artifacts; check trusted source, paired keys and build outputs"); process.exitCode = 1; }
}
