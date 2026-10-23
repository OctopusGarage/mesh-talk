import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { loadContracts } from "../validation/contracts.mjs";
import { buildAffectedPlan } from "../validation/affected.mjs";
import { buildFrontendGraph } from "../validation/frontend-graph.mjs";

const contracts = [
  {
    id: "profile",
    paths: ["core/profile.rs"],
    rust: [{ package: "core", filters: ["profile::"] }],
    consumers: ["host"],
    escalation: ["Signed profile changes need loopback/restart evidence."],
  },
  {
    id: "host",
    paths: ["host/session.rs"],
    rust: [{ package: "host", filters: ["session::"] }],
  },
];
const graph = (sources) => buildFrontendGraph(new Map(Object.entries(sources)));

test("selects module owner and declared transitive consumers with reasons, deduplicating commands", () => {
  const plan = buildAffectedPlan(["core/profile.rs", "host/session.rs"], {
    contracts,
  });
  assert.deepEqual(
    plan.owners.map((item) => item.id),
    ["host", "profile"],
  );
  assert.deepEqual(
    plan.rust.map((item) => item.package),
    ["core", "host"],
  );
  assert.match(
    plan.owners.find((item) => item.id === "host").reasons.join(" "),
    /consumer of profile/,
  );
  assert.equal(plan.blockers.length, 0);
  assert.match(plan.escalation.join(" "), /loopback/);
  assert.ok(!JSON.stringify(plan).includes("--workspace"));
});

test("unknown files block execution, documentation-only changes select no tests", () => {
  assert.match(
    buildAffectedPlan(["core/unknown.rs"], { contracts }).blockers.join(" "),
    /unmapped.*core\/unknown.rs/i,
  );
  const docs = buildAffectedPlan(["docs/example.md", "README.md"], {
    contracts,
  });
  assert.deepEqual(docs.rust, []);
  assert.deepEqual(docs.blockers, []);
});

test("explicit owners require a reason before acknowledging unmapped files", () => {
  assert.throws(
    () => buildAffectedPlan(["unknown"], { contracts, owners: ["host"] }),
    /reason/i,
  );
  assert.throws(
    () =>
      buildAffectedPlan([], {
        contracts,
        owners: ["typo"],
        reason: "manual review",
      }),
    /unknown owner/i,
  );
  const plan = buildAffectedPlan(["unknown"], {
    contracts,
    owners: ["host"],
    reason: "Reviewed shared fixture; only session uses it",
  });
  assert.equal(plan.blockers.length, 0);
  assert.match(plan.owners[0].reasons.join(" "), /Reviewed shared fixture/);
  assert.deepEqual(plan.acknowledged, ["unknown"]);
});

test("frontend tests follow transitive imports, aliases, reexports, mocks and shared fixture users", () => {
  const frontend = graph({
    "frontend/src/owner.ts": "export const value = 1;",
    "frontend/src/barrel.ts": 'export { value } from "@/owner";',
    "frontend/src/a.test.ts": 'import {value} from "./barrel";',
    "frontend/src/b.test.ts": 'vi.mock("@/owner", () => ({}));',
    "frontend/src/c.test.ts": 'import("./unrelated");',
    "frontend/src/unrelated.ts": "",
  });
  const plan = buildAffectedPlan(["frontend/src/owner.ts"], {
    contracts,
    frontend,
  });
  assert.deepEqual(plan.frontend, ["src/a.test.ts", "src/b.test.ts"]);
  assert.equal(plan.blockers.length, 0);
  assert.match(plan.frontendReasons["src/a.test.ts"].join(" "), /owner.ts/);
});

test("changed shared fixture reaches every importing test, including cycles", () => {
  const frontend = graph({
    "frontend/src/shared.ts": 'import "./loop";',
    "frontend/src/loop.ts": 'import "./shared";',
    "frontend/src/a.test.ts": 'import "./shared";',
    "frontend/src/b.test.ts": 'import "./loop";',
  });
  assert.deepEqual(
    buildAffectedPlan(["frontend/src/shared.ts"], { contracts, frontend })
      .frontend,
    ["src/a.test.ts", "src/b.test.ts"],
  );
});

test("former dependency edges retain consumers after deletion or import removal", () => {
  const frontend = buildFrontendGraph(
    new Map([["frontend/src/a.test.ts", ""]]),
    new Map([
      ["frontend/src/old.ts", "export const value = 1;"],
      ["frontend/src/a.test.ts", 'import "./old";'],
    ]),
  );
  assert.deepEqual(
    buildAffectedPlan(["frontend/src/old.ts"], { contracts, frontend })
      .frontend,
    ["src/a.test.ts"],
  );
});

test("changed tests select themselves but missing tests cannot be silently successful", () => {
  const frontend = graph({ "frontend/src/a.test.ts": "" });
  assert.deepEqual(
    buildAffectedPlan(["frontend/src/a.test.ts"], { contracts, frontend })
      .frontend,
    ["src/a.test.ts"],
  );
  const noTests = buildAffectedPlan(["frontend/src/untested.ts"], {
    contracts,
    frontend: graph({ "frontend/src/untested.ts": "" }),
  });
  assert.match(noTests.blockers.join(" "), /no affected unit tests/i);
  assert.match(noTests.escalation.join(" "), /browser|native/i);
});

test("deleted tests are reported for review, never passed to Vitest as nonexistent files", () => {
  const frontend = buildFrontendGraph(
    new Map(),
    new Map([["frontend/src/a.test.ts", ""]]),
  );
  const plan = buildAffectedPlan(["frontend/src/a.test.ts"], {
    contracts,
    frontend,
  });
  assert.deepEqual(plan.frontend, []);
  assert.match(plan.blockers.join(" "), /no affected unit tests/i);
});

test("invalid consumer references and duplicate owner IDs fail closed", () => {
  assert.throws(
    () => buildAffectedPlan([], { contracts: [...contracts, contracts[0]] }),
    /duplicate/i,
  );
  assert.throws(
    () =>
      buildAffectedPlan([], {
        contracts: [{ id: "bad", paths: ["bad"], consumers: ["missing"] }],
      }),
    /consumer/i,
  );
});

test("mapped source owners with no executable tests cannot produce a successful empty plan", () => {
  const plan = buildAffectedPlan(["runtime.rs"], {
    contracts: [{ id: "runtime", paths: ["runtime.rs"] }],
  });
  assert.match(plan.blockers.join(" "), /no executable tests/i);
});

test("repository grant policy expands through service, HTTP, IPC and desktop/UI/MCP consumers", async () => {
  const registered = await loadContracts(fileURLToPath(new URL("../../", import.meta.url)));
  const plan = buildAffectedPlan(
    ["crates/mesh-talk-integrations/src/policy.rs"],
    { contracts: registered },
  );
  assert.deepEqual(
    plan.owners.map((owner) => owner.id),
    [
      "desktop-integrations",
      "frontend-local-agent-access",
      "integration-client",
      "integration-host",
      "integration-http",
      "integration-ipc",
      "integration-policy",
      "integration-service",
      "mcp-adapter",
    ],
  );
  assert.ok(plan.frontend.includes("src/features/chat/settings/LocalAgentSettings.test.ts"));
  assert.ok(plan.rust.some((entry) => entry.package === "mesh-talk-integrations" && entry.filters.includes("http::tests::")));
});

test("misspelled contract fields cannot silently drop downstream checks", () => {
  assert.throws(
    () =>
      buildAffectedPlan([], {
        contracts: [{ id: "owner", paths: ["owner.rs"], consumer: ["host"] }],
      }),
    /unknown contract field.*consumer/i,
  );
});

test("frontend configuration and runtime assets invalidate all registered unit consumers", () => {
  const frontend = graph({ "frontend/src/a.test.ts": "", "frontend/src/b.test.ts": "" });
  for (const file of ["frontend/vite.config.ts", "frontend/package-lock.json", "docs/help/catalog.json", "site/market/catalog.json", "Cargo.toml"]) {
    const plan = buildAffectedPlan([file], { contracts, frontend });
    assert.deepEqual(plan.blockers, [], file);
    assert.deepEqual(plan.frontend, ["src/a.test.ts", "src/b.test.ts"], file);
    assert.match(plan.frontendReasons["src/a.test.ts"].join(" "), /global|shared/i);
  }
});

test("shared Cargo configuration selects every workspace crate via owning contracts", async () => {
  const { readFile } = await import("node:fs/promises");
  const roots = ["scripts/validation", "frontend", "crates/mesh-talk-core", "src-tauri", "crates/mesh-talk-integrations", "crates/mesh-talk-mcp"];
  const registered = (await Promise.all(roots.map(async root => JSON.parse(await readFile(new URL(`../../${root}/validation.json`, import.meta.url), "utf8")).contracts))).flat();
  for (const file of ["Cargo.lock", ".cargo/config.toml", "rust-toolchain.toml"]) {
    const plan = buildAffectedPlan([file], { contracts: registered });
    assert.deepEqual(plan.blockers, [], file);
    assert.deepEqual(plan.rust.map(item => item.package), ["mesh-talk", "mesh-talk-core", "mesh-talk-integrations", "mesh-talk-mcp"]);
  }
  const profile = buildAffectedPlan(["crates/mesh-talk-core/src/node/account_profile.rs"], { contracts: registered });
  assert(!profile.owners.some(owner => owner.id === "core-node"), "established focused ownership must not widen through the catch-all Node prefix");
  assert.deepEqual(profile.rust.find(item => item.package === "mesh-talk-core").filters, [
    "node::account_profile::tests::",
    "node::node::tests::own_signed_profile_and_clear_survive_restart_for_new_peers",
    "node::node::tests::setting_an_avatar_propagates_to_a_peer_account_over_loopback",
    "node::node::tests::signed_avatar_reaches_a_peer_through_a_post_office_without_direct_tcp",
    "node::node::tests::signed_avatar_retries_after_relay_publication_is_reenabled",
    "node::node::tests::trust_card_exhausted_avatar_revision_rejects_before_either_store_mutates",
    "node::node::tests::trust_card_linked_device_pair_preserves_metadata_and_requires_card_parent",
    "node::node::tests::trust_card_projection_retry_and_restart_preserve_avatar_and_identity_lifetime",
    "node::node::tests::trust_card_propagates_signed_claims_over_loopback",
    "node::node::tests::trust_card_reconciles_newer_sibling_legacy_avatar",
    "node::node::tests::trust_card_terminal_legacy_avatar_reconciles_on_restart",
    "node::opaque_relay::",
    "node::profile::tests::",
    "node::profile_store::tests::",
    "node::runtime::tests::profile_republish_targets_fires_on_first_sight_and_on_reconnect",
    "node::trust_card::tests::",
  ]);
  const consentFixture = buildAffectedPlan(["crates/mesh-talk-core/src/node/file_consent_tests.rs"], {contracts:registered});
  assert.deepEqual(consentFixture.owners.map(owner => owner.id), ["core-file-consent-tests"]);
  assert.deepEqual(consentFixture.rust, [{package:"mesh-talk-core",filters:["node::file_consent_tests::"]}]);
});

test("fallback exclusions are literal validated ownership paths, not a way to skip consumers", () => {
  const registered = [
    { id: "fallback", paths: ["core/"], excludePaths: ["core/profile.rs"], rust: [{package:"core",filters:["node::"]}] },
    { id: "profile", paths: ["core/profile.rs"], rust: [{package:"core",filters:["profile::"]}], consumers: ["fallback"] },
  ];
  const selected = buildAffectedPlan(["core/profile.rs"], {contracts:registered});
  assert.deepEqual(selected.owners.map(owner => owner.id), ["fallback", "profile"]);
  assert.throws(() => buildAffectedPlan([], {contracts:[{...registered[0],excludePaths:["../unsafe"]}]}), /exclude/i);
});

test("business capability references accompany owners without changing mandatory consumers", () => {
  const capability = {
    summary: "Keep local intents distinct from accepted events",
    guide: "docs/rules.md",
    invariants: [{ name: "Exact accepted identity", reference: "CONTEXT.md#acceptance-and-delivery" }],
  };
  const plan = buildAffectedPlan(["core/profile.rs"], {
    contracts: contracts.map(item => item.id === "profile" ? { ...item, capability } : item),
  });
  assert.deepEqual(plan.owners.find(item => item.id === "profile").capability, capability);
  assert.deepEqual(plan.rust.map(item => item.package), ["core", "host"]);
});

test("capability contracts reject misspellings, missing obligations and nonlocal references", () => {
  const valid = { summary: "Session authority", guide: "docs/rules.md", invariants: [{ name: "Session generation", reference: "CONTEXT.md#identity-and-access" }] };
  for (const capability of [
    { ...valid, typo: true }, { ...valid, summary: "" }, { ...valid, invariants: [] },
    { ...valid, guide: "https://example.com/rules.md" }, { ...valid, guide: "../rules.md" },
    { ...valid, invariants: [{ name: "Session", reference: "docs/missing.txt" }] },
    { ...valid, invariants: [{ name: "Session", reference: "docs/rules.md", typo: true }] },
  ]) assert.throws(() => buildAffectedPlan([], { contracts: [{ ...contracts[0], consumers: [], capability }] }), /capability/i);
});

test("acceptance harness changes select their focused integrity checks without claiming future files", async () => {
  const { readFile } = await import("node:fs/promises");
  const roots = ["scripts/validation", "frontend", "crates/mesh-talk-core", "src-tauri", "crates/mesh-talk-integrations", "crates/mesh-talk-mcp"];
  const registered = (await Promise.all(roots.map(async root => JSON.parse(await readFile(new URL(`../../${root}/validation.json`, import.meta.url), "utf8")).contracts))).flat();
  const cases = [
    ["frontend/e2e/packs.spec.ts", "frontend", "src/lib/pack.test.ts"],
    ["frontend/e2e/ui-audit-composer.spec.ts", "frontend", "src/features/chat/ComposerScreenshot.test.ts"],
    ["docs/evals/group-reception-ai-eval-suite.json", "node", "scripts/test-ai-eval.mjs"],
    ["docs/evals/linked-device-sync-ai-eval-suite.json", "node", "scripts/test-ai-eval.mjs"],
    ["docs/evals/validation-velocity-ai-eval-suite.json", "node", "scripts/test-ai-eval.mjs"],
    ["docs/evals/README.md", "node", "scripts/test-ai-eval.mjs"],
    ["docs/evals/prompts/agent-contract-evaluator.md", "node", "scripts/test-ai-eval.mjs"],
    ["scripts/test-ai-eval.mjs", "node", "scripts/test-ai-eval.mjs"],
    ["scripts/diagnostics/hidden-contacts.mjs", "node", "scripts/diagnostics/native-ipc-boundary.test.mjs"],
    ["scripts/diagnostics/hidden-contacts-report.mjs", "node", "scripts/diagnostics/hidden-contacts-report.test.mjs"],
    ["scripts/diagnostics/hidden-contacts-report.test.mjs", "node", "scripts/diagnostics/hidden-contacts-report.test.mjs"],
    ["scripts/diagnostics/native-multicast-probe.mjs", "node", "scripts/diagnostics/native-core-scenarios.test.mjs"],
    ["scripts/diagnostics/native-capture.mjs", "node", "scripts/diagnostics/native-capture.test.mjs"],
    ["scripts/diagnostics/native-capture.test.mjs", "node", "scripts/diagnostics/native-capture.test.mjs"],
    ["scripts/diagnostics/native-core-scenarios.mjs", "node", "scripts/diagnostics/native-core-scenarios.test.mjs"],
    ["scripts/diagnostics/native-core-scenarios.test.mjs", "node", "scripts/diagnostics/native-core-scenarios.test.mjs"],
  ];
  for (const [file, kind, selected] of cases) {
    const plan = buildAffectedPlan([file], { contracts: registered });
    assert.deepEqual(plan.blockers, [], file);
    assert.ok(plan[kind].includes(selected), `${file} must select ${selected}`);
  }
  const { requiredBrowserPaths } = await import("../evals/browser-report.mjs");
  for (const name of new Set(requiredBrowserPaths("core").map(path => path[0]))) {
    const file = `frontend/e2e/${name}`;
    const plan = buildAffectedPlan([file], { contracts: registered });
    assert.deepEqual(plan.blockers, [], file);
    assert.ok(plan.node.includes("scripts/diagnostics/browser-eval-report.test.mjs"), `${file} must select portable inventory parity`);
  }
  assert.ok(buildAffectedPlan(["frontend/playwright.portable.config.ts"], { contracts: registered }).node.includes("scripts/diagnostics/browser-eval-report.test.mjs"));
  for (const file of ["frontend/e2e/future.spec.ts", "scripts/diagnostics/future-native.mjs", "docs/evals/future-suite.json"]) {
    assert.match(buildAffectedPlan([file], { contracts: registered }).blockers.join(" "), /unmapped/i, file);
  }
});
