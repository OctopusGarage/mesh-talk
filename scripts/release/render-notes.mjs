import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { validateReleaseTag } from "./release-state.mjs";
import { compareVersions } from "./version.mjs";

// This is an emptiness predicate, not an HTML sanitizer. Return the original
// curated text unchanged; comment-only sections do not describe a release.
function hasNonCommentText(body) {
  let offset = 0;
  while (offset < body.length) {
    const comment = body.indexOf("<!--", offset);
    if (comment === -1) return Boolean(body.slice(offset).trim());
    if (body.slice(offset, comment).trim()) return true;
    const end = body.indexOf("-->", comment + 4);
    if (end === -1) return false;
    offset = end + 3;
  }
  return false;
}

// Only maintained h2 release sections are eligible. Fenced examples are not headings.
export function releaseHighlights(
  changelog,
  version,
  { required = true } = {},
) {
  compareVersions(version, version);
  assert.doesNotMatch(version, /[\r\n]/, "invalid numeric product version");
  assert.equal(typeof changelog, "string", "changelog text required");
  const lines = changelog.split(/\r?\n/);
  const sections = [];
  let fence = null;
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    const delimiter = line.match(/^ {0,3}(`{3,}|~{3,})(.*)$/);
    if (fence) {
      if (
        delimiter &&
        delimiter[1][0] === fence[0] &&
        delimiter[1].length >= fence.length &&
        !delimiter[2].trim()
      )
        fence = null;
      continue;
    }
    if (delimiter) {
      fence = delimiter[1];
      continue;
    }
    const heading = line.match(/^ {0,3}#{1,2}[ \t]+(.*)$/);
    if (!heading) continue;
    if (sections.length && sections.at(-1).end === null)
      sections.at(-1).end = index;
    const title = line.match(/^ {0,3}##[ \t]+(.*?)[ \t]*$/)?.[1];
    const candidate = title?.match(
      /^(?:([0-9]+\.[0-9]+\.[0-9]+)|\[([0-9]+\.[0-9]+\.[0-9]+)\]\([^\r\n]*\))(?:[ \t]+\(\d{4}-\d{2}-\d{2}\))?$/,
    );
    if (candidate && (candidate[1] ?? candidate[2]) === version)
      sections.push({ start: index + 1, end: null });
  }
  assert.ok(
    sections.length <= 1,
    `duplicate changelog sections for ${version}`,
  );
  if (!sections.length) {
    assert.ok(!required, `missing changelog section for ${version}`);
    return "";
  }
  const { start, end } = sections[0];
  const body = lines
    .slice(start, end ?? lines.length)
    .join("\n")
    .trim();
  assert.ok(hasNonCommentText(body), `empty changelog section for ${version}`);
  return body;
}

export function renderReleaseNotes(
  template,
  { tag, repository, ref, sha, runId, runAttempt, nightly = false, changelog },
) {
  validateReleaseTag(tag, nightly);
  assert.match(repository, /^[\w.-]+\/[\w.-]+$/);
  assert.match(sha, /^[a-f\d]{40}$/);
  assert.match(runId, /^[1-9]\d*$/);
  assert.match(runAttempt, /^[1-9]\d*$/);
  assert.equal(
    ref,
    nightly ? "refs/heads/main" : `refs/tags/${tag}`,
    "release notes must use the actual publication source ref",
  );
  if (nightly)
    assert.ok(
      tag.endsWith(`-nightly.${runId}.${runAttempt}`),
      "Nightly tag must match this build run and attempt",
    );
  const highlights = releaseHighlights(
    changelog,
    tag.slice(1).split("-nightly.")[0],
    { required: !nightly },
  );
  const highlightsMarker = "__RELEASE_HIGHLIGHTS__";
  assert.equal(
    template.split(highlightsMarker).length,
    2,
    "release notes require exactly one highlights marker",
  );
  const stableBlock =
    /<!-- STABLE_ONLY_START -->\n([\s\S]*?)<!-- STABLE_ONLY_END -->\n?/g;
  let notes = template.replace(stableBlock, (_, contents) =>
    nightly ? "" : contents,
  );
  const replacements = {
    TAG: tag,
    REPOSITORY: repository,
    SOURCE_REF: ref,
    SOURCE_SHA: sha,
    CONTAINER_IMAGE: `ghcr.io/${repository.split("/")[0].toLowerCase()}/mesh-talk-post-office`,
  };
  for (const [key, value] of Object.entries(replacements))
    notes = notes.replaceAll(`__${key}__`, value);
  assert.doesNotMatch(
    notes.replace(highlightsMarker, ""),
    /__[A-Z][A-Z_]*__/,
    "unknown release notes placeholder",
  );
  assert.doesNotMatch(
    notes,
    /STABLE_ONLY/,
    "unclosed release notes conditional",
  );
  notes = notes.replace(highlightsMarker, () =>
    highlights ? `### Release highlights\n\n${highlights}` : "",
  );
  if (nightly) {
    notes = `> **Nightly preview — experimental**\n> This build may contain unfinished features or regressions. Back up important data before trying it.\n> Source: [${sha}](https://github.com/${repository}/commit/${sha}) · [Build ${runId}, attempt ${runAttempt}](https://github.com/${repository}/actions/runs/${runId}/attempts/${runAttempt})\n\n${notes}`;
  }
  return notes;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  const notes = renderReleaseNotes(
    readFileSync(".github/release-notes.md", "utf8"),
    {
      changelog: readFileSync("CHANGELOG.md", "utf8"),
      tag: process.env.ARTIFACT_TAG,
      repository: process.env.SOURCE_REPO,
      ref: process.env.SOURCE_REF,
      sha: process.env.SOURCE_SHA,
      runId: process.env.GITHUB_RUN_ID,
      runAttempt: process.env.GITHUB_RUN_ATTEMPT,
      nightly: process.env.RELEASE_NIGHTLY === "true",
    },
  );
  writeFileSync("release_notes.md", notes);
}
