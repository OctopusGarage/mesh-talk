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

## Reuse compatible work with verifiable inputs

Prefer one producer for an expensive output when consumers need the same inputs
and artifact transfer is cheaper than rebuilding. Include the producer dependency
and its effect on parallelism in the measurement. Reuse compilation separately
from test execution: each required consumer still supplies its own assertions.
Keep production, coverage, sanitizer and mutation configurations distinct unless
their compatibility has been established.

A cache is an optional accelerator. A cold or evicted cache must still allow
correct execution. Measure restored bytes, restore/save time, useful hits and
eviction; a reported hit alone does not prove a faster build. Preserve the
existing trust boundaries and write policy. GitHub documents cache matching,
branch access and eviction in its [dependency caching reference](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching).

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

The UI HTML collector follows the mandatory full-shard producers, independently
of the optional extended-browser jobs. If Health prevents desktop shards from
starting, their missing reports remain incomplete evidence. The collector checks
all four current-attempt shards per OS; fixing the upstream failure and rerunning
the selected producers supplies that evidence, rather than omitting those OSes.

## Bound parallelism and preserve isolation

Choose shards from observed work and available runner capacity. Count setup and
artifact overhead as well as execution. More shards can increase queueing and
runner minutes; more workers per machine can increase memory pressure, port
exhaustion and test interference. Review the product of platform, architecture,
browser, feature and shard dimensions before expanding a matrix.

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
groups that do not accidentally couple unrelated workflows or branches. Preserve
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

Revisit timing and failure evidence when a gate becomes slower, starts rerunning,
gains another platform/dependency, or repeatedly exceeds its expected budget.
During existing maintenance reviews, inspect slow cases, duplicated setup,
shard imbalance, cache churn and abandoned temporary exceptions. Retire obsolete
work with its consumers and selectors rather than accumulating permanent layers.

Keep this review proportional: a documentation-only change needs applicable
formatting, links and structural checks, not a new build benchmark. Rules should
be enforceable through existing contracts and diagnostics where practical;
adding this guide does not create a scheduled job or another required check.
