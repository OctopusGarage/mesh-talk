import assert from "node:assert/strict";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { execFileSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import {
  releaseHighlights,
  renderReleaseNotes,
} from "../release/render-notes.mjs";

const template = readFileSync(
  new URL("../../.github/release-notes.md", import.meta.url),
  "utf8",
);
const changelog =
  "# Changelog\n\n## 0.1.6\n\n- Curated release benefit.\n\n## 0.1.5\n\n- Older benefit.\n";
const source = {
  repository: "OctopusGarage/mesh-talk",
  sha: "a".repeat(40),
  runId: "123",
  runAttempt: "2",
};

for (const nightly of [false, true]) {
  test(`${nightly ? "Nightly" : "Stable"} notes bind verification to the actual build source`, () => {
    const tag = nightly ? "v0.1.6-nightly.123.2" : "v0.1.6";
    const ref = nightly ? "refs/heads/main" : `refs/tags/${tag}`;
    const notes = renderReleaseNotes(template, {
      ...source,
      tag,
      ref,
      nightly,
      changelog,
    });
    assert.match(notes, /mesh-talk_v0\.1\.6/);
    assert.ok(
      notes.includes(
        `--certificate-identity 'https://github.com/${source.repository}/.github/workflows/release.yml@${ref}'`,
      ),
    );
    assert.ok(
      notes.includes(`--source-ref '${ref}' --source-digest '${source.sha}'`),
    );
    assert.ok(
      notes.includes(
        `--signer-workflow '${source.repository}/.github/workflows/release.yml'`,
      ),
    );
    assert.ok(notes.includes("--deny-self-hosted-runners"));
    assert.doesNotMatch(
      notes,
      /identity-regexp|from tag|__[A-Z_]+__|STABLE_ONLY/,
    );
    if (nightly) {
      assert.match(notes, /^> \*\*Nightly preview — experimental\*\*/);
      assert.ok(notes.includes(source.sha));
      assert.ok(notes.includes("actions/runs/123/attempts/2"));
      assert.doesNotMatch(notes, /ghcr\.io|verified Linux container image/);
    } else {
      assert.match(
        notes,
        /ghcr\.io\/octopusgarage\/mesh-talk-post-office:v0\.1\.6/,
      );
      assert.match(notes, /same verified Linux binaries/);
      assert.doesNotMatch(notes, /Nightly preview/);
    }
  });
}

test("notes reject publication/source mode mismatches and unsafe identities", () => {
  const nightly = {
    ...source,
    tag: "v0.1.6-nightly.123.2",
    ref: "refs/heads/main",
    nightly: true,
  };
  for (const change of [
    { ref: "refs/heads/dev" },
    { sha: "main" },
    { runId: "124" },
    { runAttempt: "3" },
    { repository: "owner/repo'" },
    { nightly: false },
  ]) {
    assert.throws(() =>
      renderReleaseNotes(template, { ...nightly, ...change }),
    );
  }
  assert.throws(() =>
    renderReleaseNotes(template, {
      ...source,
      tag: "v0.1.6",
      ref: "refs/heads/main",
      nightly: false,
    }),
  );
});

test("the workflow renderer writes notes from the GitHub build identity", (t) => {
  const root = mkdtempSync(join(tmpdir(), "mesh-release-notes-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, ".github"));
  writeFileSync(join(root, ".github/release-notes.md"), template);
  writeFileSync(join(root, "CHANGELOG.md"), changelog);
  execFileSync(
    process.execPath,
    [fileURLToPath(new URL("../release/render-notes.mjs", import.meta.url))],
    {
      cwd: root,
      env: {
        ...process.env,
        ARTIFACT_TAG: "v0.1.6-nightly.123.2",
        SOURCE_REPO: source.repository,
        SOURCE_REF: "refs/heads/main",
        SOURCE_SHA: source.sha,
        GITHUB_RUN_ID: source.runId,
        GITHUB_RUN_ATTEMPT: source.runAttempt,
        RELEASE_NIGHTLY: "true",
      },
    },
  );
  assert.match(
    readFileSync(join(root, "release_notes.md"), "utf8"),
    /Curated release benefit/,
  );
  const workflow = readFileSync(
    new URL("../../.github/workflows/release.yml", import.meta.url),
    "utf8",
  );
  assert.match(
    workflow,
    /name: Prepare release notes[\s\S]*?run: node scripts\/release\/render-notes\.mjs/,
  );
});

test("highlights select only the exact simple or linked version section", () => {
  const body =
    "### Features\n\n- Keep drafts after restarting.\n\n### Fixes\n\n- Recover interrupted sends.";
  for (const heading of [
    "## 0.4.5",
    "## [0.4.5](https://example.com/v0.4.5) (2026-10-24)",
  ]) {
    const text = `# Changelog\n\n## 0.4.50\n\n- Future notes.\n\n${heading}\n\n${body}\n\n## 0.4.4\n\n- Historical notes.`;
    assert.equal(releaseHighlights(text, "0.4.5"), body);
  }
});

test("highlights reject invalid numeric versions and missing, duplicate or empty sections", () => {
  for (const version of [
    "v0.4.5",
    "0.04.5",
    "0.4.5-nightly.1.1",
    "0.4.5\n",
    "9007199254740992.0.0",
  ]) {
    assert.throws(
      () => releaseHighlights("## 0.4.5\n- Valid.", version),
      /version/,
    );
  }
  for (const text of [
    "## 0.4.50\n- Other.",
    "## 0.4.5-nightly.1.1\n- Preview.",
    "### 0.4.5\n- Nested.",
  ]) {
    assert.throws(() => releaseHighlights(text, "0.4.5"), /missing/i);
    assert.equal(releaseHighlights(text, "0.4.5", { required: false }), "");
  }
  for (const text of [
    "## 0.4.5\n\n## 0.4.4\n- Older.",
    "## 0.4.5\n<!-- TODO -->",
    "## 0.4.5\n- One.\n## [0.4.5](https://example.com)\n- Two.",
  ]) {
    for (const required of [true, false])
      assert.throws(
        () => releaseHighlights(text, "0.4.5", { required }),
        /empty|duplicate/i,
      );
  }
});

test("version-looking headings inside fenced code do not select or bound sections", () => {
  const body =
    "- Real benefit.\n\n```markdown\n## 0.4.4\nExample only.\n```\n\n~~~markdown\n## 0.4.5\nExample only.\n~~~";
  assert.equal(
    releaseHighlights(`## 0.4.5\n\n${body}\n\n## 0.4.4\n- Older.`, "0.4.5"),
    body,
  );
  assert.throws(
    () => releaseHighlights("```markdown\n## 0.4.5\n- Fake.\n```", "0.4.5"),
    /missing/i,
  );
});

test("Stable notes require curated highlights while Nightly never borrows another version", () => {
  const stable = { ...source, tag: "v0.1.6", ref: "refs/tags/v0.1.6" };
  assert.throws(
    () =>
      renderReleaseNotes(template, {
        ...stable,
        changelog: "## 0.1.5\n- Stale.",
      }),
    /missing/i,
  );
  const notes = renderReleaseNotes(template, { ...stable, changelog });
  assert.match(notes, /### Release highlights\n\n- Curated release benefit/);
  assert.doesNotMatch(
    notes,
    /Older benefit|Open the exact message selected in search/,
  );
  const nightly = renderReleaseNotes(template, {
    ...source,
    tag: "v0.1.7-nightly.123.2",
    ref: "refs/heads/main",
    nightly: true,
    changelog,
  });
  assert.doesNotMatch(
    nightly,
    /Release highlights|Curated release benefit|Older benefit/,
  );
  assert.match(nightly, /Verifying your download/);
});

test("template controls cannot be supplied by curated text and unknown tokens still fail", () => {
  const options = {
    ...source,
    tag: "v0.1.6",
    ref: "refs/tags/v0.1.6",
    changelog:
      "## 0.1.6\n- Literal __TAG__, $& and <!-- STABLE_ONLY_START --> examples.",
  };
  const notes = renderReleaseNotes(template, options);
  assert.ok(
    notes.includes(
      "Literal __TAG__, $& and <!-- STABLE_ONLY_START --> examples.",
    ),
  );
  assert.throws(
    () => renderReleaseNotes(`${template}\n__UNKNOWN_TOKEN__`, options),
    /unknown release notes placeholder/,
  );
});
