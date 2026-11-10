import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { posix } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { buildAffectedPlan } from "../validation/affected.mjs";
import { loadContracts } from "../validation/contracts.mjs";
import { loadRequirements } from "../validation/pr-scenarios.mjs";

const rootUrl = new URL("../../", import.meta.url);
const root = fileURLToPath(rootUrl);
const packages = new Map([
  ["mesh-talk-core", "crates/mesh-talk-core"],
  ["mesh-talk", "src-tauri"],
  ["mesh-talk-integrations", "crates/mesh-talk-integrations"],
  ["mesh-talk-mcp", "crates/mesh-talk-mcp"],
]);
const read = (path) => readFileSync(new URL(path, rootUrl), "utf8");
// Development ownership also applies to new source before it is staged.
const sourceFiles = execFileSync("git", ["ls-files", "--cached", "--others", "--exclude-standard", "-z", ...packages.values()], {
  cwd: root,
  encoding: "utf8",
})
  .split("\0")
  .filter(Boolean);
const contracts = await loadContracts(root);
const plan = (path) => buildAffectedPlan([path], { contracts });
const owners = (path) => plan(path).owners.map(({ id }) => id);

test("every Rust source, crate manifest and test fixture has executable development ownership", () => {
  const required = sourceFiles.filter((path) =>
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

test("CLI profile ownership changes select shared subprocess locks and binary restart evidence", () => {
  const selected = plan("crates/mesh-talk-core/src/bin/mesh-talk-node.rs");
  assert.deepEqual(selected.blockers, []);
  assert.ok(owners("specifications/deployment_guide.md").includes("core-cli-harness"));
  const escalation = selected.escalation.join(" ");
  assert.match(escalation, /--lib storage::profile::tests::profile_lock/u);
  assert.match(escalation, /participant_cli_reopens_same_profile_on_requested_port_after_quit_eof_and_input_error/u);
  const owner = selected.owners.find(({ id }) => id === "core-cli-harness");
  assert.equal(owner?.capability?.guide,
    "specifications/deployment_guide.md#data-directory-ownership");
});

test("delivery observations retain Node consumers, evidence rules and native acceptance", async () => {
  const selected = plan("crates/mesh-talk-core/src/node/delivery_observation.rs");
  assert.deepEqual(selected.blockers, []);
  for (const expected of ["core-node", "desktop-session", "integration-service"]) {
    assert.ok(selected.owners.some(({ id }) => id === expected), expected);
  }
  const owner = selected.owners.find(({ id }) => id === "core-node");
  assert.ok(owner.capability.invariants.some(({ reference }) =>
    reference === "docs/network-design-reference.md#runtime-delivery-observations"));
  assert.match(selected.escalation.join(" "), /observation.*recipient.*confirmation/iu);
  const requirements = await loadRequirements(["crates/mesh-talk-core/src/node/delivery_observation.rs"], root);
  assert.ok(requirements.owners.includes("discovery-native"));
  assert.ok(requirements.native.includes(process.platform));
});

test("desktop search reaches its session and frontend consumers", () => {
  const selected = plan("src-tauri/src/owner_commands/search.rs");
  assert.deepEqual(selected.blockers, []);
  for (const expected of ["desktop-search", "desktop-session", "frontend-chat", "frontend-owner"]) {
    assert.ok(selected.owners.some(({ id }) => id === expected), expected);
  }
  assert.ok(selected.frontend.includes("src/lib/api.test.ts"));
});

test("control task lifetime changes select their source-backed regression", () => {
  const selected = plan("crates/mesh-talk-core/src/node/account_session.rs");
  assert.deepEqual(selected.blockers, []);
  assert.ok(selected.owners.some(({ id }) => id === "core-account-group-operations"));
  const name = "control_task_lifetime_scope_restores_outer_witness_after_panic";
  const filters = selected.rust.find(({ package: value }) => value === "mesh-talk-core").filters;
  assert.deepEqual(filters.filter((filter) => filter.includes(name)), [name]);
});

// Keep logical module aliases for #[path] declarations. Test filenames need not
// match their module names (node/node.rs declares participant_storage_tests.rs).
function inlineModules(source) {
  // Mask strings/comments before balancing braces; fixture bodies often contain
  // JSON, format strings and raw SQL with braces unrelated to Rust scope.
  const masked = source.replace(
    /\/\/[^\n]*|\/\*[\s\S]*?\*\/|(?:br|cr|r)(#*)"[\s\S]*?"\1|(?:b|c)?"(?:\\[\s\S]|[^"\\])*"|'(?:\\.|[^'\\])'/gu,
    token => " ".repeat(token.length),
  );
  const modules = [];
  const pattern = /\bmod\s+(\w+)\s*\{/gu;
  let match;
  while ((match = pattern.exec(masked))) {
    const start = pattern.lastIndex;
    let depth = 1, end = start;
    while (end < masked.length && depth) {
      if (masked[end] === "{") depth++;
      if (masked[end] === "}") depth--;
      end++;
    }
    if (depth !== 0) throw Error(`Unclosed source module ${match[1]}`);
    modules.push({ name: match[1], source: source.slice(start, end - 1), start, end });
    pattern.lastIndex = end;
  }
  return modules;
}

function librarySources(directory, files, readSource) {
  const existing = new Set(files);
  const sources = new Map();
  function add(path, module, ancestors = [], source = readSource(path)) {
    const key = `${module}:${path}`;
    if (sources.has(key) || ancestors.includes(path)) return;
    const children = inlineModules(source);
    let directSource = source;
    for (const { start, end } of children.toReversed()) {
      directSource = directSource.slice(0, start) + "\n" + directSource.slice(end);
    }
    const tests = [...directSource.matchAll(/((?:\s*#\[[^\]]+\])+\s*)(?:async\s+)?fn\s+(\w+)\s*\(/gu)]
      .filter(([, attributes]) => /#\[(?:tokio::)?test(?:\(|\])/u.test(attributes) && !/#\[ignore/u.test(attributes))
      .map(([, , name]) => [...module, name].join("::"));
    sources.set(key, tests);
    for (const child of children) {
      add(path, [...module, child.name], ancestors, child.source);
    }
    for (const [, relative, child] of directSource.matchAll(
      /#\[path\s*=\s*"([^"]+)"\]\s*(?:#\[[^\]]+\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;/gu,
    )) {
      const referenced = posix.normalize(posix.join(posix.dirname(path), relative));
      if (existing.has(referenced)) add(referenced, [...module, child], [...ancestors, path]);
    }
  }
  for (const path of files) {
    if (!path.startsWith(`${directory}/src/`) || path.startsWith(`${directory}/src/bin/`) || !path.endsWith(".rs")) continue;
    const module = path.slice(`${directory}/src/`.length, -3).split("/");
    if (["lib", "mod"].includes(module.at(-1))) module.pop();
    add(path, module);
  }
  return [...new Set([...sources.values()].flat())];
}

function hasTestEvidence(sources, filter) {
  // Cargo's positional filter is a substring of the qualified test name, not
  // an exact function name. Several owners intentionally select a module name.
  return sources.some(name => name.includes(filter));
}

test("source evidence follows declared path modules and Cargo name prefixes without borrowing unrelated or ignored tests", () => {
  const fixtures = new Map([
    ["crate/src/node/node.rs", '#[path = "../fixtures/checks.rs"]\nmod participant_storage_tests;'],
    ["crate/src/fixtures/checks.rs", '#[test]\nfn participant_event_reopens() {}\n#[test]\n#[ignore]\nfn ignored_only() {}'],
    ["crate/src/node/unrelated.rs", '#[test]\nfn absent_in_selected_module() {}\nmod scoped { #[test] fn nested() { let text = "}"; } }'],
    ["crate/src/bin/tool.rs", '#[test]\nfn binary_only() {}'],
  ]);
  const sources = librarySources("crate", [...fixtures.keys()], path => fixtures.get(path));
  assert.ok(hasTestEvidence(sources, "node::node::participant_storage_tests::"));
  assert.ok(hasTestEvidence(sources, "node::node::participant_storage_tests::participant_event_"));
  assert.ok(hasTestEvidence(sources, "event_reopen"));
  assert.ok(hasTestEvidence(sources, "participant_storage_tests"));
  assert.ok(hasTestEvidence(sources, "node::unrelated::scoped::nested"));
  for (const filter of ["node::node::wrong_alias::", "node::node::participant_storage_tests::absent_in_selected_module", "node::unrelated::scoped::absent_in_selected_module", "node::unrelated::nested", "ignored_only", "binary_only"]) {
    assert.equal(hasTestEvidence(sources, filter), false, filter);
  }
});

// Source evidence catches accidental empty/fabricated filters without compiling
// four crates. Cargo's actual per-platform --list inventory remains authoritative.
test("every Rust selector has nonignored library test evidence in its owning module", () => {
  const sources = new Map([...packages].map(([name, directory]) => [name,
    librarySources(directory, sourceFiles.filter(path => existsSync(new URL(path, rootUrl))), read),
  ]));
  for (const contract of contracts) {
    for (const { package: packageName, filters } of contract.rust ?? []) {
      const directory = packages.get(packageName);
      assert.ok(directory, packageName);
      for (const filter of filters) {
        assert.ok(
          hasTestEvidence(sources.get(packageName), filter),
          `${contract.id}: ${packageName} ${filter} lacks nonignored source evidence`,
        );
      }
    }
  }
});
