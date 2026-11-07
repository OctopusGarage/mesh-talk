import { execFileSync } from "node:child_process";
import assert from "node:assert/strict";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

test("release asset verifier accepts valid fixtures and rejects incomplete or corrupt releases", () => {
  execFileSync(process.platform === "win32" ? "python" : "python3", ["-B", "scripts/release/test_verify_assets.py"], {
    cwd: new URL("../../", import.meta.url),
    stdio: "pipe",
  });
});

test("post-office packaging returns only the archive path for the signing caller", { skip: process.platform === "win32" }, () => {
  const directory = realpathSync(mkdtempSync(join(tmpdir(), "mesh-release-output-")));
  try {
    mkdirSync(join(directory, "target/x86_64-unknown-linux-gnu/release"), { recursive: true });
    mkdirSync(join(directory, "docs"));
    writeFileSync(join(directory, "target/x86_64-unknown-linux-gnu/release/mesh-talk-node"), "relay fixture");
    copyFileSync(new URL("../../docs/post-office-release.md", import.meta.url), join(directory, "docs/post-office-release.md"));
    const output = execFileSync("bash", [new URL("../release/package-post-office.sh", import.meta.url).pathname, "x86_64-unknown-linux-gnu", "v0.4.0"], { cwd: directory, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    const archive = join(directory, "release_zips/mesh-talk-post-office_v0.4.0_linux_x86_64.zip");
    assert.equal(output, `${archive}\n`);
    assert.ok(existsSync(output.trim()), "the captured signing input must name an existing archive");
    execFileSync("unzip", ["-t", archive], { stdio: "pipe" });
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("application artifact helper collects exact names and rejects invalid packaging before signing", { skip: process.platform === "win32" }, () => {
  const helper = new URL("../release/package-app.sh", import.meta.url).pathname;
  const root = realpathSync(mkdtempSync(join(tmpdir(), "mesh-app-artifacts-")));
  const env = { ...process.env, ARTIFACT_TAG: "v0.4.4-nightly.123.1", TARGET: "x86_64-pc-windows-msvc" };
  const run = (mode, overrides = {}) => execFileSync("bash", [helper, mode], { cwd: root, env: { ...env, ...overrides }, stdio: "pipe" });
  try {
    for (const [format, extension] of [["nsis", "exe"], ["msi", "msi"], ["dmg", "dmg"], ["appimage", "AppImage"], ["deb", "deb"], ["rpm", "rpm"]]) {
      const directory = join(root, `target/${env.TARGET}/release/bundle/${format}`);
      mkdirSync(directory, { recursive: true });
      writeFileSync(join(directory, `Mesh Talk_0.4.4_x64.${extension}`), `native ${format}`);
    }
    run("collect");
    for (const extension of ["exe", "msi", "dmg", "AppImage", "deb", "rpm"]) assert.ok(existsSync(join(root, `release/Mesh Talk_${env.ARTIFACT_TAG}_x64.${extension}`)));
    assert.throws(() => run("collect", { ARTIFACT_TAG: "" }), "missing tag must fail");
    assert.throws(() => run("package", { TARGET: "unsupported-target" }), "unknown targets must fail before signing");
    assert.throws(() => run("package", { TARGET: "" }), "missing target must fail");
    assert.throws(() => run("unknown"), "unknown operations must fail");
    run("checksums");
    mkdirSync(join(root, "scripts/release"), { recursive: true });
    writeFileSync(join(root, "scripts/release/sign-blob.mjs"), "process.exit(23);");
    assert.throws(() => run("package"), "signer failure must propagate");
    assert.ok(!existsSync(join(root, `release_zips/mesh-talk_${env.ARTIFACT_TAG}_windows_x86_64.zip.bundle`)));
    for (const extension of ["exe", "msi"]) rmSync(join(root, `release/Mesh Talk_${env.ARTIFACT_TAG}_x64.${extension}`));
    assert.throws(() => run("package"), "another platform's installers cannot satisfy the target");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
