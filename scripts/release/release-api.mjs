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

export function getReleaseByTag(repo, tag) {
  const pages = JSON.parse(execFileSync("gh", ["api", "--paginate", "--slurp", `repos/${repo}/releases?per_page=100`], { encoding: "utf8" }));
  return releaseFromPages(pages, tag);
}
