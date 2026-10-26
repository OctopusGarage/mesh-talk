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

The public site uses a deep blue-green ground, mint actions and generous spacing.
Keep product imagery ahead of implementation detail: a centered introduction,
real app previews, then communication, recovery and privacy explanations. The
[landing stylesheet](../site/landing.css) owns responsive composition;
[progressive interactions](../site/site.js) own the mobile menu, keyboard-operable
appearance and platform tabs, and the native screenshot viewer.
The [download adapter](../site/downloads.js) owns public Stable metadata lookup,
platform detection, direct asset links and platform-specific installation help;
see [website download behavior](updates.md#public-website-downloads). Tabs use arrow,
Home and End keys; closing the viewer restores focus to the originating link.
Avoid autoplay, scroll interception, remote fonts or analytics dependencies.
Download choices lead to the authoritative Stable release assets and notes;
they must not hard-code a current version or guess a binary URL.

User-selected download platforms override initial OS detection. Remember explicit
architecture choices in session storage only; a denied storage permission must
not block downloads. The installation dialog offers the exact selected download
again and keeps keyboard focus on a visible control when its chooser closes.
Clipboard results from a dismissed dialog cannot update a later dialog visit.
The source-build disclosure uses the README's documented commands and shares
the same copy feedback and manual-selection fallback.

The custom Pages `404.html` uses absolute `/mesh-talk/` recovery links and shared
styles so nested obsolete URLs can return to the home page, user guide or downloads.
Keep its wording neutral: a missing route does not prove content has moved.

Screenshot browsing provides previous/next buttons, arrow keys, a position label,
an original-image link and optional horizontal touch swipes. Cancelled gestures
reset immediately; vertical scrolling and pinch zoom remain available. Restore
focus explicitly after closing dialogs, including WebKit. FAQ answers have stable
fragment links, and opening those URLs expands the relevant answer. Feature
summaries link directly to their maintained help topics. Mobile navigation moves
focus to its destination instead of leaving it inside a collapsed menu.

Help search stores its query in the URL, restores it on browser Back, and offers
clear, Escape, `/` focus, timeout and retry behavior. Keep static topics available
when the search index fails. Article headings precede the mobile contents list;
section/subsection anchors and neighboring-topic links support continued reading.
Browser tests serve the actual `/mesh-talk/` HTTP path. Intercepted WebKit
attachment responses do not emit native download events; those fixture cases
assert exact outgoing package requests and modal behavior, while Chromium and
Firefox also assert native download events. Real hosted/native evidence is a
separate claim from those fixtures. Run the focused browser matrix with
`cd frontend && npx playwright test --config playwright.site.config.ts`;
install its Chromium, WebKit and Firefox engines with `npx playwright install`
when needed. This matrix needs neither the application dev server nor Rust.
Include 200% text sizing and touch-target checks in responsive review.

The landing page leads with a neutral, real conversation capture. Optional theme
examples belong in the Marketplace section alongside avatar and sticker packs;
make the marketplace and installation guide discoverable without making appearance
the product's main story. Captures use deterministic demo data and must load all
avatars and messages before publication. The public footer credits OctopusGarage.

The shared [language owner](../site/i18n.js) provides English, Simplified Chinese,
Traditional Chinese, Japanese, Spanish, Cantonese, Portuguese and Korean
via complete local JSON
resources in `site/locales/`. English HTML remains the no-JavaScript fallback.
Use English source strings as keys, preserve template placeholders, and bind
runtime messages through the same owner rather than reconstructing translated
DOM. Locale values are text, never executable markup. Explicit URL `lang` wins
over a saved choice and browser preference. Persist explicit choices and language
URLs; automatic browser detection alone must not become a fixed saved preference.
Tag native language names with their own language for assistive technology.
Preserve other query parameters and
fragments; internal reader links carry the language even when storage is denied.
Failed, incomplete or superseded resource requests preserve the previous complete
language. Every current/versioned help build copies its own language resources.
Help navigation and search translate; authored article content stays explicitly
English with a translated fallback notice, not a claim of translated articles.

The optional [motion layer](../site/motion.js) and
[stylesheet](../site/motion.css) provide one brief product entrance, directional
image continuity and native dialog arrivals. Never delay download initiation,
modal dismissal or focus restoration for animation. Cancel stale image work and
active animations when the tab is hidden or reduced motion is enabled. Avoid
continuous decorative loops, scroll interception and repeated section reveals.
The [topology demonstration](../site/network-flow.js) is the one intentionally
repeating explanatory animation: packets traverse direct LAN, optional custody,
routed forwarding and a recipient path on a shared timeline. It is illustrative,
not live connection health, custody or recipient-confirmation evidence. Provide
localized pause/resume and replay controls; preserve a complete static diagram
without JavaScript or under reduced motion. Remove animation work when below 25%
visibility or in a hidden tab, and retain an explicit user pause across re-entry.

Preview changes preload and decode the new bitmap before committing the image,
caption and selected state together. Keep the previous complete frame while a
request is pending or fails; stale decodes cannot overwrite a newer selection.
Product screenshots use native 2× captures and lossless WebP exports, with
responsive 1280/2560-pixel candidates and full-resolution viewer links. Preserve
responsive selection during decoded theme changes. The
[screenshot guide](screenshots/README.md) owns capture and export instructions.
The [FAQ motion owner](../site/faq-motion.js) bridges native disclosures from the
current measured height (270 ms opening, 190 ms closing), reverses interrupted
transitions, removes closing links from focus, and settles immediately on reduced
motion, hidden tabs, translated content or wrapping changes. No-JavaScript and
fragment navigation keep native disclosure semantics. These boundaries have
focused `site-network-flow`, `site-faq-motion` and `site-motion` browser coverage.

Validate these behaviors with `e2e/site-motion.spec.ts` and language persistence,
resource failure/races and layout with `e2e/site-language.spec.ts`.

All product content, screenshots, download choices, FAQs and help navigation
remain available without JavaScript. Preserve ordinary image links when the
viewer is unavailable, and keep initial content visible under reduced motion.
The help generator owns article section anchors and the on-page contents list;
current and versioned readers must share relative links, search behavior and
Markdown alternatives. Reader layouts use the same public identity with a
quieter reading measure and respect the visitor's light/dark preference.
Validate these surfaces with `e2e/site-audit.spec.ts` and
`e2e/help-website.spec.ts` and `e2e/site-downloads.spec.ts`, including mobile overflow, keyboard interactions,
JavaScript-disabled navigation, reader color schemes and accessibility scans.

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
