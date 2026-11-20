# Development roadmap and requirements

## Plan ownership and execution boundary

This is the maintained global plan for the thirty user-supplied requirements,
REQ-001–REQ-030. It owns consolidation, priority, dependencies, phase exits,
readiness, task claims and future directions. The original intake is provenance,
not a competing execution backlog. The complete source mapping below preserves
all thirty requirements. The previous T-numbered execution proposal is superseded;
its numbers or historical claims are not evidence of readiness in this plan.
Existing work owned by other sessions must still be checked before claiming a task.

The delivery order is **foundations → integrated foundation acceptance → current
IM improvements → separately activated future features**. Performance, security,
verification and maintained decisions apply throughout. Forum, mobile PWA,
marketplace, external SDK and other extensions are not current assignments.

This planning change updates [agent rules](../AGENTS.md) and this roadmap together.
It must pass the existing PR/review/current-head gates and merge into `main`
before execution starts. After merge, read the accepted main revision afresh,
claim F01 and follow the dependency rules below. A planning merge does not prove
that foundations, measurements or product acceptance are complete. No public
release or product implementation belongs in the planning PR.

[Implementation status](../specifications/TODO.md) owns implemented capabilities;
[module ownership](module-boundaries.md) owns implementation responsibilities.
Existing topic guides retain authority over product/security/resource rules.
Do not invent historical rationale, measurements, deadlines or effort estimates.
Unknown implementation facts are explicit inputs to bounded investigations.

## Priority and readiness

| Priority     | Scope                                                                             | Admission rule                                                                                                                                                                        |
| ------------ | --------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P0           | F01–F10: finite foundations for current IM, including performance and validation. | First F01; then independent evidence/contract lanes; construction follows accepted relevant inputs. Verified urgent security, data-loss or hard-freeze defects override normal order. |
| P1           | M01–M04: practical current-messenger improvements after foundation acceptance.    | F10 and each task's product/interface decisions accepted; independent ready tasks may run together.                                                                                   |
| Roadmap only | Future capabilities and conditional platform replacements.                        | No implementation claim until a concrete scenario, product decision, relevant contract, budget and first-slice acceptance are recorded. Their dependencies do not enlarge P0.         |

Priority is program order, not a claim that every foundation change has identical
risk/value. Within construction, rank findings by measured user impact, correctness
risk and enabling value. Choose retain/qualify, targeted fix, feature or experiment
from evidence. A no-change decision can satisfy a task when its acceptance is met;
otherwise name the unverified scenario and keep it open.

## Delivery policy

1. Define a finite foundation scope around current IM scenarios and real reuse.
   Do not implement speculative forum, browser or SDK architecture in advance.
2. Establish reproducible correctness and performance evidence before selecting
   major optimization, module extraction, database or UI changes.
3. Make boundaries useful: one policy owner, explicit inputs/results/errors,
   lifecycle and cancellation, durable commit authority and bounded resource use.
   A file split alone is not a completed module boundary.
4. Deliver and validate small changes to existing consumers. Preserve identity,
   privacy, wire/storage compatibility, delivery and recovery guarantees.
5. Accept the agreed foundation release before opening feature development.
   Severe verified security, data-loss or hard-freeze defects take precedence
   whenever discovered. Optional future ideas do not extend the foundation gate.
6. Keep performance, verification and decision records active throughout later
   work. Foundation acceptance does not mean these concerns are permanently done.

## Phases and exit criteria

| Phase                                 | Purpose and scope                                                                                                                | Exit evidence                                                                                                                                                           | Parallelism                                                                                                      |
| ------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| 0. Bound the foundations              | F01: current concepts, owners, consumers, stores and critical scenarios; identify gaps without assuming defects.                 | Accepted responsibility map, existing/reusable/unknown distinctions, bounded foundation scope and authoritative guide for each work package.                            | Read-only inventories may run independently; one coordinator reconciles shared concepts.                         |
| 1. Establish evidence and contracts   | F02 verification baseline, F03 runtime baseline, F04 authority/lifecycle/interface contracts.                                    | Reproducible scenarios, representative timing/failure evidence, correctness checks and a ranked, scoped foundation change list. Unknowns have an owner and next action. | F02/F03/F04 can proceed together after relevant F01 inputs. Measurement windows stay free from build/test noise. |
| 2. Build and optimize the foundations | F05 module boundaries, F06 events/scheduling, F07 communication/sync, F08 storage/lifecycles; F09 protects each delivered slice. | Current consumers use accepted boundaries; selected costs improve or current behavior is retained with evidence; failure/recovery and compatibility remain qualified.   | Integrate F05 interfaces incrementally, then admit independent F06/F07/F08 slices. F09 accompanies every slice.  |
| 3. Accept the foundation release      | F10: qualify the agreed current-IM scenarios across the affected boundaries.                                                     | Foundation acceptance checklist met; severe risks resolved; remaining bounded gaps explicitly accepted or owned. No implied public release or publication.              | Independent acceptance scenarios may run together if their environments and data do not interfere.               |
| 4. Improve the current messenger      | Approved monitoring UI, practical anti-harassment, manual QR and package-size slices.                                            | Each feature meets its own contract, failure/recovery and resource criteria using the accepted foundations.                                                             | Separate feature lanes after F10, with one owner for any shared interface.                                       |
| Future roadmap                        | Browser review, meetings, PWA, forum/feed, alternative clients/market, external SDK and incentives.                              | Separate product decision, relevant foundation evidence and an independently testable first slice before activation.                                                    | No current assignments. Future independent features may run together after their own gates.                      |

Phase progression is evidence-based, not calendar-based. Existing working
capabilities can satisfy a package through inspection and qualification; every
package is not a mandate to replace its implementation.

## Foundation work packages and dependencies

F identifiers name the foundation program. They do not convert previous task
states. All tasks begin **unclaimed and unstarted** until the execution cycle
begins from the merged planning baseline. A dependency requires an accepted
relevant output, not the completion of an unrelated whole document or subsystem.
For construction packages, select one bounded real-consumer slice per delivery
branch; a broad package is not permission for an all-app rewrite.

| Package                                             | Outcome / scope                                                                                                                                                                          | Required inputs                                                                                               | Acceptance / boundary                                                                                                                                                                                     |
| --------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| F01 — Concepts and responsibility map               | Explain current business concepts, module responsibilities, real callers, stores and extension points. Identify authoritative rules and existing evidence.                               | The thirty source requirements and later authorized implementation inspection.                                | Each foundation package has an owner, real consumers, invariant and inspection gap; no blanket refactor or duplicated inventory.                                                                          |
| F02 — Effective and timely verification             | Map local/CI/native/release checks; measure waiting, setup, compilation, execution, failures and retries; assess actual assertion gaps.                                                  | Relevant F01 ownership and scenarios.                                                                         | A reproducible baseline and check-allocation proposal preserving meaningful failure detection. Evaluate supplied methods explicitly; not every tool becomes a mandatory gate.                             |
| F03 — Runtime baseline and attribution              | Measure startup, idle, active IM and return to idle, including app-related processes, CPU/memory/disk/network and available energy/wakeup evidence. Inventory reusable observations.     | Relevant F01 workload/store/process owners.                                                                   | Comparable build/platform/data/window provenance, resource attribution, ranked findings and measurement limits. No assumption that timers, SQLite or UI technology are the root cause.                    |
| F04 — Authority, lifecycle and interface contracts  | Specify identity/privacy/permissions, personal and group sync, announcement/member distinctions, pending/effective state, retention/deletion/recovery and operation lifetime.            | Relevant F01 ownership and current behavior inspection.                                                       | Resolve rules needed by the selected foundation slices; name remaining decisions. Business-specific authority stays explicit. No preselected Raft, global deletion, anonymity or external SDK.            |
| F05 — Useful module boundaries                      | Clarify/extract the necessary interfaces and migrate actual existing consumers. Separate core policy, host adaptation and presentation; define transport/sync/business responsibilities. | Relevant F01/F04 outputs, an adequate F02 safety check and F03 baseline for affected performance paths.       | One authoritative policy owner, explicit errors/cancellation/commit boundary and tested real consumers. Retain good existing seams. No SDK packaging or cosmetic file-count target.                       |
| F06 — Event and scheduling governance               | Reuse meaningful events and shared scheduling admission; remove evidenced duplicate polling/work; preserve required timeout, retry and recovery.                                         | Relevant F02/F03/F04 outputs and F05 interfaces used by the slice.                                            | Trigger, coalescing, bounded work, progress, cancellation and return-to-idle evidence; no duplicate legacy worker left active. A single universal scheduler is not presumed necessary.                    |
| F07 — Communication and synchronization foundations | Qualify and improve needed connection/transport/sync/delivery boundaries; personal replay, group propagation/recovery, announcements and member authority remain distinct.               | Relevant F02/F03/F04 outputs and F05 interfaces used by the slice.                                            | Agreed duplicate/reorder/partition/restart/revoke/compatibility scenarios with bounded resource behavior. Share transfer mechanisms without erasing business-specific permissions or merge rules.         |
| F08 — Storage and resource lifecycles               | Improve selected incremental I/O, startup/history/search, cache/memory reclamation, retention/capacity and deletion behavior.                                                            | Relevant F02/F03/F04 outputs and F05 interfaces; accepted F07 rules for sync-range/deletion-dependent slices. | Measured targeted costs and repeated recovery/return-to-idle checks. Separate local visibility, durable deletion, account decisions and recall. Marker reclamation requires safe replay/restore evidence. |
| F09 — Regression protection and maintained rules    | Bind each foundation change to reusable correctness/performance scenarios, efficient validation selection, AI reuse guidance and supported decision rationale.                           | F02/F03/F04 plus each selected F05–F08 delivery.                                                              | Tests detect relevant failures, resource observations are attributable, selectors/docs match behavior and recurring verification cost is recorded. Deliver continuously, not as a final testing backlog.  |
| F10 — Foundation acceptance                         | Integrate accepted packages and verify current IM behavior, performance, authority, lifecycle and reuse on the agreed source/platform scope.                                             | Accepted F01–F09 outputs for the agreed scope.                                                                | The checklist below is satisfied. This gate opens feature planning/development; it does not publish a build or complete future features.                                                                  |

### Sequencing within foundation construction

- Start with F01, then prepare F02/F03/F04 in parallel. Do not wait for an
  elaborate monitoring UI to collect performance evidence.
- Select the smallest useful F05 boundary changes. Publish their contract and
  migration order before other packages change the same callers or state owners.
- F06/F07/F08 can overlap once their required interface slices are accepted.
  Communication correctness improvements may proceed independently from history
  pagination; deletion semantics cannot proceed independently from replay rules.
- Pair each module/interface change with qualification of its existing consumer;
  optimize measured costs in the same or a following bounded slice. Do not defer
  every performance improvement until after an app-wide refactor.
- Deliver F09 with each slice. Assess F10 only after all mandatory scoped
  foundation outputs are accepted. Reopen an upstream contract deliberately when
  evidence invalidates it; do not silently patch incompatible rules downstream.

## Execution waves and concrete handoffs

After the planning PR merges, use these waves for dispatch. Each accepted output
is reusable; no lane must finish an exhaustive survey before another lane can
make a bounded change. Current claims remain separate from this readiness map.

| Wave                                  | Work admitted                                                                                                                                                                                                                   | Required handoff before the next wave                                                                                                                                                                                                              |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A. Bound the current-IM program       | F01: identify account/device/session meanings, current messaging/sync/file/history flows, storage categories and real owners. Select representative scenarios below.                                                            | One scoped list separating existing qualified behavior, unknowns, mandatory fixes/qualification and parked extensions. Assign a decision owner, not an assumed new module, to every boundary.                                                      |
| B. Prepare safe, measurable changes   | F02 minimal affected safety checks and timing/failure baseline; F03 reproducible resource observations; F04 the authority/lifecycle rules needed by the selected scenarios. These three lanes may overlap.                      | A ranked first change list. Each entry names its user impact or measurable cost, invariant, actual owner/consumer, accepted input contract, test/recovery evidence and stop condition. Broad verification-method assessment continues through F09. |
| C. Integrate small foundation slices  | Begin with the F05 seam needed by the highest-impact current-IM change; then execute independently ready F06/F07/F08 slices, each with F09. Keep instrumentation only where it supports diagnosis/regression with bounded cost. | One reviewed result per slice: change or retained behavior, migrated consumer, comparable resource result where affected, exercised failure/recovery cases and explicit remaining limits. Revisit the finding order after each meaningful result.  |
| D. Qualify the integrated foundations | F10 rechecks the accepted scenarios on the integrated candidate and closes cross-lane compatibility gaps.                                                                                                                       | A clear accepted-scope result and residual-risk disposition before any post-foundation feature is activated. No automatic release, SDK publication or future-feature assignment.                                                                   |

The first construction slice is not preselected as a timer rewrite,
database migration or networking replacement: requirement concerns do not prove
which mechanism is costly. Use measured user impact, risk and dependency value to
choose it. A clear high-impact optimization can precede unrelated module cleanup,
provided its own owner/contract and safety checks are understood. F05 is incremental,
not an app-wide rewrite that must finish before performance work can begin.

### Required scenario inventory

This inventory bounds foundation qualification; it is not executed evidence or
a requirement to create a new suite for each row. F01–F04 must bind actual owners,
supported platforms, representative scales, observation windows and expected
outcomes before accepting construction scope. Reuse adequate checks and record
gaps. Unsupported or unavailable environments need an owner and explicit risk
acceptance; do not silently mark them passed.

| Scenario                                         | Correctness and recovery observation                                                                                      | Cost / lifecycle observation                                                                                                        |
| ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| Startup and unlock with retained history         | Usable current conversation, correct authority and durable history; interrupted initialization has a defined outcome.     | Cold/warm identity, time to usable UI, peak working set and reads/writes; separate migrations and catch-up.                         |
| Settled visible and hidden idle                  | Message reception and required liveness remain available without unrelated history/setting reloads.                       | App process family, wakeups/CPU, disk/network work and unbounded queue/metadata growth; observe after real settling.                |
| Direct and group messaging                       | Durable acceptance versus custody/recipient evidence, duplicate handling and policy checks remain distinct.               | Interaction/reception latency and work/bytes as the chosen recipient scale changes; settle after delivery or bounded blocked retry. |
| Linked-device and group catch-up                 | Relevant duplicate/reordered/interrupted replay, membership/revocation and legitimate missing-history recovery.           | Incremental transfer, progress and backpressure; no unchanged replay loop after completion.                                         |
| History, search and repeated navigation          | Bounded current view and correct access; stale results cannot replace another conversation/session.                       | Query/decode/render work and cache growth across repeated use, including return to idle.                                            |
| Attachment transfer and media use                | Chosen consent, cancellation, failed-source/retry and integrity/completion cases.                                         | Transfer/decode bounds, temporary resources and memory/task cleanup after leave or replacement.                                     |
| Delete, restart and restore                      | Agreed local/account/recall intent survives the relevant replay/recovery cases; unauthorized scope expansion is rejected. | Retained deletion evidence, index/marker growth and safe reclamation limits. No age-only marker deletion assumption.                |
| Lock, logout, sleep/resume and owner replacement | Stale work loses commit authority; required recovery remains possible when the owner returns.                             | Work/subscriptions stop with their owner; background retry, cached state and outstanding workers have bounded lifetimes.            |

### Parallel admission and integration rule

Read-only analysis may overlap. Before code changes, each slice must have an
accepted shared contract, nonoverlapping mutation ownership, an existing-consumer
migration plan and an integration order. F07/F08 share persisted sync/deletion
rules; F06/F07 share retry/progress ownership; metric and validation lanes share
measurement resources. Those are coordination boundaries, not permission for
concurrent conflicting edits. If an interface is still moving, parallelize
consumer/test preparation read-only or serialize the dependent mutations.

Do not turn a quiet-host measurement window into a blanket ban on parallel work:
other lanes can continue on separate machines or noninterfering inputs. Do not
interpret unlimited worker availability as unlimited review/native-test capacity.
After each merge to the integration branch, dependent branches consume the
accepted new interface before final qualification.

## Mandatory versus optional scope

Use this boundary to make F10 finite. These classifications define the
program scope, not assertions that a capability is currently missing or broken.
An existing implementation may satisfy the mandatory outcome through evidence.

| Area                           | Mandatory foundation outcome                                                                                                                           | Separate later feature or conditional work                                                                                  |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------- |
| Module boundaries              | Existing IM consumers have clear responsibility, policy ownership and lifecycle at the affected seams.                                                 | A generic framework for hypothetical consumers, public SDK packaging or arbitrary extension points.                         |
| Performance                    | Establish representative baselines, address the agreed high-impact findings and protect against recurrence.                                            | Unlimited micro-optimization, new UI implementation or an unmeasured rewrite.                                               |
| Monitoring                     | Obtain enough attributable observations to explain and verify the selected costs, including disabled-mode overhead.                                    | Polished panel/menu-bar views, persistent chart history and user alerts.                                                    |
| Sync and transport             | Qualify current personal/group exchange, authorized recovery, progress/resource bounds and compatibility in the agreed scenarios.                      | New transport families, global consensus, forum replication or browser gateways without a current-IM need.                  |
| Announcements and membership   | Keep their authority/state semantics separate; define the transitions required by the selected current-IM scope, including pending/effective meanings. | A new announcement editor/history/confirmation UX solely to complete an architecture program.                               |
| Retention and capacity         | Explain existing data categories, authority, limits, cleanup/recovery and their measured growth.                                                       | New quota-management screens, notification UX or automatic cleanup policies not yet agreed.                                 |
| Deletion                       | Qualify the agreed local/account/recall meanings against relevant replay/restart/restore cases; preserve required deletion authority.                  | Marker compaction without measured need and a safe reclamation argument.                                                    |
| Privacy and unwanted contact   | Establish identity, disclosure/admission and revocation boundaries; fix severe verified enforcement defects.                                           | New stranger-request UX, broad product policy modes or endorsement counts. Practical controls remain prioritized after F10. |
| Verification and documentation | Maintain risk-based checks, resource regressions, source-bound evidence and discoverable rules/decisions.                                              | Every named atlas tool on every PR, an exhaustive competitor survey or a second documentation system.                       |

If investigation reveals that a deferred interface is required to make an
existing invariant usable or enforceable, promote only that necessary slice with
its rationale and acceptance. Conversely, a desirable new feature does not become
mandatory merely because it could reuse a foundation module.

## Foundation acceptance checklist

The foundation program is complete when the agreed outputs below are accepted,
not when every hypothetical architecture improvement has been attempted.

- **Ownership and reuse:** current consumers use the accepted interfaces; no
  competing policy owner or abandoned duplicate implementation remains.
- **Resource behavior:** comparable startup/idle/active/return-to-idle results
  meet the agreed budgets or an explicitly reviewed exception. Work queues,
  caches and retained metadata have limits and reclamation/recovery rules.
- **Sync and durability:** agreed current-IM cases cover interruption, reconnect,
  duplicate/reordered data, membership/revocation, deletion, restart and restore;
  outstanding topology/platform limitations are named.
- **Privacy and authority:** account/device meanings, accepted operations and
  observer/peer/relay exposure are explicit and enforced at the owning boundary.
- **Verification:** checks have meaningful assertions and local/CI/native roles;
  feedback latency and failure causes are observable. Missing evidence is not a pass.
- **Maintainability:** current behavior and important decisions are documented at
  their owners; future features can locate the contract and its regression checks.
- **Residual risk:** severe security/data-loss/recovery defects are resolved.
  Other gaps have an impact, owner and explicit disposition. Optional extensions
  cannot indefinitely delay foundation acceptance.

Numeric targets, representative data sizes, platforms and the mandatory scenario
set must be agreed from F01–F04 evidence before construction scope is accepted.
The existing [performance requirements](performance-requirements.md) and
[testing workflow](testing-feedback-loop.md) remain their authoritative owners;
this roadmap introduces no competing budget or gate policy.

## Current-IM work after foundation acceptance

M tasks remain unclaimed until F10 and their own decisions are accepted.
Their order below is the default priority within P1; independent ready work may
run together without consuming a constrained higher-priority owner. Package-size
attribution can be researched earlier without changing artifacts; optimization
still needs affected runtime/install/update evidence.

| Lane                                            | Sources          | Foundation inputs and remaining product choices                                                                                                                                    | Conflict boundary                                                                                                          |
| ----------------------------------------------- | ---------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| M01 — Practical anti-harassment                 | REQ-025          | F04 identity/permissions and F07 enforcement. Decide account/device blocking, stranger policy, group effects and linked-device behavior.                                           | Core enforcement and UI presentation have distinct owners; endorsements are not an input dependency.                       |
| M02 — Monitoring panel and native compact entry | REQ-001, REQ-016 | Reusable F03 metrics, F06 lifecycle, F09 overhead checks. Decide units/freshness, acquisition defaults, history and panel/tray relationship. Extra collection must be disableable. | One metric pipeline; no separate collector for each view or whole-machine-monitoring expansion.                            |
| M03 — Manual image QR                           | REQ-010          | F05 image/contact interfaces, bounded F06 work and F04 sensitive-content rules. Decide supported cards and multiple/invalid-code behavior.                                         | Explicit image-menu/preview trigger; display results before chosen action; no background scanning or automatic navigation. |
| M04 — Package-size optimization                 | REQ-014          | F01 dependency/resource inventory and F02/F09 packaging checks. Compare same OS/architecture/format and record installed size separately.                                          | Coordinate shared dependency/build owners; no forced 10M target or assumed UI migration.                                   |

## Future roadmap and activation conditions

Keep these directions visible without assigning dates, workers or immediate
implementation. F10 is the common program gate. Each direction also needs the
specific decisions below; completing foundations alone does not settle them.

| Direction                           | Sources          | Additional activation conditions and first-slice boundary                                                                                                                                                                            |
| ----------------------------------- | ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Browser sharing/review              | REQ-002          | Select formats, link/link+code access, separate permissions, comments/revisions and host availability. Start with no-install viewing/comments; simultaneous editing remains later.                                                   |
| Group voice meetings                | REQ-023          | Agree participant scale, voluntary join, member admission, initiator exit and media/native resource evidence. Video and independent rooms are later separate decisions.                                                              |
| Mobile PWA                          | REQ-027          | Define real mobile scenarios, supported browser limits, authorized-node pairing/key ownership, reachable transport and offline behavior; assess suitable existing work when activated. No assumed JS rewrite or direct browser mDNS. |
| Decentralized forum and forwarding  | REQ-019, REQ-024 | Resolve product form, audience/propagation consent, post/comment provenance, three-day disappearance/manual pinning, local versus retained copies and selection costs. Hot/latest/own counts remain proposals.                       |
| Private feed and profile visibility | REQ-026          | Selected-contact audience per post and independent profile visibility; define later audience changes, interactions and retained/offline copies. Do not inherit forum TTL.                                                            |
| Alternative interaction clients     | REQ-021          | Define a supported core/client contract and real alternative consumer, lifecycle and compatibility; service versus embedded form remains open.                                                                                       |
| Client catalog/marketplace          | REQ-021          | First qualify the client contract and an actual alternative adapter; then choose listing versus installation, publisher trust, compatibility, update and revocation.                                                                 |
| External Rust SDK                   | REQ-020          | A real external Rust consumer, minimal supported surface, storage/runtime/key ownership and compatibility obligations. Internal modularity does not itself require public packaging; bindings remain later.                          |
| Trusted-contact endorsements        | REQ-025          | Demonstrated contact-trust need, attestation meaning, visibility/revocation and abuse/privacy evidence. Counts never automatically grant contact rights.                                                                             |
| Optional-resource incentives        | REQ-028          | An actual optional file/forum/cache allocation problem, useful contribution evidence and bounded benefits with anti-gaming/fairness costs. Protect basic chat and content authority.                                                 |
| Alternative desktop UI              | REQ-012          | After qualifying current-stack optimization, show a remaining material problem through equivalent workloads and platform/IME/accessibility parity, maintenance and migration/rollback costs.                                         |
| Peer-tool opportunity research      | REQ-029          | Choose a concrete unmet scenario and verify primary sources when activated. Reference lists, quoted rankings and radio ideas are leads, not feature commitments.                                                                     |

Future branches may proceed independently when their own inputs are accepted.
The important additional paths are: client contract → actual adapter → marketplace;
real optional-resource service/problem → incentive experiment; voice qualification
→ a separate video decision. Forum, PWA and voice are not dependencies of one
another merely because they share communication foundations.

## Consolidation and complete source mapping

Keep REQ identifiers for traceability. Shared investigation does not merge
independent business authority, user policies or acceptance conditions.

| Source  | Disposition / execution owner                                                                                                                             |
| ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| REQ-001 | Performance/source inventory in F03; monitoring UI after F10.                                                                                             |
| REQ-002 | Future browser viewing/comments; editing later.                                                                                                           |
| REQ-003 | Merge with REQ-030 as F02 verification effectiveness/latency and F09 protection.                                                                          |
| REQ-004 | F01 concepts/responsibilities and targeted F05 boundaries.                                                                                                |
| REQ-005 | F06 events/scheduling, with F09 AI reuse rules.                                                                                                           |
| REQ-006 | F03 attribution and selected F08 incremental I/O/history work.                                                                                            |
| REQ-007 | Current stack/store inventory in F01; tuning in F08 if evidenced. Replacement is a separate conditional decision.                                         |
| REQ-008 | F04/F05/F07 internal foundations; no early external SDK.                                                                                                  |
| REQ-009 | Share F01 investigation with REQ-004; maintain existing technical guides through F09.                                                                     |
| REQ-010 | Independent manual QR candidate after F10.                                                                                                                |
| REQ-011 | Cross-cutting F03/F09/F10 resource quality, sustained during later delivery.                                                                              |
| REQ-012 | Future UI decision using qualified runtime evidence; no foundation migration mandate.                                                                     |
| REQ-013 | F04 privacy/exposure model; selected required protections in foundation construction. Stronger anonymity remains a trade-off decision.                    |
| REQ-014 | Comparable artifact attribution can be independent; selected size optimization after F10.                                                                 |
| REQ-015 | Distribute communication to F07, lifecycle to F08 and performance to F03/F09; remove the duplicate umbrella rewrite. SDK remains future.                  |
| REQ-016 | Share diagnosis/budgets with REQ-001/011 in F03/F09; monitoring feedback after F10. User alerts remain a separate choice.                                 |
| REQ-017 | Split startup/history, memory, retention/capacity, sync scope, deletion preservation and marker cost across F04/F07/F08. No unapproved automatic cleanup. |
| REQ-018 | F04/F07: personal sync, group sync, announcements and member permissions are separate contracts; pending/effective distinction retained.                  |
| REQ-019 | Merge with REQ-024 in future forum planning; external copies/publication stay optional separate choices.                                                  |
| REQ-020 | Future externally consumed Rust SDK, activated by a real consumer.                                                                                        |
| REQ-021 | Current internal client boundaries in F05; alternative clients and marketplace remain future separate stages.                                             |
| REQ-022 | Decision reasons/status/revisit conditions throughout F01–F10 and later work; use existing guides.                                                        |
| REQ-023 | Future existing-group voluntary voice; video/independent rooms later.                                                                                     |
| REQ-024 | Future forum forwarding/selection/expiry under REQ-019; do not apply these rules to chats.                                                                |
| REQ-025 | Foundation identity/permission rules in F04; practical anti-harassment after F10; endorsements future.                                                    |
| REQ-026 | Future private per-post sharing and independent profile audience.                                                                                         |
| REQ-027 | Future mobile PWA with its own identity/browser/transport decisions.                                                                                      |
| REQ-028 | Future optional-resource incentive experiment; basic chat remains protected.                                                                              |
| REQ-029 | Future scenario-driven research, not a required survey before current IM improvements.                                                                    |
| REQ-030 | Share F02/F09 with REQ-003; assess all supplied method families for fit, including security, generative, native, recovery and actual agent surfaces.      |

## Evidence-dependent decisions and their owners

These are scheduled outputs, not reasons to reopen global ordering or assume a
technology choice. Resolve facts through investigation first; involve the product
owner for actual user-policy trade-offs. An unresolved decision blocks only its
affected child slices and must name an owner and next action.

| Decision                                              | Resolving task / owner                                                        | Closure evidence                                                                                                                                                  |
| ----------------------------------------------------- | ----------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| First bottleneck and construction slice               | F02/F03/F04 → F05; integration owner with validation/runtime/core owners      | Comparable cost or verified correctness risk, invariant, bounded change and failure/recovery acceptance.                                                          |
| Runtime budgets and supported evidence scope          | F03; performance owner with product/integration owner for exceptions          | Existing authoritative budgets, build/process/data/platform identity, reliable observations and named misses; no competing invented target table.                 |
| Sync, announcement/member, deletion and privacy rules | F04; relevant core/storage/privacy owner and product owner for user semantics | Current behavior and authority/recovery trace, accepted per-domain rule, compatibility and selected failure cases. No Raft choice by default.                     |
| Retain, tune or replace storage/interfaces/UI         | F01/F03/F05/F08; owning module and integration owner                          | Demonstrated current-IM need and measured benefit versus migration/rollback/cost. New production UI remains roadmap-only.                                         |
| Verification methods and local/CI/native allocation   | F02/F09; validation owner                                                     | Risk/assertion coverage, actual failure/attempt evidence and recurring cost. Existing protected checks are not weakened by this plan.                             |
| Monitoring, contact and QR product choices            | M02/M01/M03; relevant feature/core owner and product owner                    | Supported metric/default/retention behavior, account/device/contact policy or supported card/actions, with accepted scope and bounded cost before implementation. |
| Future capability activation                          | Relevant future direction; product/integration and authority owner            | Concrete user/consumer scenario, accepted contracts and resources, first-slice acceptance and an explicit activation record.                                      |

### Preserved research leads and verification landscape

The intake's references are leads, not verified claims, adopted technologies or
required dependencies. Verify primary sources when their owning task activates.
Do not install tools or expand scope merely to reproduce a supplied example.

- REQ-001: [monitoring visual reference](https://x.com/HiTw93/status/2116890236320092498?a=xiwtWvIa7enA_OlbL1Q&s=20).
  The supplied image suggests a compact native entry/detail view; whole-computer
  fan/battery controls are outside the app/process-family scope. The earlier post
  retrieval was unsuccessful; the visual does not establish metric availability.
- REQ-012: [GPUI](https://gpui.rs/), [GPUI Kit](https://gpui-kit.com/),
  [Kit source](https://github.com/longbridge/gpui-kit),
  [Slint](https://github.com/slint-ui/slint), [Iced](https://github.com/iced-rs/iced)
  and [Rune](https://github.com/anomalyco/rune). Their suitability is unverified.
  Compare desktop macOS/Windows/Linux parity; Web is optional for this comparison
  and does not cancel the separate mobile PWA direction.
- REQ-029: [Columba](https://github.com/torlando-tech/columba),
  [Bitchat](https://github.com/permissionlesstech/bitchat),
  [LAN Messenger source](https://github.com/lanmessenger/lanmessenger) and
  [site](https://lanmessenger.github.io/), [LANChat](https://github.com/cap153/LANChat),
  [Mesh-Talk source](https://github.com/OctopusGarage/mesh-talk) and
  [site](https://octopusgarage.github.io/mesh-talk/),
  [Squiggle](https://github.com/hasankhan/Squiggle),
  [BeeBEEP](https://www.beebeep.net/) and [BLIP](https://github.com/krwg/blip).
  Technitium Mesh/HostTalk exact sources remain unknown. Quoted rankings,
  platform/security claims, Bluetooth/LoRa and Reticulum ideas are not adopted.
- Other named leads: baoyu-design/deepseek harness browser sharing, Google Drive
  external copies, Raft comparison and existing `feature/mobile-pwa` work.
  Existing-work inspection must check source/PR/owner before duplication; a named
  service or branch does not prove support or authorize exposing a local service.

For REQ-030, F02/F09 must explicitly classify applicability, existing evidence,
gaps/defer reasons and owners across static/type/semantic/SAST/formal methods;
unit/integration/E2E/contract checks; property/fuzz/mutation methods; runtime/DAST;
load/stress/spike/soak/capacity; memory/leak/undefined-behavior/race checks;
statement/branch/condition/function/path/MC/DC coverage; maintainability/review;
CVE/secrets/dependency/supply chain; licenses/relevant compliance; fault injection,
degradation/recovery/chaos; and actual agent tool/format/authorization/multi-turn/
factuality surfaces. RAG/ML evaluations apply only if that surface actually exists.
Separate method, scope, purpose and quality; tool counts or coverage percentages
alone do not demonstrate meaningful fault detection. Named tools remain examples,
not a requirement to run every tool on every PR.

## Task claims, branches and integration

[Agent rules](../AGENTS.md) and [module ownership](module-boundaries.md) govern
execution. Read the current main, roadmap, live tracker and relevant existing
branches/PRs before claiming work. Do not infer a free task from this initial table
or overwrite another session's claim. A branch name is not a lock.

| Task           | Current claim/state    | Readiness and next action                                                                                                                    |
| -------------- | ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| WIN-FLAGS | In review; native evidence pending | Owner/integration: Codex; explicit user-requested Windows country-flag fix from `5fe57c380b4b047ced19337b1233013d6d0ffcd7` on `fix/windows-country-flags`. Owns `frontend/src/features/chat/AboutDialog.tsx`, `frontend/src/features/chat/AboutDialog.test.ts`, `frontend/src/lib/platform{,.test}.ts`, `frontend/src/index.css`, `frontend/tailwind.config.js`, `frontend/src/assets/fonts/`, `frontend/e2e/country-flags.spec.ts`, `frontend/validation.json`, this claim and the emoji contract in `docs/module-boundaries.md`. Accepted inputs: current platform/font owners and the user-approved Windows-only local font design. Preserve Unicode and existing macOS/Linux behavior. Local affected verification and four browser scenarios passed; deployed development build to host alias `windows10`, which reports Windows 11. Next: PR review/current-head CI and real Windows 10 WebView2 visual, copy/delete and zoom evidence; deployment/startup is not flag-rendering acceptance. Retain claim until those gates pass; no foundation task reassigned. |
| F01            | Unclaimed; not started | First eligible task after planning merge; produce the bounded inventory and downstream handoff below.                                        |
| F02/F03/F04    | Unclaimed; not started | Wait for accepted relevant F01 owners/scenarios, then open independent evidence/contract lanes.                                              |
| F05            | Unclaimed; not started | Wait for relevant F01/F04 outputs and F02/F03 safety/baseline evidence; select one justified interface/consumer slice.                       |
| F06/F07/F08    | Unclaimed; not started | Wait for their accepted evidence/contracts/interface slices; serialize shared policy mutations.                                              |
| F09            | Unclaimed; not started | Attach protection/documentation work to each foundation slice after relevant F02/F03/F04 inputs; do not postpone it until construction ends. |
| F10            | Unclaimed; not started | Wait for the agreed mandatory F01–F09 scope and assess the integrated candidate.                                                             |
| M01–M04        | Unclaimed; not started | Wait for F10 and each task's decisions; use independent interface ownership.                                                                 |
| Future roadmap | Deferred; no claims    | Record the activation decision and create a bounded task before any implementation.                                                          |

Record each live claim in this table or one linked issue/PR tracker designated by
the integration owner; do not maintain conflicting live trackers. Required fields:

- Task/source IDs, outcome/non-goals, agent/owner and actual base/parent SHA.
- Exact owned paths after inspection; shared-interface/decision/integration owner.
- Accepted hard inputs, product decisions and expected downstream consumers.
- Invariants, selected tests, platform/resource evidence and acceptance limits.
- State, evidence link, next action, blocker owner and claim-release condition.

States: **Unclaimed → Investigating → Ready → Implementing → In review →
Validated → Merged**. Use **Needs evidence**, **Needs decision**, **Retained** or
**Deferred** with reason and revisit condition. A research result is not a product
implementation. A passing subset is not complete platform/release acceptance.

### Parallel admission

Start parallel mutations only when all relevant inputs and shared interfaces are
accepted, path and policy ownership are independent, a review/integration owner
is available and no known upstream change is expected to invalidate the result.
Separate files alone do not establish independence. If a dependency remains
unstable, wait or do nonmutating preparation instead of building something that
will need reimplementation after merge. Stop/reassess if an accepted interface
changes. Incidental textual conflicts can occur; expected semantic rework is a
reason not to dispatch.

Use a declared integration branch when coordinating related parallel tasks.
Create each child from its accepted tip/parent, validate and review it, then merge
compatible results back in dependency order. Update dependent branches against
the integrated interface before final qualification. The integration branch
still needs existing current-head PR/review/CI gates before main. Do not treat
this workflow as permission to merge unreviewed children or bypass any gate.

Keep task notes/plans/profiles in ignored `tmp/` or validation artifacts. Preserve
and clean task-owned worktrees, disposable branches and dedicated build paths
under the repository lifecycle rules. An active task/PR is an explicit retention
reason; unrelated worktrees remain user-owned.

## First execution handoff

After this planning change merges, start F01 from that main revision. F01 is an
inspection/documentation task; it does not authorize product edits. Its output
must be sufficient to dispatch F02/F03/F04 without repeating the inventory:

1. Map account/device/session, conversation/membership, delivery/read/custody and
   local/account deletion meanings to their current owning guides and actual
   producer/consumer paths. Separate UI presentation from core authority.
2. Inventory runtime/event/scheduling, transport/sync, storage/history/search,
   attachment, privacy and validation owners. Mark implemented, partial, missing
   within inspected entries and unknown; distinguish legacy/experimental paths.
3. Bind every required scenario above to representative callers, existing
   assertions/selectors and platform/native/resource gaps. Existing test presence
   is static evidence, not a fresh passing run.
4. Hand F02 the check/attempt owners, F03 the build/process/workload/storage owners
   and F04 the authority/lifecycle decisions. Give each unknown a falsifiable next
   action and owner; classify contradictions separately from suspected defects.
5. Update the maintained owning inventory/guides, roadmap state and justified
   selectors under existing gates. Record source SHA, inspection scope, commands,
   reviews and remaining limits in the task PR. No full-suite/profiling run is
   required merely to prove a static map exists.

F02/F03 then collect measurements, and F04 resolves the selected current-IM
contracts. Choose construction changes from that evidence. Do not restart the
global prioritization exercise or activate parked features because a task reveals
an appealing technology option.
