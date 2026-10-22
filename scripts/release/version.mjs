#!/usr/bin/env node
/** Product version authority and scoped generated projections. No network or dependency resolution. */
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { rustPackages } from "../pr-platform/rust-packages.mjs";

const numericVersion = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
function parts(version) {
  assert.equal(typeof version, "string", "numeric product version required");
  assert.match(
    version,
    numericVersion,
    "product version must be numeric SemVer (MAJOR.MINOR.PATCH)",
  );
  const values = version.split(".").map(Number);
  assert.ok(
    values.every(Number.isSafeInteger),
    "product version components must be safe integers",
  );
  return values;
}
export function compareVersions(left, right) {
  const a = parts(left),
    b = parts(right);
  for (let index = 0; index < a.length; index++) {
    if (a[index] !== b[index]) return a[index] < b[index] ? -1 : 1;
  }
  return 0;
}
// Read only the explicitly owned simple TOML sections. Unsupported forms fail
// closed instead of guessing at arbitrary TOML or changing similarly named fields.
function section(source, name) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const matches = [
    ...source.matchAll(
      new RegExp(`^\\[${escaped}\\][ \\t]*(?:#.*)?\\r?$`, "gm"),
    ),
  ];
  assert.equal(matches.length, 1, `expected exactly one [${name}] section`);
  const start = matches[0].index + matches[0][0].length;
  const next = source.slice(start).search(/^\[/m);
  const end = next === -1 ? source.length : start + next;
  return { start, end, text: source.slice(start, end) };
}
function scalar(source, key) {
  const matches = [
    ...source.matchAll(
      new RegExp(`^${key}\\s*=\\s*"([^"\\r\\n]+)"[ \\t]*(?:#.*)?\\r?$`, "gm"),
    ),
  ];
  assert.equal(matches.length, 1, `expected exactly one ${key} string`);
  return matches[0][1];
}
export function readVersion(root = process.cwd()) {
  const manifest = readFileSync(path.join(root, "Cargo.toml"), "utf8");
  const version = scalar(
    section(manifest, "workspace.package").text,
    "version",
  );
  parts(version);
  return version;
}

function projectionPlan(root, version) {
  const changes = new Map();
  const read = (file) => readFileSync(path.join(root, file), "utf8");
  const json = (file, update) => {
    const value = JSON.parse(read(file));
    update(value);
    changes.set(file, `${JSON.stringify(value, null, 2)}\n`);
  };
  for (const pkg of rustPackages) {
    const manifest = section(
      read(`${pkg.directory}/Cargo.toml`),
      "package",
    ).text;
    assert.equal(
      scalar(manifest, "name"),
      pkg.package,
      `unexpected Rust package at ${pkg.directory}`,
    );
    assert.match(
      manifest,
      /^version\.workspace\s*=\s*true[ \t]*(?:#.*)?\r?$/m,
      `${pkg.package} must inherit workspace version`,
    );
    assert.doesNotMatch(
      manifest,
      /^version\s*=/m,
      `${pkg.package} must inherit workspace version`,
    );
  }
  const lock = read("Cargo.lock");
  const localNames = new Set(rustPackages.map((pkg) => pkg.package));
  const seen = new Set();
  const updatedLock = lock.replace(
    /^\[\[package\]\][\s\S]*?(?=^\[\[package\]\]|$(?![\s\S]))/gm,
    (block) => {
      const name = scalar(block, "name");
      if (!localNames.has(name) || /^source\s*=/m.test(block)) return block;
      assert.ok(!seen.has(name), `duplicate local Cargo.lock package ${name}`);
      seen.add(name);
      scalar(block, "version");
      return block.replace(/^(version\s*=\s*")[^"]+(".*)$/m, `$1${version}$2`);
    },
  );
  for (const name of localNames)
    assert.ok(seen.has(name), `missing local Cargo.lock package ${name}`);
  changes.set("Cargo.lock", updatedLock);
  json("src-tauri/tauri.conf.json", (value) => {
    delete value.version;
  });
  json("package.json", (value) => {
    value.private = true;
    value.version = version;
  });
  json("package-lock.json", (value) => {
    assert.ok(
      value.packages?.[""],
      "missing root package-lock package metadata",
    );
    value.version = version;
    value.packages[""].version = version;
  });
  json("frontend/package.json", (value) => {
    value.private = true;
    delete value.version;
  });
  json("frontend/package-lock.json", (value) => {
    assert.ok(
      value.packages?.[""],
      "missing frontend package-lock package metadata",
    );
    delete value.version;
    delete value.packages[""].version;
  });
  return changes;
}

export function checkVersions(root = process.cwd()) {
  const version = readVersion(root);
  for (const [file, expected] of projectionPlan(root, version)) {
    const current = readFileSync(path.join(root, file), "utf8");
    // JSON formatting is not a version invariant; compare generated values.
    const matches = file.endsWith(".json")
      ? JSON.stringify(JSON.parse(current)) ===
        JSON.stringify(JSON.parse(expected))
      : current === expected;
    assert.ok(
      matches,
      `product version/private metadata drift in ${file}; run node scripts/release/version.mjs sync`,
    );
  }
  return version;
}
function apply(root, changes) {
  for (const [file, text] of changes) {
    if (readFileSync(path.join(root, file), "utf8") !== text)
      writeFileSync(path.join(root, file), text);
  }
}
export function syncVersions(root = process.cwd()) {
  const version = readVersion(root);
  apply(root, projectionPlan(root, version));
  return version;
}
export function prepareVersion(version, root = process.cwd()) {
  assert.ok(
    compareVersions(version, readVersion(root)) >= 0,
    "next development target version cannot go backwards",
  );
  const changes = projectionPlan(root, version);
  const file = "Cargo.toml";
  const source = readFileSync(path.join(root, file), "utf8");
  const owned = section(source, "workspace.package");
  changes.set(
    file,
    source.slice(0, owned.start) +
      owned.text.replace(/^(version\s*=\s*")[^"]+(".*)$/m, `$1${version}$2`) +
      source.slice(owned.end),
  );
  apply(root, changes);
  return version;
}
function main(args) {
  let root = process.cwd();
  const rootAt = args.indexOf("--root");
  if (rootAt !== -1) {
    assert.ok(
      args[rootAt + 1] && !args[rootAt + 1].startsWith("--"),
      "--root requires a directory",
    );
    root = path.resolve(args[rootAt + 1]);
    args.splice(rootAt, 2);
  }
  const [command, version, ...extra] = args;
  assert.ok(
    extra.length === 0 &&
      (command === "prepare" ? version : version === undefined),
    "Usage: version.mjs check|sync|prepare VERSION [--root PATH]",
  );
  const action = new Map([
    ["check", checkVersions],
    ["sync", syncVersions],
    ["prepare", () => prepareVersion(version, root)],
  ]).get(command);
  assert.ok(
    action,
    "Usage: version.mjs check|sync|prepare VERSION [--root PATH]",
  );
  console.log(`Product version ${command}: ${action(root)}`);
}
if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
