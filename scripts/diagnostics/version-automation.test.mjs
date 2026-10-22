import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { buildAffectedPlan } from "../validation/affected.mjs";
import {
  assertDraftBuild,
  nextDevelopmentVersion,
  validateAutomationPR,
} from "../release/maintenance.mjs";
import { rustPackages } from "../pr-platform/rust-packages.mjs";

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

test("lock synchronization refuses foreign PRs and unexpected branches", () => {
  const pr = {
    state: "OPEN",
    baseRefName: "main",
    headRefName: "release-please--branches--main",
    headRefOid: "a".repeat(40),
    headRepository: { nameWithOwner: "owner/repo" },
    author: { login: "github-actions[bot]" },
  };
  assert.equal(validateAutomationPR(pr, "owner/repo"), pr.headRefName);
  assert.equal(
    validateAutomationPR(
      { ...pr, author: { login: "app/github-actions" } },
      "owner/repo",
    ),
    pr.headRefName,
  );
  for (const change of [
    { headRepository: { nameWithOwner: "attacker/repo" } },
    { author: { login: "someone" } },
    { headRefName: "main" },
    { baseRefName: "other" },
    { state: "CLOSED" },
  ]) {
    assert.throws(() =>
      validateAutomationPR({ ...pr, ...change }, "owner/repo"),
    );
  }
});

test(
  "release PR synchronization commits only generated metadata to its existing bot branch",
  { skip: process.platform === "win32" },
  (t) => {
    const directory = mkdtempSync(join(tmpdir(), "mesh-maintenance-"));
    t.after(() => rmSync(directory, { recursive: true, force: true }));
    const source = fileURLToPath(new URL("../../", import.meta.url));
    const root = join(directory, "repo");
    const remote = join(directory, "remote.git");
    const bin = join(directory, "bin");
    mkdirSync(root);
    mkdirSync(bin);
    const git = (...args) =>
      execFileSync("git", args, {
        cwd: root,
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
      }).trim();
    for (const path of [
      "Cargo.toml",
      "Cargo.lock",
      "package.json",
      "package-lock.json",
      "frontend/package.json",
      "frontend/package-lock.json",
      "src-tauri/tauri.conf.json",
      ...rustPackages.map((pkg) => `${pkg.directory}/Cargo.toml`),
    ]) {
      mkdirSync(dirname(join(root, path)), { recursive: true });
      writeFileSync(join(root, path), readFileSync(join(source, path)));
    }
    git("init", "--initial-branch=main");
    git("config", "user.name", "Fixture");
    git("config", "user.email", "fixture@example.test");
    git("config", "core.hooksPath", "/dev/null");
    git("add", ".");
    git("commit", "-m", "fixture main");
    git("init", "--bare", remote);
    git("remote", "add", "origin", remote);
    git("push", "origin", "main");
    git("switch", "-c", "release-please--branches--main");
    const original = readFileSync(join(root, "Cargo.toml"), "utf8");
    const canonical = original.replace(
      /(\[workspace\.package\][\s\S]*?version = ")[^"]+/,
      "$10.9.1",
    );
    writeFileSync(join(root, "Cargo.toml"), canonical);
    git("add", "Cargo.toml");
    git("commit", "-m", "chore: release proposal");
    git("push", "origin", "HEAD");
    const oid = git("rev-parse", "HEAD");
    git("switch", "main");
    const pr = {
      state: "OPEN",
      baseRefName: "main",
      headRefName: "release-please--branches--main",
      headRefOid: oid,
      headRepository: { nameWithOwner: "owner/repo" },
      author: { login: "github-actions[bot]" },
    };
    writeFileSync(
      join(bin, "gh"),
      `#!${process.execPath}\nprocess.stdout.write(process.env.FIXTURE_PR);\n`,
      { mode: 0o755 },
    );
    execFileSync(
      process.execPath,
      [
        fileURLToPath(new URL("../release/maintenance.mjs", import.meta.url)),
        "sync-pr",
        "123",
      ],
      {
        cwd: root,
        stdio: "pipe",
        env: {
          ...process.env,
          PATH: `${bin}${delimiter}${process.env.PATH}`,
          FIXTURE_PR: JSON.stringify(pr),
          GITHUB_REPOSITORY: "owner/repo",
          GITHUB_REF: "refs/heads/main",
        },
      },
    );
    git("fetch", "origin", "release-please--branches--main");
    assert.equal(
      git("show", "FETCH_HEAD:Cargo.toml"),
      canonical.trim(),
      "canonical release proposal remains untouched",
    );
    assert.deepEqual(
      git("diff", "--name-only", oid, "FETCH_HEAD").split(/\r?\n/).sort(),
      ["Cargo.lock", "package-lock.json", "package.json"],
    );
    assert.equal(
      JSON.parse(git("show", "FETCH_HEAD:package.json")).version,
      "0.9.1",
    );
    assert.match(
      git("show", "FETCH_HEAD:Cargo.lock"),
      /name = "mesh-talk"\nversion = "0\.9\.1"/,
    );
    assert.equal(
      git("rev-parse", "HEAD"),
      git("rev-parse", "origin/main"),
      "automation never writes main",
    );
  },
);

test("release coordination uses the whole product, draft-only releases and scoped Cargo updates", () => {
  const config = JSON.parse(
    readFileSync(new URL("../../release-please-config.json", import.meta.url)),
  );
  assert.equal(config.draft, true);
  assert.equal(config["force-tag-creation"], true);
  assert.deepEqual(Object.keys(config.packages), ["."]);
  assert.equal(config.packages["."]["release-type"], "node");
  assert.deepEqual(config.packages["."]["extra-files"], [
    {
      type: "toml",
      path: "Cargo.toml",
      jsonpath: "$.workspace.package.version",
    },
  ]);
  assert.equal(config["bump-minor-pre-major"], true);
  assert.equal(config["bump-patch-for-minor-pre-major"], true);
  const workflow = readFileSync(
    new URL("../../.github/workflows/release-maintenance.yml", import.meta.url),
    "utf8",
  );
  assert.match(workflow, /googleapis\/release-please-action@[a-f\d]{40}/);
  assert.match(workflow, /peter-evans\/create-pull-request@[a-f\d]{40}/);
  assert.match(workflow, /node scripts\/release\/maintenance\.mjs draft-build/);
  assert.match(workflow, /node scripts\/release\/maintenance\.mjs sync-pr/);
  assert.match(
    workflow,
    /node scripts\/release\/maintenance\.mjs next-development/,
  );
  assert.match(workflow, /cancel-in-progress: false/);
  const publisher = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(publisher, /workflows\/release-maintenance\.yml\/dispatches/);
  assert.match(publisher, /inputs\[stable_tag\]/);
  assert.match(publisher, /id: maintenance\n        continue-on-error: true/);
  assert.match(publisher, /steps\.maintenance\.outcome == 'failure'/);
  assert.match(publisher, /Do not retry publication/);
});

test("generated changelog changes retain the release validation owner", () => {
  const { contracts } = JSON.parse(
    readFileSync(
      new URL("../validation/validation.json", import.meta.url),
      "utf8",
    ),
  );
  const plan = buildAffectedPlan(["CHANGELOG.md"], {
    contracts: contracts.filter((owner) => owner.id === "release-publication"),
  });
  assert.deepEqual(plan.blockers, []);
  assert.ok(plan.owners.some((owner) => owner.id === "release-publication"));
});
