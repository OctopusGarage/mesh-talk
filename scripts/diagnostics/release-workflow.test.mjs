import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import test from "node:test";

const workflow = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");

test("release matrix builds native Windows and Linux ARM64 packages", () => {
  assert.match(workflow, /runner: windows-11-(?:vs2026-)?arm[\s\S]*?target: aarch64-pc-windows-msvc/);
  assert.match(workflow, /runner: ubuntu-22\.04-arm[\s\S]*?target: aarch64-unknown-linux-gnu/);
  assert.match(workflow, /aarch64-pc-windows-msvc\)[\s\S]*?ARCH="arm64"/);
});

test("Windows ARM64 installs a pinned x64 Cosign binary for emulated signing", () => {
  assert.match(workflow, /name: Install cosign\n\s+if: matrix\.target != 'aarch64-pc-windows-msvc'/);
  assert.match(workflow, /name: Install Cosign for Windows ARM64\n\s+if: matrix\.target == 'aarch64-pc-windows-msvc'/);
  assert.match(workflow, /cosign-windows-amd64\.exe/);
  assert.match(workflow, /9b85a88ebff2d9dd30ff4984a6f61f2cedc232dd87d81fa7f2ff3c0ed96c241c/);
});

for (const runner of ["Windows", "Linux", "macOS"]) {
  test(`${runner} checksum step hashes real files and preserves filenames with spaces`, () => {
    const step = workflow.split(/^      - /m).find((value) => value.startsWith("name: Generate SHA256 checksums"));
    assert.ok(step);
    const commands = step.split("        run: |\n")[1];
    assert.ok(commands);
    const script = commands.split("\n").filter((line) => /^ {10}/.test(line) || /^\s*$/.test(line)).map((line) => line.replace(/^ {10}/, "")).join("\n");
    const root = mkdtempSync(join(tmpdir(), "mesh-talk-checksums-"));
    try {
      const release = join(root, "release");
      mkdirSync(release);
      const files = new Map([["installer with spaces.exe", Buffer.from("binary installer\0data")], ["package.msi", Buffer.from("second installer")]]);
      for (const [name, bytes] of files) writeFileSync(join(release, name), bytes);
      for (let attempt = 0; attempt < 2; attempt++) {
        execFileSync("bash", ["-c", script], { cwd: root, env: { ...process.env, RUNNER_OS: runner } });
        const lines = readFileSync(join(release, "SHA256SUMS"), "utf8").trim().split(/\r?\n/);
        assert.equal(lines.length, files.size, "checksums must not include SHA256SUMS itself");
        const names = new Set();
        for (const line of lines) {
          const match = line.match(/^([a-f\d]{64}) [ *](?:\.\/)?(.+)$/i);
          assert.ok(match, `checksum entry must contain a SHA-256 digest and full filename: ${line}`);
          const bytes = files.get(match[2]);
          assert.ok(bytes, "checksum must reference an actual installer");
          assert.ok(!names.has(match[2]), "each installer must appear exactly once");
          names.add(match[2]);
          assert.equal(match[1].toLowerCase(), createHash("sha256").update(bytes).digest("hex"));
        }
      }
      // A partial hasher failure must propagate even if another file was hashed.
      const bin = join(root, "bin");
      mkdirSync(bin);
      const digest = createHash("sha256").update(files.get("installer with spaces.exe")).digest("hex");
      writeFileSync(join(bin, "sha256sum"), `#!/usr/bin/env bash\nif [[ "$1" == '-c' ]]; then exit 0; fi\nfor file in "$@"; do\n  [[ "$file" != */package.msi ]] || exit 23\n  printf '%s  %s\\n' '${digest}' "$file"\ndone\n`, { mode: 0o755 });
      assert.throws(() => execFileSync("bash", ["-c", script], { cwd: root, env: { ...process.env, RUNNER_OS: runner, PATH: `${bin}${delimiter}${process.env.PATH}` } }), "a partial checksum failure must stop packaging/signing");
      for (const name of files.keys()) rmSync(join(release, name));
      assert.throws(() => execFileSync("bash", ["-c", script], { cwd: root, env: { ...process.env, RUNNER_OS: runner } }), "empty releases must fail before packaging/signing");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
}

test("platform uploads remain drafts until all release artifacts are verified", () => {
  const uploads = workflow.split(/^      - /m).filter((step) => /uses: softprops\/action-gh-release@/.test(step));
  assert.ok(uploads.length > 0, "release upload action must be present");
  for (const upload of uploads) {
    const inputs = upload.match(/^        with:\n((?:^          .*\n)+)/m)?.[1];
    assert.ok(inputs, "release upload must declare its inputs");
    assert.match(inputs, /^          draft: true\s*$/m, "platform upload must not publish an incomplete release");
  }
});
