import assert from "node:assert/strict";
import {
  mkdtemp,
  mkdir,
  writeFile,
  readFile,
  stat,
  rm,
  symlink,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  prepareUbuntuAptSources,
  rewriteUbuntuAptSource,
  rewriteUbuntuAptMirrorList,
} from "./ubuntu-apt-source.mjs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const hosted = {
  GITHUB_ACTIONS: "true",
  RUNNER_ENVIRONMENT: "github-hosted",
  RUNNER_OS: "Linux",
};
// The CLI rejects Windows; its APT mutation boundary requires POSIX file
// ownership/modes and symlinks. Context rejection still runs on every platform.
const posixOnly = {
  skip:
    process.platform === "win32"
      ? "APT mutation requires a POSIX filesystem"
      : false,
};
async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), "mesh-talk-apt-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await mkdir(join(root, "etc/apt/sources.list.d"), { recursive: true });
  await writeFile(
    join(root, "etc/os-release"),
    'ID=ubuntu\nVERSION_ID="24.04"\n',
  );
  return root;
}

test(
  "hosted Ubuntu changes only the Azure archive URI and preserves signed repository configuration",
  posixOnly,
  async (t) => {
    const root = await fixture(t);
    const path = join(root, "etc/apt/sources.list.d/ubuntu.sources");
    const before = `Types: deb\nURIs: http://azure.archive.ubuntu.com/ubuntu/\nSuites: noble noble-updates noble-backports\nComponents: main restricted universe multiverse\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n\nTypes: deb\nURIs: http://security.ubuntu.com/ubuntu/\nSuites: noble-security\nComponents: main restricted universe multiverse\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n`;
    await writeFile(path, before, { mode: 0o640 });
    assert.equal(
      await prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
      1,
    );
    assert.equal(
      await readFile(path, "utf8"),
      `Types: deb\nURIs: https://archive.ubuntu.com/ubuntu/\nSuites: noble noble-updates noble-backports\nComponents: main restricted universe multiverse\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n\nTypes: deb\nURIs: http://security.ubuntu.com/ubuntu/\nSuites: noble-security\nComponents: main restricted universe multiverse\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n`,
    );
    assert.equal((await stat(path)).mode & 0o777, 0o640);
    assert.equal(
      await prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
      0,
    );
  },
);

test(
  "legacy lists and folded URI fields retain options, comments, other origins and line endings",
  posixOnly,
  async (t) => {
    const root = await fixture(t);
    const list = join(root, "etc/apt/sources.list");
    const sources = join(root, "etc/apt/sources.list.d/mixed.sources");
    await writeFile(
      list,
      "# deb http://azure.archive.ubuntu.com/ubuntu noble main\r\ndeb [arch=amd64 signed-by=/keys/ubuntu.gpg] https://azure.archive.ubuntu.com/ubuntu noble main # retain\r\ndeb http://security.ubuntu.com/ubuntu noble-security main\r\ndeb https://azure.archive.ubuntu.com.example/ubuntu noble main\r\n",
    );
    await writeFile(
      sources,
      "Types: deb\nURIs: https://mirror.example/ubuntu\n http://azure.archive.ubuntu.com/ubuntu\nSuites: noble\nSigned-By:\n -----BEGIN PGP PUBLIC KEY BLOCK-----\n http://azure.archive.ubuntu.com/ubuntu\n -----END PGP PUBLIC KEY BLOCK-----\n",
    );
    assert.equal(
      await prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
      2,
    );
    assert.equal(
      await readFile(list, "utf8"),
      "# deb http://azure.archive.ubuntu.com/ubuntu noble main\r\ndeb [arch=amd64 signed-by=/keys/ubuntu.gpg] https://archive.ubuntu.com/ubuntu noble main # retain\r\ndeb http://security.ubuntu.com/ubuntu noble-security main\r\ndeb https://azure.archive.ubuntu.com.example/ubuntu noble main\r\n",
    );
    assert.equal(
      await readFile(sources, "utf8"),
      "Types: deb\nURIs: https://mirror.example/ubuntu\n https://archive.ubuntu.com/ubuntu\nSuites: noble\nSigned-By:\n -----BEGIN PGP PUBLIC KEY BLOCK-----\n http://azure.archive.ubuntu.com/ubuntu\n -----END PGP PUBLIC KEY BLOCK-----\n",
    );
  },
);

test("non-Azure sources are a byte-for-byte no-op", async (t) => {
  const root = await fixture(t);
  const path = join(root, "etc/apt/sources.list.d/ubuntu.sources");
  const text =
    "Types: deb\nURIs: https://archive.ubuntu.com/ubuntu/ https://mirror.example/ubuntu\nSuites: noble\nSigned-By: /keys/ubuntu.gpg\n";
  await writeFile(path, text);
  const original = await stat(path);
  assert.equal(
    await prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
    0,
  );
  assert.equal(await readFile(path, "utf8"), text);
  assert.equal((await stat(path)).ino, original.ino);
});

test("unsupported Azure URLs fail before any otherwise valid file changes", async (t) => {
  for (const uri of [
    "HTTP://AZURE.ARCHIVE.UBUNTU.COM/ubuntu",
    "ftp://azure.archive.ubuntu.com/ubuntu",
    "http://azure.archive.ubuntu.com/ubuntu/dists",
    "http://azure.archive.ubuntu.com:80/ubuntu",
    "https://user@azure.archive.ubuntu.com/ubuntu",
    "http://azure.archive.ubuntu.com/ubuntu?extra=true",
  ]) {
    const root = await fixture(t);
    const first = join(root, "etc/apt/sources.list");
    const last = join(root, "etc/apt/sources.list.d/z.sources");
    const original = "deb http://azure.archive.ubuntu.com/ubuntu noble main\n";
    await writeFile(first, original);
    await writeFile(last, `URIs: ${uri}\nSuites: noble\n`);
    await assert.rejects(
      prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
      /Unsupported Azure APT/,
    );
    assert.equal(await readFile(first, "utf8"), original);
    assert.equal(await readFile(last, "utf8"), `URIs: ${uri}\nSuites: noble\n`);
  }
});

test("non-hosted, non-Linux and non-Ubuntu contexts reject without writes", async (t) => {
  const root = await fixture(t);
  const path = join(root, "etc/apt/sources.list");
  const original = "deb http://azure.archive.ubuntu.com/ubuntu noble main\n";
  await writeFile(path, original);
  for (const [env, platform] of [
    [{}, "linux"],
    [{ ...hosted, RUNNER_ENVIRONMENT: "self-hosted" }, "linux"],
    [hosted, "darwin"],
    [{ ...hosted, RUNNER_OS: "Windows" }, "win32"],
  ]) {
    await assert.rejects(
      prepareUbuntuAptSources({ root, env, platform }),
      /GitHub-hosted Linux runner/,
    );
  }
  await writeFile(join(root, "etc/os-release"), "ID=debian\nID_LIKE=ubuntu\n");
  await assert.rejects(
    prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
    /requires Ubuntu/,
  );
  assert.equal(await readFile(path, "utf8"), original);
});

test("browser and native dependency installation use the same Linux-only preparation", async () => {
  const ui = await readFile(
    new URL("../../.github/workflows/e2e-ui.yml", import.meta.url),
    "utf8",
  );
  const native = await readFile(
    new URL(
      "../../.github/workflows/hidden-contacts-native.yml",
      import.meta.url,
    ),
    "utf8",
  );
  const shards = await readFile(
    new URL("../../.github/workflows/ui-shards.yml", import.meta.url),
    "utf8",
  );
  let installers = 0;
  for (const [name, workflow] of [
    ["UI caller", ui],
    ["reusable shards", shards],
  ]) {
    for (const section of workflow.split(/^  [a-z][\w-]*:\n/m).slice(1)) {
      const install = section.indexOf("playwright install --with-deps");
      if (install === -1) continue;
      installers++;
      const preparations = section.split(
        /^      - name: Prepare Ubuntu archive source\n/m,
      );
      assert.equal(
        preparations.length,
        2,
        `${name}: every browser installer needs exactly one guarded preparation`,
      );
      const step = preparations[1].split(/^      - /m)[0];
      assert.match(step, /if: runner\.os == 'Linux'/);
      assert.match(
        step,
        /sudo --preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,RUNNER_OS/,
      );
      assert.match(step, /\.\.\/scripts\/ci\/ubuntu-apt-source\.mjs/);
      const prepare = section.indexOf("name: Prepare Ubuntu archive source");
      assert.ok(
        prepare > section.indexOf("uses: actions/setup-node@"),
        `${name}: establish Node before preparing APT`,
      );
      assert.ok(prepare < install, `${name}: prepare APT before the installer`);
    }
  }
  assert.ok(installers > 0, "browser dependency installers must be discovered");
  assert.ok(
    native.indexOf("name: Prepare Ubuntu archive source") <
      native.indexOf("name: Linux native webview dependencies"),
  );
  assert.match(
    native,
    /sudo --preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,RUNNER_OS.*scripts\/ci\/ubuntu-apt-source\.mjs/,
  );
  for (const name of ["ci.yml", "mutants.yml"]) {
    const workflow = await readFile(
      new URL(`../../.github/workflows/${name}`, import.meta.url),
      "utf8",
    );
    const prepare = workflow.indexOf("name: Prepare Ubuntu archive source");
    assert.ok(
      prepare > workflow.indexOf("uses: actions/setup-node@"),
      `${name} must establish the pinned Node before preparing APT`,
    );
    assert.ok(
      prepare < workflow.indexOf("name: Install Tauri system dependencies"),
      `${name} must prepare APT before Linux dependency installation`,
    );
    assert.match(
      workflow,
      /sudo --preserve-env=GITHUB_ACTIONS,RUNNER_ENVIRONMENT,RUNNER_OS.*scripts\/ci\/ubuntu-apt-source\.mjs/,
    );
  }
  const baseline = shards.split("  e2e:\n")[1].split(/^  [a-z][\w-]*:\n/m)[0];
  assert.match(baseline, /timeout-minutes: 25/);
  assert.match(
    baseline,
    /--workers=1 --retries=0 --shard=\$\{\{ matrix.shard \}\}\/4/,
  );
});

test(
  "a symlinked APT source cannot redirect a privileged replacement",
  posixOnly,
  async (t) => {
    const root = await fixture(t);
    const target = join(root, "outside.list");
    const original = "deb http://azure.archive.ubuntu.com/ubuntu noble main\n";
    await writeFile(target, original);
    await symlink(target, join(root, "etc/apt/sources.list.d/ubuntu.list"));
    await assert.rejects(
      prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
      /regular file/,
    );
    assert.equal(await readFile(target, "utf8"), original);
  },
);

test("pure deb822 rewriting preserves folded URIs and every signed configuration field", () => {
  const original =
    "# azure.archive.ubuntu.com stays a comment\r\nTypes: deb deb-src\r\nURIs: http://azure.archive.ubuntu.com/ubuntu/ https://mirror.example/ubuntu\r\n https://azure.archive.ubuntu.com/ubuntu\r\nSuites: noble noble-updates\r\nComponents: main universe\r\nSigned-By:\r\n -----BEGIN PGP PUBLIC KEY BLOCK-----\r\n http://azure.archive.ubuntu.com/ubuntu\r\n -----END PGP PUBLIC KEY BLOCK-----\r\n\r\nURIs: http://security.ubuntu.com/ubuntu\r\nSuites: noble-security\r\n";
  const expected =
    "# azure.archive.ubuntu.com stays a comment\r\nTypes: deb deb-src\r\nURIs: https://archive.ubuntu.com/ubuntu/ https://mirror.example/ubuntu\r\n https://archive.ubuntu.com/ubuntu\r\nSuites: noble noble-updates\r\nComponents: main universe\r\nSigned-By:\r\n -----BEGIN PGP PUBLIC KEY BLOCK-----\r\n http://azure.archive.ubuntu.com/ubuntu\r\n -----END PGP PUBLIC KEY BLOCK-----\r\n\r\nURIs: http://security.ubuntu.com/ubuntu\r\nSuites: noble-security\r\n";
  assert.equal(rewriteUbuntuAptSource(original, true), expected);
  assert.equal(rewriteUbuntuAptSource(expected, true), expected);
});

test("pure legacy rewriting preserves options, non-Azure origins and comment URLs", () => {
  const original =
    "deb [arch=amd64 signed-by=/keys/ubuntu.gpg] http://azure.archive.ubuntu.com/ubuntu noble main # http://azure.archive.ubuntu.com/ubuntu\ndeb-src https://azure.archive.ubuntu.com/ubuntu/ noble universe\ndeb http://security.ubuntu.com/ubuntu noble-security main\ndeb https://azure.archive.ubuntu.com.example/ubuntu noble main\n";
  const expected =
    "deb [arch=amd64 signed-by=/keys/ubuntu.gpg] https://archive.ubuntu.com/ubuntu noble main # http://azure.archive.ubuntu.com/ubuntu\ndeb-src https://archive.ubuntu.com/ubuntu/ noble universe\ndeb http://security.ubuntu.com/ubuntu noble-security main\ndeb https://azure.archive.ubuntu.com.example/ubuntu noble main\n";
  assert.equal(rewriteUbuntuAptSource(original, false), expected);
  assert.equal(rewriteUbuntuAptSource(expected, false), expected);
});

test("pure URI validation rejects unsupported Azure URLs on every platform", () => {
  for (const uri of [
    "ftp://azure.archive.ubuntu.com/ubuntu",
    "http://azure.archive.ubuntu.com/ubuntu/dists",
    "http://azure.archive.ubuntu.com:80/ubuntu",
    "https://user@azure.archive.ubuntu.com/ubuntu",
    "HTTP://AZURE.ARCHIVE.UBUNTU.COM/ubuntu",
    "http://azure.archive.ubuntu.com/ubuntu?x=1",
  ]) {
    assert.throws(
      () => rewriteUbuntuAptSource(`URIs: ${uri}\nSuites: noble\n`, true),
      /Unsupported Azure APT URI/,
    );
    assert.throws(
      () => rewriteUbuntuAptSource(`deb ${uri} noble main\n`, false),
      /Unsupported Azure APT URI/,
    );
  }
});

test("the actual CLI rejects a non-GitHub context before accessing host APT sources", () => {
  const child = spawnSync(
    process.execPath,
    [fileURLToPath(new URL("./ubuntu-apt-source.mjs", import.meta.url))],
    {
      env: { ...process.env, GITHUB_ACTIONS: "false" },
      encoding: "utf8",
    },
  );
  assert.equal(child.error, undefined);
  assert.equal(child.status, 1);
  assert.match(
    child.stderr,
    /requires a disposable GitHub-hosted Linux runner/,
  );
});

test(
  "the runner's referenced local mirror list replaces only its Azure URI and keeps priorities",
  posixOnly,
  async (t) => {
    const root = await fixture(t);
    const source = join(root, "etc/apt/sources.list.d/ubuntu.sources");
    const legacy = join(root, "etc/apt/sources.list");
    const mirror = join(root, "etc/apt/apt-mirrors.txt");
    const signed =
      "Types: deb\nURIs: mirror+file:/etc/apt/apt-mirrors.txt\nSuites: noble noble-updates noble-backports noble-security\nComponents: main universe\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n";
    await writeFile(source, signed);
    await writeFile(
      legacy,
      "deb mirror+file:/etc/apt/apt-mirrors.txt noble main\n",
    );
    const rows =
      "http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\nhttps://archive.ubuntu.com/ubuntu/\tpriority:2\nhttp://security.ubuntu.com/ubuntu/\tpriority:3\n\n";
    assert.equal(Buffer.byteLength(rows), 144);
    await writeFile(mirror, rows, { mode: 0o640 });
    assert.equal(
      await prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
      1,
    );
    assert.equal(
      await readFile(mirror, "utf8"),
      "https://archive.ubuntu.com/ubuntu/\tpriority:1\nhttps://archive.ubuntu.com/ubuntu/\tpriority:2\nhttp://security.ubuntu.com/ubuntu/\tpriority:3\n\n",
    );
    assert.equal(await readFile(source, "utf8"), signed);
    assert.equal(
      await readFile(legacy, "utf8"),
      "deb mirror+file:/etc/apt/apt-mirrors.txt noble main\n",
    );
    assert.equal((await stat(mirror)).mode & 0o777, 0o640);
    assert.equal(
      await prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
      0,
    );
  },
);

test("pure mirror-list rewriting preserves priority annotations, comments, security and other origins", () => {
  const input =
    "# http://azure.archive.ubuntu.com/ubuntu\nhttp://azure.archive.ubuntu.com/ubuntu/\tpriority:1 arch:amd64\nhttps://archive.ubuntu.com/ubuntu/\tpriority:2\nhttp://security.ubuntu.com/ubuntu/\tpriority:3\nhttps://mirror.example/ubuntu\tpriority:4\n";
  const expected =
    "# http://azure.archive.ubuntu.com/ubuntu\nhttps://archive.ubuntu.com/ubuntu/\tpriority:1 arch:amd64\nhttps://archive.ubuntu.com/ubuntu/\tpriority:2\nhttp://security.ubuntu.com/ubuntu/\tpriority:3\nhttps://mirror.example/ubuntu\tpriority:4\n";
  assert.equal(rewriteUbuntuAptMirrorList(input), expected);
  assert.equal(rewriteUbuntuAptMirrorList(expected), expected);
});

test("unsafe or unsupported mirror-list references reject in both source formats", () => {
  for (const uri of [
    "mirror+file:/etc/apt/../secrets",
    "mirror+file://host/etc/apt/apt-mirrors.txt",
    "mirror+file:///etc/apt/apt-mirrors.txt",
    "mirror+file:/tmp/mirrors",
    "mirror+https://mirror.example/list",
    "mirror+file:/etc/apt/apt-mirrors.txt?extra=1",
  ]) {
    assert.throws(
      () => rewriteUbuntuAptSource(`URIs: ${uri}\n`, true),
      /Unsupported APT mirror-list reference/,
    );
    assert.throws(
      () => rewriteUbuntuAptSource(`deb ${uri} noble main\n`, false),
      /Unsupported APT mirror-list reference/,
    );
  }
});

test("only referenced mirror files are inspected or rewritten", async (t) => {
  const root = await fixture(t);
  const mirror = join(root, "etc/apt/apt-mirrors.txt");
  const rows = "http://azure.archive.ubuntu.com/ubuntu priority:1\n";
  await writeFile(mirror, rows);
  assert.equal(
    await prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
    0,
  );
  assert.equal(await readFile(mirror, "utf8"), rows);
});

test("a missing referenced mirror list rejects before a valid direct source is changed", async (t) => {
  const root = await fixture(t);
  const direct = join(root, "etc/apt/sources.list");
  const original = "deb http://azure.archive.ubuntu.com/ubuntu noble main\n";
  await writeFile(direct, original);
  await writeFile(
    join(root, "etc/apt/sources.list.d/mirror.sources"),
    "URIs: mirror+file:/etc/apt/apt-mirrors.txt\nSuites: noble\n",
  );
  await assert.rejects(
    prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
    /Referenced APT mirror list is missing/,
  );
  assert.equal(await readFile(direct, "utf8"), original);
});

test("a referenced mirror file cannot be a symlink", posixOnly, async (t) => {
  const root = await fixture(t);
  const outside = join(root, "outside-mirrors.txt");
  const original = "http://azure.archive.ubuntu.com/ubuntu priority:1\n";
  await writeFile(outside, original);
  await symlink(outside, join(root, "etc/apt/apt-mirrors.txt"));
  await writeFile(
    join(root, "etc/apt/sources.list"),
    "deb mirror+file:/etc/apt/apt-mirrors.txt noble main\n",
  );
  await assert.rejects(
    prepareUbuntuAptSources({ root, env: hosted, platform: "linux" }),
    /regular file/,
  );
  assert.equal(await readFile(outside, "utf8"), original);
});

test("malformed active source rows reject regardless of hostname text", () => {
  for (const row of ["deb", "deb-src", "deb [arch=amd64]"]) {
    assert.throws(
      () => rewriteUbuntuAptSource(`${row}\n`, false),
      /Unsupported APT/,
    );
  }
});

test("host matching ignores Azure text in credentials, paths, queries and lookalike hosts", () => {
  for (const uri of [
    "https://azure.archive.ubuntu.com.example/ubuntu",
    "https://notazure.archive.ubuntu.com/ubuntu",
    "https://azure.archive.ubuntu.com@mirror.example/ubuntu",
    "https://mirror.example/azure.archive.ubuntu.com",
    "https://mirror.example/ubuntu?host=azure.archive.ubuntu.com",
  ]) {
    const source = `deb ${uri} noble main\n`;
    assert.equal(rewriteUbuntuAptSource(source, false), source);
    const deb822 = `URIs: ${uri}\n`;
    assert.equal(rewriteUbuntuAptSource(deb822, true), deb822);
    assert.equal(rewriteUbuntuAptMirrorList(`${uri}\n`), `${uri}\n`);
  }
});

test("invalid URI tokens reject in source fields and mirror lists", () => {
  for (const uri of ["not-a-url", "http://[invalid]/ubuntu"]) {
    assert.throws(
      () => rewriteUbuntuAptSource(`deb ${uri} noble main\n`, false),
      /Unsupported APT URI/,
    );
    assert.throws(
      () => rewriteUbuntuAptSource(`URIs: ${uri}\n`, true),
      /Unsupported APT URI/,
    );
    assert.throws(
      () => rewriteUbuntuAptMirrorList(`${uri}\n`),
      /Unsupported APT URI/,
    );
  }
});
