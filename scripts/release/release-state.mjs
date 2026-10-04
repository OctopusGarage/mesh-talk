import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

export function draftSnapshot(release, tag) {
  assert.ok(release?.draft === true, "release must still be a draft");
  assert.equal(release.tag_name, tag, "release tag changed");
  assert.ok(Number.isSafeInteger(release.id), "invalid release ID");
  assert.ok(Array.isArray(release.assets), "missing release assets");
  const names = new Set();
  const assets = release.assets.map(({ id, name, size, digest }) => {
    assert.ok(Number.isSafeInteger(id) && Number.isSafeInteger(size) && size > 0, "invalid release asset");
    assert.ok(typeof name === "string" && !names.has(name) && !/[\\/]/.test(name), "duplicate or invalid asset name");
    assert.match(digest ?? "", /^sha256:[a-f\d]{64}$/, "missing release asset digest");
    names.add(name);
    return { id, name, size, digest };
  }).sort((a, b) => a.name.localeCompare(b.name));
  return { id: release.id, tag, assets };
}

export function requireSameDraft(release, snapshot) {
  assert.deepEqual(draftSnapshot(release, snapshot.tag), snapshot, "release or asset identities/digests changed since verification");
}

async function fileDigest(path) {
  const digest = createHash("sha256");
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return `sha256:${digest.digest("hex")}`;
}

async function main() {
  const [mode, tag, directory, workflowDirectory] = process.argv.slice(2);
  const repo = process.env.SOURCE_REPO;
  assert.match(repo, /^[\w.-]+\/[\w.-]+$/);
  assert.match(tag, /^v\d+\.\d+\.\d+$/);
  assert.ok(["guard", "record", "publish"].includes(mode), "unsupported release-state operation");
  let release;
  try {
    release = JSON.parse(execFileSync("gh", ["api", `repos/${repo}/releases/tags/${tag}`], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }));
  } catch (error) {
    if (mode === "guard" && /HTTP 404/.test(error.stderr?.toString() ?? "")) return;
    throw error;
  }
  if (mode === "guard") {
    assert.equal(release.draft, true, "cannot upload to a published release");
    return;
  }
  if (mode === "record") {
    const snapshot = draftSnapshot(release, tag);
    assert.deepEqual(snapshot.assets.map((asset) => asset.name).sort(), readdirSync(directory).sort(), "downloaded release asset set changed");
    assert.deepEqual(snapshot.assets.map((asset) => asset.name).sort(), readdirSync(workflowDirectory).sort(), "workflow/release asset sets differ");
    for (const asset of snapshot.assets) {
      assert.equal(await fileDigest(join(directory, asset.name)), asset.digest, "downloaded release asset digest changed");
      assert.equal(await fileDigest(join(workflowDirectory, asset.name)), asset.digest, "release asset differs from verified workflow output");
    }
    assert.match(process.env.SOURCE_SHA, /^[a-f\d]{40}$/);
    writeFileSync("verified-draft.json", JSON.stringify({ source: process.env.SOURCE_SHA, repository: repo, snapshot }, null, 2));
    return;
  }
  const evidence = JSON.parse(readFileSync("verified-draft.json", "utf8"));
  assert.equal(evidence.source, process.env.SOURCE_SHA, "verification source changed");
  assert.equal(evidence.repository, repo, "verification repository changed");
  assert.equal(evidence.snapshot.tag, tag, "verification tag changed");
  requireSameDraft(release, evidence.snapshot);
  execFileSync("gh", ["api", "--method", "PATCH", `repos/${repo}/releases/${evidence.snapshot.id}`, "-F", "draft=false", "-f", "make_latest=true"], { stdio: "inherit" });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) await main();
