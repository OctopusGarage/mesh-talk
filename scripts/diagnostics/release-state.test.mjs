import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join, resolve } from "node:path";
import { draftSnapshot, requireSameDraft } from "../release/release-state.mjs";

const release = { id: 100, tag_name: "v0.1.6", draft: true, assets: [{ id: 1, name: "package.zip", size: 10, digest: `sha256:${"a".repeat(64)}` }] };

test("release-state CLI binds downloaded bytes and refuses changed publication evidence", { skip: process.platform === "win32" }, (t) => {
  const root = mkdtempSync(join(tmpdir(), "mesh-talk-release-state-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const folder of ["bin", "draft", "workflow"]) mkdirSync(join(root, folder));
  const bytes = "verified package";
  const current = { ...release, assets: [{ ...release.assets[0], size: Buffer.byteLength(bytes), digest: `sha256:${createHash("sha256").update(bytes).digest("hex")}` }] };
  const api = join(root, "release.json");
  writeFileSync(api, JSON.stringify(current));
  writeFileSync(join(root, "bin", "gh"), `#!${process.execPath}\nconst fs = require('node:fs'); if (process.argv.includes('PATCH')) fs.writeFileSync(process.env.PATCH_LOG, JSON.stringify(process.argv)); else process.stdout.write(fs.readFileSync(process.env.RELEASE_API));\n`, { mode: 0o755 });
  const script = resolve("scripts/release/release-state.mjs");
  const env = { ...process.env, PATH: `${join(root, "bin")}${delimiter}${process.env.PATH}`, SOURCE_REPO: "owner/repo", SOURCE_SHA: "a".repeat(40), RELEASE_API: api, PATCH_LOG: join(root, "patch.json") };
  const run = (mode, extra = {}) => spawnSync(process.execPath, [script, mode, "v0.1.6", join(root, "draft"), join(root, "workflow")], { cwd: root, env: { ...env, ...extra }, encoding: "utf8" });
  for (const folder of ["draft", "workflow"]) writeFileSync(join(root, folder, "package.zip"), bytes);
  for (const folder of ["draft", "workflow"]) {
    writeFileSync(join(root, folder, "package.zip"), "tampered");
    const result = run("record");
    assert.notEqual(result.status, 0, result.stderr);
    assert.equal(existsSync(join(root, "verified-draft.json")), false);
    writeFileSync(join(root, folder, "package.zip"), bytes);
  }
  assert.equal(run("record").status, 0);
  writeFileSync(api, JSON.stringify({ ...current, assets: [{ ...current.assets[0], id: 99 }] }));
  assert.notEqual(run("publish").status, 0);
  assert.equal(existsSync(env.PATCH_LOG), false);
  writeFileSync(api, JSON.stringify(current));
  assert.notEqual(run("publish", { SOURCE_SHA: "b".repeat(40) }).status, 0);
  assert.equal(existsSync(env.PATCH_LOG), false);
  assert.equal(run("publish").status, 0);
  assert.ok(JSON.parse(readFileSync(env.PATCH_LOG, "utf8")).includes("repos/owner/repo/releases/100"));
});

test("verified release snapshots require a draft and content digests", () => {
  draftSnapshot(release, "v0.1.6");
  assert.throws(() => draftSnapshot({ ...release, draft: false }, "v0.1.6"), /draft/);
  assert.throws(() => draftSnapshot(release, "v0.1.7"), /tag/);
  assert.throws(() => draftSnapshot({ ...release, assets: [{ ...release.assets[0], digest: null }] }, "v0.1.6"), /digest/);
});

test("publication rejects release state or asset replacement after verification", () => {
  const snapshot = draftSnapshot(release, "v0.1.6");
  requireSameDraft(release, snapshot);
  for (const replacement of [
    { ...release, draft: false }, { ...release, id: 101 },
    { ...release, assets: [] },
    { ...release, assets: [{ ...release.assets[0], id: 2 }] },
    { ...release, assets: [{ ...release.assets[0], digest: `sha256:${"b".repeat(64)}` }] },
  ]) assert.throws(() => requireSameDraft(replacement, snapshot));
});
