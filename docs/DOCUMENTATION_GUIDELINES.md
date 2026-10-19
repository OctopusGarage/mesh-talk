# Documentation guidelines

Mesh-Talk documentation should help a reader make a decision or complete a task.
Keep one coherent product story across surfaces, with details in the document that
owns them. A feature does not need a new paragraph everywhere it is mentioned.

## Audience and ownership

| Surface                                                      | Reader's question                                                     | Owns                                                                                                                                               |
| ------------------------------------------------------------ | --------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Project site](../site/index.html)                           | Is Mesh-Talk for me?                                                  | Product model, trust boundaries in plain language, representative UI, download and documentation paths.                                            |
| [Root README](../README.md)                                  | What is it, how do I start, and where is the source?                  | Concise orientation, supported communication paths, first run, repository map.                                                                     |
| [Documentation hub](README.md)                               | Which guide answers my question?                                      | Task-based routes and a complete catalog of maintained guides and evidence.                                                                        |
| [Architecture](ARCHITECTURE.md)                              | How does the system work?                                             | Protocol, modules, data flow, crypto, wire compatibility, and technical limits.                                                                    |
| [Network architecture baseline](network-design-reference.md) | Which communication model and contracts must future changes preserve? | Network design principles, module ownership, topology scenarios, policy interactions, resource budgets and feature-specific acceptance boundaries. |
| Feature and operations guides                                | How do I configure or use this capability?                            | Exact steps, options, examples, constraints, and troubleshooting for their topic.                                                                  |
| [Evaluation inputs](evals/README.md)                         | Which reusable cases and rubrics apply?                               | Maintained suites, inventory and prompts; generated evidence belongs in PR/CI artifacts.                                                           |

The site and README may summarize a capability, but link to its owning guide
for setup, flags, support matrices, and changing limits. Do not copy a CLI option
table, release procedure, or full feature inventory into either overview.

## Shared domain language

[CONTEXT.md](../CONTEXT.md) owns domain definitions; [DDD conventions](domain-design.md)
own modeling vocabulary, identifier mappings and the reference index. Use those
terms in guides, code comments and reviews. Keep technical invariants in the
architecture/network references and implementation ownership in the module map.
Do not create feature-local glossaries that redefine account/device identity,
conversation membership, lifecycle or delivery outcomes.

## Shared project narrative

Explain the whole communication model in this order, regardless of which feature
changed most recently:

1. **Encrypted communication on a LAN:** local identities, signed discovery and
   authenticated direct connections; no internet or central messaging service is
   required for reachable LAN peers.
2. **Decentralized synchronization:** authorized peers exchange signed,
   content-addressed events. Multi-node synchronization is bounded by retained data
   and available paths, rather than dependent on one permanent sender.
3. **Other LANs and sites:** existing routed paths or opted-in, trusted post-office
   bridges/federation connect participants. Mesh-Talk does not provide a VPN, forward
   all LAN multicast or merge subnets.
4. **Offline delivery:** optional peer-operated post offices temporarily hold
   recipient-encrypted events, subject to capacity and a 30-day maximum age.
5. **Reconnection and group recovery:** authorized members with sharing enabled can
   supply retained messages and eligible file chunks, even without the original
   sender. A reachable source, receiving approval and retained content are required.
6. **Linked devices:** certified devices can synchronize selected account history,
   file cards and attachment contents over eligible direct device paths.

Use “no central server required” when describing serverless operation. Optional
post offices are services run by peers; serverless does not mean the project can
never use a server. Decentralized does not mean unrestricted routing, automatic
trust, infinite storage or guaranteed delivery. Encryption protects content; peers
and relays still observe some connection and routing metadata.

Keep outcomes precise: local acceptance, relay custody, recipient confirmation,
reading, file-card visibility and file-byte availability are distinct. A completed
peer recovery check is a local observation, not proof that every offline device
has no additional history. Calls and linked-device synchronization have their own
reachability requirements. Link to [the glossary](../CONTEXT.md),
[network contracts](network-design-reference.md) and
[group reception](group-conversation-reception.md) for the authoritative definitions.

## Public documentation styling

The [shared public theme](../site/theme.css) owns the landing page and generated
help reader's colors, self-hosted Space Grotesk headings, system body font, focus
states and reduced-motion behavior. The [reader stylesheet](../tools/help/style.css)
owns reading measure, topic navigation and article layout. The help build copies
the same theme and licensed font into both current and versioned readers, keeping
them self-contained under GitHub Pages project paths. Edit these sources rather
than generated CSS or HTML. Application surfaces remain governed by
[the desktop visual system](../frontend/DESIGN.md).

Use a restrained hierarchy, short paragraphs, meaningful diagrams, consistent
navigation and descriptive links. Keep screenshots labeled as demo data. Group
manual topics by task; use the catalog as the source for search and navigation.
Technical references keep their detailed contracts and scoped evidence rather
than repeating the landing page's product copy. Prefer consistent terminology
over making every document use identical prose.

## Writing and maintenance rules

- Start with the reader's task. Use concrete headings and short paragraphs;
  put commands near the step that needs them.
- Name the actor and the condition: “an opted-in client registers with a pinned
  relay” is clearer than “the network becomes available.”
- Distinguish implemented behavior, required configuration, tested scenarios,
  and intended future work. Do not promote a test result into a universal claim.
- Prefer stable links to the owning guide over duplicated instructions. Use
  relative links within the repository; the static site links to GitHub's
  published guides.
- Keep screenshots representative, accessible, and labeled as demo data when
  appropriate. See the [screenshot guide](screenshots/README.md).
- Write durable copy in English. Avoid release-number-dependent claims on the
  site; the release page owns current binaries and verification commands.
- When implementation changes, update its owning guide and any overview claim
  that materially changes. Recheck links, commands, platform scope, and
  negative-space claims such as “no server.”

## Review checklist

1. Identify the owning guide and confirm each changed claim against the current
   code or a current release artifact.
2. Update the site or README only if the change affects a newcomer's decision
   or first-run path. Remove stale copy instead of appending another feature row.
3. Keep this hub complete and route the reader to the guide from the relevant
   task path.
4. Run formatting and link checks on touched documents. For site changes,
   inspect desktop and narrow layouts, keyboard focus, reduced motion, and
   image loading. Reserve full E2E and full-suite runs for PR/CI or an explicit
   validation request; see [testing feedback loop](testing-feedback-loop.md).

## User help and AI retrieval

The [user manual](help/README.md) owns task-oriented steps and menu locations.
Its English Markdown and catalog generate the online reader, offline help and AI
exports. Maintain those sources rather than editing generated pages or keeping
a separate Wiki copy. Existing feature guides retain detailed policy ownership.
See [help system maintenance](help-system.md) for supported Markdown, stable IDs,
version scope, controlled UI targets and focused validation.

## Repository hygiene

Maintain one owner for each topic. Merge durable decisions into that guide and
remove superseded proposals, implementation plans, investigation notes, duplicate
runbooks and completed-work reports. Git history preserves prior versions; do not
add archive directories or placeholder documents just to retain old filenames.

Use ignored `tmp/` for implementation research and scratch scripts. Generated
validation output belongs under ignored `target/`, coverage or Playwright output
directories. Delete disposable notes when the work is complete. Keep screenshots
only when they serve the maintained product/user documentation or an executable
visual baseline, not as a diary of an earlier investigation.

`docs/evals/` accepts only its README, named `*-suite.json` inputs,
`module-coverage.json` and reusable Markdown prompts. Operational validation
guidance belongs in [platform validation](platform-validation.md),
[quality gates](quality-gates.md) or the [testing workflow](testing-feedback-loop.md).

`.gitignore` owns the artifact policy. `node scripts/check-repository-hygiene.mjs`
checks the whole Git index, including force-added files. The pre-commit hook, CI
and health gate run the same check. For a new maintained guide or script, give it
an owner and a real caller/navigation path; do not add another report or copied
command wrapper. Filename checks cannot determine whether prose is obsolete, so
review this ownership rule when changing documentation.
