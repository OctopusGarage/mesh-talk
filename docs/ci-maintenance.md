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

Stop an already failed candidate before starting another expensive compilation
partition. Linux/macOS native testing finishes all selected Host cases, then
admits the remaining complete workspace only if Host passed. Both partitions
still have to pass before reporting coverage; per-partition terminal result
collection, independent matrix jobs, default targets and zero retries remain
unchanged. The existing actual-tool coverage regression checks Host rejection,
remaining-partition rejection, successful coverage accumulation, timing pipelines
and report errors. Preserve partial backend evidence after a failure, while
requiring evidence on passing jobs; a producer skipped by an upstream failure
must not create a second missing-artifact error.

Keep complete tests distinct from duplicate instrumentation. macOS instruments
Host for its native desktop/updater branches, then runs every remaining default
workspace target with pinned Nextest without coverage instrumentation. Linux and
Windows retain full instrumented workspace coverage. This removes the separate
instrumented Core test build on Apple Silicon; it does not claim full macOS Core
line coverage. The actual-tool regression executes both workflow blocks, requires
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
The real-runner control derives both configured watchdogs, accelerates only their
duration, requires actual parked-case failure and subsequent-case execution, and
verifies descendant retirement before removing its fixture.

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

Download pinned prebuilt development tools through the existing installer where
available. Compiling tools with `go install` or `cargo install` is recurring setup
work; retain source installation only when the required tool has no suitable
trusted binary or the test explicitly validates its build.

Build twice consecutively with identical source, profile, features and environment.
Inspect unexpected second-build work before enlarging caches or runner counts:
missing `rerun-if-changed` inputs, generated files that change every invocation,
unnecessary frontend rebuilds, and container layers that copy source before
installing locked dependencies. A bundler may intentionally rebuild quickly;
judge the observed cost instead of requiring it to behave like Cargo's no-op build.

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
unexpectedly skipped required work. An `always()` condition schedules a collector;
it does not validate its dependencies. See [required-check troubleshooting](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).

Before adding docs-only path filters, inspect actual branch protection and the
app-bound checks it requires, including external coverage and security checks.
Define which documentation is inert: generated help, executable examples, eval
prompts, fixtures and policy inputs can change product or acceptance behavior.
Keep applicable documentation checks. Use workflow-level filters only when no
required consumer will wait for their missing result. A lightweight selection or
collector job must retain the required check identity and reject failed selection;
it must not fabricate coverage or security results belonging to another app.
Preserve complete main-commit verification required by release consumers even
when a PR has a separately validated docs-only path.

The UI HTML collector follows the mandatory full-shard producers, independently
of the optional extended-browser jobs. If Health prevents desktop shards from
starting, their missing reports remain incomplete evidence. The collector checks
all four current-attempt shards per OS; fixing the upstream failure and rerunning
the selected producers supplies that evidence, rather than omitting those OSes.

## Bound parallelism and preserve isolation

Run independent checks in parallel, but inspect each `needs` edge before removing
it: artifact availability, coverage provenance and deliberate runner admission
are real dependencies even when no explicit job output is passed. Put portable
formatting, source lint and dependency auditing on Ubuntu. Retain macOS/Windows
execution when assertions exercise their native behavior. Count macOS jobs across
all workflows against the account's actual concurrency allowance; moving one
workflow's bottleneck into another workflow's queue is not an improvement.

Choose shards from observed work and available runner capacity. Count setup and
artifact overhead as well as execution. More shards can increase queueing and
runner minutes; more workers per machine can increase memory pressure, port
exhaustion and test interference. Review the product of platform, architecture,
browser, feature and shard dimensions before expanding a matrix.

macOS browser coverage uses `macos-26-intel` for WebKit and Chromium, including
extended scenarios. Its contract is the macOS/browser behavior; the ARM workspace
and real native app/CLI checks retain `macos-latest`. Keep artifact labels and
per-OS collectors aligned with the actual selected image. Avoid coupling browser
admission to scarce ARM capacity needed by native builds. A job that never obtains
a runner has no test result; preserve its infrastructure failure separately.
UI coverage and reports require one complete attempt, so recover that workflow
as a whole when an unallocated producer prevents its current-attempt evidence.

ASan uses eight serial Nextest partitions to reduce the measured uneven execution
tail of the former four partitions. Each runner retains the same case budgets,
instrumented standard library and clean/error controls. The required collector
checks the exhaustive disjoint inventory and current-attempt execution receipts;
additional shards do not permit omitted cases or retries. Include the extra
compilation and queue time when comparing hosted performance.

Retain fresh process/context isolation, owned temporary directories, dynamically
allocated ports where supported, and deterministic cleanup. Keep compile jobs,
test workers and network/rendering resource limits explicit. Do not multiply
them independently and assume the machine can absorb the combined load.
[Nextest archives](https://nexte.st/docs/ci-features/archiving/) can separate build
and execution, while [Playwright sharding](https://playwright.dev/docs/test-sharding)
distributes browser work. Archive consumers need the same source revision and
required runtime assets; shard distribution does not guarantee equal durations.
Both need compatible inputs and complete inventories, with isolation preserved.

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

Separate expensive fixture preparation from the operation's measured window when
preparation is outside the requirement. Continue using the real signing,
persistence, transport or UI owner when it is part of the assertion. Preserve
production deadlines and real lifecycle coverage where those are the subject of
the test; changing the clock or injecting state cannot silently remove them.
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
