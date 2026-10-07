import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { appendFileSync, readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { getReleaseByTag } from "./release-api.mjs";

export function releaseInputs({ ref, runId, version, desktopVersion, existingRelease, publishRelease = false }) {
  assert.match(version, /^\d+\.\d+\.\d+$/, "workspace version must be a stable release");
  assert.equal(version, desktopVersion, "workspace and desktop version must match");
  assert.match(runId, /^\d+$/, "invalid workflow run ID");
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
  const manifest = readFileSync("Cargo.toml", "utf8").split("[workspace.package]")[1]?.split(/^\[/m)[0];
  const version = manifest?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const desktopVersion = JSON.parse(readFileSync("src-tauri/tauri.conf.json", "utf8")).version;
  let existingRelease = null;
  if (ref.startsWith("refs/tags/")) {
    // Validate ref/version before passing a tag to an external command.
    releaseInputs({ ref, runId, version, desktopVersion, existingRelease });
    const comparison = JSON.parse(execFileSync("gh", ["api", `repos/${repo}/compare/main...${sha}`], { encoding: "utf8" }));
    assert.ok(["identical", "behind"].includes(comparison.status), "release source must already be merged into main");
    existingRelease = getReleaseByTag(repo, ref.slice(10));
  }
  const result = releaseInputs({ ref, runId, version, desktopVersion, existingRelease, publishRelease: process.env.RELEASE_PUBLICATION === "true" });
  appendFileSync(process.env.GITHUB_OUTPUT, `version=${result.version}\ntag=${result.tag}\n`);
  console.log(`Release preflight passed: ${result.tag}`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main();
