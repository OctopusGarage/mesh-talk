import assert from "node:assert/strict";
import {
  existsSync,
  readFileSync,
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  rmSync,
} from "node:fs";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { buildAffectedPlan } from "../validation/affected.mjs";
import * as maintenance from "../release/maintenance.mjs";
const { assertDraftBuild, assertReleaseBaseline, nextDevelopmentVersion } =
  maintenance;

test("legacy release migration requires the exact reachable baseline and unchanged equivalent source trees", () => {
  const verified = {
    historyFloor: "335a0010812d5367c153fee5d8e40f59a2c9c090",
    baselineCommit: "335a0010812d5367c153fee5d8e40f59a2c9c090",
    legacyCommit: "9b9f91b77214bf432814f65a17f806920ffa1541",
    baselineTree: "11a8bde065f329c8df6a67d75fa57cca015f49c2",
    legacyTree: "11a8bde065f329c8df6a67d75fa57cca015f49c2",
    baselineIsAncestor: true,
  };
  assert.doesNotThrow(() => assertReleaseBaseline(verified));
  for (const change of [
    { historyFloor: "a".repeat(40) },
    { baselineCommit: null },
    { legacyCommit: "b".repeat(40) },
    { baselineTree: "c".repeat(40) },
    { legacyTree: "d".repeat(40) },
    { baselineIsAncestor: false },
  ]) {
    assert.throws(() => assertReleaseBaseline({ ...verified, ...change }));
  }
});

test("post-publication development floor advances without downgrading an existing target", () => {
  assert.equal(nextDevelopmentVersion("0.4.3", "v0.4.3"), "0.4.4");
  assert.equal(nextDevelopmentVersion("0.4.4", "v0.4.3"), null);
  assert.equal(nextDevelopmentVersion("0.5.0", "v0.4.3"), null);
  assert.equal(nextDevelopmentVersion("0.4.3", "v0.5.0"), "0.5.1");
  for (const tag of ["v0.4.3-nightly.1.1", "0.4.3", "v00.4.3"]) {
    assert.throws(() => nextDevelopmentVersion("0.4.3", tag));
  }
});

test("only the exact draft source can start a stable build", () => {
  const sha = "a".repeat(40);
  const release = { tag_name: "v0.4.4", draft: true, prerelease: false };
  assert.doesNotThrow(() => assertDraftBuild("v0.4.4", sha, release, sha));
  assert.throws(
    () => assertDraftBuild("v0.4.4", sha, release, "b".repeat(40)),
    /source/,
  );
  assert.throws(
    () => assertDraftBuild("v0.4.4", sha, { ...release, draft: false }, sha),
    /draft/,
  );
  assert.throws(
    () =>
      assertDraftBuild("v0.4.4", sha, { ...release, prerelease: true }, sha),
    /stable/,
  );
  assert.throws(
    () =>
      assertDraftBuild("v0.4.4", sha, { ...release, tag_name: "v0.4.5" }, sha),
    /tag/,
  );
});

const sha = "a".repeat(40);
const draft = { tag_name: "v0.4.5", draft: true, prerelease: false };
const input = () => ({
  repo: "owner/repo",
  tag: "v0.4.5",
  version: "0.4.5",
  sha,
  tagSha: null,
  release: null,
  latestStableTag: "v0.4.4",
  changelog: "# Changelog\n\n## 0.4.5\n\n- Restore unfinished drafts.\n",
  template: readFileSync(
    new URL("../../.github/release-notes.md", import.meta.url),
    "utf8",
  ),
  runId: "123",
  runAttempt: "1",
});
function effects(failure) {
  const calls = [];
  const io = Object.fromEntries(
    [
      "checkSource",
      "createTag",
      "createDraft",
      "assertDraft",
      "writeNotes",
      "dispatchBuild",
    ].map((name) => [
      name,
      (...args) => {
        calls.push({ name, args });
        if (name === failure) throw Error(`failed ${name}`);
      },
    ]),
  );
  return { io, calls };
}

test("manual draft preparation validates curated notes and exact-source CI before tag/draft mutation", () => {
  const { io, calls } = effects();
  maintenance.prepareStableDraft(input(), io);
  assert.deepEqual(
    calls.map((c) => c.name),
    ["checkSource", "createTag", "createDraft", "assertDraft", "dispatchBuild"],
  );
  assert.ok(
    calls
      .find((c) => c.name === "createDraft")
      .args[0].includes("Restore unfinished drafts."),
  );
  assert.ok(calls.find((c) => c.name === "createDraft").args[0].includes(sha));
});

test("existing same-source unpublished drafts reuse their immutable tag and rebuild without publication", () => {
  const { io, calls } = effects();
  maintenance.prepareStableDraft(
    { ...input(), tagSha: sha, release: draft },
    io,
  );
  assert.deepEqual(
    calls.map((c) => c.name),
    [
      "checkSource",
      "assertDraft",
      "writeNotes",
      "assertDraft",
      "dispatchBuild",
    ],
  );
  assert.throws(
    () =>
      maintenance.prepareStableDraft(
        { ...input(), tagSha: "b".repeat(40), release: draft },
        io,
      ),
    /source/,
  );
});

test("manual preparation refuses invalid versions, absent notes and published or conflicting release state before effects", () => {
  for (const change of [
    { tag: "v0.4.6" },
    { tag: "v00.4.5" },
    { version: "0.4.5-nightly.1.1" },
    { sha: "main" },
    { repo: "bad/repo;command" },
    { latestStableTag: "v0.4.5" },
    { latestStableTag: "v0.5.0" },
    { changelog: "# Changelog\n\n## 0.4.4\n\n- Old notes.\n" },
    { tagSha: "b".repeat(40) },
    { release: draft },
    { tagSha: sha, release: { ...draft, draft: false } },
    { tagSha: sha, release: { ...draft, prerelease: true } },
    { tagSha: sha, release: { ...draft, tag_name: "v0.4.6" } },
  ]) {
    const { io, calls } = effects();
    assert.throws(
      () => maintenance.prepareStableDraft({ ...input(), ...change }, io),
      JSON.stringify(change),
    );
    assert.deepEqual(
      calls,
      [],
      "invalid intent must not read source checks or mutate remote state",
    );
  }
});

test("a missing draft may reuse only the exact source tag", () => {
  const { io, calls } = effects();
  maintenance.prepareStableDraft({ ...input(), tagSha: sha }, io);
  assert.deepEqual(
    calls.map((c) => c.name),
    ["checkSource", "createDraft", "assertDraft", "dispatchBuild"],
  );
});

test("failed checks, mutation, fresh draft validation or notes stop every later operation", () => {
  for (const phase of [
    "checkSource",
    "createTag",
    "createDraft",
    "assertDraft",
    "dispatchBuild",
  ]) {
    const { io, calls } = effects(phase);
    assert.throws(
      () => maintenance.prepareStableDraft(input(), io),
      new RegExp(`failed ${phase}`),
    );
    assert.equal(calls.at(-1).name, phase);
  }
  const { io, calls } = effects("writeNotes");
  assert.throws(
    () =>
      maintenance.prepareStableDraft(
        { ...input(), tagSha: sha, release: draft },
        io,
      ),
    /failed writeNotes/,
  );
  assert.deepEqual(
    calls.map((c) => c.name),
    ["checkSource", "assertDraft", "writeNotes"],
  );
});

test("ordinary main pushes cannot generate release proposals; preparation is explicit main-only dispatch", () => {
  const workflow = readFileSync(
    new URL("../../.github/workflows/release-maintenance.yml", import.meta.url),
    "utf8",
  );
  assert.doesNotMatch(workflow, /googleapis\/release-please|^  push:/m);
  assert.match(workflow, /next-development/);
  assert.match(workflow, /stable_tag:/);
  assert.match(workflow, /peter-evans\/create-pull-request@[a-f\d]{40}/);
  const manual = readFileSync(
    new URL("../../.github/workflows/prepare-stable.yml", import.meta.url),
    "utf8",
  );
  assert.match(manual, /workflow_dispatch:/);
  assert.doesNotMatch(manual, /^  (push|schedule|release):/m);
  assert.match(manual, /github.ref == 'refs\/heads\/main'/);
  assert.match(manual, /ref: main/);
  assert.match(manual, /security-events: read/);
  assert.match(manual, /checks: read/);
  assert.match(manual, /node scripts\/release\/maintenance\.mjs prepare-draft/);
  assert.equal(
    existsSync(new URL("../../release-please-config.json", import.meta.url)),
    false,
  );
  assert.equal(
    existsSync(new URL("../../.release-please-manifest.json", import.meta.url)),
    false,
  );
  const publisher = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(publisher, /generate_release_notes: false/);
  assert.match(publisher, /workflows\/release-maintenance\.yml\/dispatches/);
  assert.match(publisher, /inputs\[stable_tag\]/);
});

test("curated changelog and deleted proposal configs retain their release validation owner", () => {
  const { contracts } = JSON.parse(
    readFileSync(
      new URL("../validation/validation.json", import.meta.url),
      "utf8",
    ),
  );
  const plan = buildAffectedPlan(
    [
      "CHANGELOG.md",
      "release-please-config.json",
      ".release-please-manifest.json",
      ".github/workflows/prepare-stable.yml",
    ],
    { contracts: contracts.filter((c) => c.id === "release-publication") },
  );
  assert.deepEqual(plan.blockers, []);
  assert.ok(plan.owners.some((c) => c.id === "release-publication"));
});

test("remote tag reads distinguish exact lightweight/annotated tags from same-named branches, absence and failed reads", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "mesh-release-tag-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const root = join(directory, "repo"),
    remote = join(directory, "remote.git");
  mkdirSync(root);
  mkdirSync(join(directory, "hooks"));
  const git = (...args) =>
    execFileSync("git", args, {
      cwd: root,
      encoding: "utf8",
      stdio: "pipe",
    }).trim();
  git("init", "--initial-branch=main");
  git("config", "user.name", "Fixture");
  git("config", "user.email", "fixture@example.test");
  git("config", "core.hooksPath", join(directory, "hooks"));
  git("config", "commit.gpgsign", "false");
  git("config", "tag.gpgsign", "false");
  writeFileSync(join(root, "value"), "first");
  git("add", "value");
  git("commit", "-m", "first");
  const first = git("rev-parse", "HEAD");
  git("tag", "v0.4.5");
  writeFileSync(join(root, "value"), "second");
  git("commit", "-am", "second");
  const second = git("rev-parse", "HEAD");
  git("branch", "v0.4.5");
  git("tag", "-a", "v0.4.6", "-m", "annotated");
  git("init", "--bare", remote);
  git("remote", "add", "origin", remote);
  git(
    "push",
    "origin",
    "refs/heads/main",
    "refs/heads/v0.4.5",
    "refs/tags/v0.4.5",
    "refs/tags/v0.4.6",
  );
  assert.equal(maintenance.existingTagSource("v0.4.5", { cwd: root }), first);
  assert.equal(maintenance.existingTagSource("v0.4.6", { cwd: root }), second);
  assert.equal(maintenance.existingTagSource("v0.4.7", { cwd: root }), null);
  assert.throws(() => maintenance.existingTagSource("v00.4.5", { cwd: root }));
  git("remote", "set-url", "origin", join(directory, "missing.git"));
  assert.throws(
    () => maintenance.existingTagSource("v0.4.7", { cwd: root }),
    /could not|repository|remote/i,
  );
});
