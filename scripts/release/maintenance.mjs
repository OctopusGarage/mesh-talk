import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { appendFileSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { getReleaseByTag, getLatestPublishedStable } from "./release-api.mjs";
import {
  readVersion,
  prepareVersion,
  syncVersions,
  checkVersions,
} from "./version.mjs";

const versionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

function parts(version) {
  assert.match(version, versionPattern, "expected a numeric product version");
  const result = version.split(".").map(Number);
  assert.ok(
    result.every(Number.isSafeInteger),
    "version components must be safe integers",
  );
  return result;
}

export function nextDevelopmentVersion(current, stableTag) {
  assert.match(
    stableTag,
    /^v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/,
    "expected a Stable tag",
  );
  const stable = parts(stableTag.slice(1));
  const existing = parts(current);
  for (let index = 0; index < stable.length; index += 1) {
    if (existing[index] > stable[index]) return null;
    if (existing[index] < stable[index]) break;
  }
  stable[2] += 1;
  assert.ok(Number.isSafeInteger(stable[2]), "patch version overflow");
  return stable.join(".");
}

export function assertDraftBuild(tag, sha, release, tagSha) {
  parts(tag.replace(/^v/, ""));
  assert.ok(tag.startsWith("v"), "Stable tag must start with v");
  assert.match(sha, /^[a-f\d]{40}$/, "exact release source required");
  assert.equal(release?.tag_name, tag, "draft tag changed");
  assert.equal(release?.draft, true, "release must remain a draft");
  assert.equal(release?.prerelease, false, "release must be stable");
  assert.equal(tagSha, sha, "release tag source changed");
}

export function validateAutomationPR(pr, repo) {
  assert.equal(pr.state, "OPEN", "release PR must be open");
  assert.equal(pr.baseRefName, "main", "release PR must target main");
  assert.equal(
    pr.headRepository?.nameWithOwner,
    repo,
    "release PR must belong to this repository",
  );
  // gh's GraphQL formatter may display bot logins using its app/ prefix.
  assert.ok(
    ["github-actions[bot]", "app/github-actions"].includes(pr.author?.login),
    "release PR must belong to the workflow bot",
  );
  assert.match(
    pr.headRefName,
    /^release-please--branches--main(?:--components--[\w.-]+)?$/,
    "unexpected release branch",
  );
  assert.match(pr.headRefOid, /^[a-f\d]{40}$/, "exact PR source required");
  return pr.headRefName;
}

function command(binary, args, cwd) {
  return execFileSync(binary, args, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "inherit"],
  }).trim();
}

function gh(args) {
  return command("gh", args);
}

function git(args, cwd) {
  return command("git", args, cwd);
}

function output(name, value) {
  if (process.env.GITHUB_OUTPUT)
    appendFileSync(process.env.GITHUB_OUTPUT, `${name}=${value}\n`);
  console.log(`${name}=${value}`);
}

function tagSource(repo, tag) {
  return JSON.parse(gh(["api", `repos/${repo}/commits/${tag}`])).sha;
}

function syncPullRequest(repo, number) {
  assert.match(number ?? "", /^[1-9]\d*$/, "release PR number required");
  const pr = JSON.parse(
    gh([
      "pr",
      "view",
      number,
      "--repo",
      repo,
      "--json",
      "state,baseRefName,headRefName,headRefOid,headRepository,author",
    ]),
  );
  const branch = validateAutomationPR(pr, repo);
  // Execute only the trusted main copy of this helper. PR content is data; no
  // package scripts, Cargo build scripts, or code from the PR are executed.
  git(["fetch", "origin", `refs/heads/${branch}`]);
  assert.equal(
    git(["rev-parse", "FETCH_HEAD"]),
    pr.headRefOid,
    "release PR changed before synchronization",
  );
  const directory = mkdtempSync(join(tmpdir(), "mesh-release-pr-"));
  try {
    git(["worktree", "add", "--detach", directory, pr.headRefOid]);
    syncVersions(directory);
    checkVersions(directory);
    const changed = git(["diff", "--name-only"], directory)
      .split(/\r?\n/)
      .filter(Boolean);
    const allowed = new Set([
      "Cargo.lock",
      "package.json",
      "package-lock.json",
      "frontend/package.json",
      "frontend/package-lock.json",
    ]);
    assert.ok(
      changed.every((path) => allowed.has(path)),
      "unexpected version synchronization change",
    );
    if (changed.length === 0) return;
    git(["config", "user.name", "github-actions[bot]"], directory);
    git(
      [
        "config",
        "user.email",
        "41898282+github-actions[bot]@users.noreply.github.com",
      ],
      directory,
    );
    git(["add", "--", ...changed], directory);
    git(
      [
        "commit",
        "-m",
        "chore(release): synchronize generated version metadata",
      ],
      directory,
    );
    // A normal fast-forward push refuses concurrent bot or maintainer changes.
    git(["push", "origin", `HEAD:refs/heads/${branch}`], directory);
  } finally {
    git(["worktree", "remove", "--force", directory]);
    rmSync(directory, { recursive: true, force: true });
  }
}

function dispatchDraftBuild(repo, tag, sha) {
  assertDraftBuild(tag, sha, getReleaseByTag(repo, tag), tagSource(repo, tag));
  // GITHUB_TOKEN-created tags do not trigger push workflows. Dispatch the
  // existing verifier explicitly; this path never requests publication.
  gh([
    "workflow",
    "run",
    "release.yml",
    "--repo",
    repo,
    "--ref",
    tag,
    "-f",
    "publish_release=false",
    "-f",
    "publish_nightly=false",
  ]);
}

function prepareDevelopment(repo, tag) {
  const published = getReleaseByTag(repo, tag);
  assert.ok(
    published && !published.draft && !published.prerelease,
    "requested Stable release must be published",
  );
  nextDevelopmentVersion(readVersion(), tag); // Validate user/event input before API use.
  const latest = getLatestPublishedStable(repo);
  assert.ok(latest, "no published Stable release found");
  const sha = tagSource(repo, latest.tag_name);
  git(["merge-base", "--is-ancestor", sha, "HEAD"]);
  const next = nextDevelopmentVersion(readVersion(), latest.tag_name);
  output("changed", next !== null);
  if (next === null) return;
  prepareVersion(next);
  checkVersions();
  output("version", next);
}

function main() {
  const [mode, ...args] = process.argv.slice(2);
  const repo = process.env.GITHUB_REPOSITORY;
  assert.match(repo ?? "", /^[\w.-]+\/[\w.-]+$/, "repository required");
  assert.ok(
    process.env.GITHUB_REF === "refs/heads/main" ||
      (mode === "next-development" &&
        process.env.GITHUB_EVENT_NAME === "release"),
    "release maintenance runs only from trusted main or publication events",
  );
  assert.equal(
    git(["rev-parse", "HEAD"]),
    git(["rev-parse", "refs/remotes/origin/main"]),
    "release maintenance must check out trusted main",
  );
  if (mode === "sync-pr") return syncPullRequest(repo, args[0]);
  if (mode === "draft-build") return dispatchDraftBuild(repo, args[0], args[1]);
  if (mode === "next-development") return prepareDevelopment(repo, args[0]);
  throw new Error(
    "usage: maintenance.mjs sync-pr NUMBER | draft-build TAG SHA | next-development TAG",
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href)
  main();
