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
// Derive section anchors from already escaped renderer output. Duplicate titles
// receive stable suffixes, and existing inline formatting stays intact.
function articleSections(body) {
  const used = new Set();
  const sections = [];
  const html = body.replace(/<h([23])>(.*?)<\/h\1>/g, (_, level, label) => {
    const text = label.replace(/<[^>]*>/g, "");
    const slug =
      text
        .replace(/&[^;]+;/g, "-")
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .replace(/^-|-$/g, "") || "section";
    let id = slug,
      suffix = 2;
    while (used.has(id)) id = `${slug}-${suffix++}`;
    used.add(id);
    if (level === "2") sections.push(`<a href="#${id}">${text}</a>`);
    return `<h${level} id="${id}">${label}</h${level}>`;
  });
  const contents = sections.length
    ? `<nav class="article-contents" aria-label="On this page"><p>On this page</p>${sections.join("")}</nav>`
    : "";
  return { html, contents };
}
function page(data, title, content, { id, projectHref } = {}) {
  return `<!doctype html><html lang="en" class="manual"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="light dark"><title>${e(title)} · Mesh-Talk Help</title><link rel="stylesheet" href="theme.css"><link rel="stylesheet" href="style.css"><script src="i18n.js" defer></script><link rel="describedby" type="text/plain" href="llms.txt">${id ? `<link rel="alternate" type="text/markdown" href="${id}.md">` : ""}</head><body><a class="skip" href="#main">Skip to content</a><header><div class="breadcrumb"><a class="wordmark" href="${projectHref}">Mesh-Talk</a><span aria-hidden="true">/</span><a href="index.html">User guide</a></div><div class="header-links"><a href="index.html">Search help</a><small data-version="${e(data.version)}">Version ${e(data.version)} · English</small><label class="language-control" hidden><span class="sr-only">Language</span><select id="site-language" aria-label="Language"><option value="en" lang="en">English</option><option value="es" lang="es">Español</option><option value="pt-BR" lang="pt-BR">Português</option><option value="ja" lang="ja">日本語</option><option value="ko" lang="ko">한국어</option><option value="zh-Hans" lang="zh-Hans">简体中文</option><option value="zh-Hant" lang="zh-Hant">繁體中文</option><option value="yue" lang="yue">粵語</option></select></label></div></header><div class="layout"><aside><details><summary>Browse help topics</summary><nav aria-label="Help topics">${navigation(data, id)}</nav></details></aside><main id="main" tabindex="-1">${content}</main></div><script type="module" src="navigation.mjs"></script><p id="language-status" class="language-status" role="status"></p></body></html>`;
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
    await copyFile(
      new URL("../../site/i18n.js", import.meta.url),
      join(dir, "i18n.js"),
    );
    await mkdir(join(dir, "locales"), { recursive: true });
    for (const locale of [
      "en",
      "es",
      "pt-BR",
      "ja",
      "ko",
      "zh-Hans",
      "zh-Hant",
      "yue",
    ])
      await copyFile(
        new URL(`../../site/locales/${locale}.json`, import.meta.url),
        join(dir, "locales", `${locale}.json`),
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
        '<h1>Mesh-Talk help</h1><p class="intro">Connect on your LAN, reach another site, and keep conversations moving when devices reconnect. Practical guides to messaging, files and privacy.</p><form id="help-search" role="search"><label for="search">Search help</label><div class="search-controls"><input id="search" name="q" type="search" placeholder="Try notifications or connecting" autocomplete="off" aria-controls="results" aria-describedby="status" aria-keyshortcuts="/"><button id="search-clear" type="button" hidden>Clear search</button></div></form><p id="status" role="status"></p><button id="search-retry" type="button" hidden>Retry search</button><noscript><p>Search requires JavaScript. Browse all topics below or use the topic navigation.</p></noscript><ul id="results">' +
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
      const sections = articleSections(body);
      const intro = sections.html.match(
        /^<h1>(.*?)<\/h1>\s*(?:<p>.*?<\/p>\s*)?/,
      );
      const heading = intro
        ? intro[0].replace("<h1>", '<h1 id="article-title">')
        : "";
      const article = intro
        ? sections.html.slice(intro[0].length)
        : sections.html;
      const index = data.topics.indexOf(t);
      const neighbors = [
        [data.topics[index - 1], "Previous"],
        [data.topics[index + 1], "Next"],
      ]
        .filter(([topic]) => topic)
        .map(
          ([topic, direction]) =>
            `<a href="${topic.id}.html"><span>${direction}</span>${e(topic.title)}</a>`,
        )
        .join("");
      await writeFile(
        join(dir, t.id + ".html"),
        page(
          data,
          t.title,
          `<p class="help-language-note" hidden>This guide is written in English. Navigation and search follow your language.</p><div class="article-heading" lang="en">${heading}</div><div class="article-layout"><article lang="en" aria-labelledby="article-title">${article}<footer><a href="${t.id}.md">Read as Markdown</a> · <a href="index.html">All help</a><nav class="topic-neighbors" aria-label="More help topics">${neighbors}</nav></footer></article>${sections.contents}</div>`,
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
