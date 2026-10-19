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
