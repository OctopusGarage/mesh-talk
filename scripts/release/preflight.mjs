import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { getReleaseByTag, getPublishedNightly, getLatestPublishedStable } from "./release-api.mjs";

import { releaseHighlights } from "./render-notes.mjs";
import { checkVersions, compareVersions } from "./version.mjs";

export function releaseInputs({ ref, runId, version, desktopVersion = version, existingRelease, publishRelease = false, nightly = false, runAttempt, latestStableTag = null }) {
  assert.match(version, /^\d+\.\d+\.\d+$/, "workspace version must be a stable release");
  assert.equal(version, desktopVersion, "workspace and desktop version must match");
  assert.match(runId, /^\d+$/, "invalid workflow run ID");
  if (nightly) {
    assert.equal(ref, "refs/heads/main", "Nightly publication requires main");
    if (latestStableTag !== null) {
      assert.match(latestStableTag, /^v\d+\.\d+\.\d+$/, "invalid latest Stable tag");
      assert.ok(compareVersions(version, latestStableTag.slice(1)) > 0, "Nightly target must be newer than the latest published Stable; prepare the next development target");
    }
    assert.equal(publishRelease, false, "Nightly and stable publication intents are exclusive");
    assert.match(runId, /^[1-9]\d*$/, "invalid Nightly run ID");
    assert.match(runAttempt ?? "", /^[1-9]\d*$/, "invalid Nightly run attempt");
    assert.ok(existingRelease === null || existingRelease.draft === true, "cannot overwrite a published release");
    return { version, tag: `v${version}-nightly.${runId}.${runAttempt}` };
  }
  if (ref.startsWith("refs/tags/")) {
    const tag = ref.slice("refs/tags/".length);
    assert.equal(tag, `v${version}`, "tag and application version must match");
    assert.ok(existingRelease === null || existingRelease.draft === true, "cannot overwrite a published release");
    return { version, tag };
  }
  assert.match(ref, /^refs\/heads\/.+$/, "unsupported release source ref");
  assert.equal(publishRelease, false, "publication requires a version tag, not a branch");
  return { version, tag: `v${version}-ci.${runId}` };
}

function main() {
  const { GITHUB_REF: ref, GITHUB_RUN_ID: runId, GITHUB_REPOSITORY: repo, GITHUB_SHA: sha } = process.env;
  assert.match(repo, /^[\w.-]+\/[\w.-]+$/);
  assert.match(sha, /^[a-f\d]{40}$/);
  const version = checkVersions();
  const nightly = process.env.RELEASE_NIGHTLY === "true";
  const latestStableTag = nightly ? getLatestPublishedStable(repo)?.tag_name ?? null : null;
  const args = { ref, runId, version, latestStableTag, nightly, runAttempt: process.env.GITHUB_RUN_ATTEMPT, publishRelease: process.env.RELEASE_PUBLICATION === "true" };
  const candidate = releaseInputs({ ...args, existingRelease: null });
  if (nightly || ref.startsWith("refs/tags/")) {
    releaseHighlights(readFileSync("CHANGELOG.md", "utf8"), version, { required: !nightly });
  }
  if (nightly && process.env.GITHUB_EVENT_NAME === "schedule") {
    const published = getPublishedNightly(repo, sha);
    if (published) {
      appendFileSync(process.env.GITHUB_OUTPUT, `version=${version}\ntag=${candidate.tag}\nnightly=true\nskip=true\n`);
      console.log(`Skipping unchanged source already published as ${published.tag_name}`);
      return;
    }
  }
  let existingRelease = null;
  if (ref.startsWith("refs/tags/")) {
    // Validate ref/version before passing a tag to an external command.
    releaseInputs({ ref, runId, version, existingRelease });
    const comparison = JSON.parse(execFileSync("gh", ["api", `repos/${repo}/compare/main...${sha}`], { encoding: "utf8" }));
    assert.ok(["identical", "behind"].includes(comparison.status), "release source must already be merged into main");
    existingRelease = getReleaseByTag(repo, ref.slice(10));
  }
  if (nightly) existingRelease = getReleaseByTag(repo, candidate.tag);
  const result = releaseInputs({ ...args, existingRelease });
  appendFileSync(process.env.GITHUB_OUTPUT, `version=${result.version}\ntag=${result.tag}\nnightly=${nightly}\nskip=false\n`);
  console.log(`Release preflight passed: ${result.tag}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
