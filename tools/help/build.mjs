import { mkdir, writeFile, copyFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { loadHelp } from "./load.mjs";
import { renderMarkdown, escapeHtml as e } from "./render.mjs";
const css = `:root{color-scheme:light dark;--bg:#f7f6f2;--fg:#252e2b;--muted:#626e67;--line:#d7ddd5;--accent:#287260}@media(prefers-color-scheme:dark){:root{--bg:#1b201e;--fg:#e2e7e1;--muted:#a0afa5;--line:#3b4840;--accent:#86c9b1}}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.7 system-ui,sans-serif}a{color:var(--accent);text-underline-offset:4px}a:focus-visible,input:focus-visible{outline:2px solid var(--accent);outline-offset:4px}header{padding:28px 5vw;border-bottom:1px solid var(--line)}header a{font-weight:650;text-decoration:none}small{color:var(--muted)}.layout{display:grid;grid-template-columns:280px minmax(0,1fr);max-width:1280px;margin:auto}aside{align-self:start;position:sticky;top:0;max-height:100vh;overflow:auto;padding:32px 24px;border-right:1px solid var(--line)}main{padding:42px 5vw 80px;max-width:82ch;min-width:0}h1{font-size:2rem;line-height:1.15;letter-spacing:-.04em;font-weight:600}h2{margin-top:2em;font-size:1.3rem}summary{cursor:pointer}nav a[aria-current]{font-weight:650}.skip{position:absolute;left:16px;top:-100px}.skip:focus{top:10px;background:var(--bg);padding:8px;z-index:1}nav a{display:block;padding:8px 0}input{width:100%;font:inherit;background:transparent;color:inherit;border:1px solid var(--line);border-radius:4px;padding:10px}#results{padding-left:20px}#results li{padding:14px 0;border-bottom:1px solid var(--line)}#results p{margin:4px 0;color:var(--muted)}code{font-size:.9em;overflow-wrap:anywhere}article{max-width:70ch}footer{margin-top:3em;border-top:1px solid var(--line);padding-top:1em}@media(max-width:720px){.layout{display:block}aside{position:static;max-height:none;overflow:visible;border-right:0;border-bottom:1px solid var(--line);padding:20px 6vw}nav{display:flex;flex-wrap:wrap;gap:4px 20px}main{padding:28px 6vw 60px}}`;
function page(data, title, content, { id, projectHref } = {}) {
  return `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light dark"><title>${e(title)} · Mesh-Talk Help</title><link rel="stylesheet" href="style.css"><link rel="describedby" type="text/plain" href="llms.txt">${id ? `<link rel="alternate" type="text/markdown" href="${id}.md">` : ""}</head><body><a class="skip" href="#main">Skip to content</a><header><a href="${projectHref}">Mesh-Talk</a> / <a href="index.html">Help</a> <small>Version ${e(data.version)} · English</small></header><div class="layout"><aside><details><summary>Browse help topics</summary><nav aria-label="Help topics">${data.topics.map((t) => `<a href="${t.id}.html"${id === t.id ? ' aria-current="page"' : ""}>${e(t.title)}</a>`).join("")}</nav></details></aside><main id="main" tabindex="-1">${content}</main></div><script type="module" src="navigation.mjs"></script></body></html>`;
}
export async function buildHelp({
  root,
  out,
  url = "https://octopusgarage.github.io/mesh-talk/help/",
} = {}) {
  if (!out) throw Error("Output directory required");
  const data = await loadHelp({ root });
  const base = new URL(url);
  if (!base.pathname.endsWith("/")) throw Error("Help URL must end in /");
  for (const dir of [resolve(out), resolve(out, "v", data.version)]) {
    const projectHref =
      dir === resolve(out) ? "../index.html" : "../../../index.html";
    await mkdir(dir, { recursive: true });
    await writeFile(
      join(dir, "index.json"),
      JSON.stringify(data, null, 2) + "\n",
    );
    await writeFile(
      join(dir, "version.json"),
      JSON.stringify({
        version: data.version,
        language: data.language,
        schemaVersion: 1,
      }) + "\n",
    );
    await writeFile(join(dir, "style.css"), css);
    await copyFile(
      new URL("./navigation.mjs", import.meta.url),
      join(dir, "navigation.mjs"),
    );
    await copyFile(
      new URL("./search.mjs", import.meta.url),
      join(dir, "search.mjs"),
    );
    await copyFile(
      new URL("./site.mjs", import.meta.url),
      join(dir, "site.mjs"),
    );
    await writeFile(
      join(dir, "index.html"),
      page(
        data,
        "Help",
        '<h1>Mesh-Talk help</h1><p>Guides to connecting, talking, and keeping control of your conversations.</p><label for="search">Search help</label><input id="search" type="search" placeholder="Try notifications or connecting" autocomplete="off"><p id="status" role="status"></p><ul id="results">' +
          data.topics
            .map(
              (t) =>
                `<li><a href="${t.id}.html">${e(t.title)}</a><p>${e(t.summary)}</p></li>`,
            )
            .join("") +
          '</ul><script type="module" src="site.mjs"></script>',
        { projectHref },
      ),
    );
    for (const t of data.topics) {
      const body = renderMarkdown(t.body).replace(
        /href="([a-z0-9-]+)\.md(#[^"]*)?"/g,
        'href="$1.html$2"',
      );
      await writeFile(
        join(dir, t.id + ".html"),
        page(
          data,
          t.title,
          `<article>${body}<footer><a href="${t.id}.md">Read as Markdown</a> · <a href="index.html">All help</a></footer></article>`,
          { id: t.id, projectHref },
        ),
      );
      await writeFile(join(dir, t.id + ".md"), t.body);
    }
    const prefix = dir === resolve(out) ? "" : `v/${data.version}/`;
    await writeFile(
      join(dir, "llms.txt"),
      `# Mesh Talk Help\n\nEnglish product help for Mesh Talk ${data.version}. Public documentation only.\n\n- [Complete help index](${new URL(prefix + "index.json", base)})\n` +
        data.topics
          .map(
            (t) =>
              `- [${t.title}](${new URL(prefix + t.id + ".md", base)}): ${t.summary}`,
          )
          .join("\n") +
        "\n",
    );
  }
  return data;
}
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  const args = process.argv.slice(2);
  const out = args[args.indexOf("--out") + 1];
  if (!args.includes("--out") || !out)
    throw Error(
      "Usage: node tools/help/build.mjs --out <directory> [--url <public-base-url>]",
    );
  await buildHelp({
    out,
    url: args.includes("--url") ? args[args.indexOf("--url") + 1] : undefined,
  });
}
