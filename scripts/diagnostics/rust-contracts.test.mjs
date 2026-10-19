import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { posix } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { buildAffectedPlan } from "../validation/affected.mjs";

const rootUrl = new URL("../../", import.meta.url);
const root = fileURLToPath(rootUrl);
const packages = new Map([
  ["mesh-talk-core", "crates/mesh-talk-core"],
  ["mesh-talk", "src-tauri"],
  ["mesh-talk-integrations", "crates/mesh-talk-integrations"],
  ["mesh-talk-mcp", "crates/mesh-talk-mcp"],
]);
const read = (path) => readFileSync(new URL(path, rootUrl), "utf8");
const tracked = execFileSync("git", ["ls-files", "-z", ...packages.values()], {
  cwd: root,
  encoding: "utf8",
})
  .split("\0")
  .filter(Boolean);
const contracts = [...packages.values()].flatMap(
  (path) => JSON.parse(read(`${path}/validation.json`)).contracts,
);
const plan = (path) => buildAffectedPlan([path], { contracts });
const owners = (path) => plan(path).owners.map(({ id }) => id);

test("every tracked Rust source, crate manifest and test fixture has executable development ownership", () => {
  const required = tracked.filter((path) =>
    /\.rs$|\/Cargo\.toml$|\/tests\/fixtures\//u.test(path),
  );
  const missing = required.flatMap((path) => plan(path).blockers);
  assert.deepEqual(missing, []);
});

test("protocol dependencies reach Node, desktop and integration consumers transitively", () => {
  const background = plan("src-tauri/src/background_command.rs");
  assert.ok(background.owners.some(owner => owner.id === "desktop-discovery"));
  assert.ok(background.node.includes("scripts/diagnostics/background-command.test.mjs"));
  for (const module of [
    "channel",
    "discovery",
    "dm",
    "eventlog",
    "file",
    "identity",
    "postoffice",
    "ratchet",
    "recovery",
    "storage",
    "transport",
    "util",
  ]) {
    const path = `crates/mesh-talk-core/src/${module}${module === "dm" ? ".rs" : "/mod.rs"}`;
    for (const expected of [
      "core-node",
      "desktop-session",
      "integration-service",
      "mcp-adapter",
    ]) {
      assert.ok(
        owners(path).includes(expected),
        `${path} must reach ${expected}`,
      );
    }
  }
  for (const expected of [
    "desktop-integrations",
    "desktop-chat",
    "desktop-contact-policy",
    "desktop-app-lock",
  ]) {
    assert.ok(owners("src-tauri/src/state.rs").includes(expected), expected);
  }
  for (const expected of [
    "integration-service",
    "integration-host",
    "integration-ipc",
    "integration-client",
    "mcp-adapter",
    "desktop-integrations",
  ]) {
    assert.ok(
      owners("crates/mesh-talk-integrations/src/model.rs").includes(expected),
      expected,
    );
  }
  for (const [path, expected] of [
    ["crates/mesh-talk-core/src/ratchet/mod.rs", "core-channel"],
    ["crates/mesh-talk-core/src/util/mod.rs", "core-file"],
    ["crates/mesh-talk-core/src/storage/mod.rs", "core-identity"],
    ["crates/mesh-talk-core/src/identity/mod.rs", "core-transport"],
    ["crates/mesh-talk-core/src/expiry.rs", "core-eventlog"],
  ]) {
    assert.ok(
      owners(path).includes(expected),
      `${path} must reach ${expected}`,
    );
  }
});

test("binary, integration harness and native build inputs retain explicit acceptance escalation", () => {
  for (const path of [
    "crates/mesh-talk-core/src/bin/mesh-talk-node.rs",
    "crates/mesh-talk-core/tests/support/mod.rs",
    "crates/mesh-talk-integrations/src/bin/mesh-talk-integrations.rs",
    "crates/mesh-talk-mcp/src/main.rs",
    "src-tauri/src/main.rs",
    "src-tauri/build.rs",
    "src-tauri/capabilities/default.json",
  ]) {
    const selected = plan(path);
    assert.deepEqual(selected.blockers, [], path);
    assert.match(
      selected.escalation.join(" "),
      /CLI|integration|native/iu,
      path,
    );
  }
  for (const module of ["identity", "ratchet", "storage", "transport"]) {
    assert.match(
      plan(`crates/mesh-talk-core/src/${module}/mod.rs`).escalation.join(" "),
      /crypto|wire|persist|storage/iu,
    );
  }
});

// Source evidence catches accidental empty/fabricated filters without compiling
// four crates. Cargo's actual per-platform --list inventory remains authoritative.
test("every Rust selector has nonignored library test evidence in its owning module", () => {
  for (const contract of contracts) {
    for (const { package: packageName, filters } of contract.rust ?? []) {
      const directory = packages.get(packageName);
      assert.ok(directory, packageName);
      for (const filter of filters) {
        const segments = filter.split("::").filter(Boolean);
        const exactName = filter.endsWith("::") ? null : segments.pop();
        if (segments.at(-1) === "tests") segments.pop();
        const modulePath = `${directory}/src/${segments.join("/")}`;
        const sourcePaths = tracked.filter(
          (path) =>
            path.startsWith(`${directory}/src/`) &&
            !path.startsWith(`${directory}/src/bin/`) &&
            path.endsWith(".rs") &&
            (!segments.length ||
              path === `${modulePath}.rs` ||
              path.startsWith(`${modulePath}/`)),
        );
        // Rust modules can place their library tests outside src/ using #[path].
        for (const path of [...sourcePaths]) {
          for (const [, relative] of read(path).matchAll(
            /#\[path\s*=\s*"([^"]+)"\]/gu,
          )) {
            const referenced = posix.normalize(
              posix.join(posix.dirname(path), relative),
            );
            if (tracked.includes(referenced)) sourcePaths.push(referenced);
          }
        }
        const evidence = sourcePaths.some((path) => {
          const source = read(path);
          const tests = source.matchAll(
            /((?:\s*#\[[^\]]+\])+\s*)(?:async\s+)?fn\s+(\w+)\s*\(/gu,
          );
          return [...tests].some(
            ([, attributes, name]) =>
              /#\[(?:tokio::)?test(?:\(|\])/u.test(attributes) &&
              !/#\[ignore/u.test(attributes) &&
              (!exactName || name === exactName),
          );
        });
        assert.ok(
          evidence,
          `${contract.id}: ${packageName} ${filter} lacks nonignored source evidence`,
        );
      }
    }
  }
});
