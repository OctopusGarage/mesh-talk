import { mkdir, writeFile, copyFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { loadHelp, helpCategories } from "./load.mjs";
import { renderMarkdown, escapeHtml as e } from "./render.mjs";
function navigation(data, id) {
  return Object.entries(helpCategories)
    .map(([category, label]) => {
      const topics = data.topics.filter((topic) => topic.category === category);
      return topics.length
        ? `<section><h2>${e(label)}</h2>${topics.map((t) => `<a href="${t.id}.html"${id === t.id ? ' aria-current="page"' : ""}>${e(t.title)}</a>`).join("")}</section>`
        : "";
    })
    .join("");
}
function page(data, title, content, { id, projectHref } = {}) {
  return `<!doctype html><html lang="en" class="manual"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light dark"><title>${e(title)} · Mesh-Talk Help</title><link rel="stylesheet" href="theme.css"><link rel="stylesheet" href="style.css"><link rel="describedby" type="text/plain" href="llms.txt">${id ? `<link rel="alternate" type="text/markdown" href="${id}.md">` : ""}</head><body><a class="skip" href="#main">Skip to content</a><header><div class="breadcrumb"><a class="wordmark" href="${projectHref}">Mesh-Talk</a><span aria-hidden="true">/</span><a href="index.html">User guide</a></div><small>Version ${e(data.version)} · English</small></header><div class="layout"><aside><details><summary>Browse help topics</summary><nav aria-label="Help topics">${navigation(data, id)}</nav></details></aside><main id="main" tabindex="-1">${content}</main></div><script type="module" src="navigation.mjs"></script></body></html>`;
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
    await copyFile(
      new URL("./style.css", import.meta.url),
      join(dir, "style.css"),
    );
    await copyFile(
      new URL("../../site/theme.css", import.meta.url),
      join(dir, "theme.css"),
    );
    await mkdir(join(dir, "fonts"), { recursive: true });
    await copyFile(
      new URL("../../site/fonts/space-grotesk-latin.woff2", import.meta.url),
      join(dir, "fonts/space-grotesk-latin.woff2"),
    );
    await copyFile(
      new URL("../../site/fonts/OFL.txt", import.meta.url),
      join(dir, "fonts/OFL.txt"),
    );
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
        '<h1>Mesh-Talk help</h1><p class="intro">Connect on your LAN, reach another site, and keep conversations moving when devices reconnect. Practical guides to messaging, files and privacy.</p><label for="search">Search help</label><input id="search" type="search" placeholder="Try notifications or connecting" autocomplete="off"><p id="status" role="status"></p><ul id="results">' +
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
      `# Mesh-Talk Help\n\nEnglish product help for Mesh-Talk ${data.version}. Public documentation only.\n\n- [Complete help index](${new URL(prefix + "index.json", base)})\n` +
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
