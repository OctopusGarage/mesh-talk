import assert from "node:assert/strict";
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { rustPackages } from "../pr-platform/rust-packages.mjs";

async function owner() {
  return import("../release/version.mjs");
}
function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), "mesh-version-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const put = (file, text) => {
    mkdirSync(dirname(join(root, file)), { recursive: true });
    writeFileSync(join(root, file), text);
  };
  const json = (file, value) =>
    put(file, `${JSON.stringify(value, null, 2)}\n`);
  put(
    "Cargo.toml",
    '[workspace]\nmembers = []\n[workspace.package]\nversion = "0.4.3"\nedition = "2024"\n[workspace.dependencies]\nunrelated = "0.4.3"\n',
  );
  for (const pkg of rustPackages)
    put(
      `${pkg.directory}/Cargo.toml`,
      `[package]\nname = "${pkg.package}"\nversion.workspace = true\n`,
    );
  put(
    "Cargo.lock",
    "version = 4\n" +
      [
        ...rustPackages.map(
          (pkg) => `[[package]]\nname = "${pkg.package}"\nversion = "0.4.3"\n`,
        ),
        '[[package]]\nname = "third-party"\nversion = "0.4.3"\nsource = "registry+example"\n',
      ].join("\n"),
  );
  json("src-tauri/tauri.conf.json", {
    productName: "Mesh Talk",
    version: "0.4.3",
  });
  for (const prefix of ["", "frontend/"]) {
    json(`${prefix}package.json`, {
      name: `${prefix}tooling`,
      version: "0.4.3",
      dependencies: { unrelated: "0.4.3" },
    });
    json(`${prefix}package-lock.json`, {
      name: `${prefix}tooling`,
      version: "0.4.3",
      packages: {
        "": { name: `${prefix}tooling`, version: "0.4.3" },
        "node_modules/unrelated": { version: "0.4.3" },
      },
    });
  }
  put("history.md", "Released 0.4.3 remains immutable.\n");
  return {
    root,
    put,
    json,
    read: (file) => readFileSync(join(root, file), "utf8"),
  };
}

test("prepare updates only canonical and derived product metadata", async (t) => {
  const { prepareVersion, checkVersions, readVersion } = await owner();
  const f = fixture(t);
  assert.equal(prepareVersion("0.4.4", f.root), "0.4.4");
  assert.equal(readVersion(f.root), "0.4.4");
  assert.equal(checkVersions(f.root), "0.4.4");
  assert.match(f.read("Cargo.toml"), /unrelated = "0.4.3"/);
  assert.match(f.read("Cargo.lock"), /name = "third-party"\nversion = "0.4.3"/);
  assert.equal(f.read("history.md"), "Released 0.4.3 remains immutable.\n");
  assert.equal(JSON.parse(f.read("package.json")).version, "0.4.4");
  for (const file of [
    "frontend/package.json",
    "frontend/package-lock.json",
    "src-tauri/tauri.conf.json",
  ])
    assert.equal(JSON.parse(f.read(file)).version, undefined);
  assert.equal(
    JSON.parse(f.read("frontend/package-lock.json")).packages[""].version,
    undefined,
  );
  assert.equal(
    JSON.parse(f.read("frontend/package-lock.json")).packages[
      "node_modules/unrelated"
    ].version,
    "0.4.3",
  );
  const before = f.read("Cargo.lock");
  prepareVersion("0.4.4", f.root);
  assert.equal(f.read("Cargo.lock"), before);
});

test("check detects drift; sync repairs projections without changing canonical target", async (t) => {
  const { syncVersions, checkVersions } = await owner();
  const f = fixture(t);
  assert.throws(() => checkVersions(f.root), /version|private/i);
  assert.equal(syncVersions(f.root), "0.4.3");
  assert.equal(checkVersions(f.root), "0.4.3");
  f.json("package.json", { private: true, version: "0.4.2" });
  assert.throws(() => checkVersions(f.root), /package.json/);
  syncVersions(f.root);
  assert.equal(checkVersions(f.root), "0.4.3");
});

test("prepare rejects invalid or backward targets before writing", async (t) => {
  const { prepareVersion, syncVersions } = await owner();
  const f = fixture(t);
  syncVersions(f.root);
  const before = f.read("Cargo.toml");
  for (const value of [
    "0.4.2",
    "v0.4.4",
    "0.4.4-dev",
    "01.4.4",
    "1.2",
    "1.2.9007199254740992",
  ]) {
    assert.throws(() => prepareVersion(value, f.root), /version|target/i);
    assert.equal(f.read("Cargo.toml"), before);
  }
});

test("all Rust crates must inherit the canonical target and lock records must be local", async (t) => {
  const { syncVersions, checkVersions, prepareVersion } = await owner();
  const f = fixture(t);
  syncVersions(f.root);
  f.put(
    "crates/mesh-talk-core/Cargo.toml",
    '[package]\nname = "mesh-talk-core"\nversion = "0.4.3"\n',
  );
  assert.throws(() => checkVersions(f.root), /inherit/);
  assert.throws(() => prepareVersion("0.4.4", f.root), /inherit/);
  assert.match(f.read("Cargo.toml"), /version = "0.4.3"/);
  f.put(
    "crates/mesh-talk-core/Cargo.toml",
    '[package]\nname = "mesh-talk-core"\nversion.workspace = true\n',
  );
  f.put(
    "Cargo.lock",
    f
      .read("Cargo.lock")
      .replace(
        'name = "mesh-talk-core"\nversion = "0.4.3"',
        'name = "mesh-talk-core"\nversion = "0.4.3"\nsource = "registry+example"',
      ),
  );
  assert.throws(
    () => checkVersions(f.root),
    /local.*mesh-talk-core|mesh-talk-core.*local/,
  );
});

test("standards reports version drift in a real product workspace", async (t) => {
  const { checkStandards } = await import("../validation/standards.mjs");
  const { contractFiles } = await import("../validation/contracts.mjs");
  const { spawnSync } = await import("node:child_process");
  const f = fixture(t);
  for (const file of contractFiles) f.json(file, { schema: 1, contracts: [] });
  assert.equal(
    spawnSync("git", ["init", "--quiet"], { cwd: f.root }).status,
    0,
  );
  assert.ok(
    (await checkStandards(f.root)).issues.some(
      (issue) => issue.code === "product-version",
    ),
  );
  (await owner()).syncVersions(f.root);
  assert.ok(
    !(await checkStandards(f.root)).issues.some(
      (issue) => issue.code === "product-version",
    ),
  );
});

test("CLI supports explicit roots and rejects unknown commands without writes", async (t) => {
  const { spawnSync } = await import("node:child_process");
  const { fileURLToPath } = await import("node:url");
  const cli = fileURLToPath(new URL("../release/version.mjs", import.meta.url));
  const f = fixture(t);
  const run = (...args) =>
    spawnSync(process.execPath, [cli, ...args, "--root", f.root], {
      encoding: "utf8",
    });
  assert.notEqual(run("check").status, 0);
  for (const command of ["constructor", "toString", "missing", "prepare"]) {
    const result = run(command);
    assert.notEqual(result.status, 0, command);
    assert.match(result.stderr, /Usage:/);
  }
  const result = run("prepare", "0.4.4");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /Product version prepare: 0.4.4/);
  assert.equal(run("check").status, 0);
});
