import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { renderReleaseNotes } from "../release/render-notes.mjs";

const template = readFileSync(new URL("../../.github/release-notes.md", import.meta.url), "utf8");
const source = { repository: "OctopusGarage/mesh-talk", sha: "a".repeat(40), runId: "123", runAttempt: "2" };

for (const nightly of [false, true]) {
  test(`${nightly ? "Nightly" : "Stable"} notes bind verification to the actual build source`, () => {
    const tag = nightly ? "v0.1.6-nightly.123.2" : "v0.1.6";
    const ref = nightly ? "refs/heads/main" : `refs/tags/${tag}`;
    const notes = renderReleaseNotes(template, { ...source, tag, ref, nightly });
    assert.match(notes, /mesh-talk_v0\.1\.6/);
    assert.ok(notes.includes(`--certificate-identity 'https://github.com/${source.repository}/.github/workflows/release.yml@${ref}'`));
    assert.ok(notes.includes(`--source-ref '${ref}' --source-digest '${source.sha}'`));
    assert.ok(notes.includes(`--signer-workflow '${source.repository}/.github/workflows/release.yml'`));
    assert.ok(notes.includes("--deny-self-hosted-runners"));
    assert.doesNotMatch(notes, /identity-regexp|from tag|__[A-Z_]+__|STABLE_ONLY/);
    if (nightly) {
      assert.match(notes, /^> \*\*Nightly preview — experimental\*\*/);
      assert.ok(notes.includes(source.sha));
      assert.ok(notes.includes("actions/runs/123/attempts/2"));
      assert.doesNotMatch(notes, /ghcr\.io|verified Linux container image/);
    } else {
      assert.match(notes, /ghcr\.io\/octopusgarage\/mesh-talk-post-office:v0\.1\.6/);
      assert.match(notes, /verified Linux container image/);
      assert.doesNotMatch(notes, /Nightly preview/);
    }
  });
}

test("notes reject publication/source mode mismatches and unsafe identities", () => {
  const nightly = { ...source, tag: "v0.1.6-nightly.123.2", ref: "refs/heads/main", nightly: true };
  for (const change of [{ ref: "refs/heads/dev" }, { sha: "main" }, { runId: "124" }, { runAttempt: "3" }, { repository: "owner/repo'" }, { nightly: false }]) {
    assert.throws(() => renderReleaseNotes(template, { ...nightly, ...change }));
  }
  assert.throws(() => renderReleaseNotes(template, { ...source, tag: "v0.1.6", ref: "refs/heads/main", nightly: false }));
});


test("the workflow renderer writes notes from the GitHub build identity", (t) => {
  const root = mkdtempSync(join(tmpdir(), "mesh-release-notes-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, ".github"));
  writeFileSync(join(root, ".github/release-notes.md"), template);
  execFileSync(process.execPath, [new URL("../release/render-notes.mjs", import.meta.url).pathname], {
    cwd: root,
    env: { ...process.env, ARTIFACT_TAG: "v0.1.6-nightly.123.2", SOURCE_REPO: source.repository, SOURCE_REF: "refs/heads/main", SOURCE_SHA: source.sha, GITHUB_RUN_ID: source.runId, GITHUB_RUN_ATTEMPT: source.runAttempt, RELEASE_NIGHTLY: "true" },
  });
  assert.match(readFileSync(join(root, "release_notes.md"), "utf8"), /Nightly preview/);
  const workflow = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");
  assert.match(workflow, /name: Prepare release notes[\s\S]*?run: node scripts\/release\/render-notes\.mjs/);
});
