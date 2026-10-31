# Local test feedback loop

During feature development, run the smallest tests that cross the changed
boundary. The pre-commit hook checks staged whitespace and source formatting;
it does not launch test suites. Record the focused commands and results in the
PR. Commit the candidate and use `./scripts/create-pr.sh -- <gh pr create options>`
for fast publication after cheap source, whitespace, structural and hygiene checks.
CI runs the comprehensive suites after publication; all required checks must pass
on the current PR head before merge. Creating a PR does not certify acceptance.
For comprehensive local validation, pass `--full-preflight` before the `--`
separator with affected selectors/specs, or run `./scripts/prepare-pr.sh`
directly. Preparation runs affected platform preflight followed by full health,
including `make test`'s workspace test stage. Run backend or full UI E2E sooner
when explicitly requested or when the changed behavior needs process/browser evidence.

| Changed boundary            | Fast local command                                                                                          | Broader trigger                                             |
| --------------------------- | ----------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| Core unit module            | `make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=node::account_sync`                              | `make test` locally or matching CI before merge             |
| Host settings/session       | `make test-focused TEST_PACKAGE=mesh-talk TEST_FILTER=owner_scoped_node_handle`                             | `make test` locally or matching CI before merge             |
| One Rust integration target | `make test-focused TEST_PACKAGE=mesh-talk-core TEST_TARGET=decoder_smoke`                                   | `make e2e` when requested or for release validation         |
| Frontend state/controller   | `cd frontend && npm test -- src/features/chat/settingsDialogSession.test.ts`                                | `npm run test:coverage` in PR preflight                     |
| Frontend rendering          | Run the affected component test, for example `cd frontend && npm test -- src/features/chat/Sidebar.test.ts` | Pass relevant Playwright specs to `scripts/pr-preflight.sh` |

Use `cargo test -p <package> --lib <module_or_test_name>` directly when a
filter is easier to spell than a Make variable. `make test-focused` requires a
package plus either a filter or an integration target, so an omitted selector
cannot silently start the whole workspace. Ignored process tests require an
explicit `cargo test ... -- --ignored --test-threads=1`; the focused target does
not enable them automatically.

Changes to configured portable browser specs and the portable configuration
select the `portable-browser-reporting` diagnostic. It compares both core and extended
inventories with Playwright discovery without launching browsers. Register new
scenarios in `requiredBrowserPaths` and additional spec files in the owning
validation contract. Discovery checks do not replace executed browser evidence.

PR Chromium portable core acceptance reuses the complete full-suite execution on
each desktop OS. `scripts/evals/chromium-evidence.mjs` extracts the authoritative
core scenario paths from each of four shards, copies and validates screenshots,
success traces and raw scenario metadata, then validates the exact once-only
union using the current source/run/attempt/platform identity. Missing, failed,
flaky or skipped portable scenarios fail acceptance. Windows uses the same
`windows-latest` runner as the full suite; Chromium context settings match the
standalone portable configuration. The collector reopens evidence on Linux with
an explicit expected source platform, rather than trusting a producer verdict.
Linux and desktop shards call one reusable `ui-shards.yml` implementation: four
Linux plus eight macOS/Windows runners. Linux coverage depends on its own shards
and Health, so late desktop scheduling cannot delay that merge. Portable collection
and final UI acceptance still require both complete groups. One optional job
renders all three HTML reports outside acceptance.
Desktop browser shards start after Health completes, limiting the initial runner
burst. Portable Health and independent backend jobs use the standard
`macos-26-intel` pool. All browser suites, the Rust workspace and real native
desktop acceptance retain `macos-latest` Apple Silicon execution. Measure both
queue and execution before moving a workload: Intel qualification substantially
increased full Chromium execution time, and cold WebKit startup left too little
of the existing scenario budget for its assertions. Vite warmup did not show a
repeatable improvement in fresh-server comparisons and is not enabled.
Staging desktop admission keeps full Chromium out of the initial ARM burst. Intel is a
[standard hosted runner](https://docs.github.com/en/actions/reference/runners/github-hosted-runners),
not a larger-runner purchase; both pools still require measured queue and execution
evidence. Keep artifact consumers and app-bound required check names aligned when
changing labels.
The full suite keeps its existing harness budgets: Linux 90 seconds when collecting
browser coverage, Windows 90 seconds and macOS 30 seconds; standalone portable uses
Linux/macOS 30 seconds and Windows 120 seconds. These are harness watchdogs, not
product latency assertions. Chromium's device and timezone are aligned; the
Windows image is the full suite's `windows-latest`, not the former standalone
`windows-2022` image.

For transient message highlights, start `helpers/message-highlight.ts` observation
before navigation. Its browser-side predicate captures the visible exact target
and rejects competing highlights within the existing assertion budget. Keep
subsequent navigation/context assertions and the production highlight lifetime;
controller round trips must not determine whether short-lived feedback occurred.
Recovery-batch fixtures send ordered events in one browser call and yield a render
turn between deliveries. Preserve every payload and completion marker, and verify
intermediate unread counts; a synchronous burst changes the scenario being tested.

The browser harness still relies on Vite source-module imports, explicit lazy
module failure routes and an unbundled media harness. A built-preview replacement
must first supply equivalent bundled test seams, preserve negative controls and
validate source-mapped coverage. Changing only the server command drops those
contracts and is not a qualified optimization.

Dedicated WebKit and Firefox core run on ordinary PR, main and merge candidates
through `scripts/evals/portable-runner.mjs`. Scheduled/manual extended runs add
Chromium and Firefox extended inventories. Firefox uses the same complete evidence
validator and existing job budget in both modes; browser-specific core regressions
must be detected before source acceptance, rather than first by the next scheduled run.
Windows uses four sequential Playwright shards with separate browser/dev-server
lifetimes; other platforms use one. Each shard writes a unique nested evidence
root and must exit successfully. The runner merges every outcome and validates
the unchanged full scenario inventory before the existing screenshot/trace
validator runs. Missing, duplicate, skipped, flaky or failed cases remain failures.
The current 79-case core and 80-case extended inventories keep each Windows
process to at most 20 scenarios; discovery regressions verify this bound and the
exact once-only union. Two 38-case processes exceeded the 20-minute hosted job
deadline after 75 of 76 scenarios passed, with later page loads slowing down.
Four processes reduce that accumulation without increasing the job deadline,
retrying failed scenarios or establishing a product latency guarantee.

A media-cache/virtualization fixture must establish both reader intent and the
observed navigation boundary. Its trusted Home key reaches React's reader handler
before a one-shot window bubble listener prevents default and sets the history
start in the same event task. This fixture owns that cache-remount boundary;
it does not assert browser paging semantics. Large wheel deltas are normalized,
Home behavior differs across platforms, and requiring every PageUp to make
monotonic progress adds an unrelated native-navigation assumption. A bare
programmatic scroll can instead be treated as layout correction. The missing-reader
negative control must fail. Retain explicit live-edge setup, exact history-start
position, unmount, return-to-latest, row geometry and media URL reuse assertions.

Geometry polling treats an unavailable bounding box as pending. Poll callbacks
must return that unavailable state rather than dereference it or invent zero-sized
geometry; callback exceptions abort polling. Capture a positive baseline and keep
accepted image/frame rectangles from one visible decoded instance, as in the reply
preview fixture. A temporary disappearance must recover within the existing assertion
budget, while a permanently missing row must still fail the same geometry assertion.

Presence expiry fixtures observe consumption of the distinctive lease source
before failing subsequent reads. Deleted-contact fixtures await the post-change
roster refresh before asserting absence. A sleep shorter or longer than a polling
period proves neither of those operation boundaries; delayed-consumption and
paused-polling controls must still exercise expiry, recovery and reappearance.

Custody propagation fixtures first settle the newly created channel's signed
inventory/control heads and the exact linked routes. Readiness observed before
channel creation does not cover that new work. Hold a real authenticated inventory
response to verify that setup remains pending until release; accept authenticated
superseding heads so an obsolete captured version cannot strand the fixture.
Keep the existing custody observation budget, real network exchange, offline-holder
and restart assertions. This setup boundary does not establish a production latency
guarantee or a dependency between independent mandatory/optional worker lanes.

Keep intentional blob corruption separate from automatic custody propagation.
A real sibling read during a missing-blob interval can change its protocol retry
state even after the source repairs the blob. The storage-repair fixture uses a
real external holder and awaited encrypted delivery/recheck, with no autonomous
source worker during corruption. It proves unknown rather than zero, repair
without signed metadata churn and durable readability after reopen. Automatic
propagation retains its original deadline with the external holder already
offline, without an explicit pull or prior custody warmup. Controlled fault-path
evidence does not by itself identify the cause of a historical timeout; matching
sanitizer execution remains required.

Explicit-file result fixtures establish authenticated File capabilities in both
directions before admitting their first request. A forward role view does not
prove that the executor can fetch the sender's prepared assets: document import
can wake work before its authenticated capability receipt is installed. Hold a
real reverse Control response to verify this setup boundary, then retain the
original operation deadlines, matching-retry and byte assertions. Deliberate
capability-refusal scenarios must preserve their missing-capability state.

Both the initial requester and the fresh result requester finish cold capability
setup before invoking the unchanged three-second signed-role proof. Running nodes
observe their existing producers. The fresh requester intentionally has no
background worker before retry admission, so its cold phase drives the required
Control exchange and retains the shared owner's cursor, authenticated interruption
classification and admission backoff. Do not start its worker early: that would
erase the result-adoption race. A real fresh Control response held beyond the role
budget must complete through cold setup before the original matching-retry and
byte assertions. Static phase markers distinguish initial and fresh setup failures.

Dialog-to-menu regressions distinguish fixture state from actual focus ownership.
Hold the real deferred close-focus callback, remove the old dialog, open the next
menu through a real click, and release the callback. The new menu must retain its
focus and open state. Waiting for an animation alone would hide this production
handoff bug. Preserve ordinary/nested close restoration and custom handlers;
[the shared dialog owner](module-boundaries.md#dialog-focus-ownership) defines the
contract. Observe expanded/open menu attributes in navigation helpers.

## Validation stages and agent responsibilities

Validation depth and business capability are separate dimensions. Use the same
owning contracts at each depth; a development result never promotes itself into
candidate acceptance. This follows staged pipelines and actionable feedback as
described by [Fowler](https://martinfowler.com/bliki/DeploymentPipeline.html) and
[Software Engineering at Google](https://abseil.io/resources/swe-book/html/ch23.html).

| Stage                | Trigger and purpose                                                                          | Entry or evidence                                                                    |
| -------------------- | -------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| Understand and plan  | Start work or change its scope; identify business owner, consumers and obligations           | Owning guides, `make verify-plan`                                                    |
| Change feedback      | Validate a small change with affected static checks and executable tests                     | `make verify-changed`, development feedback JSON                                     |
| Boundary validation  | Change cross-module interfaces, runtime IPC, visible behavior, persistence or lifecycle      | Plan requirements and escalation review; focused integration/browser/native evidence |
| Candidate acceptance | Accept the current committed PR head before merge                                            | Required protected CI; optional local `prepare-pr.sh` and full health                |
| Extended validation  | Scheduled, release or explicitly triggered compatibility, stress and long-running properties | Owning backend/UI/native workflows and evaluation guides                             |

Read the selected capability's authoritative guide before changing behavior.
Declare semantic risks that source imports cannot express, inspect automatic
consumers and boundary requirements, and add missing owners/scenarios with a
reason. AI may expand scope; it cannot remove mandatory selections because it
judges a change low risk. Preserve the distinction between local acceptance,
recipient delivery and read state, and between frontend ownership and host
session authority. Summarize observed results and remaining obligations before
moving to candidate acceptance.

## Maintaining standards across requirements

For every new requirement, the implementation owner follows this sequence:

1. Locate the existing decision owner and authoritative business rule. Describe
   observable acceptance criteria, including applicable failure, permission,
   persistence and lifetime behavior. Clarify the rule in its owning guide when
   its meaning changes; preserve the canonical glossary.
   Select applicable [maintenance inventory](maintenance-checklist.md) items for
   documentation, generated exports, external publications and other consumers;
   record evidence, justified exclusions and owned follow-ups in the PR.
2. Preview `make verify-plan` against the actual diff. Review consumers and
   browser/native escalation, including runtime relationships that imports cannot
   reveal. Extend the owning implementation before creating another rule owner.
3. Maintain the owning `validation.json` in the same change when paths, shared
   fixtures, consumers, selectors or escalation requirements change. New business
   capabilities should declare their guide and invariant references. Existing
   contracts do not need invented metadata or a duplicate specification. Explain
   why the current contract remains sufficient when no change is needed.
4. Add independent assertions derived from the requirement or a historical
   failure. For a reproducible bug, show the regression failing before the fix.
   Run affected checks while editing and replan when scope changes.
   When a confirmed defect exposes a reusable failure pattern, update the
   [AI coding reliability guide](ai-coding-reliability.md#keep-the-guide-useful-as-defects-are-found)
   with its trigger, owning contract and regression evidence; keep incident logs
   in ignored output or PR/CI artifacts.
5. Run `make standards-check` and `make verify-changed`. Report the tested source,
   observed outcomes and outstanding obligations in the PR. Complete the existing
   protected CI checks before claiming merge readiness; comprehensive local
   candidate preparation remains optional.

`make standards-check` is a cheap structural audit of all registered validation
contracts and source ownership, rather than another build or test suite. It uses
one shared contract loader and the established planner/import graph. It checks
schema and consumers, explicit test-file references, capability guides and
supported heading anchors, and executable development selections for Rust and
frontend source. A selection means tests are declared, not that they executed or
contain useful assertions. Runtime wiring, complete requirement coverage and
independent review still require human judgment.

Capability anchors support ATX (`# Heading`) and setext headings outside fenced
code, basic Markdown markup/link stripping, and numbered duplicate-heading suffixes.
Headings containing `<` or `>` (including inline HTML and autolinks), custom HTML
anchors and complete GitHub Markdown rendering are outside this checker's supported
reference syntax; use ordinary headings for contract links. Anchored references to
documents containing HTML-bearing headings fail, including references to later plain
headings: skipping unsupported headings would invalidate duplicate-anchor numbering.
Unanchored file references still work. The checker does not sanitize or render HTML.

Exact-path frontend bootstrap, auth-screen, chat-shell and window-control owners
use conservative existing state/helper unit selections where their composition
has no importing unit test. Their `prScenarios` rules also require the owning
browser suites. These fallback selections do not establish root rendering or
native correctness, and new source outside those explicit paths still needs
ownership or import reachability. The avatar primitive has a direct rendering
regression.

The checker runs in pre-commit, before expensive PR preparation and health work,
and in the existing CI platform checks after frontend dependencies are installed.
Install dependencies with `npm ci --prefix frontend` on a fresh checkout; missing
parser dependencies fail rather than silently skipping frontend analysis. Hooks
inspect the working tree, including untracked source. They do not construct the
partially staged candidate, so staged-only correctness is established by CI on
that committed candidate. No local hook or structural audit replaces full gates.

When a failure reveals a weakness, classify it using the cause-to-correction
mapping under [structured feedback](#structured-development-feedback). Correct
its owning contract, task, fixture or business rule and add a known-change or
real-execution regression that would catch the same omission. Update reusable
AI evaluation scenarios when the reviewed mechanism changes; model availability
is not a prerequisite for deterministic enforcement. Keep generated evidence in
ignored output or CI artifacts, and record durable decisions in this guide or
the relevant owning guide.

During maintenance and before proposing gate reductions, compare like-for-like
latency reports and same-source selection audits: first actionable feedback,
required-gate p50/p95, observed runner minutes, first-attempt failures and observed
selection omissions. Missing data stays unknown. Investigate newly slow or flaky
checks before adding retries or global obligations. A policy change needs an
explicit rationale and review; this process does not automatically rewrite rules,
relax thresholds or infer a speedup or zero omissions from a small sample.

Private account-action fixtures follow the [account/device ownership contract](module-boundaries.md#account-and-device-ownership): source commit, durable requester adoption and public member receipt are separate observations. The generic History-off/signed-provenance functional fixture uses a thirty-second watchdog around source and requester commitment, exact effect IDs and durable private projections. This is a functional completion bound, not an eight-second business latency guarantee. Dedicated request/result and held-traffic performance fixtures retain their independent eight-second assertions. Verify public receipt afterward through the existing authenticated sync owner, retaining original signature, author, sealed payload and reduction assertions. Never install an event or decrypted body directly to make receipt pass. Dedicated automatic-fanout fixtures retain coverage for scheduling, backpressure, delayed work and real member delivery.

When a reference-device fixture requires a synchronous capability-cache override
and admission on one current-thread runtime, keep that requester and its watchers
there. Run the other simulated devices' complete accept/background owners on
independent owned current-thread runtimes. Transfer listeners through standard
sockets before registering them on the owning reactor; release all server start
gates together without warmup traffic. Preserve each explicitly scoped functional
or performance deadline, resource budgets and signed-provenance assertions. Post-commit public network observations
run on the observing device's runtime. Shutdown closes admission, cancels and joins
producers, clears idle sessions, drains admitted writers and joins every owned
thread, including panic cleanup. A same-source topology comparison diagnoses a
fixture constraint; it does not establish the cause of every CI timing failure.

Modern self-control fixtures allow thirty seconds for cold, bidirectional
capability and role readiness before admission, matching the existing owner-rename
setup boundary. This is a bounded setup watchdog, not an operation latency claim:
retain every full-identity capability and signed-provenance predicate, record
setup duration and complete/capacity diagnostics, and keep the independent
admitted-action deadline at eight seconds. The archive-marker setup and dedicated
held-traffic latency assertions also retain their existing eight-second bounds.
Cold authentication and control exchanges keep their production network limits.

Reference self-control fixtures hold requester-side public membership recovery
for the selected group through the remote action proof. Disabling account History
does not disable that recovery owner; it can otherwise create a ready local
executor during setup. Use an instance-scoped test gate before privacy and
membership locks, retain real private role/capability exchanges and release the
gate on every exit. Assert the admitted executor's full identity and derive its
diagnostic label from admission, rather than assuming the setup peer was chosen.

Role-bootstrap helpers bind readiness to the requested group, the exact full source identity and a signed head floor captured when invoked. If none exists yet, pin the first non-tombstone source head observed within the same setup deadline. Accept that exact metadata or a strictly newer signed revision for the same key and full source; reject older, tombstoned or same-revision conflicting records. A later concurrent publication must not move an already satisfied snapshot barrier. Helpers may observe that durable role through the existing local document watch while an unrelated page tail remains in flight. A prior completed capability response for the exact full source identity is required before cancelling that redundant read. Preserve the setup deadline, current-binding validation and completed protocol-error checks; an incomplete first capability page cannot establish readiness.

Pagination fixtures distinguish a completed comparison sweep from a settled full
scope. A selected signed row can advance after its completion digest was captured;
`pending == false` alone does not prove `up_to_date`. Start a fresh comparison
through the existing cursor owner after a completed mismatch, retain partial
continuations and the original total pull bound, and require both full settlement
and the complete exact source metadata set. Failure diagnostics should report
body-free cursor flags and metadata-class differences rather than message bodies.

Growing-history fixtures observe exact durable IDs without blocking their
current-thread network runtime on repeated full history projections. Once those
IDs are present, retain the public history projection and exact original-ID
assertion within the same watchdog. Preserve the original corpus, source appends,
continuation-page requirement and legacy protocol coverage. Forward progress
through a changing archive does not require its moving digest to settle.

Explicit custody-page fixtures follow the production cursor's local-admission
contract. A typed local-capacity rejection exchanges no bytes: preserve the same
cursor and continuation, wait for its existing retry time, and retry within the
functional fixture's thirty-second watchdog. Two typed local-admission failures
can legitimately require two five-second deferrals before a signed page completes;
retain real admission and authenticated-page checks rather than clearing cooldowns. An early successful no-op while admission is still
deferred does not prove a completed exchange. Other errors fail immediately;
History-off, exact signed negative versions and repaired-positive assertions stay
unchanged. Do not clear cooldowns, create a new cursor or warm the connection to
avoid this boundary.

File retry fixtures that require the initial best-effort attempt to fail can
accept the real privacy handshake and immediately close before any application
frame. Keep the original sending API, durable pending work, relay custody,
consent and complete-byte assertions. Borrow the fixture listener only during
that setup call; drop its responder before starting the normal production accept
owner. This models an actual authenticated transport failure without spending
unrelated handshake deadlines at an unserviced listening socket.

For manifest frame boundaries, binary search a monotonic encoded-size predicate
within the original range. Check the immediately preceding fitting case, the
first protected overflow and ordinary-frame compatibility, then pass the original
prospective manifest shape to the production selector. Keep real serialization,
proof/expiry overhead and frame limits.

Mutation timing comparisons must retain the actual package/workspace command,
test inventory and process count. A package-only baseline does not establish
that a larger workspace mutation run fits the same deadline. Improve measured
fixture overhead without narrowing test scope. Budget the full inventory from
observed work and runner variance; retain process isolation and per-test hang
detection when correcting a whole-suite budget.
A resource-bound argument may explain an equivalent surviving guard mutation;
it cannot turn an interrupted or timed-out run into a completed advisory survivor.

The development profile gives the protocol core optimization level 1 because
its callers instantiate generic authenticated-encryption loops. Dependency-only
crypto optimization does not optimize all of those loops. Tests and mutations
inherit this setting while retaining debug assertions, overflow checks, complete
inventories and existing worker/deadline limits. Release settings are independent.
Compare execution time separately from initial and incremental compilation cost
when assessing this tradeoff; a focused speedup does not certify a full baseline.

## Change-aware development plans

Use the affected-test planner during development:

```bash
make test-plan
make test-changed
# Compose affected tests with formatting, lint, types and necessary builds:
make verify-plan
make verify-changed
# Include committed changes since a comparison branch, plus current edits:
node scripts/validation/changed.mjs --base origin/main --json
```

The default comparison is `HEAD`, so staged, unstaged and untracked edits are
included. An explicit base uses its merge base with HEAD and includes committed
changes since then. Deleted files remain selection inputs; renames are treated
as deletion plus addition. Preview reads source/contracts only: it does not
compile, start browsers or run tests. It exits with status 2 when selection is
blocked. `--json` emits the same plan as structured data for agents.

The plan also includes `requirements.browser`, `requirements.native` and their
owning scenario IDs, loaded from the same `prScenarios` used by PR preparation.
They include deleted/renamed source boundaries and remain pending during a
focused development run. Semantic review can add evidence with repeatable options:

```bash
node scripts/validation/changed.mjs --checks \
  --browser e2e/personal-messages.spec.ts --native host \
  --reason "Reviewed dynamic IPC consumers and session lifecycle"
```

These additions require `--reason`, validate existing browser specs and supported
platforms, and union with mandatory rules; they do not launch browser/native work
or replace PR preparation. Carry extra selections into candidate preparation.
Source analysis and these explicit additions do not prove all semantic risks
were identified.

`validation.json` files beside each Rust crate, `src-tauri/`, `frontend/` and the
validation tooling own focused selectors, explicit downstream consumers and
escalation notes. Paths are repository-relative exact names or prefixes ending
in `/`; IDs are unique across contracts. Consumer links expand transitively and
selected Rust filters are combined per package. A broad owner can declare literal
`excludePaths` for source with an established focused owner; exclusions affect
path matching only and never suppress explicit or transitive consumer selection.
Unowned exclusions still block. The Node fallback excludes the four existing
profile-owner files so their established five-filter slice remains focused. Its
private `file_consent_tests.rs` module also has a test-only owner selecting that
module alone; production and shared-fixture changes still expand to consumers.
Add shared fixture consumers at the owner's contract rather than copying
selection rules into callers. Contract
changes themselves select the planner's regression tests.

An optional `capability` adds a concise `summary`, an authoritative local Markdown
`guide`, and `invariants` containing `{name, reference}` links. It describes why
an owner is selected without duplicating business specifications or task mappings.
The first pilots are `frontend-outgoing` and `desktop-session`: their selectors
and consumers retain their existing meaning. The planner validates metadata and
referenced files; pilot diagnostics also check their maintained anchors and
executable obligation tests. Other contracts can adopt the same metadata as their
business boundaries change.

Frontend selection also follows transitive source imports, type imports,
re-exports, literal dynamic imports and literal Vitest mocks. It includes all
importing unit tests and retains former edges for changed/deleted source. The
registered unit-file convention is `src/**/*.test.ts`, matching Vite. Tests that
share an imported fixture therefore run together. Runtime IPC/events, computed
imports, glob discovery and configuration dependencies cannot be established
completely through static imports. Computed discovery produces an escalation
note; review runtime wiring and select browser/native evidence where needed.

Rust contracts cover the tracked sources, crate manifests, shared fixtures and
entry points of all four crates. Core owners conservatively include Node and
known desktop/integration/MCP consumers; shared Cargo/lockfile/toolchain changes
select every crate. Some package, binary and integration-harness inputs use
library fallback selectors with explicit escalation, because this development
runner does not automatically launch binaries, integration targets or native
checks. Library coverage is not evidence that those scenarios ran. Frontend
configuration, package/lockfiles, bundled marketplace assets and public help inputs
select all registered frontend unit files; dynamic runtime dependencies still
need browser/native review.

Unknown inputs continue to block. Add their owning contract, or explicitly select
all affected owners and explain the decision:

```bash
node scripts/validation/changed.mjs \
  --owner core-transport --owner desktop-session \
  --reason "Reviewed transport consumers; this change affects session retirement" \
  --checks --run
```

An explicit selection acknowledges every otherwise unmapped path, so inspect the
preview first. This records judgment, not proof of complete dependency inference.
Missing/invalid selectors and owners with no executable tests still fail closed.

`make verify-changed` runs the same affected tests as `make test-changed`, preceded
by the applicable static checks: Rust formatting and package-scoped Clippy with
`--all-targets -- -D warnings`; frontend formatting, ESLint, source/E2E TypeScript
and production build; workflow actionlint and changed-shell shellcheck. Cheap
format/lint/type checks acquire `focused-static` before the runner queues for the
heavy build/test resource; they can fail while another worktree is compiling or
running browsers. The Make entry acquires the managed build-retention guard only
after this phase; it then acquires the shared build resource in the existing
lock order. Build/Clippy and tests retain the existing resource isolation. The
outer attempt includes retention waiting in its execution envelope; the inner
`affected-heavy` attempt reports the shared-resource queue separately. Composed
receipts bind required build outputs as well as source inputs.
Selected Rust tests execute through Cargo after binary inventory, preserving
repository environment settings such as the `RUST_TEST_THREADS` limit in
`.cargo/config.toml`.
Failed frontend JSON reports are summarized before temporary-file cleanup, so
the command log retains suite errors and failed assertion diagnostics.
Each phase recomputes its plan and checks a fingerprint of the source revision,
changed source bytes (including untracked/deleted paths), obligations and ignored
frontend environment inputs. Edits while queued or executing require fresh
validation, including for dirty development worktrees. The two phases share the
existing cancellation group and overall deadline. Tauri checks prepare real
frontend assets before compiling the host. The test-only
entry remains useful during edits; the composed entry is the task-completion
check. Both remain development commands, not publication certification.

Execution uses existing cross-worktree stage locks and keeps child processes
within its cancellation group, with a 30-minute overall queue/run deadline. Rust
compilation uses the shared heavy lock; focused frontend and Node checks use
separate locks so they do not queue behind a full browser gate. If edits change
the required resource class while queued, execution fails and requires a new plan.
It compiles only selected Rust library targets, checks every filter against actual nonignored test names before running
any tests, and invokes each test binary once with the combined filters. It never
automatically enables ignored tests or launches workspace/E2E suites. Each Node
diagnostic must run passing tests; every selected Vitest file must report at
least one passing assertion. Output separates Cargo compilation/inventory from
test execution time; existing stage logs retain queue time and overall duration.

For parallel agents, assign one module owner and its focused plan to each agent.
After combining their changes, one integration owner reruns the combined plan
and the applicable mandatory gates. Reuse remains limited to the existing clean,
revision-bound evidence policy; dirty edits never create reusable success.

The test-only command selects development tests, not static checks or release
evidence. Use `make verify-changed` for its composed static checks. Expand validation for public
interface, persistence, crypto, authorization, browser and native behavior changes,
using the boundary table above. Existing PR preflight, full-health, protected CI
and release obligations remain unchanged. Compare median/p95 feedback latency,
compile time, resource queue time, first-attempt failures and defects caught only
by broader tests before claiming a speedup.

### Cold fixture preparation versus measured progress

Account-group Noise latency fixtures share the existing test owner's bounded
30-second `cold_account_setup` for reciprocal capability establishment and the
first authenticated response that creates a controlled hold. These waits happen
before operation admission or before the external custody source starts. They
establish the fixture's real connection state; they are not an eight-second
operation or custody latency assertion, and they do not drive extra RPCs, warm
session pools, retry submissions or suppress background workers.

Account-group setup uses the shared test-local `cold_account_control_setup` to
finish cold capability observation before invoking the three-second signed-role
page helpers. Each caller retains its exact full peer identities, directional
operation-kind requirements and any stronger document-settlement predicate inside
one existing cold budget. Autonomous fixtures observe their existing background
producers. The inbound-only incoming-Wake fixture drives its existing two setup
directions inside that budget before observing readiness; a passive wait there
would have no producer to satisfy it. No extra warmup exchange is added.

`selected_source_cold_capability_setup_precedes_role_page_budget` holds the real
first authenticated Control responses beyond the role-page budget in a two-device
bootstrap fixture. It then requires reciprocal capabilities and the exact signed
source role through the same shared phase owner. The original held-operation
fixture retains its separate eight-second operation, receipt and recovery proofs.
Both fault controllers belong to their fixture task sets, with drop guards that
release held responses and cleanup that joins tasks and drains admitted writers.

Capture account document health before closing runtime admission or stopping
peers. The test-local `capture_account_sync_before_cleanup` owns that observation
boundary while still awaiting all caller-owned cleanup before the assertion.
Never clear errors or wait for them to disappear. Validate the captured statuses
with the unchanged no-document-error predicate; diagnostics follow the
[sensitive diagnostic rules](../specifications/development_conventions.md#sensitive-diagnostics).
`account_fixture_health_snapshot_precedes_real_peer_shutdown` holds an acknowledged
real document request, stops its peer and observes the resulting real sync error;
it verifies that teardown cannot replace the previously captured health evidence.

Modern self-control fixtures observe validated Directory and Control scope
completions in both directions for the selected full peer identities. These
observations reset before final requester enrollment. The aggregate device sync
phase includes unrelated lanes and is not a prerequisite for group admission.
Keep the original role, capability and retained-membership proofs alongside these
scope completions; the observations neither issue requests nor stop workers.

Same-source partial-page fixtures prepare their immutable signed prefix before
arming the held request. Require matching metadata at both endpoints and a current
Control completion without an outstanding pair in every participating sibling
direction; possessing the rows alone does not settle an older comparison. Only
then add the controlled tail through the existing source owner. A natural origin
and an unprotected dispatch do not imply an unpaired task. Preserve the complete
corpus, exact live-request checks and checkpoint repayment assertions when moving
this preparation outside the operation budget.
If a real Wake retires that preparation request, a later eligible comparison may
advance the same signed tail key within the existing setup bound. Stop preparation
once a live hold is captured; never retry the measured operation. The fault
regression must prove cancellation, retirement and capture of a distinct live
request before admission.

Once the original operation is admitted, or the external possession proof starts,
the existing eight-second progress assertions remain unchanged. The same applies
to held-traffic assertions, checkpoint repayment, archive recovery, the explicit
Wake five/eight-second deadlines, three-second role-page bootstrap and five-second
work drains. The overall nextest per-test 60-second and mutation 360-second
limits remain authoritative; stage bounds do not add together into a larger test
budget. Only waits with this specific pre-admission/controlled-hold purpose use
the cold helper; do not replace unrelated eight-second waits by matching their
numeric duration.

Projection-work regressions distinguish root membership-control reconstruction
from nested legacy parent-cut proof validation. Keep the original root-control
counts and observe nested work separately. Content-only additions, including
content after a real departure, must leave both counts unchanged; do not pin a
particular nested count or hide nested work from total-work assertions.

## Structured development feedback

Every affected execution writes a unique JSON report under
`target/validation/feedback/` and prints its path. A preview writes no evidence.
To select a stable destination for a consumer:

```bash
make verify-changed \
  VALIDATION_ARGS="--report target/validation/feedback/current.json"
```

Custom destinations must be JSON files under the worktree's ignored `target/` or
`tmp/`; they cannot overwrite source. Reports identify source/base, dirty state,
input fingerprint, selected capability references, phase obligations and observed
command outcomes. Both test-only and composed execution check the fingerprint
before and after work, including edits to a dirty worktree while queued.

Phase status distinguishes `not-run`, `running`, `passed`, `failed`,
`unavailable`, `invalidated` and `cancelled`. Planning blockers produce `blocked`;
an empty selection produces `no-checks`, never passing tests. `unavailable` means
an observed command/environment launch failure, including a missing tool below a
static stage wrapper. A nonzero executed check is `failed`; an interrupted run is
not evidence of a failed assertion. Missing browser/native evidence remains
`not-run` until separately attempted. An abrupt supervisor kill can leave a
`running` report; treat it as incomplete.

Passing test-case counts establish nonempty execution, not assertion-call counts
or assertion quality. Evaluate test quality separately; a named passing test can
still contain no useful assertion.

Command records describe process execution, not test assertions: successful Rust
inventory or wrapper commands alone cannot make a phase pass. Existing nonempty
inventory/assertion checks and unchanged-input checks must also succeed. Reused
phase evidence is explicit and includes its receipt, with no fabricated command
executions. Reports omit command output and environment values; use the phase
logs for failure details. They are development observations, not acceptance
certificates or a second receipt store.

`remaining` and `next` retain browser/native requirements, semantic escalation
review and candidate preparation. `candidateReady` is always `false`, including
when all selected local checks pass. Review those obligations and carry manual
browser/native additions into preparation; the report does not verify or import
external acceptance evidence.

When CI finds something local feedback missed, fix the owning cause:

| Cause                                      | Owning correction                                                                   |
| ------------------------------------------ | ----------------------------------------------------------------------------------- |
| Relevant test was omitted                  | Update the capability's consumer/selection contract and selector regressions        |
| Local and CI flags/features differ         | Align the existing task definition and toolchain inputs                             |
| Platform or real-environment behavior      | Add the affected boundary's platform/scenario requirement                           |
| Flaky timing, ports or resource contention | Fix isolation and the wait being proved; measure repeated results                   |
| Requirement was misunderstood              | Clarify the authoritative business rule and add an independent regression assertion |

Use `make validation-audit` with same-source broader evidence to track selection
misses. Keep complete candidate gates while gathering that evidence; do not add
a global mandatory check for every isolated incident or narrow gates from one
successful sample.

## Local evidence reuse and resource ownership

Local gates use `scripts/validation/stage.mjs` for diagnostics, frontend builds,
lint/typechecking, unit coverage, workspace tests, and browser runs. By default an identical
successful stage on a clean committed HEAD can be reused for up to 24 hours.
Receipts bind the revision, working directory, platform, tool versions, command
arguments, relevant environment and required output digests. Base-sensitive local
preflight also binds the resolved comparison ref. Changed or missing artifacts,
failed/interrupted stages, dirty sources, and changed inputs require execution.
`CI=1` and `MESH_VALIDATION_FRESH=1` always execute checks. Advisory and secret
scans continue to run directly rather than reusing time-sensitive scan verdicts.

`make test` and full health share the same workspace-test stage. Preflight and
health share frontend static/unit stages. With `--full-preflight`, the PR wrapper
invokes local preparation; its local work can reuse intact evidence, while both
required remote platforms always execute. An unavailable environment blocks that
optional full preparation. Fast publication runs cheap checks instead; required
hosted CI must pass before merge.
Deterministic frontend formatting/lint/type/build/unit stages may opt into
`--inputs frontend`. Their explicit closure includes tracked frontend sources,
configuration/tests, public help code/content, marketplace assets, workspace
version, validation scripts and root npm/Git configuration. Ignored frontend
`.env` files and `VITE_*` environment values also invalidate reuse. Identical
inputs can reuse a successful local stage across unrelated clean commits, with
the original source SHA retained as provenance. Required build/coverage artifacts
must still exist with matching digests. Dirty trees, source/input changes during
execution, tool/environment changes, failures, cancellation, TTL expiry and
`CI=1`/fresh requests prevent reuse. Browser, native, platform, security and Rust
stages retain revision-bound/fresh policies; this is trusted local acceleration,
not a claim of hermetic execution or transferable signed evidence.

No local receipt replaces a protected GitHub check. Local receipts are trusted
workspace acceleration, not signed attestations against a hostile local user.

Running-stage heartbeats exclude queue time, matching the structured execution
duration rather than counting resource waits as test runtime.

Heavy local stages share a resource lock across Git worktrees, preventing browser,
coverage and Rust compilation workloads from competing. Node unit tests default
to two workers. Cargo owns Rust test concurrency through `.cargo/config.toml`
(four threads); the supervisor does not inject a competing default into aggregate
stages or their Cargo children. Explicit `RUST_TEST_THREADS` values remain
authoritative for serial Windows/sanitizer checks and invalidate matching
evidence. Direct test-binary callers must supply their intended thread budget.
Each browser stage owns an available port and private results/report directory.
The full browser process has a two-hour ceiling; selected/portable processes have
30 minutes. `MESH_BROWSER_TIMEOUT_SECONDS` can set an explicit 1–7200 second
ceiling and participates in evidence identity. Individual scenario timeouts remain
unchanged.
Coverage cleanup and merging occur inside that stage's exclusive resource window.

Before browser coverage or remote execution, preflight compiles the workspace
library test executables once and checks selectors against libtest's actual test
names without executing tests. Invalid filters fail locally with the crate and
selector. Choose module filters present on the preparing platform for tests whose
individual names are platform-specific. Remote execution still rejects selectors
that run zero tests on either guest.

Per-stage logs, atomic success receipts and per-attempt JSON history (including
failures, cancellation and reuse) live under `target/validation/`. Cancellation
retains resource ownership through descendant-process cleanup. Nested POSIX
stage commands share the outer managed group and its single escalation owner;
nested leases are not released before group termination. Subsequent waiters
recover locks whose nested owner was terminated.
POSIX cleanup confirms that the managed group has no runnable descendants after
escalation; unreaped zombies do not count as running work. Dead nested owners
cannot release a lease while their group still runs. If process-state inspection
is unavailable or Windows `taskkill /T /F` fails, termination is unverified:
the cancelled attempt produces no successful receipt and its locks remain
quarantined. Inspect the attempt log and the lock's `owner.json`, establish that
all descendants have stopped, and remove only those quarantined lock directories
to recover. Automatic dead-owner recovery never clears quarantine. A Windows
taskkill success is OS process-tree evidence, not a native UI acceptance test.
It relies on the OS command's result; it does not independently enumerate and
verify every terminated Windows descendant.
Tool versions and mutable configuration digests are rechecked after queueing,
before reuse and after execution; changes invalidate evidence rather than
publishing success under the earlier identity.
Tracked input and configuration inode/time metadata is sampled through
finalization to detect ordinary edits restored before the final content snapshot.
These are trusted local accelerators, not isolation from hostile editors or
unobserved transient creation/removal of ignored inputs; stop editing while
validating. Publication still requires committed
platform archives and fresh protected CI. Log-write failures take the same
descendant-cleanup path as cancellation. Reuse reports avoided historical
execution time; its legacy `savedExecutionMs` field is not a measured wall-clock
speedup.
Console output reports queue time, execution time, reuse, and running-stage
heartbeats. Platform preflight reports each platform's completion immediately and
prints progress while the other continues. Missing/corrupted evidence is a cache
miss; it is never a successful verification result.
On macOS, preparation holds a child-scoped `caffeinate -is` assertion through both
platform preflight and full health. It ends with preparation and changes no power
settings; browser timeouts must not be masked by a retry after idle sleep.

## CI ownership and browser sharding

For decisions about adding, reusing, splitting or retiring CI work, follow
[CI maintenance](ci-maintenance.md). This section owns the current execution
arrangement; the measurement section below owns timing commands and interpretation.

The standalone `scripts/check-health.sh` runs the full local gate when comprehensive
local verification is requested. Hosted workflows divide the responsibilities so that
independent checks run concurrently and heavy suites are not repeated by health.

| Owner                                 | Checks                                                                                                                                                                                                                                                                |
| ------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `check-health.yml` platform jobs      | Complete frontend units, help and diagnostic/Python regressions on Linux/macOS/Windows; unit coverage, formatting, ESLint, source/E2E TypeScript, npm/help advisory policy and eval smoke once on Linux; same-run UI coverage line map                                |
| `check-health.yml` source job         | Rust formatting, typos, cargo-deny, cargo-machete, shellcheck and actionlint on Linux                                                                                                                                                                                 |
| `ci.yml`                              | Frontend builds, built-JS compatibility, Clippy `--all-targets`, explicit workspace builds and Rust tests on all three platforms; native-platform Rust coverage; Linux multi-process coverage and exact scenario evidence, ignored bridge and large-folder acceptance |
| `e2e-ui.yml` and `ui-shards.yml`      | Full Chromium regression on Linux/macOS/Windows, independent Linux coverage, optional-pack checks, portable evidence from full shards, dedicated WebKit/Firefox core and scheduled extended scenarios                                                                 |
| Independent security/native workflows | Full-history Gitleaks, dependency review, CodeQL, mutation/sanitizer checks, backend process rigs and native desktop evaluation                                                                                                                                       |

Health is called by the UI workflow for every PR base and merge candidate, with
its own weekly/manual entry points. Direct source and platform checks are required
individually; no new VM is allocated merely to copy their aggregate result. The
replacement policy and source-release consumers share
`scripts/ci/required-checks.mjs`, including current-source ASan and mutation-runner
acceptance for release; migrate administrator branch protection only
after current-source replacement checks pass, as described in
[quality gates](quality-gates.md). CI retains its weekly Rust run; UI runs full
browser matrices daily. Dispatch CI and UI when complete hosted verification is
needed. A manual Health dispatch runs its portable/static responsibilities.
The protected `Full Linux UI regression` name remains the coverage merge job,
which rejects failed, skipped or incomplete dependencies.

Portable validation reuses Health's existing Node, Python and frontend dependency
setup. Native CI no longer waits for frontend unit tests, help audits or diagnostic
regressions before starting Clippy and workspace builds. Health uploads frontend
coverage from Linux; macOS and Windows run the same complete Vitest inventory
without collecting unused duplicate reports. Linux still enforces the configured
coverage thresholds and supplies the same-run browser line map. Test deadlines
and assertions are identical in both modes. CI uploads Rust coverage from each
platform. Both workflows remain protected, including strict
coverage upload failures. Native CI has a 90-minute job budget; Health portable
jobs have a 25-minute budget. These are execution bounds, not latency promises.
Compare hosted queue, execution and all-required-check completion after deployment.

Full Chromium tests use four Playwright shards per OS, with one worker and zero
retries per runner. This keeps the existing per-machine rendering budget while
running all scenarios. Portable/native scenarios and snapshot assertions remain
in place. Sharding trades more concurrent runner slots and setup work for shorter
elapsed time; account concurrency limits can reduce the gain.

Disposable GitHub-hosted Ubuntu CI, mutation, browser and native UI runners
prepare their APT sources before dependency installation.
`scripts/ci/ubuntu-apt-source.mjs` replaces only the exact Azure Ubuntu archive URI
with the official HTTPS Ubuntu archive, including URI entries in the referenced
`/etc/apt/apt-mirrors.txt` runner mirror list. It preserves suites, components,
signing keys, security repositories, other origins and mirror priorities. The
helper rejects other host environments, unsupported Azure URIs, unrecognized
mirror-list references and symlinks. It validates all candidate files before
writing and preserves file ownership and permissions. This avoids a slow
runner-image mirror consuming the existing job budget; browser workers, retries,
test inventories and deadlines are unchanged.

Windows full-regression runners expand the IPv4 TCP client port pool to
10000–65534 before browser execution. Repeated fresh contexts load Vite's module
graph over loopback; the default 16K pool can exhaust while closed connections
remain in `TIME_WAIT`, producing `ERR_NO_BUFFER_SPACE` or a blank login page.
The pool excludes Vite's port 5173. Failed Windows jobs print the configured
range and socket-state counts; retain those diagnostics when investigating
resource failures rather than increasing assertion timeouts or enabling retries.

Each Linux shard uploads raw browser coverage plus its source SHA and shard
identity. A single same-workflow Linux unit-coverage producer supplies the line map to
the full and optional-pack consumers. Consumers check source SHA, workflow/run/attempt,
platform/architecture, Node major and LCOV digest before restoring it. The
coverage job requires that producer and all four distinct current-source shards before collecting their parts and producing
one browser LCOV upload. It prefixes filenames so reports from different runners
cannot overwrite each other and clears old output only after validating every
input. Unit coverage artifacts are attempt-bound: use **Re-run all jobs** for this
workflow when retrying a failed attempt, so producer and shards belong to the
same attempt. Health's existing Linux job produces this artifact; no additional
unit-test runner is allocated. Include artifact transfer and consumer validation
costs when measuring the benefit of reuse.
The final UI gate requires successful browser, coverage, optional-pack and
portable jobs and Firefox core (or the complete extended Firefox inventory when selected).
HTML report rendering runs independently
and is not a merge acceptance dependency; each test shard still requires its raw
blob/trace evidence upload to succeed. Blob reports and failure
traces are retained per OS/shard/attempt, and an HTML report is merged per OS even
when browser execution fails.

Successful shard uploads contain the raw Playwright blob (including its
attachments) and structured JSON results, without a second copy of
`test-results/`. Failed or interrupted UI execution additionally retains raw
attachments under `ui-failure-attachments-*`, so incomplete blobs do not discard
available diagnostics. Portable evidence extraction and its verified collector
remain separate consumers. The actual Playwright round-trip in
`scripts/diagnostics/playwright-artifacts.test.mjs` removes raw output before
rebuilding JSON/HTML and verifies passed and failed case attachments byte for byte.

The Rust build cache maps the workspace root to `target/`, matching
`cargo metadata --no-deps --format-version 1`. The CI cache has its own build
configuration key; native release, mutation and sanitizer caches stay separate.
Cached build output never replaces executing checks on the current revision.
Verification jobs restore caches on all selected events, while only `main` writes
Rust caches. This avoids accumulating PR-private compiled copies that other PRs
cannot reuse. A PR changing dependencies may rebuild them until its lockfile lands
on main; normal main checks refresh the shared baseline. Existing caches are not
deleted by this policy. See [GitHub cache scope](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching#restrictions-for-accessing-a-cache).

The protected `mutants` check verifies the pinned runner on PRs, merge groups
and main pushes using real tiny-crate caught, survivor, baseline-failure, hanging
and empty-selection scenarios. Broad workspace mutation sampling is daily/manual
assurance, outside merge acceptance; it is not claimed as PR mutation coverage.
The gate requires runner-contract success on every event. It requires complete
successful deep shards on daily/manual runs and an explicitly skipped deep job
on other events; unexpected missing, cancelled or failed work never passes.
Daily failures remain visible in Actions and require owner triage before the next
scheduled run. Dispatch Mutation Testing after a critical invariant change when
immediate deep evidence is needed. This policy keeps all ordinary platform,
security, unit, integration and coverage acceptance gates in place.

Mutation runners list their exact rotating/diff selection before installing native
system libraries, restoring compiled caches or building the frontend. An empty
selection skips those execution costs; listing errors fail the runner. Nonempty
selections still execute their own unmodified baseline and strict outcome validation.
The preflight does not narrow the sample or certify any test result. Pinned-tool
smoke tests cover empty and nonempty selections without compiling a failing fixture.

Daily/manual mutation testing retains one rotating workspace sample across four runners and
four isolated test processes per runner. The 180-minute job budget covers setup,
the baseline and the observed sample of up to 17 mutants. Each complete workspace
suite has a 360-second bound; individual tests retain the 60-second hang detector
and five-second termination grace. Failed or interrupted reports still fail the gate.

Run 40922461468 exposed a budget mismatch: the package baseline ran 2,228 tests
and took 223 seconds on the slower runner, while every mutation ran 2,829 tests.
The baseline core work plus observed host work already exceeds a 240-second
four-process budget, even before the remaining workspace tests.
At cancellation, the active tests were only milliseconds to ten seconds old;
none had reached the individual hang limit. Other shards completed the same full
inventory in 183–209 seconds, confirming runner variance rather than a universal
hang. The six-minute suite budget includes the larger inventory and variance;
it does not reclassify those interrupted runs as successes or change test scope.
Reassess budgets using actual commands, inventories and build/test timings as the
workspace grows, rather than comparing a package baseline with a workspace run.

The pinned cargo-mutants 27.1.0 baseline selects only packages containing the
selected mutations, even with `--workspace --test-workspace true`; individual
mutations still run the complete workspace. Record both actual commands and test
inventories. A package-only baseline is not a measured workspace baseline, and
its wall time must not be compared directly with a larger mutation run. Acceptance
also requires the separate complete workspace CI gate; no mutation baseline is
skipped or reported as broader than it ran.

The mutation runner explicitly sets `CARGO_INCREMENTAL=1` for both tool child
processes because rust-cache exports `0` through the job environment. The real-tool
smoke regression starts from that disabled parent environment and checks actual
Rust incremental query-cache output. The existing fifteen-second UDP gateway
proof-withdrawal fixture has higher scheduling priority only in the mutation
profile, so its three refreshes can overlap other tests. The in-memory stalled
policy-write fixture completes its original Noise handshake before pausing Tokio
time, proves the real send owns the guard, and checks it remains pending at
9.999 seconds, then the unchanged ten-second production timer must complete before
the original twelve-second outer bound and release that guard. Paused time
auto-drives the awaited timer, including Tokio's millisecond tick rounding. No real
UDP clock is accelerated. Test inventory, four
processes and the sixty-second per-test limit remain unchanged; the complete
workspace uses the 360-second suite budget described above.
Scheduling estimates are not completed-run evidence; validate the full unchanged
workspace inventory on the current candidate before claiming a timing improvement.

The real Cargo retention fixture compiles its disarmed build script before
starting the lock probe. An explicit `rerun-if-env-changed` input then arms that
compiled script inside a new Cargo process, preserving actual Cargo/cleanup lock
interoperability in both directions. Cold compilation has a separate five-minute
setup bound; probe readiness and reverse lock contention each use a fifteen-second
bound. No cleanup assertion is skipped when Windows linking is slow.

Loopback UDP reply fixtures connect their receiving socket to their own listener
so concurrent discovery traffic cannot masquerade as a reply. Tests must still
reject extra replies from that listener after a refresh.

Nickname publication fixtures seed the existing signed account profile alongside
the account key before starting a sibling, then install reciprocal device certificates. A key-only clone creates a new local
profile and can race the intended edit with a newer host-name card. Assert this
starting state explicitly; nickname updates retain their existing deadlines and
preservation checks. Successful outbound profile reconciliation must project any
newer sibling events it pulls before marking publication complete. The loopback
regression `publishing_profile_projects_newer_sibling_profile_pulled_in_same_round`
verifies durable event import and immediate signed profile/avatar projection
without a reverse connection or background runtime masking the result.

Registered desktop IPC fixtures wait for their signed peer to appear and repeat
its UDP announcement within the existing setup deadline. A successful UDP send is
not receipt evidence; a lost first datagram must not fail an unrelated privacy or
session test before its assertions run. The dropped-first-announcement regression
in `owner_commands::tests` exercises this setup path. Shared-fixture changes keep
the existing `desktop-session` selector over all owner-command tests and require
their consumers to run; they do not change production discovery guarantees.

To assess performance after deployment, compare at least several unchanged-scope
runs with warm caches and a cold-cache run. Record time from PR update to all
required checks passing, queue delay, slowest browser shard, native build/test
steps, total runner minutes, and first-attempt failures. The pre-change October 17
sample had 27–38 minute health jobs and a 31.5 minute full Linux UI job. These are
baseline observations, not promised speedups; hosted runs must establish the new
latency and reliability.

### Windows workspace archives and slow fixtures

Ordinary workspace and backend CI use Cargo's `line-tables-only` debug information
for the built-in dev/test profiles. This retains application stack file/line
locations while avoiding type/variable debug data. Local development defaults,
optimization, assertions, overflow checks, production KDFs, and the independently
owned sanitizer, mutation and release profiles remain unchanged. The actual-tool
`scripts/ci/rust-profile.test.mjs` compares source-line coverage against full debug
information and checks panic diagnostics, debug assertions and overflow handling;
its no-debug negative control distinguishes stack locations from panic headers.
See [Cargo debug profiles](https://doc.rust-lang.org/cargo/reference/profiles.html#debug).

The Windows producer runs structural checks, frontend compilation, Clippy,
an ordinary workspace build and separate doctests. It builds the instrumented
workspace test binaries once with pinned cargo-nextest 0.9.148 and
cargo-llvm-cov 0.8.7, then publishes one immutable archive. Two fresh consumers
extract that archive and run independent hash partitions. Each consumer runs
one test at a time without retries; network/disk fixtures do not compete with
another test on that machine. Nextest uses a fresh process per test, so tests
must not depend on another test's process-local state.
The producer retains the former Windows workspace cache namespace through an
explicit shared key. Only the cache step normalizes the inherited color/thread
environment to its previous key inputs; actual tests keep serial execution and
plain diagnostic output. Pull requests restore caches; only main publishes them.

The owning `scripts/ci/windows-coverage.mjs` helper validates every default Cargo test target, the full inventory,
default ignored markers and the exact disjoint union of both runnable partitions.
Producer and consumers must match source/run/attempt, compiler, Cargo, dev/test
profile environment, feature
resolution, lock/config/helper hashes, platform and the original absolute checkout
root. The archive digest is verified before extraction. Fixed paths preserve
compiled `CARGO_BIN_EXE` and manifest references. Source is checked out at the
workflow SHA even if main advances during execution. Unqualified build-directory
layouts fail explicitly; see the helper's upstream compatibility reference.

Consumers have no dependency cache, frontend rebuild or test compilation. Reused
official binary/Cargo metadata and offline reporting must work from a fresh Cargo
home. Execution receipts verify each selected test starts and passes exactly
once; missing, duplicate, failed or skipped selected cases fail the check.
Each consumer validates and uploads its partial LCOV plus source/inventory/digest
receipts. Both consumer checks and the producer are directly required; a skipped
consumer after a failed build cannot certify acceptance. Diagnostic timing keeps
machine-readable events separate from successful-test phase output.

The producer executes a tiny real archive/consumer CLI regression before expensive
workspace compilation or accepting the full archive. Its local Git fixture, both partitions, runtime and compiled
helper paths, default ignored case, clean source, offline LCOV and captured timing
exercise the same helper. Run its tests with the pinned nextest/coverage tools and
LLVM tools matching rustc; portable Health does not install those tools.

Linux CI produces both strict backend scenario evidence and instrumented
coverage in the same execution, including the separate ignored bridge and
large-folder acceptance. macOS and Windows keep their real backend executions.
Accumulation uses `--no-report` between executions; combining it with `--no-clean`
is invalid. The real CLI regression first writes a normal unit report, executes
ignored scenarios and the workflow's exact filtered large-folder integration command,
then verifies the final report retains distinct coverage from all three executions.
Its second prefix-matching integration case must remain unexecuted. Positional test-name filtering
uses the explicit `cargo llvm-cov test` subcommand. Production builds,
native desktop acceptance and sanitizer checks remain distinct.
All three large-folder gates validate the exact successful scenario and Cargo
summary from saved stdout and stderr, preserving the Cargo failure through `tee`.
A renamed filter that selects zero tests cannot silently pass. The dedicated
`--test large_directory_enqueue` target uses `--locked` and exact ignored-case
selection, avoiding compilation of the entire inline library test harness for
this separate acceptance step. Its real 110 MiB fixture decrypts the exact returned
event from the persisted log after dropping the Node writer and requires a V4
manifest with matching file identity, directory metadata and more than 2,000
chunks. It proves enqueue and staging; recipient transfer retains separate backend
coverage. The small compact-manifest policy case remains in the ordinary unit suite,
while the disk fixture stays an explicit escalation in the owning validation contract.

Measure fixture preparation, protocol phases and teardown before changing a
network deadline. HTTP expiry regressions establish real TCP admission and observe
shutdown readiness before advancing Tokio's unchanged 30-second production clock.
They retain structured timeout responses and connection/handler slot release.
Search limit regressions
prepare most events through the real signing/persistence/indexing owner and keep
registered send/search wiring. Delivery timing spans distinguish card preparation
from restart fairness and transfer completion. These changes must retain timeout,
resource-release, identity and immutable-byte assertions.

Control scheduling fixtures bind each authenticated request nonce to its exact
client task, immutable dispatch attributes and a non-owning lifetime witness.
Live hold gates reject completed or cancelled tasks; the latest peer-level trace
is diagnostic history, not evidence that a particular request is still active.
Check the intended held task again inside the operation-admission callback, then
retain the scenario's permitted preemption, continuation and receipt assertions.
Natural origin, protection and wake hints remain independent attributes.
Lifetime observation is scoped to synchronous test hooks and expires on return,
abort or panic without calling fixture code during destruction. The delayed-server
regression uses a real signed sibling publication to retire an older request and
requires a fresh live request before measuring the existing receipt deadline.

## Measure feedback before tuning capacity

For hosted CI, run `node scripts/ci/timing-report.mjs OWNER/REPO COMMIT_SHA --history 30`, or add `--json` to preserve the raw API snapshot
in ignored output. It reads all current-attempt jobs and current-source checks
without triggering or changing runs. Runner minutes sum known execution, not
billing. Job creation-to-start intervals may include upstream dependencies and
must not be labeled exact runnable queue time. The latest completing job is an
observed tail, not a reconstructed critical path. Historical P50/P95 describe
bounded API run spans for first attempts of the same workflow/event; configuration
and runner changes are uncontrolled. Incomplete snapshots and missing durations
remain explicit, and timings never establish acceptance.

Use the measurement entry point with local attempt history, an offline snapshot,
or a read-only GitHub sample:

```bash
make validation-report
make validation-report VALIDATION_ARGS="--repo OctopusGarage/mesh-talk --limit 20 --json --output tmp/validation-latency.json"
make validation-report VALIDATION_ARGS="--input tmp/validation-latency.json"
```

The JSON output retains its raw snapshot for replay. Reports separate queue and
execution time, named jobs and OSes, setup/compilation/test steps, cancellations,
unfinished work, reuse and measured runner-minutes. Quantiles use nearest rank;
small samples are descriptive, not stable p95 estimates. Queue can include job
dependency waits; step categories use names. Workflow elapsed and execution-span
are observed envelopes, not a reconstructed dependency DAG or a sum of jobs.
Legacy receipts contain successful executions only and are labeled accordingly.
Failures and cancellation require attempt history; absent records mean unknown.
Source/dirty labels describe provenance, not reusable verification proof.

Local reports also read `target/validation/feedback/` for observed development
feedback. JSON snapshots retain those source reports for replay. An offline
`--input` or GitHub `--repo` sample does not silently append local evidence;
`--feedback-dir <directory>` explicitly adds it. Only root development reports
are collected, not command-progress files or nested stage results. If a live
append revisits the same report path and invocation start, the latest observation
replaces that invocation; later retries remain separate samples.

The feedback section reports the first observed command result, first actionable
issue, overall completion and selected-scope passed completion, with outcome and
platform groups. A command result may be compilation, test inventory or a cached
stage wrapper; it is not proof of business correctness. Reused phase evidence
produces no fabricated command result. Issues retain their `failed`, `unavailable`,
`invalidated` or `blocked` classification; cancellation alone is not a test
failure and cannot erase an earlier observed failure. These observations cannot
identify flaky tests, regressions or escaped defects without separate evidence.

New reports time from CLI planning/execution entry after argument parsing;
`timingOrigin` identifies that boundary. Older reports may start after planning
and are labelled `legacy-report-start`. Missing, backwards or out-of-envelope
timestamps remain unknown; unfinished reports can expose an issue while completion
stays unknown. Compare equivalent selected scopes, modes, platforms and timing
origins before claiming an improvement. `passedCompletion` covers selected local
validation, with browser/native and candidate acceptance still pending.

Compare representative docs, frontend, integration, core and dependency changes,
including cold/warm caches. Record first useful failure, full-green wall time,
queue, compile, fixture/test execution, cache restore/upload and rerun cost.
Benchmark shard count against actual capacity before adding concurrency. Keep
local resource limits and network-test isolation unless measured evidence supports
a different budget. Native production/driver builds and coverage instrumentation
have distinct inputs; do not treat their outputs as interchangeable. Cargo
`--timings` identifies compilation bottlenecks before another crate or a compiler
cache is introduced. nextest can improve execution scheduling but not compilation;
sccache trades away rustc incremental support and cannot cache every link target.

Check eviction as well as cache hits. [Local build retention](build-retention.md)
keeps recently used compiler caches during automatic post-command cleanup, with
a bounded idle window, so an over-budget machine does not immediately discard a
fresh build after each task. Explicit/scheduled cleanup retains its budget policy;
measure available capacity and active/preserved outputs before changing the budget.

### Locate slow serial Rust tests

For an existing timestamped Windows Cargo log whose command/environment confirms
`--test-threads=1` or `RUST_TEST_THREADS=1`, inspect completion gaps without
changing the runner or recompiling:

```bash
# Download only the Windows test-step log, or retain its original timestamped log.
gh run view RUN_ID --job WINDOWS_JOB_ID --log > tmp/windows-test.log
make test-timing VALIDATION_ARGS="--input tmp/windows-test.log --serial --top 20"
```

`--json` emits test/binary/module context, passed/failed/ignored/unfinished states,
suite totals and diagnostics. The serial assertion is mandatory: concurrent
completion gaps cannot identify per-test duration. Estimates include harness and
reporting overhead plus timestamp buffering; missing/backwards timestamps stay
unknown. Compilation/binary/suite transitions reset timing anchors. Compare sums
with the suite's declared runtime, then repeat a specific compiled executable's
`--exact <name> --test-threads=1` run for a controlled wall-time measurement.
Rust stable's JSON output and `--report-time` are still [unstable libtest
options](https://doc.rust-lang.org/rustc/tests/); the offline profiler does not
enable nightly features or change acceptance.

Optimize the wait being proved. A federation test that needs unavailable direct
clients can use an endpoint that accepts then closes before authentication,
avoiding OS-specific closed-port refusal delays while preserving relay/consent
and complete-byte assertions. Keep actual socket/refusal acceptance in tests
whose purpose is connection failure. For purely in-memory Tokio timeout tests,
consider paused virtual time only after checking every clock source; mixed
`std::time::Instant`/Tokio timers cannot be paused indiscriminately. Replace a
reconciliation sleep with an observed completed round when that round is the
actual invariant. Preserve real loopback, restart, expiry and concurrency budgets;
neither reducing delays nor increasing Windows workers is a default speed fix.

The slow-route file scheduler fixture acknowledges TCP acceptance before holding
the handshake for 600 ms and rejects a worker that finishes during that hold.
Its original 15-second Dial budget includes the hold; each subsequent ordered
phase shares one existing exchange budget across all of its turns. Unexpected
phases, redials, missing workers and server failures fail explicitly. Delivery
confirmation and exact received bytes remain required. A negative control that
cancels the actual Dial worker at 400 ms must fail the held-handshake assertion;
a whole-transfer wall-clock limit is not a substitute for that regression oracle.

Patch line coverage follows the frontend unit-coverage exclusions: registered
`.test.ts` files and `.d.ts` declarations do not enter its production denominator.
A test/declaration-only or deletion-only diff reports `not applicable`, without a
coverage percentage. Added production lines with no matching measured lines in
any supplied report still fail; the existing minimum remains required for measured
production changes. This scope decision does not establish that tests executed.

## Qualify affected selection alongside full gates

Generate a clean committed candidate's explainable plan against the intended
base, then compare it with normalized broader failure evidence for that exact SHA:

```bash
node scripts/validation/changed.mjs --base origin/main --json > tmp/affected-plan.json
make validation-audit VALIDATION_ARGS="--plan tmp/affected-plan.json --results tmp/broader-results.json --json"
```

The supplying test-report adapter owns this normalized results shape:

```json
{
  "source": "0123456789abcdef0123456789abcdef01234567",
  "complete": true,
  "suites": ["document the actual broader scope here"],
  "failures": [
    { "kind": "rust", "package": "mesh-talk-core", "test": "node::example" },
    { "kind": "frontend", "path": "src/store/chat.test.ts" },
    { "kind": "node", "path": "scripts/diagnostics/affected-plan.test.mjs" },
    { "kind": "native", "test": "describe the failed native scenario" }
  ]
}
```

Rust entries refer to library tests selected by this runner; integration/process
failures require a scenario/native review entry rather than assuming a matching
library name proves execution. Complete/source/suite fields are assertions made
by the adapter; the comparison tool does not collect or authenticate full-suite
execution. It rejects dirty, blocked, incomplete or mismatched-source samples,
reports selected failures and missed applicable failures, and keeps browser/native
failures as separate required review. Exit 0 means no misses observed in that
finite sample, 1 identifies a missed regression, and 2 means unqualified, invalid
or scenario review required. No result removes current full gates. Investigate
misses and strengthen the owning contract; a green sample is not proof that
future selection is sound.

## Merge queues and required merge checks

Required Actions workflows accept `merge_group: checks_requested`, including
actual base/head dependency review. This prepares them for a queue without
changing strict branch protection or enabling a queue. Check GitHub plan support,
required external CodeQL/Codecov statuses, candidate SHA/evidence semantics and
actual hosted completion before activation. Workflows cancel superseded candidate
work where safe; publication/release operations retain their own policies.

Protected platform/browser/native and health checks remain required before merge.
Fast PR publication does not change workflow triggers, required checks or branch
protection. Queueing protects combined changes against the latest base; it can
add verification work and does not itself make tests faster. Keep agent tasks
small, identify owner/acceptance commands before edits, and validate the combined
diff after integration.

## Fast PR creation and optional full local preparation

Commit the candidate and use the repository's PR entry point:

```bash
./scripts/create-pr.sh -- --draft --fill
```

The default checks a clean committed candidate, the comparison base, safe PR
arguments, whitespace, structural standards and repository hygiene. It does not
start Docker, SSH, browser suites or full health. Required hosted checks run
after publication and must all pass before merge. Local results do not replace
those checks.

To finish comprehensive local validation before creating a PR, opt in:

```bash
./scripts/create-pr.sh --full-preflight \
  --core-filter node::delivery_runtime_tests \
  --host-filter owner_scoped_node_handle \
  e2e/ui-visual-regression.spec.ts \
  -- --draft --fill
```

`--full-preflight` belongs before `--` and invokes `prepare-pr.sh`; its full
health and platform checks remain unchanged. The following selector and
platform requirements apply when running full local preparation.

Arguments before `--` select tests; arguments after it go to `gh pr create`.
Use filters and browser specs relevant to your change. Core, host, integration and MCP filters can be repeated. Use
`--integration-filter policy::` and `--mcp-filter tests::` for the two integration
crates; a changed Rust crate requires its corresponding filter. Shared Cargo,
toolchain or Clippy configuration requires selections for all four crates.
Examples, benchmarks, package configuration and Tauri build inputs also select
their owning package. Each selected guest package runs Clippy with
`--all-targets -- -D warnings` before its focused tests. Use `--core-test decoder_smoke` to select a nonignored core integration
target instead of a library filter. A selector matching zero tests fails rather than producing misleading
evidence. This selects unit tests and explicitly named browser scenarios; it
does not automatically launch the full workspace, ignored process rigs, or
the full browser suite.

Changes to the platform runner, PR entry point or source-evidence helpers also
select the fixed Node workflow regression set on both guests. This catches
Git, shell and path compatibility independently of application tests; a
workflow-only diagnostic plan does not install frontend dependencies.
Verification archives suppress export substitutions and text/filter conversions
in private Git metadata so the archived bytes match the committed source.

Frontend changes require affected browser specs. Owning `prScenarios` rules in
`validation.json` add required visual specs automatically, so an unrelated
explicit spec cannot replace shell/layout/theme evidence. The plan preview
shows the selected scenario owners and required native platforms. Renames
include both old and new paths. Frontend unit coverage is a shared local stage; the two platform guests do not repeat
the whole frontend unit suite. To target another PR base, pass
`--base origin/release` before `--`; the wrapper uses that same base for
change selection, coverage and `gh pr create --base release`. A local revision
such as `HEAD` cannot stand in for a PR target branch.

`scripts/pr-preflight.sh` automatically sends the same test plan to Linux
Docker and Windows over SSH after its existing local static, coverage and
portable browser checks. Cheap local failures therefore block remote builds
before they consume time. Both platforms test an archive of committed HEAD. Workspaces are
isolated from installed applications and post-office services. Both results
are collected even if one fails. Logs and revision-bound reports remain
untracked under `target/pr-platform/`; dependency and build caches can be
reused, but successful verdicts are never reused for another revision. Required
remote execution is never replaced by local receipts.
On macOS the preflight holds a temporary `caffeinate -is` assertion until it
exits, preventing idle/system sleep on AC without changing power settings or
keeping the display lit. This avoids Docker timer failures during maintenance
sleep. Only one local preflight should run at a time. Avoid overlapping Docker
compilation and browser workloads with the timing-sensitive network fixtures.
Windows reuses an installed Cargo home only when its resolved physical
directory is on drive D, including a C: junction that points to D. This
preserves that directory's registry and network configuration without logging
its contents. Otherwise, Cargo uses the isolated D: cache. Source, build
output, browser downloads and temporary directories remain isolated.

For an existing PR, `scripts/prepare-pr.sh` provides optional comprehensive local
validation before pushing a candidate. Use `scripts/pr-preflight.sh` alone for
focused platform investigation; it does not certify full candidate acceptance.
After a broader run has produced evidence, a focused follow-up can
use `--base <previous-candidate>` with the tests affected by the delta. Retain
the earlier reports and record both revisions, the comparison base, remaining
failures and exact test scope in the PR. Shared fixture changes require all
cases that use that fixture. An incremental pass is not whole-branch
certification; final required CI still runs on the complete current PR head.
The new-PR wrapper always uses the actual `origin/<target>` base.

Preview the plan without starting Docker or SSH:

```bash
node scripts/pr-platform/run.mjs --plan \
  --core-filter node::delivery_runtime_tests \
  --host-filter owner_scoped_node_handle \
  e2e/ui-visual-regression.spec.ts
```

| Setting                            | Default / purpose                                                                                                           |
| ---------------------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| `MESH_PR_WINDOWS_HOST`             | `windows10`; an SSH host with noninteractive access, Rust, Node and Git tools available                                     |
| `MESH_PR_WINDOWS_ROOT`             | `D:/MeshTalk/PRPreflight`; Windows source, caches, browser downloads and temporary data must stay on drive D                |
| `MESH_PR_LINUX_HOST`               | Optional SSH host, such as `mac2015`, to run Docker remotely; unset keeps local Docker.                                     |
| `MESH_PR_LINUX_PLATFORM`           | Optional `linux/amd64` or `linux/arm64`; remote Docker defaults to `linux/amd64` and rejects mismatched guest architecture. |
| `MESH_PR_LINUX_DOCKER`             | Remote Docker executable; defaults to `/Applications/Docker.app/Contents/Resources/bin/docker`.                             |
| `MESH_PR_LINUX_ROOT`               | Remote temporary source/evidence root; defaults to `/tmp/mesh-talk-pr-preflight`.                                           |
| `MESH_PR_DOCKER_IMAGE`             | Optional prepared image override; the default image is built from `scripts/pr-platform/Dockerfile`                          |
| `MESH_PR_PLATFORM_TIMEOUT_SECONDS` | `1800`; platform execution limit, not a protocol timeout                                                                    |
| `PATCH_COVERAGE_BASE` / `--base`   | `origin/main`; the comparison base for changed-file planning                                                                |

To run native Linux amd64 containers on the Intel Mac while preparing a PR:

```bash
MESH_PR_LINUX_HOST=mac2015 MESH_PR_LINUX_PLATFORM=linux/amd64 \
  ./scripts/create-pr.sh --full-preflight --core-filter discovery:: --host-filter discovery \
  e2e/settings-recovery.spec.ts -- --title "Your PR title" --body-file /tmp/pr-body.md
```

The remote adapter uploads the same committed source archive, plan and Dockerfile
used by local Docker. Mounts and named caches belong to the remote Docker host;
local filesystem paths are never passed as remote bind mounts. Selected test
containers enable IPv6 loopback so routed discovery tests exercise `::1` even
when Docker's bridge network has no IPv6 subnet. This is a container setting;
the Docker daemon's network configuration stays unchanged. Test
reports and logs are copied back before the source revision and archive digest
are validated. Remote run directories remain under the configured temporary root
for diagnosis; named caches are reusable. Docker's executable directory is added
to the remote command's PATH so GUI-installed credential helpers work over SSH.
SSH uses configured host keys and noninteractive authentication, as for Windows.

Local PR browser traces, screenshots and the HTML report use a unique directory
under `target/pr-preflight-playwright/<commit>-<process>/`. They do not share
Playwright's default `frontend/test-results` or `frontend/playwright-report`
directories with the full health gate, so starting a preflight cannot delete
another active run's traces.

Required environments being unavailable blocks preparation; it is not a pass.
Fix the environment or report the blocker. Documentation-only changes without
explicit selectors report platform tests as not applicable. Container reports state the actual
architecture: Linux ARM64 on an Apple Silicon Mac does not replace Linux
x86-64 sanitizer coverage. Browser checks exercise the real frontend with its
test IPC fixture. Native webview, desktop input, multicast across real hosts,
and ASan remain separate CI or explicitly requested acceptance gates.

The PR creation wrapper also checks that HEAD and the working tree did not
change during verification. Alternate head/repository options and `GH_REPO`
overrides are rejected. Use it instead of invoking `gh pr create` directly
so the platform checks are part of the normal workflow. GitHub branch
protection remains the final enforcement for CI; a local script cannot
intercept arbitrary GitHub CLI or website actions.

The current four-crate Rust workspace separates protocol logic from
the Tauri host. Within the core, `node/node.rs` still coordinates many domain
modules; keep moving bounded parsing and state transitions behind narrow
module interfaces so `--lib <module>` reaches them. In the host, ownership
validation belongs to `NodeState`; commands should use its owner-scoped handle
instead of repeating runtime locks and session comparisons. In the frontend,
the Settings dialog's session controller and store tests provide narrow targets;
continue extracting field-specific state transitions from the presentation
component as those fields change. Prefer tests at those interfaces over extra
process/browser cases for pure state rules.

For local speed, keep the existing Cargo build cache and the test-only cheap
KDF parameters. Avoid invoking Clippy, full Rust tests, coverage, and browser
tests after every small edit; run the focused test during the loop, then the
appropriate static check and final gates at the triggers above. Do not change
the CI commands or the explicit `make test`, `make e2e`, and health-check
entries: they provide the comprehensive evidence for review and release.

## Native requirements and publication evidence

The owning core and desktop sidecars require native acceptance for discovery,
process/session lifecycle and OS-specific UI boundaries. `host` resolves to the
preparing OS. Additional `darwin`, `win32` or `linux` entries require evidence
from each named platform; one platform cannot stand in for another. These rules
are maintained acceptance boundaries, not exhaustive dependency inference.
Review escalation notes and add owning rules when new runtime boundaries appear.

Run the existing native runner from an unchanged, clean committed checkout with
an unlocked desktop. Build its current binaries first:

```bash
cd frontend && npm ci && cd ..
node scripts/validation/native-build.mjs
node scripts/diagnostics/hidden-contacts.mjs
MESH_PR_NATIVE_REPORTS='["target/hidden-contacts-native/report.json"]' \
  ./scripts/prepare-pr.sh --host-filter owner_scoped_node_handle
```

Linux needs the display/session prerequisites documented in
[platform validation](platform-validation.md) and the native CI workflow. The
current native harness runs its complete acceptance scenario set; there is no
claim that it supports per-scenario filtering. It runs only when the selected
boundary requires it. Keep reports and all referenced JSON, PNG and log files
together; paths are supplied as a JSON array to preserve spaces. Preparation
revalidates real native observations, artifact hashes, platform, source SHA and
the runner's clean-source observation and verified build provenance.
The build helper pins the committed source and hashes the frontend assets and
actual binaries. A clean checkout alone cannot certify previously built binaries. Cargo executable
records must match the hashed paths; redirected `build.target` output is rejected
rather than allowing old default-path binaries to be stamped as current. Old reports lacking that observation
must be regenerated. Missing environments or evidence block preparation before
heavy health/platform work; they never become a pass. Direct platform execution
also verifies native requirements; `--plan` only previews them.

Preparation runs the outer affected platform preflight before the comprehensive
health gate, catching selected platform failures sooner. It forces full health
mode even when `FAST=1` is inherited. Cheap formatting, ESLint and TypeScript checks run
before diagnostic suites and native compilation. Existing successful stages can
be reused under their source/tool/configuration and artifact checks; full health
and platform evidence remain distinct. HEAD, working tree and comparison ref
must remain stable. Hosted CI still verifies its complete matrices and external
security policy; local success does not certify every hosted environment.

For parallel agent work, one integration owner collects changes, reviews shared
interfaces, prepares the combined candidate and pushes it. Individual agents
commit locally and report focused tests. After a CI failure, reproduce the
failure and run affected neighboring checks before publishing a coherent fix.
Do not push each intermediate attempt. Keep failure evidence in untracked output
or PR artifacts, not historical reports in the repository.

Measure first-attempt PR success, corrective pushes per PR, time to the first
actionable failure and runner minutes per merged PR. Classify failures as code,
scenario/fixture, platform/environment or external service before deciding
whether an earlier check or a harness correction is appropriate. Retain the
existing cancellation, sharding and comprehensive CI gates.

## Production frontend startup

React hooks and the renderer must resolve to one production runtime. The
`react-singleton.test.mjs` diagnostic builds with the real Vite configuration
and deliberately duplicated dependency trees; both React and React DOM must
remain singletons. The diagnostic runs with the existing Node diagnostics gate.
Development-server browser checks do not exercise production module resolution.

Install deployment dependencies with `npm ci`, reusing the package manager's
download cache rather than copying or merging `node_modules`. Matching lockfiles
alone do not prove an installed dependency tree is intact. Native installation
acceptance must verify the production frontend renders its expected screen
without the display-error boundary; a process and visible window are insufficient.

## Explicit physical network acceptance

These opt-in checks use disposable profiles and test-only `fast-test-kdf`
binaries. Do not deploy those binaries as permanent services. They require the
four authorized SSH hosts and their actual Tailscale routes; failure to reach a
host is not a protocol pass. Windows fixtures, working directories, profiles and
temporary files belong on drive D. No installed relay or network service needs
to be stopped.

- `scripts/e2e-routed-sibling.py`: build `network-acceptance` for arm64 and Intel
  macOS, then exercise real SDK pairing, restart, history sharing off/on and
  continued catch-up across mac2015 and mac-mini.
- `scripts/e2e-federation-recovery.py`: build `mesh-talk-node` for arm64 macOS,
  Intel macOS and Windows GNU, plus the arm64 `network-acceptance` example.
  Exercise preferred-relay outage, a surviving three-relay route, consented file
  delivery with sender restart and SHA-256, and bridge partition/recovery.
- `frontend/e2e/call-media-physical.spec.ts`: build the `network-acceptance`
  example for native macOS and Windows GNU, then run
  `MESH_TALK_PHYSICAL_MEDIA=1 npx playwright test e2e/call-media-physical.spec.ts`
  from `frontend/`. Override `MESH_TALK_MEDIA_LOCAL_IP` and
  `MESH_TALK_MEDIA_REMOTE_IP` with the actual directly reachable Tailscale IPs.
  For two machines on one physical LAN, also set
  `MESH_TALK_MEDIA_TOPOLOGY=lan` and supply their private LAN addresses instead.
  The opt-in spec serially exercises pure voice and video using disposable SDK
  identities, authenticated backend signaling and an independently checked exact
  ICE pair between Chromium and Windows Edge. It verifies decoded audio with
  nonzero energy, video frames where applicable, the shipped call dialog's
  unmuted playback, and capture/connection cleanup on both machines. Screenshots
  and JSON evidence persist under the selected Playwright output directory.
  Synthetic capture verifies transport/decode and cleanup, not native WKWebView,
  physical microphone/camera quality or acoustic speaker output.

Set `MESH_TALK_E2E_MAC2015_SSH=ys-aquria@192.168.3.72` when the `.local` alias
is unavailable; `MESH_TALK_E2E_BASE_PORT` isolates disposable port ranges. Keep
logs and machine-specific evidence outside git. Four hosts at two physical
sites cannot establish a three-independent-LAN acceptance claim.
