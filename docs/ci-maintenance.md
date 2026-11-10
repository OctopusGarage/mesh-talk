# Keeping CI fast and reliable

Treat feedback time, reliability and resource use as maintained requirements.
Every feature, dependency and test can change CI cost, even without a workflow
edit. AI agents and contributors should preserve useful coverage while reducing
unnecessary work, and leave failures easier to reproduce and diagnose.

This guide owns CI maintenance decisions and regression prevention. It does not
introduce another workflow, command catalog or acceptance policy.

| Question                                            | Authoritative owner                                                                                                                     |
| --------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Which tests, commands and CI jobs apply?            | [Testing feedback loop](testing-feedback-loop.md), owning `validation.json` contracts and [workflow definitions](../.github/workflows/) |
| What evidence permits acceptance or release?        | [Quality gates](quality-gates.md) and [version and release lifecycle](updates.md#version-ownership-and-lifecycle)                       |
| Where does an implementation or shared rule belong? | [Module ownership](module-boundaries.md) and [script entry points](../scripts/README.md)                                                |
| Which consumers and documentation need updating?    | [Change alignment checklist](maintenance-checklist.md) and [documentation guidelines](DOCUMENTATION_GUIDELINES.md)                      |

Keep tool versions, matrix sizes, deadlines, cache keys and required-check names
in their existing executable owners. Keep run histories and investigation output
in ignored `tmp/` or PR/CI artifacts; durable decisions belong in the owning guide.

## Make the cost of a change explicit

Before adding a suite, fixture, dependency, platform or workflow, identify the
existing owner and the observable failure it must detect. Inspect the affected
plan and consumers through the [validation protocol](testing-feedback-loop.md#validation-stages-and-agent-responsibilities).
Answer four questions in the change when they are material:

- What requirement needs new evidence, and why is existing evidence insufficient?
- Which layer and platform can exercise that requirement faithfully?
- What recurring setup, compilation, execution and artifact cost will it add?
- Can an existing task, fixture or compatible output supply it without weakening
  assertions or introducing another owner for the same rule?

Extend the existing implementation first. Share policy and lifecycle rules where
their meaning is identical; keep feature-specific fixtures local. An abstraction
is useful when it removes duplicated decisions and has clear inputs, outputs and
failure behavior. Avoid both copied shell fragments and a generic CI framework
whose configuration is harder to review than the work it coordinates.

Do not make a full local suite the default for every edit. Use affected feedback
while developing, then complete the applicable boundary and candidate gates.
New tests still need independent assertions; matching the implementation or
increasing a test count does not establish useful coverage.

## Keep workflow policy thin and reject cheap failures locally

Workflows own triggers, permissions, runners, matrices, dependencies and artifact
transport. Scripts own inventory validation, artifact roles, packaging, report
collection and execution policy. Reuse the same CLI locally and in Actions;
extract substantial conditionals and loops into the existing feature owner rather
than copying YAML fragments or introducing another generic orchestration layer.
When moving execution into a script, update every caller and smoke check. Keep
launcher behavior in its owning regression instead of requiring obsolete command
text in YAML. Cross-platform filesystem paths use `fileURLToPath` rather than
`.pathname`; ESM module arguments retain a file URL or relative module specifier
instead of a native Windows absolute path. Exercise each boundary on its platform.
Keep live source/release checks immediately before each publication boundary;
those protect against changed state and are not redundant setup checks.

Consolidate pure checks only when their immutable candidate, scope and inputs are
identical. Health's full-scope Linux portable job owns source standards and Git
index hygiene after frontend dependency setup; documentation scope retains its
independent source gate. Real checker portability fixtures still run on every
Health platform. Native CI alone supplies partial acceptance: the protected
current-source Health check in the UI workflow and the complete release source
check closure remain necessary. Keep checks at distinct candidate, artifact,
trust or runtime boundaries, including archive producer and consumer identity and
cleanliness checks. Removing repeated source invocations does not establish a
hosted speedup; measure runner cost and latency separately.

The [syntax owner](../scripts/validation/syntax.mjs) parses changed Node, Bash and
Python sources without executing them or writing Python caches, and invokes
`actionlint` for changed workflows. Pre-commit selects staged paths but checks
worktree bytes, matching the existing structural checks. Affected validation also
checks new nonignored sources; PR creation checks the clean committed comparison.
Use Node 22, Bash and Python; install actionlint before changing workflows. No
checker installs tools or contacts the network. Hook installation configures hooks
without launching comprehensive health; request that gate explicitly.
Run the configured spelling check on changed files before publication, and scan
the repository when its configuration changes. Scope unavoidable external API
spellings to their language and identifier; do not exclude a source tree or
replace the API name to satisfy a prose checker.

PR creation runs `make pr-check`'s cheap-check owner against its actual comparison
base, rejects ownership blockers and checks applicable frontend patch coverage
before publication. The creation wrapper accepts repeated `--owner ID` and
`--reason TEXT` before `--` and forwards them only to that existing affected-plan
owner. Review every otherwise unmapped path before making this explicit selection;
it records responsibility, not test acceptance. Platform selectors remain separate,
and missing/invalid owners, absent reasons and checker failures still block creation.
Version projections, contract references and hygiene already
belong to structural standards; reuse that owner rather than adding duplicate
gates. Do not install dependencies, compile Rust, launch browsers or run application suites
implicitly in the cheap mode. Source changes during checking invalidate it.
Workflow edits also run the existing fast CI policy owner locally. Configuration
assertions and runner inventory discovery belong here; application execution and
native compiler fixtures do not. Keep static policy assertions independently runnable
without native tool setup. Scope fixture configuration changes to their owning
table or profile; repeated valid values elsewhere must remain unchanged. Update the existing policy assertion when its
requirement changes instead of adding a second overlapping test in another file.
Write generated reports into the task-owned evidence directory, not the source
checkout. Fixtures for source-integrity checks must preserve production ignore
rules; adding a fixture-only ignore can hide a real dirty-worktree failure.
For entry-point changes, also run the affected contract tests selected by the
existing validation plan before publication. A syntax pass or a hand-picked
filename sample cannot detect stale semantic routing expectations; cheap PR
checks do not certify those tests passed.

The frontend precheck reads the target from `codecov.yml`, includes partially
covered branches in the uncovered denominator and rejects omitted changed files.
Exercise report readers against actual producer output as well as small fixtures;
negative V8 branch counts remain uncovered rather than invalidating the report.
It verifies an existing clean-candidate `frontend-coverage` receipt through the
canonical stage owner and rechecks LCOV bytes, rather than trusting a timestamp or
raw report. Generate missing evidence explicitly with the reported command. A
verification-only miss never starts coverage, waits for a heavy lock or claims a
pass; a no-work verification lease must not imply admitted child work. Keep
verified scope termination diagnostics separate from the actual test result.
This frontend unit precheck is conservative; Rust and browser-union coverage
still need their owning evidence. Native execution, dependency resolution and
published-asset verification cannot be replaced by parsing or stale build output.

Use the existing package inventory's `frontendBuild` ownership when preparing
Rust selectors. Core, integrations and MCP selections still require real target
validation, but do not build frontend assets merely because Rust changed. Host
selections must still prepare Tauri's `frontendDist` before selector discovery.

Composed local gates must invoke shared stages through the same canonical command
and full-mode environment. Normalize `FAST=0` in both full-preflight children so
equivalent successful stages can reuse their verified receipts; do not weaken
identity checks or mutate the caller's environment to manufacture a cache hit.

Select local toolchain versions by prepending to the existing `PATH`; preserve
installed tools required by the repository's enabled Git hooks.

Local feedback does not replace committed-candidate acceptance. Git documents
that [hooks can be bypassed](https://git-scm.com/docs/githooks); web edits and other
clients may not run them. Keep a compact authoritative remote check and necessary
runtime/native evidence. Delete duplicate execution only after comparing source,
configuration, platform, assertion and consumers, with an inventory/equivalence
control in the retained owner. The two optional-pack browser cases execute once
in full Linux Chromium; their separate job retains only its unique built-asset
and syntax checks. The non-instrumented ASan archive protocol executes once before
the required sanitizer producer, while actual instrumentation controls remain on
every consumer. Pure syntax does not establish either runtime contract.

Historical renderer, tray, window and network incident comparisons are manual
workflows. Their former development-branch bootstrap triggers do not provide
ordinary candidate acceptance. Keep their diagnostic tools available; normal
cross-platform native acceptance remains in the required backend caller. Review
all workflows, including scheduled, manual, reusable and publication roles, before
calling a job redundant merely because it is not a PR check.

## Measure the bottleneck before choosing the optimization

For a CI optimization task, use `gh` to collect the last 50 workflow runs and
their paginated, attempt-specific job and step records before editing workflows.
Keep successful, failed, cancelled and incomplete observations distinguishable.
For each job, separate dispatch/wait time, runner setup (checkout, tools,
dependencies, cache transfer and compilation) and test/check execution. Preserve
artifact/reporting overhead in the accounting rather than silently dropping it.
Where a step mixes compilation and tests, use its logs or Cargo timings and mark
any unresolved split as unknown. API dispatch timestamps can include `needs`
waits; they do not by themselves measure runner queue time.

For unexplained native-runner variation, retain comparable complete observations
and collect resource evidence before changing concurrency or compile profiles.
The existing native macOS coverage owner records CPU count, physical memory,
swap/VM snapshots and per-partition real/user/system time plus maximum
child RSS. A snapshot cannot prove peak memory pressure; maximum child RSS is
not concurrent compilers' aggregate peak. Preserve the original command status
and failure precedence, and keep diagnostics outside ordinary acceptance gates.
Keep detailed product tracing opt-in; routine resource measurements must not
enable per-operation traces merely to observe compiler behavior.

Use the existing [timing reports](testing-feedback-loop.md#measure-feedback-before-tuning-capacity).
Distinguish time to actionable feedback, time until required checks complete,
runner execution minutes and failure/rerun frequency. A change can improve one
while worsening another. Runner minutes are not billing, and a green rerun does
not erase the first failure.

Compare equivalent source changes, test inventories, platforms, toolchains and
cache conditions. Retain cold and warm observations, sample size, cancellations
and missing data. P50/P95 from a small or mixed historical sample are descriptive;
one faster run is not a durable latency guarantee. Job creation-to-start time can
include dependencies, so do not label it all runner queue time or call the last
finishing job a proven critical path.

| Observed cost               | Investigate first                                                                 | Prefer before adding capacity                                                        |
| --------------------------- | --------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| Waiting to start            | Actual dependency graph, runner availability, obsolete candidates, matrix fan-out | Remove unnecessary dependencies and safely cancel superseded verification            |
| Installation or compilation | Step timing, dependency graph, cache restore/save cost and compatibility          | Remove repeated setup and reuse compatible outputs; inspect Cargo timings            |
| Long test execution         | Slow cases, fixture preparation, worker saturation, shard imbalance               | Fix expensive setup and balance measured work within resource limits                 |
| Intermittent failures       | First failure logs, traces, readiness, task lifetime and leaked resources         | Reproduce and repair the owning fixture, product behavior or infrastructure          |
| Evidence collection         | Archive sizes, compression, transfer, merge and upload time                       | Produce each required report once and separate optional presentation from acceptance |

[Cargo build timings](https://doc.rust-lang.org/cargo/reference/timings.html)
help distinguish compilation work from test execution. A test scheduler cannot
remove compilation cost. Include dependency installation, archive transfer and
reporting in the comparison; moving cost into another job is not removing it.

Set latency and resource expectations from representative observations and the
purpose of each gate. A job timeout is a safety bound, not a performance target.
Record material budget changes and their rationale in the owning configuration
and review, rather than inventing universal minute limits in this guide.

Measure the complete unmutated baseline under its actual execution profile when
inventory or resource policy changes. Selected cases and tiny tool-contract
fixtures do not establish that the full cohort fits its per-case, suite and job
budgets. Preserve executed and unrun counts when fail-fast or a watchdog truncates
that evidence.

Budget a mutation chunk for the baseline obligation, every selected mutant's
build and complete consumer suite, setup and report publication. Do not assume
mutants fail early or reuse an old selected-count constant as the inventory grows.
The planner must preserve the rotating sample's exact union while bounding each
worker's work; a shorter chunk changes scheduling, not assurance coverage.
A shared baseline may remove repeated execution only when workers validate the
same run, source, tools, configuration, complete inventory and trusted producer
attempt through the existing evidence owner. Preserve raw tool reports: an
external baseline is separate verified evidence, never an invented tool outcome.
Failed-only reruns may reuse the attested successful producer attempt from the
same run; missing or mismatched evidence must fail before mutant execution.
Keep compilation, individual-test, complete-suite and job limits distinct and
reserve time for process retirement and evidence publication. Invoked tools may
create independent process groups; retain the real tool's cancellation and build
retirement controls, not only successful-run evidence. A closed supervisor pipe
cannot replace the actual work owner's acknowledgment. See
[actual work ownership](ai-coding-reliability.md#r03-actual-work-owns-its-lifetime).

Stop an already failed candidate before starting another expensive compilation
partition. Linux and standalone macOS coverage commands finish all selected Host
cases, then admit the remaining complete workspace only if Host passed. Both
partitions still have to pass before their combined report. Hosted macOS uses
independent Host-only and archived remaining-workspace lanes instead; its required
collector accepts only when every lane succeeds. Per-partition terminal result
collection, independent matrix jobs, default targets and zero retries remain
unchanged. The existing actual-tool coverage regression checks Host rejection,
remaining-partition rejection, successful coverage accumulation, timing pipelines
and report errors. Preserve partial backend evidence after a failure, while
requiring evidence on passing jobs; a producer skipped by an upstream failure
must not create a second missing-artifact error.

The [workspace coverage owner](../scripts/ci/workspace-coverage.sh) owns native
partition commands, timing pipelines and report generation. Linux CI defers its
interim report until default and ignored backend cases have passed validation,
then generates and uploads one accumulated report. Avoid uploading an earlier
subset together with its final superset. Standalone and macOS reporting remain
immediate; actual-tool controls verify accumulation and both failure paths.

Keep complete tests distinct from duplicate instrumentation. macOS instruments
Host for its native desktop/updater branches and runs every remaining default
workspace target with pinned Nextest without coverage instrumentation. Hosted
lanes run independently; the standalone command retains Host-first sequencing. Linux and
Windows retain full instrumented workspace coverage. This removes the separate
instrumented Core test build on Apple Silicon; it does not claim full macOS Core
line coverage. The actual-tool regressions execute both coverage modes and real archive consumers, require
all default targets and the Core execution marker, proves a clean macOS report
retains Host hits without a Core test cohort, and keeps both partition failures,
report failures, timing pipelines and process termination controls. All platform
assertions, doctests, zero retries and direct required checks remain unchanged.

A progressing serial test partition can exhaust its job watchdog without a hung
case. Windows reuses one sealed instrumented workspace archive across eight
independent serial consumers; every selected case still executes once with full
coverage, default targets, existing watchdogs and zero retries. Acceptance uses
those actual native checks directly. Do not add a second qualification workflow
or extend a job timeout to compensate for oversized units. Four extra setup and
archive transfers are a material cost; measure them alongside execution and runner
variation. The existing required-check owner retires old partition names only
after every current-source replacement passes with its bound app. Preserve strict
protection, unrelated checks and exact-source release acceptance together.

Treat a browser's total functional-case watchdog separately from its action,
assertion and product deadlines. A cold page, several successful state checks and
real interactions can cumulatively exhaust a short total bound without any one
operation failing. Full Chromium uses its existing Windows/coverage safety margin
on all hosts; portable execution retains its existing Windows allowance. Keep
state-based readiness, all assertions, explicit latency limits, zero retries and
first-failure traces. Do not erase a failed assertion by increasing its timeout.
The real-runner control derives both configured watchdogs and accelerates only
the deliberately parked case. Subsequent cases retain the source configuration's
normal allowance; deliberate latency beyond the accelerated bound verifies this
isolation. Require actual parked-case failure, subsequent-case execution and
verified descendant retirement before removing the fixture.

## Reuse compatible work with verifiable inputs

Prefer one producer for an expensive output when consumers need the same inputs
and artifact transfer is cheaper than rebuilding. Include the producer dependency
and its effect on parallelism in the measurement. Reuse compilation separately
from test execution: each required consumer still supplies its own assertions.
Keep production, coverage, sanitizer and mutation configurations distinct unless
their compatibility has been established.

Hosted Ubuntu Rust jobs prepare disk capacity before restoring Cargo outputs.
The [runner storage owner](../scripts/ci/runner-storage.mjs) reclaims only three
fixed unused SDK directories from disposable image contents and reports actual
free bytes before and after. Each heavy job uploads that small measurement before
cache expansion, so a later exhausted runner does not erase the preparation evidence.
It preserves the workspace, build caches and tools
used by compilation and analysis; local/self-hosted contexts and redirected paths
are not cleanup targets. Its real filesystem controls cover preservation and
idempotence. Inspect those measurements and cache expansion before responding
to disk exhaustion; a failed runner may be unable to upload its own logs.

A cache is an optional accelerator. A cold or evicted cache must still allow
correct execution. Measure restored bytes, restore/save time, useful hits and
eviction; a reported hit alone does not prove a faster build. Preserve the
existing trust boundaries and write policy. GitHub documents cache matching,
branch access and eviction in its [dependency caching reference](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).

Prefer setup actions' built-in dependency caching: configure `setup-node` for
npm and the actual lockfiles; `setup-go` normally enables its cache by default;
use `Swatinem/rust-cache` for compatible Rust outputs. Check the repository's
current cache allowance and usage before adding archives. Keep a frontend-only
job free of Cargo cache restoration, and do not cache installed outputs that are
cheaper to recreate than transfer. A cache key must describe compatible inputs,
not merely create more apparent hits. Avoid publishing identical archives from
every shard; preserve trusted write policy and existing producer ownership.

An exact cache hit is immutable: it cannot acquire newly needed dependency
feature/profile variants after a workflow changes its compilation cohorts.
Review that transition explicitly; rotate only the affected platform's cache
epoch when needed, preserving trusted writes. A new prefix has a real initial
cold-build cost and competes with the old entry for the repository allowance.
Do not enable workspace-crate caching or add per-commit keys merely to hide a
slow build. Distinguish dependency reuse from the workspace source that must
still compile; a cache repair does not explain unrelated runner variability.

Download pinned prebuilt development tools through the existing installer where
available. Verify support in the repository-pinned installer version; an unknown
tool can select an unintended source-build or registry fallback. Bind direct
release downloads to a reviewed digest and execute the installed version check.
Keep installer revisions compatible with the pinned tools in every producer and
consumer; disable source-build fallback for managed prebuilt tools. Otherwise a
download failure can start an unrelated compilation under inherited sanitizer
flags instead of reporting the setup failure.
Check the installed host architecture as well as its version: fallback discovery
can select a different architecture across otherwise identical archive consumers.
Preserve producer/consumer identity checks instead of accepting that mismatch.
When no supported installer exists, keep download logic in its owning script.
Use the provider's supported asset API, bound transport attempts, verify the
reviewed digest before extraction and preserve the scanner's exit status.
Integrity failures must not trigger another source or unverified execution; see
the [Gitleaks owner](../scripts/ci-gitleaks.sh) and its behavioral controls.
Classify generic-key matches against their actual source expression before
adding an exception. The [scanner configuration](../.gitleaks.toml) combines an
exact fixture path and exact public-field expression with `AND`; the same value
elsewhere, another expression and a credential-shaped value in that file must
still be detected by the pinned scanner. Preserve full-history scanning.
Compiling tools with `go install` or `cargo install` is recurring setup
work; retain source installation only when the required tool has no suitable
trusted binary or the test explicitly validates its build.
A lockfile does not protect an unpinned `npx` download. Install Node-based
verification tools from the committed lockfile and reuse their owning npm script
in CI and local checks. Frontend audits use `npm run audit:ci` with the existing
`frontend/audit-ci.json` policy.
Browser execution shards should install with `npm ci --no-audit --no-fund`:
the install-time advisory summary repeats the dedicated audit without enforcing
its policy. Keep the explicit audit gate; disable only this duplicate install
work, not lifecycle scripts or checks for other dependency owners. Measure any
claimed saving separately from the full installation duration.

Build twice consecutively with identical source, profile, features and environment.
Inspect unexpected second-build work before enlarging caches or runner counts:
missing `rerun-if-changed` inputs, generated files that change every invocation,
unnecessary frontend rebuilds, and container layers that copy source before
installing locked dependencies. A bundler may intentionally rebuild quickly;
judge the observed cost instead of requiring it to behave like Cargo's no-op build.

Auxiliary diagnostic transport must not decide test acceptance or skip later
independent checks after an artifact-service outage. Keep test execution, local
result publication and required cleanup fatal; only the reviewed upload step is
best-effort. The CI policy owner enumerates these roles: Health logs and raw
coverage copies, storage measurements, crash/timing diagnostics, optional HTML
and failure attachments. Source-bound coverage maps, partition archives/unions,
native/backend and release/security evidence stay mandatory. A missing automatic
consumer alone does not make an artifact optional: authoritative human reports
and manual diagnostic deliverables can still require it. Do not retry tests to
repair transport, or suppress the original failed attempt.

An artifact used as acceptance evidence has a stronger contract. Its consumer
must validate the actual tested head or merge source, workflow/run/attempt, platform and architecture,
relevant toolchain/features, expected inventory and content digests required by
the owning protocol. A matching filename, a successful download or a cache hit
is insufficient. Digests detect content changes; provenance establishes whether
those bytes belong to the intended candidate. Reject missing, stale, duplicate,
partial or incompatible inputs before merging evidence.

Keep these checks in the existing producer/consumer owner. Do not add a second
receipt format or evidence resolver. Exercise rejected-input cases when changing
that boundary, and preserve the documented rerun procedure for attempt-bound
artifacts. GitHub's download action reports a digest mismatch as a warning;
the evidence consumer must explicitly reject it. See [artifact validation](https://docs.github.com/en/actions/tutorials/store-and-share-data#validating-artifacts)
for the platform behavior.

## Refactor coverage without losing its meaning

Two executions are redundant only when their requirement, inputs, platform and
evidence obligations are equivalent. A browser mock, real desktop process,
encrypted loopback exchange and physical network observation prove different
things. Keep the [platform evidence boundaries](platform-validation.md).

Before merging or deleting a check, map each assertion and downstream consumer
to its retained owner. Update affected selectors, fixtures, artifact consumers,
required checks and release consumers together where applicable. Validate the
replacement on the current candidate before the existing protected obligation
can be retired under the [quality gates](quality-gates.md). Eliminate redundant
aggregate jobs when direct checks provide the required enforcement; retain a
collector when it actually validates completeness or combines required evidence.

Selection and sharding need inventories, not just successful exit codes. Verify
that required cases are present, partitions are complete and disjoint where
intended, and expected platform-specific or ignored cases remain explicit.
Empty selections, skipped dependencies and incomplete collectors cannot stand in
for executed coverage. Moving Rust execution to another runner must also retain
separately owned doc-tests and integration/ignored-test obligations.

Use [same-source selection audits](testing-feedback-loop.md#qualify-affected-selection-alongside-full-gates)
before proposing narrower gates. Imports alone cannot reveal every runtime,
native or generated consumer. Moving a check from PRs to a schedule changes
detection latency and acceptance scope; describe and review that tradeoff rather
than calling it equivalent coverage delivered faster.

Workflow path filters and job conditions have different required-check behavior.
GitHub warns that a workflow skipped by path or branch filtering can leave its
required check pending; a skipped job can report success. A collector must still
run when a dependency fails and explicitly reject failed, cancelled, missing or
unexpectedly skipped required work. Use `!cancelled()` for verification collector jobs: it permits explicit rejection
after upstream failure, but releases superseded workflows on cancellation. An
unconditional `always()` collector can remain queued after its producers are
cancelled and hold the concurrency group needed by the latest candidate. Keep
necessary resource cleanup independent. The scheduling condition does not
validate dependencies; the collector must still inspect every required result. See [required-check troubleshooting](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).

Before adding docs-only path filters, inspect actual branch protection and the
app-bound checks it requires, including external coverage and security checks.
Define which documentation is inert: generated help, executable examples, eval
prompts, fixtures and policy inputs can change product or acceptance behavior.
Keep applicable documentation checks. Use workflow-level filters only when no
required consumer will wait for their missing result. A lightweight selection or
collector job must retain the required check identity and reject failed selection;
it must not fabricate coverage or security results belonging to another app.
Codecov provides a native [empty upload](https://github.com/codecov/codecov-cli#empty-upload)
for diffs it classifies as ignored; never force it for an ineligible diff. Qualify
the current-SHA provider result before relying on it. This does not replace real
CodeQL analysis or establish that documentation cannot affect product inputs.
GitHub evaluates [job conditions before matrix expansion](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idif):
a skipped matrix job does not prove that every expanded required context exists.
Inspect actual returned names and app IDs before narrowing protected work.
Preserve complete main-commit verification required by release consumers even
when a PR has a separately validated docs-only path.

The UI HTML collector follows the mandatory full-shard producers, independently
of the optional extended-browser jobs. Each OS uses its own producer conclusion:
skipped producers omit only their own reports; missing or unknown conclusions
fail. Failed or cancelled admitted producers still render available diagnostics
before incomplete evidence fails. The collector checks all platform-specific
current-attempt shards (eight macOS, four Linux/Windows); fixing the upstream
failure and rerunning the selected producers supplies that evidence, rather than
omitting those OSes.

## Documentation-only PR scope

The [scope owner](../scripts/ci/docs-scope.mjs) permits a fast path only for
same-repository PRs targeting `main` whose exact tested merge changes ordinary
README or documentation prose. It reads the NUL-delimited Git delta, including
file modes, and reuses the validation contracts to exclude owned inputs and
authority references. Help, evaluation prompts, policy guides, executable or
symlink changes, deletions, renames, mixed code changes and uncertain inputs do
not qualify. Fork PRs retain full coverage because the coverage provider's empty
upload does not have the ordinary tokenless fork authentication guarantee.

Required matrix identities remain present. Their jobs independently verify the
selection, use Ubuntu for the documentation path, and describe product tests as
not applicable rather than executed. Nonrequired heavy producers can be skipped;
full-scope collectors still require their original exhaustive, source-bound
artifacts. Do not manufacture empty execution receipts or coverage files. A failed
selector must fail required jobs even when dependency scheduling would otherwise
skip them. Defaults for standalone, scheduled and manual callers require full work.

The documentation path retains whitespace, structural ownership and hygiene
checks through their existing owners. Ubuntu source formatting, spelling, workflow
lint and supply-chain scans remain enabled. Real CodeQL, secret scanning,
dependency review and lightweight mutation-runner contracts remain enabled.
Codecov uses its native, unforced empty-upload operation; an accepted HTTP request
does not prove a passing provider check. Verify the actual current-head App check.
No broad coverage ignore rule or replacement provider status is needed.
Pin the downloaded CLI independently of the Action commit: an Action pin can
still resolve a moving `latest` tool. Keep ordinary and empty-upload clients
aligned and retain the provider's integrity verification.

Use a shallow checkout containing the merge and both parents; full ancestry is
unnecessary for this exact base-to-merge comparison. Missing objects, mismatched
source or failed output publication cannot authorize skipped work. The real Git
fixtures retain positive depth-two and negative missing-history, mode, authority,
fork and mixed-change controls. Run these and the workflow policy locally before
publication, then qualify actual required names and skipped work in a real docs PR.

Every main push, merge group, schedule and manual run retains full verification.
Release acceptance binds checks to the latest successful trusted main-push run
and its owning workflow suite; a successful PR scope decision is not release
evidence, even at the same source SHA. Add this provenance requirement without
removing the existing veto from newer failed or pending required checks.
[Quality gates](quality-gates.md) owns
acceptance and [updates](updates.md) owns publication requirements.

## Bound parallelism and preserve isolation

Run independent checks in parallel, but inspect each `needs` edge before removing
it: artifact availability, coverage provenance and deliberate runner admission
are real dependencies even when no explicit job output is passed. Put portable
formatting, source lint and dependency auditing on Ubuntu. Retain macOS/Windows
execution when assertions exercise their native behavior. Count macOS jobs across
all workflows against the account's actual concurrency allowance; moving one
workflow's bottleneck into another workflow's queue is not an improvement.

Use runner-owned scratch for native temporary files. Windows build/test jobs set
`TEMP` and `TMP` from `RUNNER_TEMP` in an early step, matching the local preflight
owner's explicit placement. Record old and selected paths; a different machine's
faster result does not prove that disk placement caused a prior failure. Preserve
durability, ordering and deadlines while qualifying the new hosted environment.

Full Chromium uses eight macOS partitions and four each on Linux and Windows.
Desktop browsers inherit the reusable workflow's four-slot per-OS limit; preserve
Health and WebKit admission before the macOS burst and the existing job watchdog.
Windows has an independent caller gated by Health and scope, without waiting for
unrelated macOS WebKit work. Portable and final acceptance still require explicit
success from Linux, macOS and Windows; HTML presentation remains independent.
Scope limits to the actual resource pool rather than applying a macOS restriction
to Windows. Inspect late queued work after other native jobs finish; an overly
small limit can leave capacity unused. Four macOS browser jobs leave nominal room
under the five-job account limit, but do not reserve global capacity or bypass
other workflows' queues. Qualify changes on hosted runners.
Strict collectors require the platform-specific complete layout, including the
last partition, and reject layout discovery failures.

WebKit partitions retain their existing stable required-check name through a
full-union collector. Bind every producer to its actual platform, source, run and
attempt; reject missing, duplicate or stale partitions and validate the complete
scenario inventory after rebasing copied evidence. A continuously progressing
suite that exhausts its budget needs measured partitioning, not a larger timeout.

Choose shards from observed work and available runner capacity. Count setup and
artifact overhead as well as execution. More shards can increase queueing and
runner minutes; more workers per machine can increase memory pressure, port
exhaustion and test interference. Review the product of platform, architecture,
browser, feature and shard dimensions before expanding a matrix.
Treat elapsed-time targets as diagnostic goals, not acceptance requirements.
Prefer a stable, simple pipeline over extra shards, retries or orchestration
introduced only to reach an arbitrary duration.
Inspect actual worker-scoped options and test-group assignment as well as case
counts: tracing modes can produce distinct worker groups and uneven tails.
Shuffle may reorder work only after shard allocation; verify its actual effect
before treating it as rebalancing. Preserve required trace evidence and evaluate
the complete DAG under the global runner quota.
For a wall-time target, calculate both the dependency critical path and the
aggregate runner work divided by the constrained pool's available slots.
Scheduling alone cannot beat that capacity bound; added shard setup consumes
more of it. Leave queueing margin in the execution budget and report full-suite
and documentation-only observations separately. A shorter watchdog is not a
performance improvement.

macOS browser coverage uses `macos-26-intel` for WebKit and Chromium, including
extended scenarios. Its contract is the macOS/browser behavior; the ARM workspace
and real native app/CLI checks retain `macos-latest`. Keep artifact labels and
per-OS collectors aligned with the actual selected image. Avoid coupling browser
admission to scarce ARM capacity needed by native builds. A job that never obtains
a runner has no test result; preserve its infrastructure failure separately.
UI coverage and reports require one complete attempt, so recover that workflow
as a whole when an unallocated producer prevents its current-attempt evidence.

ASan owns one reviewed dated nightly in the sanitizer workflow environment.
The producer installs that exact toolchain with `rust-src` once, verifies its
installed manifest date against the declaration and fingerprints `rustc` and
Cargo through that explicit toolchain before restoring the existing cache cohort.
Do not resolve a floating nightly or infer the compiler from an active default:
calendar changes otherwise invalidate reusable dependencies without source changes.
Upgrade this CI toolchain dependency through a reviewed PR with the full native
Linux union and real clean/use-after-free controls on all eight runners; a local
stable archive-protocol fixture does not qualify instrumentation or a cache hit.

ASan compiles its core library once into an official Nextest archive, then runs
eight serial partitions. Runners 1–6 use Nextest `hash:ID/6`, excluding the
four exact reserved test names owned by `scripts/ci/asan.mjs`. Runner 7 executes
the long canonical inventory payload case alone; runner 8 executes the known
unavailable directory-source acknowledgment, obsolete negative-cache head and
normal authorization-history growth cases. These fixed cohorts balance measured
outliers without another timing database. List and run use identical selectors;
full discovery remains unfiltered. Before sealing and before execution, the owner
requires every reserved name exactly once and default runnable, rejecting name
drift, duplicates and ignored cases. Preserve the complete strict union when
changing cohorts; compare actual instrumented timings before reconsidering them.
The producer seals the complete default inventory and
source/compiler/config/platform identity. Trusted job outputs bind both the
manifest and archive digests; consumers reject mismatches before extraction and
compare the extracted inventory before execution. Each runner retains the same
case budgets, instrumented standard library and real clean/error controls. The
required collector checks producer success, the exhaustive disjoint inventory
and every current-attempt execution receipt. Consumer jobs do not restore the
full Rust cache or repeat the large core compilation. Keep the already compressed
archive transient and transfer it without another compression pass. Compare
producer compilation, artifact transfer, consumer preparation and queue time
alongside test execution; more shards alone can duplicate the dominant setup.
Keep extracted runtimes outside uploaded evidence. Collect receipts, inventories
and logs; do not upload another compiled binary copy from every consumer.
This separation also applies to failed or interrupted extraction. Use an owned
staging directory on the final target's filesystem, outside artifact roots, and
retire only that directory through the extraction owner's cleanup path. Exercise
rejection after real extraction and interrupted partial writes while preserving
unrelated native build outputs. Record image
rollout metadata separately from actual compiler/runtime compatibility; incidental
image version changes must not reject otherwise identical compatible inputs.
Attempt-bound archives and receipts require a complete sanitizer-workflow rerun
when recovering a failed attempt; do not mix a previous producer with new shards.

Retain fresh process/context isolation, owned temporary directories, dynamically
allocated ports where supported, and deterministic cleanup. Keep compile jobs,
test workers and network/rendering resource limits explicit. Do not multiply
them independently and assume the machine can absorb the combined load.
The macOS workspace coverage owner admits two compiler jobs on its small hosted
ARM runner while retaining four test workers and every target. Keep this bound
after Rust cache restoration so a scheduling experiment does not also invalidate
the cache cohort. Compare complete compile/test/resource measurements before
attributing a speedup to it; compression and swap observed across a phase do not
identify which compiler or test caused the pressure.
[Nextest archives](https://nexte.st/docs/ci-features/archiving/) can separate build
and execution, while [Playwright sharding](https://playwright.dev/docs/test-sharding)
distributes browser work. Archive consumers need the same source revision and
required runtime assets; shard distribution does not guarantee equal durations.
Both need compatible inputs and complete inventories, with isolation preserved.
Archive reuse also requires the actual architecture/ABI and supported runtime
binary lookup. An archive cannot rewrite executable paths embedded at compile
time; validate a relocated fresh consumer before sharing builds across jobs.
The hosted `macos-remaining` profile in `scripts/ci/windows-coverage.mjs`
requires identical absolute checkout and target paths for embedded CLI lookups.
It selects every enabled default workspace target except exactly `mesh-talk`,
with two hash partitions, four test workers, two compiler jobs, default ignored
policy, no retries and no LLVM coverage instrumentation. Reject inherited LLVM
activation, Rust flags/wrappers and unexpected target/build overrides; bind the
accepted profile environment, compiler, source, configuration, resolved features
and run attempt. Preserve the existing Windows eight serial instrumented shards.
Hosted CI admits the remaining-workspace producer immediately after full scope,
in parallel with the native Host lane. Two archive consumers depend only on scope
and that producer; they do not wait for Host, Health or WebKit. The native lane
retains all-target Clippy, ordinary production workspace compilation, workspace
doctests, frontend preparation, native real-tool fixtures and Host-only coverage.
The standalone `workspace-coverage.sh macos` command still runs Host first and
admits the remaining partition only after Host succeeds; hosted parallelism does
not weaken either partition's failure semantics. The existing required
`checks (macos-latest)` identity collects on Ubuntu only after actual native,
producer and aggregate consumer success. Documentation scope independently
revalidates its cheap required check without manufacturing execution receipts.
Linux retains complete instrumented default and ignored coverage through the
shared archive flow below.

Preserve the old `workspace-checks-checks` shared dependency-cache cohort without
an additional job key on Ubuntu, native macOS and the remaining producer. Restore
before setting the archive step's two-compiler limit or any new profile overrides.
The native macOS lane is the sole main-branch writer of that macOS cache because
its ordinary full-workspace build and doctests own the dependency union; the
remaining producer restores only, and consumers restore no Cargo cache. A matching
configured key is not evidence of a hosted cache hit. Keep already-compressed
binary archives in separate one-day artifacts with compression disabled; retain
small metadata and execution diagnostics separately. Producer hashes and the exact
Rust version are trusted job outputs, not values recovered from downloaded files.

Additional macOS jobs share the existing five-runner capacity with native,
backend and browser work. Preserve browser readiness/admission gates and the
existing job watchdogs. Measure producer compilation, symbol packaging, transfer,
queues, consumer tails and aggregate runner minutes on the same source. The
historical roughly 47-minute serial macOS lane may move into the high twenties
before queues; that is a hypothesis, not a thirty-minute acceptance guarantee.
The historical roughly 46-minute Linux lane is the archive comparison baseline;
macOS aggregate work also consumes the shared five-runner capacity.

Linux keeps the existing `checks` job ID and `workspace-checks-checks` cache
cohort as its sole main-only native/instrumented cache writer. The producer runs
all native lint, production workspace/frontend builds, syntax checks, doctests
and real compiler/crash contracts. Native Linux default-archive and ignored
adapter fixtures gate the product archive. One full instrumented workspace
archive then admits two four-worker default partitions and one serial ignored
consumer. Windows remains eight serial consumers; macOS remains two
uninstrumented remaining-workspace consumers plus native Host coverage.

The Linux consumers restore no Rust cache, install no frontend dependencies and
compile no Cargo targets. They install the producer's exact Rust version,
matching LLVM tools, pinned prebuilt Nextest/llvm-cov and required Tauri runtime
packages. `scripts/ci/linux-workspace.sh` independently captures a successful
`cargo llvm-cov show-env --sh` before evaluation; failed environment discovery
cannot admit execution. Fixture shell phases use a dedicated literal interpreter
launch; ordinary Node/Cargo argv stays on a separate boundary. Keep derived paths
as quoted environment variables or argv entries, and exercise quotes, spaces and
literal `$()` in test paths. Keep `target/llvm-cov-target` and embedded CLI paths.
Default timing markers stay in each consumer's execution log; ignored backend
markers stay in their original redacted suite logs. Each actual Linux execution
runner retains crash setup/selftest, always-run collection and verified restoration. Diagnostic selftests must retire their own source, executable and
empty directories on success and failure before archive admission. Track which
directories the selftest created; preserve pre-existing trees and newly arrived
contents. An empty directory still violates a fresh-target contract: never weaken
archive admission or delete an unowned target to compensate. The real-filesystem
controls in `linux-crash-diagnostics.test.mjs` cover fresh and populated consumers
without claiming native kernel qualification.
Consumer jobs use the storage owner's read-only `--measure` mode without repeating the producer's SDK cleanup; inspect actual space, archive and diagnostic
image usage before changing storage policy.

The required `checks (ubuntu-latest)` aggregate independently revalidates scope.
Full scope requires actual producer, default-matrix and ignored success before
mandatory downloads. Both pure Linux and macOS aggregates use fifteen-minute infrastructure
watchdogs for receipt/LCOV transfer, validation and upload. Producer/consumer
ninety-minute watchdogs and all product case budgets remain unchanged. It reuses the shared default receipt collector and existing
backend ignored validator with trusted producer hashes and current source/run/
attempt. The original four-suite backend report, all eight backend scenarios,
complete inventory-owned Bridge ignored subset and exact large-folder case
remain required. The ignored artifact is uploaded once under the historical
`backend-core-ubuntu-24.04-ATTEMPT` compatibility name; the sealed runtime identity
owns actual ABI. Native Ubuntu version, architecture, glibc, runtime packages,
compiler and profile remain authoritative; incidental image version is retained
as provenance rather than an additional compatibility restriction.

Two default LCOV reports and the separate ignored report represent genuine
execution cohorts. Validate every hash and coverage summary before uploading all
three through the required Linux Codecov owner. This trades the former serial
report for parallel execution with three report/setup costs; a single raw-profile
collector would need the compiler, executable archive and profiles again. Measure
producer compilation, all report costs, transfer, queue delay and runner minutes
in the complete same-source hosted DAG, including the five-Mac admission limit.
Local archive mechanics and isolated durations do not establish full native Host
linkage, Linux ELF diagnostics or a thirty-minute complete CI result.

Trusted producer job outputs supply the manifest and archive hashes before
consumer extraction. The producer API returns those hashes and the Rust version;
only explicit CLI `--github-output` export writes GitHub outputs. The small
metadata artifact contains `manifest.json`, `inventory-{0,1,2}.json` and
`feature-resolution.json`; consumers also download `workspace.tar.zst`.
The pure collector requires only that metadata, both selected inventories,
receipts and execution logs, plus trusted current source/run/attempt and explicit
successful native, producer and aggregate consumer job outcomes. It checks two
distinct complete executions and their exact disjoint union without comparing
its own Ubuntu platform/root to Darwin. Never upload consumer runtime binaries
or invent LCOV for uninstrumented tests.

Archive qualification must remove producer build outputs and inspect actual
application stack frame file:line evidence separately from panic-header source
locations. Successful binary execution alone does not prove diagnostics survived.
Darwin unpacked line tables depend on compiler objects that official archives do
not retain. The macOS profile generates only metadata-selected executable dSYM
bundles with Apple `dsymutil` and includes them through official Nextest archive
extras, keeping the line-table profile unchanged. Require the producer extraction
probe and every consumer to verify the actual expected symbol inventory and
bytes, since repository configuration can override lower-priority tool extras.
The archive/manifest hashes bind these symbols. Retain a missing-bundle negative
control in `scripts/ci/macos-remaining-cli.test.mjs`; do not archive the whole
compiler cache. Measure symbol generation, added bytes and complete hosted
compile/transfer/queue cost before claiming a speedup.

Cancel superseded verification for the same candidate when safe, with concurrency
groups that separate caller workflows, event streams and branches. Reusable Health
keeps a distinct prefix because GitHub exposes the caller workflow in its context;
PR/merge candidates cancel their superseded source. Push verification uses a unique
run ID in its concurrency group: disabling cancellation alone still permits a
newer pending run to replace an older pending run. Every main commit must retain
its complete verification for release consumers, while scheduled and manual
evidence streams remain isolated. Preserve
publication and release ordering. Inspect the configured queue mode and
cancellation semantics; do not assume workflow dispatch order is execution order.
Concurrency groups do not supply merge-candidate validation. Follow GitHub's
[concurrency semantics](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency)
and the project's [merge-check rules](testing-feedback-loop.md#merge-queues-and-required-merge-checks).

## Fix instability at the owning boundary

Classify a failure before changing its budget: product regression, fixture race,
resource exhaustion, dependency/service failure or unavailable infrastructure.
Retain the failing source and attempt, first error, relevant trace and measured
phase. A passing rerun is diagnostic information, not a root-cause explanation.
Playwright explicitly [classifies a retry pass as flaky](https://playwright.dev/docs/test-retries);
keep first-attempt and retry outcomes distinct.

Synchronize with observable readiness and the exact operation being tested.
Register observers before triggering transient events; correlate signals with
request identity, immutable control state and the lifetime of the actual task.
Historical logs or a previously authenticated request do not prove that a live
request has entered the required phase. Keep cancellation, task joining and
cleanup correct on success, failure and panic.
Give pre-arm setup and the measured admission separate bounded phases; an
independent watchdog must not release a test barrier merely because unrelated
preparation consumed its observation window. Keep the owning API's pending/error
distinction in bounded observers. The
[failure-evidence rules](ai-coding-reliability.md#r10-evidence-that-exercises-the-failure)
identify the owning retirement-observer regression.
When key ordering determines the scenario, use deterministic noncolliding fixture
identities and check actual signed keys or cursors; random IDs can exercise only
the easier path. Preserve real signing, publication and receipt assertions.
Keep commit, completed publication and recipient installation distinct. A
post-timeout snapshot can block behind the publisher and cannot prove its state
existed before the cutoff; record phases at the actual work owner.
Use the owning register-before-check notification protocol for durable status;
polling an in-memory flag can still compete with the work that must publish it.
Match a fixture's executor to its requirement: preserve single-thread tests when
responsiveness is the subject, and keep independent protocol/persistence workers
for other production-runtime scenarios. Join admitted work before deleting data.
Classify fixture gates from the current decision owner's state, not a trace flag
that also describes another phase. Do not sample a later observer projection to
decide whether to capture a one-shot event; observe that evidence before releasing
the captured task and retain its exact operation and child identity.

Separate expensive fixture preparation from the operation's measured window when
preparation is outside the requirement. Continue using the real signing,
persistence, transport or UI owner when it is part of the assertion. Preserve
production deadlines and real lifecycle coverage where those are the subject of
the test; changing the clock or injecting state cannot silently remove them.
A held real socket still consumes its production timeout while a fixture waits.
Finish unrelated cold preparation before dispatching that read, using a scoped
owned barrier before I/O where necessary. Do not pause a post-send hook to conceal
the network timeout. Consolidate repeated readiness polls into that preparation
owner; retain exact live-task and measured-operation assertions at admission.

Batch unobserved durable fixture construction through an existing validated
transaction seam when individual commit boundaries are outside the requirement.
Retain every signed event, per-event validation and index update, a completed
durable commit before observation, exact corpus assertions and the original
measured operation. Never batch away the transaction boundaries under test.
Commit CLI fixture infrastructure to its private baseline before introducing the
intended source change; unrelated copied tools must not inflate changed-file
validation or consume the assertion's watchdog.

Lifecycle regressions should exercise the real supervisor and child processes
without making unrelated host tool startup part of their success condition. Full-host
network fixtures must use the existing independent device runtime owner, retain
the original offline/online cuts and observation budgets, and join actual work
before profile reuse on success and failure. A timer sharing a synchronously
stalled reactor provides no independent watchdog; isolate device scheduling
rather than extending timeouts or assigning one shared reactor more workers.
Include actual sender recovery and bound advertised listeners, not only recipient
loops and a best-effort send. Keep the genuine held-worker delivery control
alongside reconnect acceptance. [Reactor ownership](ai-coding-reliability.md#r02-thread-and-scheduler-progress)
also explains why direct observer work remains a watchdog limitation. Local
fixture success does not establish a historical lock cause or native qualification.
When tool identity is outside the assertion, reuse the existing
[tool-version fixture](../scripts/diagnostics/test-fixtures/tool-versions.mjs)
and verify that its probes were actually consumed. Keep the real command,
failure, cancellation and receipt assertions and their existing watchdogs.
When adding a local check, inspect subprocess fixtures that invoke its owner.
Supply unrelated external tools through private, observed fixture dependencies;
do not depend on developer-installed tools or add scanner setup to every runner
just to exercise scheduling. Keep real scanner acceptance in its existing job.
Exercise interrupted log finalization as well as child retirement through the
existing [process-owner controls](ai-coding-reliability.md#r03-actual-work-owns-its-lifetime).
Compare nested budgets before adding an outer subprocess timeout: several valid
inner probes can exhaust it before the behavior under test starts. A passing PR
run cannot qualify an unbounded environmental dependency for later main runs.
Do not use a fixed sleep to infer that background synchronization has happened.
Bind negative assertions to an actual post-change operation: for deletion replay,
prove the erased record was offered again and all pages were applied before
checking that it remains absent. A successful page, unchanged-digest response or
unrelated worker notification does not prove a complete replay. Keep deadlines
as failure bounds, and retain ownership and joins for admitted work on timeout.
Request that work through its existing coordinator and observe its publication
revision and terminal completion. A second hand-driven page loop can race the
normal worker and bypass its serialization, recovery and cancellation contract.
Capture immutable precondition evidence before admitting an operation that can
invalidate it. Published projections may legitimately become pending during a
commit; a post-admission read cannot prove the operation's initial role. Wait on
the existing preparation owner within the original setup deadline and fail
unexpected errors instead of retrying the whole operation.
Give counterfactual source copies their own Cargo target directory. Relative
package paths can collide across copies and reuse a mutated test executable;
the canonical candidate must never consume a negative control's build output.
Asynchronous progress notifications are observations, not readiness barriers. Cancellation
fixtures must bind readiness to still-live owned work after the required real
materialization, then assert its retirement before later admission or cleanup;
a bounded fallback may fail missing readiness but cannot manufacture a pass.

Executable fixtures must own an independent copied image, not a hard link to
the running test harness. Assert file identity before launch. After verified
process retirement, use the shared bounded filesystem cleanup helper; persistent
locks must still fail. Keep the Windows held-image failure and release controls
in `release-reuse.test.mjs`; cleanup retries never retry the test assertion or
substitute for process retirement.

Git-path and mode controls should construct Git objects when the host cannot
represent the input faithfully. Assert the actual delta and mode before checking
rejection; an empty change rejected for another reason is not a valid negative
control. Keep these cases on every platform instead of skipping them.

Browser commands use the existing process owner with a deadline inside the total
job budget, reserving time for joined cleanup and evidence upload. Keep streamed
logs and append-only active/completed-case progress when final reports cannot be
written. Timeout or uncertain cleanup remains a failure. Preserve verified closure of owned work as a diagnostic separately from the
actual child outcome: Windows
intentionally terminates even an empty job to close descendant admission.
On POSIX, verify the framework's independently grouped web server as well as the
CLI group. Use its supported graceful interruption protocol; a missing teardown
acknowledgment means cleanup remains unverified. Bind lifecycle markers to the
current invocation so old evidence cannot prove a later process stopped.
Do not await output-pipe closure indefinitely after the command leader exits.
The existing process owner bounds draining after group retirement; a separately
grouped descendant retaining stdout or stderr must yield a failed structured
result with unverified cleanup and a quarantined lease. Closing owned read ends
does not prove the escaped process stopped. Keep real natural-exit, cancellation
and no-success-receipt controls rather than signalling unknown process groups.
Authorize PID cleanup only from a complete private acknowledgment, not a numeric
prefix in a stdout chunk. Stream reads do not preserve message boundaries; a
failed readiness publication must retire the child through its creation owner.
Actual CLI regression fixtures need private output directories as well as private
source directories; parallel Playwright commands otherwise delete one another's
traces and manufacture failures. Missing hosted logs do not identify a slow case.
Local-only evidence fixtures must explicitly isolate and restore inherited CI
flags, and separately assert that real CI cannot reuse local acceptance evidence.
Health captures its absolute budget before setup and uses the existing stage
owner to stop diagnostics before the job watchdog. Preserve its structured result,
execution log and bounded Node lifecycle records, including the active file/case,
through the always-upload path. Prove interruption and joined cleanup with a real
parked command. Missing hosted logs require better evidence, not a guessed fix.
Native release stdout may omit application warnings. After actual child retirement,
the existing native capture owner retains bounded, redacted private application
file-log tails in the uploaded evidence. Prefer the current log within bounded
selection, reject linked/nonregular paths, close handles and report missing or
unreadable diagnostics without replacing the original failure. Do not collect
these logs through the IPC channel whose responsiveness is under investigation.
`native-capture.test.mjs` owns confinement, quotas, cutoff redaction and retirement
controls; file-log presence does not establish a backend failure cause.
Cancellation during preparation still needs a failed structured result. Only the
command owner can attest that admission never happened; failures after that
boundary must retain uncertain cleanup rather than manufacture a no-work result.

The [fixture timing guidance](testing-feedback-loop.md#cold-fixture-preparation-versus-measured-progress)
owns concrete examples and permitted boundaries.

Prefer bounded condition waits and meaningful failure diagnostics. Browser
actions should use [Playwright actionability and retrying assertions](https://playwright.dev/docs/actionability)
where applicable; application readiness may still need its own signal. Arbitrary
sleeps, broader timeouts, blanket retries and weaker assertions need evidence;
they are not default repairs for an unexplained failure. If a narrowly scoped
infrastructure retry is justified, keep its limit, original failure and outcomes
visible, and ensure repeating the operation is safe. Test retries still obey the
owning suite policy.

Keep asynchronous fixture discovery observations on the existing owned async
peer-cache API. A synchronous convenience snapshot can perform disk writes and
hide a cache failure behind an empty roster. The account-login fixture retains
one signed probe and its original five-second discovery budget, while a real
cache-path failure must surface the cache API's error chain before that deadline.
Retain the core cache writer's cancellation/profile-lease controls. A successful
rerun does not identify the mechanism of a previous missing-peer timeout.
A cached peer can already satisfy the identity predicate while a newly sent
query remains outstanding. The [restart fixture](../crates/mesh-talk-core/src/node/runtime_privacy_tests.rs)
also requires a response to a nonce issued by that invocation, from its exact
endpoint, with the actually accepted device/account binding. Keep the original
whole observation deadline, probe cadence and count; subsequent cooldown starts
after witnessed completion. Real warm-cache/retired-target and restart controls
must reject the cached-only shortcut. Test-only packet witnesses are opt-in,
instance-owned and bounded, expose omissions, avoid disabled formatting work and
retire real listener tasks. They add evidence, not network authority or a claimed
explanation of unobserved packet loss.

When a UI action moves into a menu, update the existing shared interaction helper
and its callers before publication. Exercise the real entry point; retain
functional, permission, focus and geometry assertions at the toolbar or popup
boundary they describe. The
[composer attachment helper](../frontend/e2e/helpers/session.ts) owns that shared
interaction; forced visibility and dropped assertions do not qualify the new flow.

Keep each observation inside a polling assertion nonwaiting when the assertion
already owns readiness. A locator operation can wait for a virtualized row and
consume the entire outer budget without producing one decode or geometry sample.
Inspect the trace to distinguish missing samples from unequal values; use one
browser-side DOM snapshot, treating an absent row as pending. Establish visible
fixture preparation before releasing held media bytes and starting decode checks.
Preserve exact geometry and cache assertions, and exercise the absent-row control
with a snapshot-only observer rather than extending the deadline. The media-scroll
remount regression owns this case. Run independent adversarial sampler controls
after the cache-remount journey, so they cannot consume its retention window.
The [group reading-anchor fixture](../frontend/e2e/group-reception.spec.ts) uses
the same nonwaiting page snapshot for its initial visible row. Capture its exact
identity and offset together; retain the outer poll budget and all subsequent
anchor, focus, unread and navigation assertions. A late successful observation
does not establish a missing-row defect or identify the source of driver latency.
Initial history preparation uses a browser-local observer for exact row identity,
text and visible geometry within the caller's original remaining budget. Keep
mutation checks immediate and a deduplicated frame fallback; absent/hidden rows
must fail and queued recovery must clean up even with frames held. Check the
monotonic deadline before and after sampling so delayed callbacks cannot accept
a late state. Preserve every later live/read, scroll and focus assertion.

The [memory guardrail](../frontend/e2e/memory.spec.ts) retains its seeded history,
25 complete conversation rounds, original watchdog and rendered-bubble ceiling.
Exercise a trusted pointer round, then activate the same DOM buttons through one
browser-side journey for repeated churn, with visible rows, exact active
destinations and loaded-history assertions after every activation. Retire each
bounded readiness observer's timer and callback. This isolates store/list
replacement from repeated driver round trips without invoking a store action
directly; the repeated phase does not qualify trusted-input latency. Preserve
the final pointer navigation and DOM bound; a deliberate extra-bubble control
must still fail. This fixture does not measure pointer latency or prove the cause
of hosted driver stalls.
Its [observer](../frontend/e2e/helpers/conversation-churn.ts) checks DOM commits
without waiting for an animation frame, retaining the frame fallback for CSS-only
readiness. A held-frame browser control must cover the original readiness timeout,
observer/frame cleanup and recovery. Retained trace timing is bounded per completed
activation; it does not qualify a late assertion after the whole-test deadline.
The owning browser selector includes both the scenario and its helper.

For a reproducible race or timeout fix, use a regression or negative control that
fails for the original mechanism and passes with the repair. Confirm that the
intended case actually ran. Preserve existing coverage while investigating; any
temporary quarantine needs an explicit owner, reason, tracking issue and removal
condition under the existing gate policy.

## Reject incomplete discovery and generated-policy drift

Successful test execution requires successful, complete input discovery.
`scripts/ci/fuzz-targets.py` compares the discovered cargo-fuzz list with the actual
manifest-owned target inventory before running anything. Failed enumeration, an
empty list, duplicates, omissions or unknown targets fail the job, as do individual
target failures. Keep this assertion at the execution owner; shell loop command
substitution alone does not propagate discovery failure under `set -e`.

`scripts/ci/glib-migration.py` evaluates every resolved glib branch, rather than
treating a newer parallel branch as proof that an older stack was removed. Its
migration claim conservatively requires stable 0.20 or newer throughout; absent
glib and mixed old/new trees do not establish migration. The
[RustSec advisory](https://rustsec.org/advisories/RUSTSEC-2024-0429.html) owns the
security version ranges; the watcher retains native build/test and reviewed-PR
requirements before changing dependencies.

Generated native input probes share one accepted-key/extended-flag metadata owner
between WebDriver mapping and emitted C# policy. Regressions must check that every
emitted key is accepted with the correct flags and unsupported keys remain
rejected, rather than only matching the generated call text. Ownership checks and
real desktop acceptance remain separate from generator/compilation checks.

## Review the change and prevent renewed drift

Follow the existing validation stages; do not invent a parallel completion
process. For changes affecting CI behavior or recurring cost, include the
following in the normal [alignment review](maintenance-checklist.md#review-and-follow-up-procedure):

- The acceptance boundary and retained owner, including consumers affected by
  deletion, merging, selection or schedule changes.
- The measured bottleneck, comparable before/after scope and remaining unknowns;
  distinguish observed latency, resource cost and reliability.
- Executed validation and source identity, with inventory/provenance checks or
  regression controls appropriate to the changed mechanism.
- Remaining hosted, cross-platform, native or release evidence and the owner of
  any follow-up. Do not claim candidate acceptance from local or older results.

Workflow, runner and tool upgrades can change CLI semantics, report formats,
selectors and archive compatibility. Validate affected real tool invocations and
failure paths through the existing diagnostic owners; YAML parsing or unit mocks
alone do not establish the behavior of the installed tool.

Consolidate frontend unit execution through the complete Linux coverage owner
when the ordinary and coverage commands select the same inventory and platform
branches use explicit mocks. Verify that inventory with the actual runner before
removing duplicate OS executions. Retain real browser/native matrices and native
diagnostic fixtures; mocked platform branches do not supply those contracts.
Keep Node/filesystem portability requirements in their owning diagnostics rather
than duplicating the entire frontend unit suite incidentally.

Keep platform-exclusive probes in the required native producer that can execute
them. Universal local selections must not include a file whose entire suite is
skipped on other hosts; preserve the nonempty-test guard and explicit native
escalation instead. Keep workflow wiring assertions in the cheap CI policy
owner, separate from the underlying tool's filesystem/compiler regressions.
Environment-specific cases must also distinguish unsupported local environments
from broken CI configuration. Skip only the new unsupported case, retain the
file's other checks, and declare its native escalation. A declared GitHub Linux
run must reach strict environment validation even when required image inputs are
missing; never turn that misconfiguration into a skip or fabricate hosted inputs.

After pushing, observe the actual hosted candidate until the relevant jobs finish.
Compare before and after setup time, test time, required-check latency and runner
minutes with sample sizes and cache conditions. Inspect first failures and queue
changes, then make another measured pass when evidence identifies remaining waste.
A pushed branch, a local green subset, or a faster isolated step does not establish
an end-to-end speedup. Keep the candidate reviewable and continue in a clean
worktree when unrelated local edits prevent publication; do not discard those
edits or treat them as a reason to stop the optimization.

Revisit timing and failure evidence when a gate becomes slower, starts rerunning,
gains another platform/dependency, or repeatedly exceeds its expected budget.
During existing maintenance reviews, inspect slow cases, duplicated setup,
shard imbalance, cache churn and abandoned temporary exceptions. Retire obsolete
work with its consumers and selectors rather than accumulating permanent layers.

Keep this review proportional: a documentation-only change needs applicable
formatting, links and structural checks, not a new build benchmark. Rules should
be enforceable through existing contracts and diagnostics where practical;
adding this guide does not create a scheduled job or another required check.

## Dependabot development acceptance

The existing exact-merge scope owner, `scripts/ci/docs-scope.mjs`, also admits
isolated npm dependency profiles for same-repository Dependabot bot PRs targeting
`dev`. Both manifests and lockfiles must already exist as regular files. Only
version changes to existing dependency entries qualify; scripts, overrides,
metadata, additions/removals, mixed paths, unmatched lockfile roots and changed
Tauri bindings retain full verification. Lockfile-only updates currently retain
full verification because their consumer boundary has not been qualified.

The `help` profile runs the public help/MCP tests, build and high-severity audit.
The `frontend` profile runs complete unit coverage, lint/types, production build,
built-JavaScript syntax checks, audit, Chromium theme/layout regressions and the
WebKit portable suite. The selected npm workflow independently checks its profile
against the actual merge. The Linux CI collector requires its success; failure,
cancellation, missing output or unexpected skipping cannot qualify. The legacy
`dev` required `verify` context collects the actual platform results. UI matrices
outside these profiles report not applicable, rather than pretending to execute.
Health retains source ownership/hygiene checks, without unrelated Rust or help
security scans blocking an isolated npm update.

Cargo, Actions and native binding updates retain complete acceptance. Main PRs,
merge groups, pushes, schedules, manual runs and release-source checks retain
full verification, apart from the separately qualified prose-only main PR rule.
The direct main/release check policy is unchanged. Do not classify by actor,
branch name, SemVer label or PR title alone. The existing real-Git scope controls
and CI policy fixtures own positive profiles, source/identity rejection and
failed/cancelled/missing selected-check evidence.

Automatic merge additionally requires verified Dependabot metadata for npm
minor/patch updates in these two directories, no Tauri dependency and no reported
maintainer change. Other updates have automatic merge disabled and require manual
review. The privileged metadata workflow never executes PR source; merge enablement
is bound to the event head. Coverage uploads requiring unavailable Dependabot
credentials remain excluded; tests and retained local artifacts are independent
of external coverage publication.

Deploy this policy through a reviewed main change, then align `dev` through a
reviewed integration before refreshing its dependency PRs. Existing `dev`
workflows cannot acquire fixes from main automatically. Preserve current required
checks; any protection migration uses its existing owner. Measure selected hosted
jobs after rollout, recording source, first failures, runner/setup/test time and
cache conditions. Local policy success is not a hosted latency or native claim.

Pinned native inspection downloads retain the exact version and SHA-256 owner in
`scripts/release/sevenzip.py`. Retryable transport failures may use the official
SourceForge distribution linked by the maintainer, within the same three-request
budget. Record bounded attempt/source/error diagnostics. Size, deadline, integrity,
installed-version and nonretryable HTTP failures remain fatal; never publish the
tool environment before successful verification. `release-sevenzip.test.mjs`
retains alternate-source, exhausted-budget and no-publication controls. Actual
installed-tool acceptance remains required on each consuming native platform.

### Synchronizing an accepted main source into dev

A pure main-to-dev synchronization introduces no product source beyond the exact
current main tip. The PR preparation owner may reuse the existing complete
trusted-main acceptance contract instead of retroactively applying the local
unit-only patch target to the entire development-branch backlog. This is limited
to `origin/dev` comparisons with a clean HEAD equal to the live remote main SHA.
`scripts/pr-platform/main-promotion.mjs` invokes the existing
`release/check-source-checks.mjs` owner: every required current-source check must
belong to its latest successful same-repository main-push workflow suite, with no
open main scanning alerts. PR-only, stale, pending, failed, skipped and wrong-suite
evidence cannot authorize promotion.

The verifier rechecks live main and local source after proof collection, bounds
network work and retires child processes through the existing process owner.
Failure, cancellation, changed source and unavailable evidence reject promotion.
This does not lower the configured patch target for ordinary new changes or
replace dev's current-head protected integration checks. Large ordinary diffs
remain fully measured within a bounded 32 MiB Git output budget; over-budget
input fails rather than becoming an empty or truncated patch.
