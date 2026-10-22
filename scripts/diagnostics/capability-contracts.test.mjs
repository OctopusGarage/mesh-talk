import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { buildAffectedPlan } from "../validation/affected.mjs";

const root = new URL("../../", import.meta.url);
const read = (path) => readFileSync(new URL(path, root), "utf8");
const contracts = [
  "frontend",
  "src-tauri",
  "crates/mesh-talk-integrations",
  "crates/mesh-talk-mcp",
].flatMap(
  (directory) => JSON.parse(read(`${directory}/validation.json`)).contracts,
);
const owner = (id) => contracts.find((contract) => contract.id === id);
const select = (path, id) => {
  const plan = buildAffectedPlan([path], { contracts });
  assert.deepEqual(plan.blockers, [], path);
  assert.deepEqual(
    plan.owners.find((selected) => selected.id === id)?.capability,
    owner(id).capability,
    `${id} must retain its business obligations in the selected plan`,
  );
  return plan;
};

// Resolve real documentation, including simple heading anchors, rather than
// duplicating the metadata schema validator. Markdown remains authoritative.
function assertDocumentReference(reference) {
  const [path, anchor] = reference.split("#");
  const markdown = read(path);
  assert.match(path, /\.md$/u);
  assert.ok(markdown.trim(), `${reference} is empty`);
  if (!anchor) return;
  const headings = [...markdown.matchAll(/^#{1,6}\s+(.+)$/gmu)].map(
    ([, heading]) =>
      heading
        .toLowerCase()
        .replace(/[^\p{L}\p{N}\s-]/gu, "")
        .trim()
        .replace(/\s/gu, "-"),
  );
  assert.ok(headings.includes(anchor), `Missing Markdown anchor ${reference}`);
}

test("capability pilots link maintained domain and architecture rules", () => {
  for (const id of ["frontend-outgoing", "desktop-session"]) {
    const { capability } = owner(id);
    assertDocumentReference(capability.guide);
    for (const invariant of capability.invariants)
      assertDocumentReference(invariant.reference);
  }
  assert.match(
    read("CONTEXT.md"),
    /core's successful admission of outgoing content/u,
  );
  assert.match(
    read("docs/ARCHITECTURE.md"),
    /Replacement awaits the old runtime's consuming\s+stop/u,
  );
});

test("outgoing capability selects tests for acceptance projection and retired frontend owners", () => {
  const plan = select(
    "frontend/src/store/outgoingIntent.ts",
    "frontend-outgoing",
  );
  const testPath = "src/store/outgoingIntent.test.ts";
  assert.ok(plan.frontend.includes(testPath));
  const source = read(`frontend/${testPath}`);
  const obligations = new Map([
    [
      "Accepted intents reconcile by exact event identity",
      "reconciles an accepted intent only with its exact event ID",
    ],
    [
      "Presentation failure preserves backend acceptance",
      "keeps durable acceptance when presentation hydration rejects",
    ],
    [
      "Retired frontend owners cannot publish send completion",
      "ignores acceptance completed after its owner generation retires",
    ],
    [
      "File offer receipt does not stop approval and byte-progress refresh",
      "continues polling file approval and bytes after the file offer is delivered",
    ],
    [
      "Channel durable IDs settle acceptance independently of fanout and custody",
      "projects member and post-office custody separately for exact channel IDs",
    ],
  ]);
  assert.deepEqual(
    owner("frontend-outgoing").capability.invariants.map(({ name }) => name),
    [...obligations.keys()],
  );
  for (const title of obligations.values()) {
    assert.ok(
      source.includes(`it("${title}",`),
      `Missing executable outgoing obligation: ${title}`,
    );
  }
  assert.ok(
    plan.frontend.includes("src/store/chat.test.ts"),
    "chat composition remains a declared consumer",
  );
  assert.match(plan.escalation.join(" "), /browser\/native/u);
});

// This inexpensive source check catches lost selectors and renamed/removed
// obligations. Cargo's platform-specific test inventory and native acceptance
// remain authoritative; this test does not execute the Rust tests themselves.
test("session capability retains executable generation and retirement obligations", () => {
  const obligations = [
    [
      "Nickname publication does not block observations behind stalled network work",
      "commands::tests::nickname_change_during_a_stalled_send_preserves_node_observations",
      "src-tauri/src/commands.rs",
    ],
    [
      "Channel acceptance returns exact IDs before admitted network fanout finishes",
      "owner_commands::tests::registered_channel_enqueues_return_exact_ids_and_reject_retired_owners",
      "src-tauri/src/owner_commands.rs",
    ],
    [
      "Replacement and logout invalidate captured session authority",
      "desktop_session::session_generation_tests::owner_replacement_and_logout_invalidate_queued_operations_but_rename_does_not",
      "src-tauri/src/desktop_session.rs",
    ],
    [
      "Stale startup cannot launch participant producers",
      "commands::tests::actual_prelaunch_guard_rejects_session_replacement_without_launching_producers",
      "src-tauri/src/commands.rs",
    ],
    [
      "Runtime replacement awaits prior runtime retirement",
      "commands::tests::replacement_begin_awaits_real_host_stop_and_latest_same_owner_ticket_wins",
      "src-tauri/src/commands.rs",
    ],
    [
      "Stalled network producers allow observations and unrelated sends while blocking runtime retirement",
      "runtime_admission::integration_lease_tests::stalled_producer_allows_observation_but_prevents_runtime_retirement",
      "src-tauri/src/runtime_admission.rs",
    ],
  ];
  assert.deepEqual(
    owner("desktop-session").capability.invariants.map(({ name }) => name),
    obligations.map(([name]) => name),
  );
  for (const path of [
    "src-tauri/src/desktop_session.rs",
    "src-tauri/src/runtime_admission.rs",
    "src-tauri/src/runtime_bootstrap.rs",
  ]) {
    const plan = select(path, "desktop-session");
    const rust = plan.rust.find(
      ({ package: packageName }) => packageName === "mesh-talk",
    );
    for (const [obligation, qualifiedName, sourcePath] of obligations) {
      assert.ok(
        rust.filters.some((filter) => qualifiedName.includes(filter)),
        `${path} lost ${obligation}`,
      );
      const name = qualifiedName.split("::").at(-1);
      assert.match(
        read(sourcePath),
        new RegExp(
          `#\\[(?:tokio::)?test(?:\\([^\\]]*\\))?\\]\\s*(?:async\\s+)?fn\\s+${name}\\s*\\(`,
          "u",
        ),
        obligation,
      );
    }
    for (const consumer of [
      "desktop-integrations",
      "desktop-app-lock",
      "desktop-contact-policy",
    ])
      assert.ok(
        plan.owners.some(({ id }) => id === consumer),
        `${path} lost ${consumer}`,
      );
    assert.match(plan.escalation.join(" "), /native/u);
  }
});
