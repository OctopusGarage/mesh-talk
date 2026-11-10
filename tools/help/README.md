# Public help tooling

The canonical English catalog and articles live in `docs/help/`. The app, static
website, and local read-only MCP server consume those same sources. Topic aliases
may use other languages; untranslated queries return English articles explicitly.

Requires Node.js 22 or newer. From the repository root:

```sh
npm ci --prefix tools/help --ignore-scripts
node --test tools/help/*.test.mjs
node tools/help/build.mjs --out /tmp/mesh-talk-help
node tools/help/build-site.mjs --out /tmp/mesh-talk-public
node tools/help/server.mjs
```

Serve the generated directory over HTTP to use browser search. Articles and the
index remain readable without JavaScript. The default public base is
`https://octopusgarage.github.io/mesh-talk/help/`; use `--url <https-base-ending-in-/>`
to build another deployment. All browser navigation uses relative links, including
inside the version snapshot at `v/<workspace-version>/`. A build does not delete
older version directories already present in the output. A clean Pages build
publishes the current version snapshot only.

The help build emits HTML, source Markdown, `index.md`, a full-body `index.json`,
`version.json`, `llms.txt` and `llms-full.txt`, both at the help root and in the
version directory, plus a current-HTML sitemap. Version comes from
`[workspace.package]` in `Cargo.toml` and identifies documentation source, not a
published Stable release. Topic metadata uses distinct current/version canonical
URLs; aggregate Markdown links retain their source scope.

Use `build-site.mjs` for the complete deployable site, as Pages and browser
fixtures do. It generates the project overview from `about-mesh-talk`, a project
documentation index/full manual, structured application metadata and a sitemap
covering the landing page and current help. Its `--url` defaults to
`https://octopusgarage.github.io/mesh-talk/` and requires an HTTPS directory URL
without credentials, query or fragment. It excludes the separately owned `market/`
assets. Serve output over HTTP for local preview. Explicit `.md` files work on
static hosting; `Accept: text/markdown` is not implemented for HTML URLs.
See [publication contracts](../../docs/help-system.md#public-discovery-and-factual-accuracy).

## MCP client configuration

Use the absolute Node executable path printed by `node -p process.execPath` and
an absolute checkout path. Example client configuration (replace both paths):

```json
{
  "mcpServers": {
    "mesh-talk-help": {
      "command": "/absolute/path/to/node",
      "args": ["/absolute/path/to/mesh-talk/tools/help/server.mjs"]
    }
  }
}
```

No API key, app login, running desktop app, or network listener is needed. The
stdio process reads only the bundled public help sources. It cannot read chats,
identities, private files, diagnostics, or change settings. Standard output is
reserved for MCP protocol messages.

Tools:

- `search_help(query, version?, platform?, locale?)` returns lightweight ranked
  results: `id`, `title`, `summary`, `platforms`, `resourceUri`, and a version-qualified
  public `url`. It includes the effective version and an explicit `localeFallback`
  flag. Platforms are `macos`, `windows`, and `linux`. Article bodies are omitted
  to keep search context small.
- `get_help_topic(id, version?)` returns the full English Markdown article.

Use search first, then retrieve only the selected article with `get_help_topic`
using the returned ID and version, or read its `resourceUri`. Each returned source
URI resolves to the same bundled Markdown available through full-topic retrieval;
the public URL identifies the corresponding versioned website article.

The server lists the public index and every topic as MCP resources under
`mesh-talk-help://<version>/`. Unsupported versions and unknown topic IDs return
tool errors. Unsupported versions never resolve silently to the latest help.
Only the checkout's current workspace version is served. Invalid input is rejected
by the SDK without terminating the server. `MESH_TALK_HELP_ROOT` can select a
separate documentation checkout for fixture tests or packaging.

The official [TypeScript SDK documentation](https://github.com/modelcontextprotocol/typescript-sdk)
was checked for the v2 stdio API. The SDK packages and Zod are pinned in the
package manifest and lockfile. Browser search and Markdown rendering have no
package dependencies or Node imports. The renderer deliberately supports only
headings, paragraphs, lists, inline code, bold, HTTPS links, and relative topic
links; raw HTML is escaped and other link schemes are rejected.

## Reader presentation

The public reader shares [the project theme](../../site/theme.css) and its licensed,
self-hosted heading font with the landing page. [Reader layout](style.css) adds
article typography and grouped topic navigation. The build copies these assets
into current and versioned output; generated HTML/CSS is not maintained by hand.
See [documentation ownership](../../docs/DOCUMENTATION_GUIDELINES.md) for content
and styling conventions. Offline application help uses the same manual sources
inside the desktop app's own visual system.
