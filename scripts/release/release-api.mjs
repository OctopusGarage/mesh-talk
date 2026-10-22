import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";

export function releaseFromPages(pages, tag) {
  assert.ok(Array.isArray(pages), "invalid release list response");
  const matches = pages.flatMap((page) => {
    assert.ok(Array.isArray(page), "invalid release list page");
    return page;
  }).filter((release) => release.tag_name === tag);
  assert.ok(matches.length <= 1, `multiple releases use tag ${tag}`);
  return matches[0] ?? null;
}

function filteredReleases(repo, filter) {
  assert.match(repo, /^[\w.-]+\/[\w.-]+$/);
  // gh applies --jq to each page before printing. Never capture the full release
  // history (including every asset) in Node's bounded child-process buffer.
  const query = `if type != "array" then error("invalid release list page") else .[] | if type != "object" or (.tag_name | type) != "string" then error("invalid release list entry") else ${filter} | @json end end`;
  const output = execFileSync("gh", ["api", "--paginate", `repos/${repo}/releases?per_page=100`, "--jq", query], { encoding: "utf8", maxBuffer: 8 * 1024 * 1024 });
  return output.trim() ? output.trim().split(/\r?\n/).map((line) => JSON.parse(line)) : [];
}

export function getReleaseByTag(repo, tag) {
  assert.equal(typeof tag, "string", "release tag required");
  const matches = filteredReleases(repo, `select(.tag_name == ${JSON.stringify(tag)})`);
  return releaseFromPages([matches], tag);
}

export function getPublishedNightly(repo, sha) {
  assert.match(sha, /^[a-f\d]{40}$/);
  const pattern = "^v[0-9]+\\.[0-9]+\\.[0-9]+-nightly\\.[1-9][0-9]*\\.[1-9][0-9]*$";
  const matches = filteredReleases(repo, `select(.draft == false and .prerelease == true and .target_commitish == ${JSON.stringify(sha)} and (.tag_name | test(${JSON.stringify(pattern)}))) | {tag_name, target_commitish, draft, prerelease}`);
  return matches[0] ?? null;
}
