import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  appendFileSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { getReleaseByTag, getLatestPublishedStable } from "./release-api.mjs";
import { renderReleaseNotes } from "./render-notes.mjs";
import {
  readVersion,
  prepareVersion,
  checkVersions,
  compareVersions,
} from "./version.mjs";

const versionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

// Historical migration evidence, not the current product-version authority.
// v0.4.3 was released before its identical source tree was squash-merged.
const legacyRelease = Object.freeze({
  tag: "v0.4.3",
  commit: "9b9f91b77214bf432814f65a17f806920ffa1541",
  baseline: "335a0010812d5367c153fee5d8e40f59a2c9c090",
  tree: "11a8bde065f329c8df6a67d75fa57cca015f49c2",
});

export function assertReleaseBaseline(evidence) {
  assert.equal(
    evidence.historyFloor,
    legacyRelease.baseline,
    "release history migration floor changed",
  );
  assert.equal(
    evidence.baselineCommit,
    legacyRelease.baseline,
    "release history baseline commit is unavailable or changed",
  );
  assert.equal(
    evidence.legacyCommit,
    legacyRelease.commit,
    "immutable legacy release tag changed",
  );
  assert.equal(
    evidence.baselineTree,
    legacyRelease.tree,
    "release history baseline source tree changed",
  );
  assert.equal(
    evidence.legacyTree,
    evidence.baselineTree,
    "legacy release and main baseline source trees differ",
  );
  assert.equal(
    evidence.baselineIsAncestor,
    true,
    "release history baseline must be an ancestor of main",
  );
}

function checkReleaseBaseline() {
  const ancestry = spawnSync(
    "git",
    ["merge-base", "--is-ancestor", legacyRelease.baseline, "HEAD"],
    { encoding: "utf8" },
  );
  assert.ok(
    ancestry.status === 0 || ancestry.status === 1,
    ancestry.stderr || "release history baseline is unavailable",
  );
  assertReleaseBaseline({
    historyFloor: legacyRelease.baseline,
    baselineCommit: git(["rev-parse", `${legacyRelease.baseline}^{commit}`]),
    legacyCommit: git(["rev-parse", `refs/tags/${legacyRelease.tag}^{commit}`]),
    baselineTree: git(["rev-parse", `${legacyRelease.baseline}^{tree}`]),
    legacyTree: git(["rev-parse", `refs/tags/${legacyRelease.tag}^{tree}`]),
    baselineIsAncestor: ancestry.status === 0,
  });
  console.log(
    `Verified ${legacyRelease.tag} equivalent-tree migration baseline ${legacyRelease.baseline}`,
  );
}

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

/** Prepare an unpublished Stable candidate only after validating its complete intent. */
export function prepareStableDraft(input, effects) {
  const { repo, tag, version, sha, tagSha, release, latestStableTag } = input;
  assert.match(repo, /^[\w.-]+\/[\w.-]+$/);
  parts(version);
  assert.equal(
    tag,
    `v${version}`,
    "tag must match the canonical Cargo version",
  );
  assert.match(sha, /^[a-f\d]{40}$/, "exact release source required");
  if (latestStableTag !== null) {
    assert.equal(
      latestStableTag,
      `v${parts(latestStableTag.slice(1)).join(".")}`,
      "invalid latest Stable tag",
    );
    assert.ok(
      compareVersions(version, latestStableTag.slice(1)) > 0,
      "target must exceed published Stable",
    );
  }
  if (tagSha !== null) assert.equal(tagSha, sha, "release tag source changed");
  if (release !== null) assertDraftBuild(tag, sha, release, tagSha);
  const notes = renderReleaseNotes(input.template, {
    tag,
    repository: repo,
    ref: `refs/tags/${tag}`,
    sha,
    runId: input.runId,
    runAttempt: input.runAttempt,
    changelog: input.changelog,
  });
  effects.checkSource();
  if (tagSha === null) effects.createTag();
  if (release === null) effects.createDraft(notes);
  else {
    effects.assertDraft();
    effects.writeNotes(notes);
  }
  effects.assertDraft();
  effects.dispatchBuild();
}

export function existingTagSource(tag, { cwd } = {}) {
  assert.equal(tag, `v${parts(tag.slice(1)).join(".")}`, "invalid Stable tag");
  // Exact refs distinguish an absent tag from authentication/network errors and
  // dereference annotated tags without accepting any other repository's ref.
  const ref = `refs/tags/${tag}`;
  const result = spawnSync(
    "git",
    ["ls-remote", "--exit-code", "--tags", "origin", ref, `${ref}^{}`],
    { cwd, encoding: "utf8" },
  );
  if (result.status === 2 && result.stdout.trim() === "") return null;
  assert.equal(result.status, 0, result.stderr || "could not read release tag");
  const refs = new Map(
    result.stdout
      .trim()
      .split(/\r?\n/)
      .map((line) => {
        const [sha, name] = line.split("\t");
        assert.match(sha, /^[a-f\d]{40}$/, "invalid remote tag source");
        assert.ok(
          name === ref || name === `${ref}^{}`,
          "unexpected remote tag ref",
        );
        return [name, sha];
      }),
  );
  return refs.get(`${ref}^{}`) ?? refs.get(ref);
}

function prepareDraft(repo, tag) {
  const version = checkVersions();
  assert.equal(tag, `v${version}`, "requested tag must match canonical Cargo");
  checkReleaseBaseline();
  const sha = git(["rev-parse", "HEAD"]);
  const directory = mkdtempSync(join(tmpdir(), "mesh-draft-notes-"));
  const notesFile = (notes) => {
    const path = join(directory, "release-notes.md");
    writeFileSync(path, notes);
    return path;
  };
  try {
    prepareStableDraft(
      {
        repo,
        tag,
        version,
        sha,
        tagSha: existingTagSource(tag),
        release: getReleaseByTag(repo, tag),
        latestStableTag: getLatestPublishedStable(repo)?.tag_name ?? null,
        changelog: readFileSync("CHANGELOG.md", "utf8"),
        template: readFileSync(".github/release-notes.md", "utf8"),
        runId: process.env.GITHUB_RUN_ID,
        runAttempt: process.env.GITHUB_RUN_ATTEMPT,
      },
      {
        checkSource: () =>
          command(process.execPath, [
            fileURLToPath(
              new URL("./check-source-checks.mjs", import.meta.url),
            ),
            repo,
            sha,
          ]),
        createTag: () =>
          gh([
            "api",
            "--method",
            "POST",
            `repos/${repo}/git/refs`,
            "-f",
            `ref=refs/tags/${tag}`,
            "-f",
            `sha=${sha}`,
          ]),
        createDraft: (notes) =>
          gh([
            "release",
            "create",
            tag,
            "--repo",
            repo,
            "--verify-tag",
            "--draft",
            "--latest=false",
            "--target",
            sha,
            "--title",
            `Mesh-Talk ${tag}`,
            "--notes-file",
            notesFile(notes),
          ]),
        assertDraft: () =>
          assertDraftBuild(
            tag,
            sha,
            getReleaseByTag(repo, tag),
            existingTagSource(tag),
          ),
        writeNotes: (notes) =>
          gh([
            "release",
            "edit",
            tag,
            "--repo",
            repo,
            "--notes-file",
            notesFile(notes),
          ]),
        // Token-created tags do not trigger push workflows; dispatch the exact tag
        // explicitly, retaining the separate verified Stable publication action.
        dispatchBuild: () =>
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
          ]),
      },
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
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
  if (mode === "check-baseline") return checkReleaseBaseline();
  if (mode === "prepare-draft") {
    assert.equal(
      process.env.GITHUB_EVENT_NAME,
      "workflow_dispatch",
      "Stable preparation requires explicit dispatch",
    );
    assert.equal(args.length, 1, "exactly one Stable tag required");
    return prepareDraft(repo, args[0]);
  }
  if (mode === "next-development") return prepareDevelopment(repo, args[0]);
  throw new Error(
    "usage: maintenance.mjs check-baseline | prepare-draft TAG | next-development TAG",
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href)
  main();
