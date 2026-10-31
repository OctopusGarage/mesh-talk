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

test("delivery details select projection, disclosure and browser acceptance", () => {
  const plan = buildAffectedPlan(
    ["frontend/src/features/chat/DeliveryDetails.tsx"],
    { contracts },
  );
  assert.deepEqual(plan.blockers, []);
  const selected = plan.owners.find(
    ({ id }) => id === "frontend-delivery-details",
  );
  assert.ok(selected, "delivery details need an explicit business owner");
  assertDocumentReference(selected.capability.guide);
  for (const path of [
    "src/store/outgoingIntent.test.ts",
    "src/features/chat/deliveryFooter.test.ts",
    "src/features/chat/DeliveryDetails.test.ts",
  ])
    assert.ok(plan.frontend.includes(path), path);
  const scenarios = JSON.parse(read("frontend/validation.json")).prScenarios;
  assert.ok(
    scenarios.some(
      ({ paths, browser }) =>
        paths.includes("frontend/src/features/chat/DeliveryDetails.tsx") &&
        browser?.includes("e2e/automatic-delivery.spec.ts"),
    ),
  );
});

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
      "Profile acceptance releases the UI before admitted network fanout completes",
      "owner_commands::tests::account_profile_save_returns_after_local_commit_with_a_stalled_peer",
      "src-tauri/src/owner_commands.rs",
    ],
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
    [
      "Typed device-linking errors preserve the failed stage and safe diagnostics across IPC without weakening desktop admission or account adoption",
      "commands::tests::device_link_errors_preserve_the_stage_and_diagnostic_in_ipc",
      "src-tauri/src/commands.rs",
    ],
    [
      "Saved login requires both credential and resume metadata writes; retry uses the private canonical account login secret under current unlocked session authority",
      "commands::tests::registered_login_reports_unsaved_when_resume_metadata_cannot_be_written",
      "src-tauri/src/commands.rs",
    ],
    [
      "Signed account nicknames project into persisted desktop and frontend state only for the current installed session; old completed markers without a signed card repair the already-bound profile, while a pending adoption never publishes its unrelated previous local nickname across restart",
      "runtime_bootstrap::tests::signed_account_nickname_persists_emits_and_rejects_retired_owner",
      "src-tauri/src/runtime_bootstrap.rs",
    ],
    [
      "Inbound callbacks queue main-thread publication before acquiring exact-session and runtime authority, preventing native-window rendezvous from inverting the session lock.",
      "runtime_bootstrap::tests::inbound_dispatch_does_not_acquire_session_until_main_thread_execution",
      "src-tauri/src/runtime_bootstrap.rs",
    ],
    [
      "Inbound DM, channel and file presentation queues before acquiring desktop-session authority; UI-thread execution revalidates the exact session and runtime without a worker/main-thread focus-query deadlock.",
      "events::notification_policy_tests::notification_presentation_queues_without_waiting_for_the_main_thread_session_lock",
      "src-tauri/src/events.rs",
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

test("private account login boundaries retain pairing, relogin and retirement evidence", () => {
  const all = [
    ...contracts,
    ...JSON.parse(read("crates/mesh-talk-core/validation.json")).contracts,
  ];
  for (const [path, id, packageName, filter, testSource, testName] of [
    [
      "crates/mesh-talk-core/src/identity/account_login.rs",
      "core-account-login",
      "mesh-talk-core",
      "identity::account_login::tests::",
      "crates/mesh-talk-core/src/identity/account_login.rs",
      "account_login_guard_denial_preserves_existing_credential_envelope",
    ],
    [
      "crates/mesh-talk-core/src/node/account_access.rs",
      "core-account-login",
      "mesh-talk-core",
      "node::account_access::tests::",
      "crates/mesh-talk-core/src/node/account_access.rs",
      "direct_siblings_converge_with_optional_sync_disabled_and_unrelated_cannot_fetch",
    ],
    [
      "src-tauri/src/shared_account_login.rs",
      "desktop-account-login",
      "mesh-talk",
      "shared_account_login::tests::",
      "src-tauri/src/shared_account_login/tests.rs",
      "v4_pairing_adopts_login_preserving_storage_device_and_encrypted_history",
    ],
    [
      "src-tauri/src/linked_account.rs",
      "desktop-account-login",
      "mesh-talk",
      "shared_account_login::tests::",
      "src-tauri/src/shared_account_login/tests.rs",
      "reconciliation_shutdown_releases_auth_when_waiting_for_runtime",
    ],
  ]) {
    const plan = buildAffectedPlan([path], { contracts: all });
    assert.deepEqual(plan.blockers, [], path);
    assert.ok(
      plan.owners.some((entry) => entry.id === id),
      `${path} lost ${id}`,
    );
    assert.ok(
      plan.rust.some(
        (entry) =>
          entry.package === packageName && entry.filters.includes(filter),
      ),
    );
    assert.match(
      read(testSource),
      new RegExp(`(?:async\\s+)?fn\\s+${testName}\\s*\\(`, "u"),
    );
    for (const invariant of all.find((entry) => entry.id === id).capability
      .invariants)
      assertDocumentReference(invariant.reference);
  }
});
