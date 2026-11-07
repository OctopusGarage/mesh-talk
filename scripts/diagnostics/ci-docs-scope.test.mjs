import assert from "node:assert/strict";
import test from "node:test";
import {
  mkdtemp,
  mkdir,
  writeFile,
  rm,
  readFile,
  copyFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { execFileSync, spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { contractFiles } from "../validation/contracts.mjs";
const modulePath = fileURLToPath(
  new URL("../ci/docs-scope.mjs", import.meta.url),
);
const repository = fileURLToPath(new URL("../../", import.meta.url));
const git = (root, ...args) =>
  execFileSync("git", args, { cwd: root, encoding: "utf8" }).trim();
async function fixture(
  t,
  change = async (root) => writeFile(path.join(root, "docs/story.md"), "new"),
) {
  const root = await mkdtemp(path.join(tmpdir(), "docs-scope-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  git(root, "init", "-q", "-b", "main");
  git(root, "config", "user.email", "test@example.com");
  git(root, "config", "user.name", "Test");
  git(root, "config", "commit.gpgsign", "false");
  git(root, "config", "core.hooksPath", path.join(root, "no-hooks"));
  for (const file of contractFiles) {
    await mkdir(path.dirname(path.join(root, file)), { recursive: true });
    await writeFile(
      path.join(root, file),
      JSON.stringify({ schema: 1, contracts: [] }),
    );
  }
  await mkdir(path.join(root, "docs"), { recursive: true });
  await writeFile(path.join(root, "docs/story.md"), "old");
  git(root, "add", ".");
  git(root, "commit", "-qm", "base");
  const base = git(root, "rev-parse", "HEAD");
  git(root, "checkout", "-qb", "topic");
  await change(root);
  git(root, "add", "-A");
  git(root, "commit", "--allow-empty", "-qm", "change");
  const head = git(root, "rev-parse", "HEAD");
  git(root, "checkout", "-q", "main");
  await writeFile(path.join(root, "base-marker"), "base");
  git(root, "add", ".");
  git(root, "commit", "-qm", "base advance");
  const mergeBase = git(root, "rev-parse", "HEAD");
  git(root, "merge", "--no-ff", "-qm", "merge", "topic");
  const source = git(root, "rev-parse", "HEAD");
  const env = {
    GITHUB_EVENT_NAME: "pull_request",
    GITHUB_SHA: source,
    GITHUB_REPOSITORY: "owner/repo",
    GITHUB_REF: "refs/pull/1/merge",
  };
  const event = {
    repository: { full_name: "owner/repo" },
    pull_request: {
      number: 1,
      base: { ref: "main", sha: mergeBase, repo: { full_name: "owner/repo" } },
      head: { sha: head, repo: { full_name: "owner/repo" } },
    },
  };
  return { root, env, event, base, head, source };
}
// Git trees can contain names that the host filesystem cannot materialize.
// Build both commits through the index, retaining the exact tested merge proof.
async function gitEntryFixture(t, file, mode = "100644") {
  const f = await fixture(t, async () => {});
  // Match hosts that cannot observe executable bits or create native symlinks.
  git(f.root, "config", "core.filemode", "false");
  git(f.root, "config", "core.symlinks", "false");
  // Windows also validates index paths. Only this disposable object fixture
  // admits the deliberately invalid name; it never checks that entry out.
  git(f.root, "config", "core.protectNTFS", "false");
  const blob = execFileSync("git", ["hash-object", "-w", "--stdin"], {
    cwd: f.root,
    encoding: "utf8",
    input: "changed",
  }).trim();
  function treeWithPath(commit) {
    git(f.root, "read-tree", commit);
    execFileSync("git", ["update-index", "-z", "--index-info"], {
      cwd: f.root,
      input: `${mode} ${blob}\t${file}\0`,
    });
    return git(f.root, "write-tree");
  }
  const head = git(
    f.root,
    "commit-tree",
    treeWithPath(f.head),
    "-p",
    f.base,
    "-m",
    "raw path head",
  );
  const source = git(
    f.root,
    "commit-tree",
    treeWithPath(f.source),
    "-p",
    f.event.pull_request.base.sha,
    "-p",
    head,
    "-m",
    "raw path merge",
  );
  git(f.root, "update-ref", "refs/heads/topic", head);
  git(f.root, "update-ref", "HEAD", source);
  f.head = head;
  f.source = source;
  f.env.GITHUB_SHA = source;
  f.event.pull_request.head.sha = head;
  return f;
}
async function resolve(options) {
  const { resolveScope } = await import("../ci/docs-scope.mjs");
  return resolveScope(options);
}
test("real merge admits unowned prose and preserves exact source delta", async (t) => {
  const f = await fixture(t);
  const d = await resolve(f);
  assert.equal(d.full, false);
  assert.deepEqual(d.files, ["docs/story.md"]);
  assert.equal(d.source, f.source);
  assert.equal(d.base, f.event.pull_request.base.sha);
});
test("rejects forged merge source metadata", async (t) => {
  const f = await fixture(t);
  f.env.GITHUB_SHA = f.head;
  await assert.rejects(resolve(f));
});
for (const name of [
  "push",
  "merge_group",
  "schedule",
  "workflow_dispatch",
  "local",
])
  test(`${name} always selects full`, async (t) => {
    const f = await fixture(t);
    f.env.GITHUB_EVENT_NAME = name;
    assert.equal((await resolve(f)).full, true);
  });
for (const file of [
  "code.rs",
  "docs/help/story.md",
  "docs/evals/story.md",
  "docs/ci-maintenance.md",
  "docs/quality-gates.md",
  "docs/testing-feedback-loop.md",
  "docs/ai-coding-reliability.md",
  "docs/DOCUMENTATION_GUIDELINES.md",
  "docs/.hidden.md",
  "docs/a\nb.md",
])
  test(`full for ${JSON.stringify(file)}`, async (t) => {
    const f = file.includes("\n")
      ? await gitEntryFixture(t, file)
      : await fixture(t, async (root) => {
          await mkdir(path.dirname(path.join(root, file)), { recursive: true });
          await writeFile(path.join(root, file), "changed");
        });
    if (file.includes("\n"))
      await assert.rejects(readFile(path.join(f.root, file)), {
        code: "ENOENT",
      });
    const decision = await resolve(f);
    assert.deepEqual(decision.files, [file]);
    assert.equal(decision.full, true);
  });
for (const mutation of ["delete", "rename", "empty", "executable", "symlink"])
  test(`full for ${mutation}`, async (t) => {
    const mode =
      mutation === "executable"
        ? "100755"
        : mutation === "symlink"
          ? "120000"
          : null;
    const f = mode
      ? await gitEntryFixture(t, "docs/story.md", mode)
      : await fixture(t, async (root) => {
          const file = path.join(root, "docs/story.md");
          if (mutation === "delete") await rm(file);
          if (mutation === "rename") {
            await rm(file);
            await writeFile(path.join(root, "docs/renamed.md"), "old");
          }
        });
    const decision = await resolve(f);
    if (mode) {
      const raw = git(
        f.root,
        "diff",
        "--raw",
        "--no-abbrev",
        "--no-renames",
        "--no-ext-diff",
        "--no-textconv",
        "-z",
        f.event.pull_request.base.sha,
        f.source,
        "--",
      );
      assert.match(
        raw,
        new RegExp(
          `^:100644 ${mode} [a-f0-9]{40} [a-f0-9]{40} ${mode === "120000" ? "T" : "M"}\\0docs/story\\.md\\0$`,
          "u",
        ),
      );
      assert.deepEqual(decision.files, ["docs/story.md"]);
      assert.equal(
        decision.reason,
        "non-prose or empty delta requires full verification",
      );
    }
    assert.equal(decision.full, true);
  });
for (const field of ["base", "head", "repository", "ref", "shallow"])
  test(`reject invalid ${field}`, async (t) => {
    const f = await fixture(t);
    if (field === "base") f.event.pull_request.base.sha = f.head;
    if (field === "head") f.event.pull_request.head.sha = f.base;
    if (field === "repository")
      f.event.pull_request.base.repo.full_name = "other/repo";
    if (field === "ref") f.env.GITHUB_REF = "refs/heads/main";
    if (field === "shallow")
      await writeFile(path.join(f.root, ".git/shallow"), f.source + "\n");
    await assert.rejects(resolve(f));
  });
test("base other than main selects full", async (t) => {
  const f = await fixture(t);
  f.event.pull_request.base.ref = "release";
  assert.equal((await resolve(f)).full, true);
});
test("owner and capability authority require full", async (t) => {
  for (const kind of ["owner", "guide", "invariant"]) {
    const f = await fixture(t);
    const c = {
      id: "test-owner",
      paths: kind === "owner" ? ["docs/story.md"] : ["code/"],
      node: ["scripts/test.mjs"],
    };
    if (kind !== "owner")
      c.capability = {
        summary: "Example",
        guide: kind === "guide" ? "docs/story.md#intro" : "docs/other.md",
        invariants: [
          {
            name: "Invariant",
            reference:
              kind === "invariant" ? "docs/story.md#rule" : "docs/other.md",
          },
        ],
      };
    await writeFile(
      path.join(f.root, contractFiles[0]),
      JSON.stringify({ schema: 1, contracts: [c] }),
    );
    assert.equal((await resolve(f)).full, true);
  }
});
test("actual inventory recognizes registered authority references", async (t) => {
  const f = await fixture(t);
  for (const file of contractFiles)
    await copyFile(path.join(repository, file), path.join(f.root, file));
  assert.equal((await resolve(f)).full, false);
});
test("corrupt contracts reject", async (t) => {
  const f = await fixture(t);
  await writeFile(path.join(f.root, contractFiles[0]), "invalid");
  await assert.rejects(resolve(f));
});
test("caller hints cannot manufacture docs success", async (t) => {
  const f = await fixture(t);
  assert.equal((await resolve({ ...f, expectedFull: "true" })).full, true);
  for (const expectedFull of ["", "invalid"])
    await assert.rejects(resolve({ ...f, expectedFull }));
  await assert.rejects(resolve({ ...f, upstreamResult: "failure" }));
  f.env.GITHUB_EVENT_NAME = "push";
  await assert.rejects(resolve({ ...f, expectedFull: "false" }));
});
test("CLI publishes only after a valid decision", async (t) => {
  const f = await fixture(t);
  const payload = path.join(f.root, "event.json"),
    output = path.join(f.root, "output"),
    summary = path.join(f.root, "summary");
  await writeFile(payload, JSON.stringify(f.event));
  const env = {
    ...process.env,
    ...f.env,
    GITHUB_EVENT_PATH: payload,
    GITHUB_OUTPUT: output,
    GITHUB_STEP_SUMMARY: summary,
  };
  let result = spawnSync(
    process.execPath,
    [modulePath, "--expected-full", "false"],
    { cwd: f.root, env, encoding: "utf8" },
  );
  assert.equal(result.status, 0, result.stderr);
  assert.equal(await readFile(output, "utf8"), "full=false\n");
  assert.match(
    await readFile(summary, "utf8"),
    /N\/A.*product tests not executed/i,
  );
  await rm(output);
  result = spawnSync(
    process.execPath,
    [modulePath, "--upstream-result", "failure"],
    { cwd: f.root, env, encoding: "utf8" },
  );
  assert.notEqual(result.status, 0);
  await assert.rejects(readFile(output));
  assert.equal(result.stdout, "");
});
test("raw parser rejects truncation, malformed and rename records", async () => {
  const { parseRawDiff, isProseChange } = await import("../ci/docs-scope.mjs");
  for (const value of [
    "broken",
    ":100644 100644 " + "a".repeat(40) + " " + "b".repeat(40) + " M\0",
    ":100644 100644 " + "a".repeat(40) + " " + "b".repeat(40) + " R100\0a\0b\0",
  ])
    assert.throws(() => parseRawDiff(value));
  for (const file of [
    "docs/../x.md",
    "docs//x.md",
    "/docs/x.md",
    "docs/.x/story.md",
    "docs/story.md\u007f",
  ])
    assert.equal(
      isProseChange({
        path: file,
        status: "M",
        oldMode: "100644",
        newMode: "100644",
      }),
      false,
    );
});
test("mixed prose and code delta requires full", async (t) => {
  const f = await fixture(t, async (root) => {
    await writeFile(path.join(root, "docs/story.md"), "new");
    await writeFile(path.join(root, "source.rs"), "code");
  });
  assert.equal((await resolve(f)).full, true);
});
test("missing parent object cannot grant docs scope", async (t) => {
  const f = await fixture(t);
  await rm(
    path.join(f.root, ".git/objects", f.head.slice(0, 2), f.head.slice(2)),
  );
  await assert.rejects(resolve(f));
});
test("invalid payload and failed Git execution reject", async (t) => {
  const f = await fixture(t);
  await assert.rejects(resolve({ ...f, event: {} }));
  await assert.rejects(
    resolve({
      ...f,
      root: path.join(f.root, "docs"),
      env: { ...f.env, GITHUB_SHA: "x" },
    }),
  );
  await assert.rejects(resolve({ ...f, root: tmpdir() }));
});
test("registered guide in actual inventory is authoritative", async (t) => {
  const f = await fixture(t, async (root) =>
    writeFile(path.join(root, "docs/updates.md"), "changed"),
  );
  for (const file of contractFiles)
    await copyFile(path.join(repository, file), path.join(f.root, file));
  assert.equal((await resolve(f)).full, true);
});

test("fork PR retains full provider verification", async (t) => {
  const f = await fixture(t);
  f.event.pull_request.head.repo.full_name = "fork/repo";
  const d = await resolve(f);
  assert.equal(d.full, true);
  assert.match(d.reason, /provider/);
});
test("missing head repository rejects", async (t) => {
  const f = await fixture(t);
  delete f.event.pull_request.head.repo;
  await assert.rejects(resolve(f));
});
test("normal added README and nested prose qualify", async (t) => {
  for (const file of ["README.md", "docs/nested/intro.md"]) {
    const f = await fixture(t, async (root) => {
      await mkdir(path.dirname(path.join(root, file)), { recursive: true });
      await writeFile(path.join(root, file), "prose");
    });
    assert.equal((await resolve(f)).full, false);
  }
});
test("unsafe false hint and malformed payload publish no scope output", async (t) => {
  const f = await fixture(t, async (root) =>
    writeFile(path.join(root, "code.rs"), "code"),
  );
  const payload = path.join(f.root, "event.json"),
    output = path.join(f.root, "output");
  await writeFile(payload, JSON.stringify(f.event));
  const env = {
    ...process.env,
    ...f.env,
    GITHUB_EVENT_PATH: payload,
    GITHUB_OUTPUT: output,
    GITHUB_STEP_SUMMARY: "",
  };
  for (const args of [
    ["--expected-full", "false"],
    ["--expected-full", ""],
  ]) {
    const result = spawnSync(process.execPath, [modulePath, ...args], {
      cwd: f.root,
      env,
      encoding: "utf8",
    });
    assert.notEqual(result.status, 0);
    assert.equal(result.stdout, "");
    await assert.rejects(readFile(output));
  }
  await writeFile(payload, "{");
  const result = spawnSync(process.execPath, [modulePath], {
    cwd: f.root,
    env,
    encoding: "utf8",
  });
  assert.notEqual(result.status, 0);
  await assert.rejects(readFile(output));
});

for (const depth of [1, 2])
  test(`real shallow clone depth ${depth} requires only available merge parents`, async (t) => {
    const f = await fixture(t);
    const directory = await mkdtemp(path.join(tmpdir(), "docs-scope-clone-"));
    t.after(() => rm(directory, { recursive: true, force: true }));
    const root = path.join(directory, "clone");
    git(
      directory,
      "clone",
      "--quiet",
      "--no-local",
      `--depth=${depth}`,
      pathToFileURL(f.root).href,
      root,
    );
    assert.equal(git(root, "rev-parse", "--is-shallow-repository"), "true");
    if (depth === 1) await assert.rejects(resolve({ ...f, root }));
    else {
      for (const commit of [f.source, f.event.pull_request.base.sha, f.head])
        git(root, "cat-file", "-e", `${commit}^{commit}`);
      assert.equal((await resolve({ ...f, root })).full, false);
    }
  });
