import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const script = fileURLToPath(new URL("../release/verify-release.sh", import.meta.url));
const sha = "a".repeat(40);

test("signature and provenance failures stop the real aggregate verifier before metadata inspection", () => {
  const root = mkdtempSync(join(tmpdir(), "mesh-talk-verifier-"));
  try {
    const bin = join(root, "bin");
    const assets = join(root, "assets");
    mkdirSync(bin);
    mkdirSync(assets);
    for (const platform of ["macos_arm64", "macos_x86_64", "windows_x86_64", "linux_x86_64"]) {
      const path = join(assets, `mesh-talk_v0.1.5_${platform}.zip`);
      writeFileSync(path, "fixture archive");
      writeFileSync(`${path}.bundle`, "fixture signature");
    }
    writeFileSync(join(bin, "cosign"), '#!/usr/bin/env bash\nexit "${SIGNATURE_EXIT:-0}"\n', { mode: 0o755 });
    const record = join(root, "provenance-args.txt");
    writeFileSync(join(bin, "gh"), '#!/usr/bin/env bash\nprintf "%s\\n" "$@" >> "$VERIFIER_ARGS_RECORD"\nexit "${PROVENANCE_EXIT:-0}"\n', { mode: 0o755 });
    const args = [script, assets, "v0.1.5", "0.1.5", "OctopusGarage/mesh-talk", "refs/tags/v0.1.5", sha];
    const env = { ...process.env, PATH: `${bin}${delimiter}${process.env.PATH}`, VERIFIER_ARGS_RECORD: record };
    assert.equal(spawnSync("bash", args, { env: { ...env, SIGNATURE_EXIT: "43" } }).status, 43);
    assert.equal(spawnSync("bash", args, { env: { ...env, PROVENANCE_EXIT: "44" } }).status, 44);
    const forwarded = readFileSync(record, "utf8").split(/\r?\n/);
    for (const value of ["--source-digest", sha, "--source-ref", "refs/tags/v0.1.5", "--signer-workflow", "OctopusGarage/mesh-talk/.github/workflows/release.yml", "--deny-self-hosted-runners"]) assert.ok(forwarded.includes(value), value);
    // All crypto verifier commands succeeding cannot bypass malformed contents.
    assert.notEqual(spawnSync("bash", args, { env }).status, 0);
    // Empty PATH is set inside the already-running Bash process, so missing
    // verification tooling is a deterministic failure on every platform.
    const missing = spawnSync("bash", ["-c", 'PATH="$1"; source "$2" "${@:3}"', "fixture", join(root, "empty-bin"), ...args]);
    assert.equal(missing.status, 127);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
