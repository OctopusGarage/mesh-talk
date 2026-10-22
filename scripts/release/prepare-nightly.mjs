import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { validateReleaseTag } from "./release-state.mjs";

// Create a lightweight technical tag at the verified source, never move a ref.
export function prepareNightly(repo, tag, sha, api = (args) => execFileSync("gh", ["api", ...args], { encoding: "utf8" })) {
  assert.match(repo, /^[\w.-]+\/[\w.-]+$/);
  assert.match(sha, /^[a-f\d]{40}$/);
  validateReleaseTag(tag, true);
  try {
    api(["--method", "POST", `repos/${repo}/git/refs`, "-f", `ref=refs/tags/${tag}`, "-f", `sha=${sha}`]);
  } catch {
    // A rerun may reuse its unpublished draft only if its tag still names this source.
    const ref = JSON.parse(api([`repos/${repo}/git/ref/tags/${tag}`]));
    assert.equal(ref.object?.type, "commit", "Nightly tag must point directly to a commit");
    assert.equal(ref.object.sha, sha, "Nightly tag source changed; tags are immutable");
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  assert.equal(process.env.RELEASE_NIGHTLY, "true", "Nightly mode required");
  prepareNightly(process.env.SOURCE_REPO, process.env.ARTIFACT_TAG, process.env.SOURCE_SHA);
}
