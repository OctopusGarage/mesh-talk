import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { validateReleaseTag } from "./release-state.mjs";

export function renderReleaseNotes(template, { tag, repository, ref, sha, runId, runAttempt, nightly = false }) {
  validateReleaseTag(tag, nightly);
  assert.match(repository, /^[\w.-]+\/[\w.-]+$/);
  assert.match(sha, /^[a-f\d]{40}$/);
  assert.match(runId, /^[1-9]\d*$/);
  assert.match(runAttempt, /^[1-9]\d*$/);
  assert.equal(ref, nightly ? "refs/heads/main" : `refs/tags/${tag}`, "release notes must use the actual publication source ref");
  if (nightly) assert.ok(tag.endsWith(`-nightly.${runId}.${runAttempt}`), "Nightly tag must match this build run and attempt");
  const stableBlock = /<!-- STABLE_ONLY_START -->\n([\s\S]*?)<!-- STABLE_ONLY_END -->\n?/g;
  let notes = template.replace(stableBlock, (_, contents) => nightly ? "" : contents);
  const replacements = {
    TAG: tag,
    REPOSITORY: repository,
    SOURCE_REF: ref,
    SOURCE_SHA: sha,
    CONTAINER_IMAGE: `ghcr.io/${repository.split("/")[0].toLowerCase()}/mesh-talk-post-office`,
  };
  for (const [key, value] of Object.entries(replacements)) notes = notes.replaceAll(`__${key}__`, value);
  assert.doesNotMatch(notes, /__[A-Z][A-Z_]*__/, "unknown release notes placeholder");
  assert.doesNotMatch(notes, /STABLE_ONLY/, "unclosed release notes conditional");
  if (nightly) {
    notes = `> **Nightly preview — experimental**\n> This build may contain unfinished features or regressions. Back up important data before trying it.\n> Source: [${sha}](https://github.com/${repository}/commit/${sha}) · [Build ${runId}, attempt ${runAttempt}](https://github.com/${repository}/actions/runs/${runId}/attempts/${runAttempt})\n\n${notes}`;
  }
  return notes;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const notes = renderReleaseNotes(readFileSync(".github/release-notes.md", "utf8"), {
    tag: process.env.ARTIFACT_TAG,
    repository: process.env.SOURCE_REPO,
    ref: process.env.SOURCE_REF,
    sha: process.env.SOURCE_SHA,
    runId: process.env.GITHUB_RUN_ID,
    runAttempt: process.env.GITHUB_RUN_ATTEMPT,
    nightly: process.env.RELEASE_NIGHTLY === "true",
  });
  writeFileSync("release_notes.md", notes);
}
