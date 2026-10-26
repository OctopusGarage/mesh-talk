# Change alignment and maintenance checklist

Use this inventory when a feature, contract, platform requirement or publication
changes. It identifies the surfaces that can drift apart, their decision owners,
and the evidence needed to review them. The change author selects the applicable
items and records the result in the PR; the reviewer checks the same source and
the remaining publication or acceptance dependencies.

This guide owns the alignment inventory and its review procedure. It does not
redefine product behavior, replace the [documentation map](README.md), or copy
the [module ownership map](module-boundaries.md). Follow the linked owning guides
for detailed rules. A row's presence is a review obligation, not a claim that it
has passed an audit.

## Sources, representations and deployed behavior

Follow the dependency chain in both directions:

1. The owning feature guide and glossary define the intended meaning and limits.
2. The implementation and independent assertions establish observed behavior.
3. Authored help, product summaries and examples explain that behavior to readers.
4. Build owners generate HTML, Markdown, schemas, indexes and packaged resources.
5. Publication and installation determine what a particular user or agent receives.

When code and an owning guide disagree, establish the intended contract before
editing either. Do not silently make the documentation endorse a regression.
Fix the maintained source and regenerate derived files. Working-tree output,
published web content, installed offline help, source/Nightly builds and Stable
releases can have different versions; identify which one supports a claim.

“LLM documentation” means machine-readable representations of the same public
facts, not an independently authored product specification. Public help retrieval,
contributor instructions and authenticated messaging tools also have different
audiences and permissions.

## Maintenance inventory

IDs are stable review references. The source paths locate the implementation;
the linked guide owns the detailed contract. Assign any cross-module decision
through [module ownership](module-boundaries.md), rather than maintaining another
person-by-person ownership list here.

### Public content and documentation

| ID  | Decision owner and maintained sources                                                                                                                                                                                | Review when affected                                                                                                                                                                                                                                                                                                                   |
| --- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| M01 | **GitHub Pages product site.** [Documentation guidelines](DOCUMENTATION_GUIDELINES.md); `site/index.html`, `site/site.js`, `site/downloads.js`, shared theme and locale resources.                                   | Product claims, supported use cases, prerequisites, limits, FAQ answers, download destinations and help links must match the owning feature guides. Review no-JavaScript reading, accessibility, responsive behavior and actual asset loading when presentation changes.                                                               |
| M02 | **User help: offline and online.** [Help system](help-system.md); `docs/help/catalog.json`, `docs/help/topics/`, `frontend/src/features/help/`, `tools/help/`.                                                       | New or changed workflows need correct steps, labels, settings targets, platform scope and failure guidance. Keep stable IDs, aliases and navigation valid. The installed app's bundled manual and generated website must derive from the same authored sources, with explicit version scope.                                           |
| M03 | **AI-facing public documentation.** [Public discovery contract](help-system.md#public-discovery-and-factual-accuracy); `tools/help/build-site.mjs`, `build.mjs`, `discovery.mjs`, `server.mjs`.                      | Regenerate overview Markdown, llms indexes/full exports, JSON catalogs, sitemaps, canonical/alternate links and application metadata. Verify absolute links and current/snapshot scope. Documentation MCP search/retrieval must expose public content only. Successful generation proves neither hosted availability nor AI citations. |
| M04 | **Project orientation and technical guides.** [Documentation map](README.md), [writing rules](DOCUMENTATION_GUIDELINES.md); root `README.md`, `PROJECT.md`, `CONTRIBUTING.md`, feature guides and `specifications/`. | Update the owning guide first; change overview text only where the reader's decision changes. Recheck architecture/module paths, examples, commands and cross-links. Remove superseded claims instead of accumulating competing instructions.                                                                                          |

### Product contracts and interfaces

| ID  | Decision owner and maintained sources                                                                                                                                                                                                       | Review when affected                                                                                                                                                                                                                                                                                                                                    |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| M05 | **Domain language.** [Canonical glossary](../CONTEXT.md), [domain modeling guide](domain-design.md).                                                                                                                                        | Keep account/device/local-user identities, session lifetimes, discovery, presence, custody, delivery and read state distinct in code, UI, help and API responses. Preserve public/stored/wire identifiers unless compatibility changes are intentional and validated.                                                                                   |
| M06 | **Architecture, networking and recovery.** [Architecture](ARCHITECTURE.md), [network contracts](network-design-reference.md), [group reception](group-conversation-reception.md), [backup/recovery](backup-recovery.md).                    | Recheck topology assumptions, eligibility, consent, resource budgets, failure modes and retained-data limits against their implementation owners. Keep diagrams and support matrices consistent. Wire/storage/recovery changes need compatibility and boundary evidence, not only revised prose.                                                        |
| M07 | **Settings, defaults and UI wording.** [Settings](settings.md), [privacy controls](privacy-controls.md); settings persistence, typed adapters, UI controls and help targets.                                                                | Align defaults, saved versus effective values, dependencies, labels, disabled reasons and platform availability. A renamed or moved control can affect help, automation, localization, screenshots and tests even when its underlying setting is unchanged.                                                                                             |
| M08 | **Localization and accessibility.** [Public language rules](DOCUMENTATION_GUIDELINES.md#public-documentation-styling), [visual system](../frontend/DESIGN.md); `frontend/src/locales/`, `site/locales/`, locale owners and parity tests.    | Keep locale keys/placeholders, accessible names, keyboard behavior, fallback and navigation aligned. Update public and app resources through their separate owners. Translated navigation does not establish a translated article; declare article language truthfully.                                                                                 |
| M09 | **Desktop IPC, local API and bots.** [Module ownership](module-boundaries.md), [bot integrations](bot-integrations.md); `frontend/src/lib/api/`, Tauri commands/registrations, integrations models/HTTP and messaging MCP.                  | Match names, types, schemas, errors, examples, authorization and retry semantics across consumers. Public HTTP help derives schemas from the real models. Recheck expiry, revocation and account/runtime boundaries when behavior changes. Public documentation MCP is separate from messaging MCP and local send authority.                            |
| M10 | **Privacy, security and diagnostics.** [Security policy](../SECURITY.md), [privacy controls](privacy-controls.md), [relay metadata limits](post-office-metadata-privacy.md), [troubleshooting](../specifications/troubleshooting_guide.md). | Review claims about plaintext, observable metadata, local exports, screen protection, credentials, logs and recovery. Diagnostic examples must avoid secrets and must not equate discovery/custody with delivery. Security-sensitive unresolved findings follow private reporting, not a public drift log.                                              |
| M11 | **Configuration, CLI and deployment examples.** [Deployment](../specifications/deployment_guide.md), [platform permissions](../specifications/platform_permissions.md), [script index](../scripts/README.md), post-office operating guides. | Recheck options, environment variables, defaults, paths, permissions, prerequisite services, startup/shutdown and recovery commands against the actual parser/service owner. Run safe focused examples where applicable; document missing hardware or environments. Never copy local credentials or machine-specific agent ports into durable examples. |

### Published artifacts and project controls

| ID  | Decision owner and maintained sources                                                                                                                                                                                                                     | Review when affected                                                                                                                                                                                                                                                                                         |
| --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| M12 | **Versions, updates and releases.** [Version lifecycle](updates.md#version-ownership-and-lifecycle); root Cargo workspace version, release tooling, changelog and release workflows.                                                                      | Use the existing version owner for derived npm/Tauri metadata. Distinguish source, Nightly and Stable availability; recheck upgrade/download/signature instructions and release assets. Version preparation, publication and next-target changes retain their reviewed authorization and exact-source gates. |
| M13 | **Screenshots, diagrams and demo content.** [Screenshot guide](screenshots/README.md), [visual system](../frontend/DESIGN.md), feature/network guides.                                                                                                    | Refresh images affected by navigation, labels, defaults or behavior. Preserve realistic deterministic demo data, privacy, alt text, native resolution and responsive candidates. A screenshot or illustrative packet animation is not native or network acceptance evidence.                                 |
| M14 | **Customization and shared Marketplace.** [Customization packs](CUSTOMIZATION_PACKS.md); app pack parser/catalog consumers and the separately published Marketplace.                                                                                      | Keep format versions, limits, image support, manifests, hashes, install/replace/remove instructions and creator tools compatible. Coordinate external catalog/Studio changes and deploy order; the project site build excludes the separately owned Marketplace content.                                     |
| M15 | **Dependencies and platform support.** Cargo/npm manifests and locks, [platform validation](platform-validation.md), [permissions](../specifications/platform_permissions.md), security/dependency workflows.                                             | Recheck runtime/toolchain prerequisites, SDK APIs, OS permissions, native packaging and documented support after dependency changes. Retain license/attribution and vulnerability evidence. A passing browser fixture does not establish native compatibility on another OS.                                 |
| M16 | **Tests, validation selection and CI.** [Testing feedback loop](testing-feedback-loop.md), [quality gates](quality-gates.md); owning `validation.json`, tests/fixtures, `prScenarios`, scripts and workflows.                                             | New paths, behavior, consumers and acceptance scenarios must remain selected. Check assertions against the intended contract and maintain a reproducing regression for bugs. A declared test, structural pass, empty selection or cached result is not interchangeable with fresh executed evidence.         |
| M17 | **Agent instructions and contributor workflows.** [Repository guidelines](../AGENTS.md), [Contributing](../CONTRIBUTING.md), [script index](../scripts/README.md), [evaluation inputs](evals/README.md); hooks, templates, commands and reusable prompts. | Keep commands, ownership pointers, permission boundaries and completion claims consistent across agent and human entry points. Recheck generated local-agent instructions through their existing owner. Never turn scratch plans, successful run histories or secrets into permanent agent context.          |
| M18 | **Organization website and external entry points.** [Public discovery contract](help-system.md#public-discovery-and-factual-accuracy); organization homepage, `projects.json`, project profile, llms exports and host-level crawler policy.               | Coordinate externally maintained summaries with this repository's facts, links, versions and deployment dependencies. Record companion PRs or issues and who will follow through. A local fix here cannot certify the organization site, shared Marketplace or search index is updated.                      |
| M19 | **Capability status and unresolved acceptance.** [Implementation status](../specifications/TODO.md), owning feature guides, linked issues and [platform evidence](platform-validation.md).                                                                | Distinguish implemented, configurable, experimental, verified and proposed behavior. Update status when acceptance changes; test names or code existence alone do not establish shipped support. Preserve unresolved native/network/model evidence explicitly.                                               |
| M20 | **Navigation, retirement and repository hygiene.** [Documentation map](README.md), [hygiene rules](DOCUMENTATION_GUIDELINES.md#repository-hygiene), `.gitignore` and hygiene checks.                                                                      | Add useful routes to new guides; update callers/anchors when moving or retiring content. Remove duplicate instructions and generated/scratch reports from the index. Keep investigation evidence in ignored output or PR/CI artifacts; use Git history rather than archive copies.                           |

## Select items from the change

Use this as a starting selection and add items for actual consumers. It is not a
replacement path selector; `validation.json` and its planner remain authoritative
for executable test selection.

| Change trigger                                                                 | Review at least                                                                                  |
| ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------ |
| Add, remove or change a user-visible feature                                   | M01–M04, M07–M08, M16, M19–M20; add its technical contract items                                 |
| Change naming, identity or outcome semantics                                   | M04–M10, M16–M19                                                                                 |
| Move/rename a setting, command or navigation action                            | M02, M04, M07–M09, M11, M13, M16–M17                                                             |
| Change discovery, routing, offline custody, sync or recovery                   | M01–M06, M09–M11, M16, M19                                                                       |
| Change privacy, retention, permissions or account lifecycle                    | M02–M10, M13, M16–M19                                                                            |
| Change an IPC/API/tool schema, CLI option or configuration default             | M02–M04, M07, M09–M11, M16–M17                                                                   |
| Change web/help publication, URLs, language or assets                          | M01–M04, M08, M13, M16, M18, M20                                                                 |
| Change a pack format or shared catalog                                         | M02–M04, M08–M10, M13–M16, M18                                                                   |
| Change version, release channel, installer or dependency support               | M01–M04, M11–M12, M15–M19                                                                        |
| Add/move a module, test, guide, script or workflow                             | M04, M16–M17, M20; update the owning implementation map if decisions move                        |
| Receive stale documentation, incorrect AI answers or missing-selector evidence | Follow the cited claim back to its owner, then select every affected representation and consumer |

For example, renaming **Connection & delivery** affects the setting UI and locale
strings, manual steps/targets, online/offline help, screenshots and associated
tests. Regenerate the AI exports from the updated manual rather than editing a
second LLM-only instruction. A new relay limit also affects its owning network
rule, configuration/examples, tests, privacy/delivery explanations and any public
summary that states that limit.

## Review and follow-up procedure

1. **At scope definition:** identify the intended contract, implementing owner,
   affected IDs and downstream consumers. Confirm whether the change applies to
   source, a release, an installed app or a deployed site.
2. **During implementation:** update owners before summaries, regenerate outputs,
   and run the affected checks. Revisit the selection if the requirement changes.
3. **Before pushing/review:** inspect `make verify-plan`, run `make verify-changed`
   and applicable boundary checks, and record the result below. Include actual
   source/runner identity and remaining external/native/model obligations.
4. **At release/publication:** verify the actual artifacts and public URLs at their
   intended version. Follow companion deployment order. Do not replace failed or
   unavailable verification with a local build result.
5. **After drift is reported:** correct the owning source and the missing
   maintenance step/selector/fixture that allowed it to lag. Add a meaningful
   regression where the failure is reproducible. Update this inventory if a new
   category of consumer was missed.

The change author coordinates this review; implementation owners confirm domain
meaning, reviewers challenge evidence and exclusions, and publication owners
verify external artifacts. If no feature work is active, maintainers can run the
same inventory before a release or in an explicitly requested maintenance sweep.
This document does not create a scheduled job or require a full suite per edit.

Record only applicable items in the PR description or a linked tracking issue:

```text
Alignment review
- Source and scope: <commit / working tree, platform, source or released version>
- Items: <IDs and affected consumers>
- Result per item: aligned / action required / not applicable / not checked
- Evidence: <commands, results, inspected claims and artifact or runner links>
- Exclusions: <why an apparent consumer is unaffected>
- Follow-ups: <owner or role, linked issue/companion PR, completion condition>
- Publication: <pending / verified, URL or artifact identity and deployment order>
```

Use **aligned** only for the named source, scope and evidence. **Action required**
means a known mismatch remains. **Not applicable** needs a reason; **not checked**
means no conclusion. Safety or compatibility requirements still obey their owning
merge/release gates; a follow-up entry does not waive them. Cross-repository work
must name its responsible owner/role and a clear completion event, not simply
“update later.” Keep open work in linked issues or the existing capability-status
owner, and close it when resolved. Do not accumulate completed audit histories or
per-commit green checkboxes in this maintained guide.

## Evidence and check selection

| Evidence                                                                                  | What it supports                                                                            | What remains separate                                                            |
| ----------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| Source/claim review and link inspection                                                   | Correct ownership, terminology, paths, stated limits and examples for the reviewed source   | Actual execution, publication and platform support                               |
| `make standards-check` and repository hygiene                                             | Registered structural ownership/references, version checks and index/artifact policy        | Semantic completeness, useful assertions and all Markdown links                  |
| `make verify-plan` / `make verify-changed`                                                | Affected selections and observed test/static/build outcomes                                 | Review pending browser/native requirements; full protected CI still applies      |
| Help tests and `node tools/help/build-site.mjs --out tmp/public-site-review`              | Generated corpus, discovery metadata, current/versioned routes and deployable output        | Hosted HTTP behavior, crawler policy, installed-app help and actual indexing     |
| Selected browser, integration and native checks                                           | Their exact scenarios, platforms and source                                                 | Untested hardware/topologies, unrelated releases and universal correctness       |
| Hosted checks and the [public discovery review](evals/prompts/public-discovery-review.md) | Observed deployed responses, citations and claim accuracy in recorded product/mode sessions | Guaranteed ranking, all providers, or causality from one before/after comparison |

Use the [testing workflow](testing-feedback-loop.md) for executable commands and
escalation rules, [help maintenance](help-system.md) for publication checks, and
[platform validation](platform-validation.md) for native evidence. Read the
selected capability guides and pending requirements rather than treating this
table as a new mandatory full-health command list.

## Keep the inventory useful

When a source, consumer or publication owner changes, update the relevant row,
trigger and navigation links in the same PR. Extend the existing validation
contract when selection or executable behavior changes; explain an unchanged
contract for prose-only maintenance. Add a category only for a distinct owner or
failure boundary. Keep detailed feature inventories, defaults, numerical limits
and release matrices with their owning guides so this checklist does not become
another stale product specification.
