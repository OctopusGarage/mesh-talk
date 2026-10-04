import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const workflow = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");

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
          const match = line.match(/^([a-f\d]{64}) {2}(?:\.\/)?(.+)$/i);
          assert.ok(match, "checksum entry must contain a SHA-256 digest and full filename");
          const bytes = files.get(match[2]);
          assert.ok(bytes, "checksum must reference an actual installer");
          assert.ok(!names.has(match[2]), "each installer must appear exactly once");
          names.add(match[2]);
          assert.equal(match[1].toLowerCase(), createHash("sha256").update(bytes).digest("hex"));
        }
      }
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
