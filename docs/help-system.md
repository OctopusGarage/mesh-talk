# User help and AI documentation

Mesh-Talk maintains one English user manual for offline reading, online reading,
and external AI retrieval. It does not embed a model, invoke an installed agent,
or send account data to an AI service. The [user guide](help/README.md) owns tasks;
existing feature and architecture guides remain authoritative for detailed policy
and protocol behavior. User topics summarize those contracts and link to them.

## Sources and owners

| Source or module              | Owns                                                                                                      |
| ----------------------------- | --------------------------------------------------------------------------------------------------------- |
| `docs/help/catalog.json`      | Stable topic IDs, titles, summaries, categories, aliases, platform scope and optional navigation targets. |
| `docs/help/topics/*.md`       | Task steps, menu locations, prerequisites, limitations and references.                                    |
| `frontend/src/features/help/` | Bundled reader, locale fallback and navigation through declared topic targets.                            |
| `tools/help/search.mjs`       | Deterministic title, alias, summary and full-text search, reused by all three readers.                    |
| `tools/help/render.mjs`       | Restricted, escaped Markdown rendering for desktop and online readers.                                    |
| `tools/help/build.mjs`        | Static HTML, Markdown, JSON and discovery exports.                                                        |
| `tools/help/server.mjs`       | Read-only stdio MCP adapter over the public manual.                                                       |

The browser and app need no additional Markdown or search dependency. Only the MCP
adapter needs its separately pinned SDK dependencies. No protocol-core changes,
local HTTP listener, vector database or vendor-operated help service are required.

## Desktop behavior

Help is available before sign-in, in the signed-in More menu, and through tray
navigation. Opening a topic does not query settings or diagnostics. A signed-in
reader may choose **Open [category]** or **Open Connection**. The UI resolves that
topic's catalog target; it does not execute model-supplied commands or URLs.

Settings navigation selects a category and focuses its declared control after
settings have loaded. It never clicks the control, opens a native picker or saves
a value. A disabled control retains the existing explanation; focus falls back to
the category panel. Opening Settings normally still starts at General. Signed-out
help offers reading and online links without authenticated navigation actions.

The app bundles the catalog and Markdown from the checked-out source. There is no
runtime fetch for offline help. Interface labels follow existing app localization;
articles declare English and show an explicit fallback notice in other languages.
Common Chinese aliases are a search aid, not translated documentation.

## Publication and version scope

The Pages workflow copies the existing marketing site and generates help under
`help/`. It triggers on changes to the manual, help tooling, site and workspace
version. The build emits:

- `help/index.html` and topic HTML pages for people.
- `help/<topic>.md` for text retrieval, with an alternate link from each HTML page.
- `help/index.json` with schema version, source language, app version and full topics.
- `help/version.json` and `help/llms.txt` for scope and discovery.
- `help/v/<workspace-version>/` with the same version-qualified exports.
- A site-root `llms.txt` copy that points to the generated help URLs.

All page navigation is relative, so a GitHub Pages project subpath is preserved.
The current build's version comes from `[workspace.package]` in `Cargo.toml`.
A clean deployment contains the current snapshot only; publication does not
promise an archive of all past releases. Older installed apps always
retain their own bundled help. Their version-qualified online link is only
available while that snapshot is published. A future archive policy can retain
previous generated snapshots without changing the format.

The local MCP serves only its checkout's version. Supplying another version
returns an explicit error listing the available version. Never substitute current
menu instructions silently for an unsupported app version. Version-qualified
content can still change while developing the same unreleased version; the Git
commit remains the exact source identity.

## Maintaining a topic

1. Confirm the task against current code and its owning feature guide. State
   permissions, platform differences and important limits beside the steps.
2. Add or update a Markdown article and catalog entry. Keep IDs stable even when
   display names change. Avoid release claims that are not verified.
3. Use headings, paragraphs, flat ordered/unordered lists, bold, inline code,
   HTTPS references and relative topic links. Raw HTML is displayed as text;
   images, tables, fenced code and arbitrary URL schemes are not rendered.
4. If a topic has an app destination, use an existing settings category and
   control ID, or diagnostics. Add focused UI coverage for a new destination.
5. Run the focused checks below. Never maintain a second editable Wiki copy;
   a Wiki can point to the manual instead.

```sh
npm ci --prefix tools/help --ignore-scripts
node --test tools/help/*.test.mjs
node tools/help/build.mjs --out /tmp/mesh-talk-help
(cd frontend && npm test -- src/features/help/targets.test.ts src/lib/localeParity.test.ts)
(cd frontend && npm run typecheck && npm run lint && npm run build)
(cd frontend && npx playwright test e2e/help.spec.ts e2e/tray-navigation.spec.ts)
```

Corpus checks verify IDs, metadata, Markdown title consistency, local topic links,
static root/version routes and declared control presence. MCP tests use a real
SDK client and spawned stdio process. The regular CI and health script include
these help checks. UI tests mock native IPC; they do not certify native tray
rendering, operating-system permissions or physical-network delivery.
