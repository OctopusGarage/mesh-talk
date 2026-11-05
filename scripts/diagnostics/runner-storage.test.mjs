import assert from "node:assert/strict";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  lstat,
  rm,
  symlink,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  assertHostedUbuntu,
  prepareRunnerStorage,
} from "../ci/runner-storage.mjs";

const hosted = {
  GITHUB_ACTIONS: "true",
  RUNNER_ENVIRONMENT: "github-hosted",
  RUNNER_OS: "Linux",
};
const posix = {
  skip:
    process.platform === "win32" ? "POSIX image filesystem required" : false,
};
async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), "mesh-talk-storage-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(join(root, "etc"));
  await writeFile(join(root, "etc/os-release"), 'ID="ubuntu"\n');
  return root;
}
function context(root) {
  return { root, env: hosted, platform: "linux" };
}
async function payload(root, path) {
  await mkdir(join(root, path), { recursive: true });
  await writeFile(join(root, path, "keep"), "payload");
}

test("rejects non-hosted, self-hosted and non-Linux contexts before mutation", async (t) => {
  const root = await fixture(t);
  for (const override of [
    { env: {} },
    { env: { ...hosted, RUNNER_ENVIRONMENT: "self-hosted" } },
    { env: { ...hosted, RUNNER_OS: "Windows" } },
    { platform: "darwin" },
  ]) {
    await assert.rejects(
      prepareRunnerStorage({ ...context(root), ...override }),
      /requires a disposable GitHub-hosted Linux runner/,
    );
  }
});

test("CLI rejects missing confirmation and arbitrary path arguments", () => {
  for (const args of [[], ["--prepare", "/tmp"], ["--root", "/tmp"]]) {
    const result = spawnSync(
      process.execPath,
      [
        fileURLToPath(new URL("../ci/runner-storage.mjs", import.meta.url)),
        ...args,
      ],
      { encoding: "utf8" },
    );
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Usage: sudo node/);
  }
});

test("rejects non-Ubuntu and symlink roots", posix, async (t) => {
  const root = await fixture(t);
  await writeFile(join(root, "etc/os-release"), "ID=debian\n");
  await assert.rejects(assertHostedUbuntu(context(root)), /requires Ubuntu/);
  const parent = await fixture(t);
  await symlink(root, join(parent, "linked"));
  await assert.rejects(
    assertHostedUbuntu(context(join(parent, "linked"))),
    /real directory/,
  );
  await assert.rejects(
    assertHostedUbuntu(context(join(parent, "linked") + "/")),
    /real directory/,
  );
});

test("removes only the fixed unused SDK directories and reports actual filesystem measurements", async (t) => {
  const root = await fixture(t);
  const unused = ["usr/share/dotnet", "usr/local/lib/android", "opt/ghc"];
  const preserved = [
    "usr/local/bin",
    "usr/local/lib/node",
    "usr/share/java",
    "opt/hostedtoolcache",
    "workspace",
    "home/runner/.cargo",
    "home/runner/.rustup",
    "usr/local/lib/python",
  ];
  for (const path of [...unused, ...preserved]) await payload(root, path);
  const report = await prepareRunnerStorage(context(root));
  assert.deepEqual(
    report.removed,
    unused.map((path) => `/${path}`),
  );
  assert.deepEqual(report.skipped, []);
  assert.equal(Number.isFinite(report.availableBytesBefore), true);
  assert.equal(Number.isFinite(report.availableBytesAfter), true);
  for (const path of unused)
    await assert.rejects(lstat(join(root, path)), { code: "ENOENT" });
  for (const path of preserved)
    assert.equal(await readFile(join(root, path, "keep"), "utf8"), "payload");
  const again = await prepareRunnerStorage(context(root));
  assert.deepEqual(again.removed, []);
  assert.equal(again.skipped.length, 3);
});

test(
  "skips symlink parents and candidates, preserving all outside data",
  posix,
  async (t) => {
    const root = await fixture(t);
    const outside = await fixture(t);
    await payload(outside, "share/dotnet");
    await payload(outside, "android");
    await mkdir(join(root, "usr/local/lib"), { recursive: true });
    await symlink(outside, join(root, "usr/share"));
    await symlink(
      join(outside, "android"),
      join(root, "usr/local/lib/android"),
    );
    await mkdir(join(root, "opt"));
    await writeFile(join(root, "opt/ghc"), "not a directory");
    const report = await prepareRunnerStorage(context(root));
    assert.deepEqual(report.removed, []);
    assert.equal(report.skipped.length, 3);
    assert.equal(
      await readFile(join(outside, "share/dotnet/keep"), "utf8"),
      "payload",
    );
    assert.equal(
      await readFile(join(outside, "android/keep"), "utf8"),
      "payload",
    );
    assert.equal(
      await readFile(join(root, "opt/ghc"), "utf8"),
      "not a directory",
    );
  },
);

test(
  "removing an allowed directory unlinks nested symlinks without deleting targets",
  posix,
  async (t) => {
    const root = await fixture(t);
    const outside = await fixture(t);
    await payload(outside, "workspace");
    await payload(root, "opt/ghc");
    await symlink(join(outside, "workspace"), join(root, "opt/ghc/link"));
    await prepareRunnerStorage(context(root));
    assert.equal(
      await readFile(join(outside, "workspace/keep"), "utf8"),
      "payload",
    );
  },
);
