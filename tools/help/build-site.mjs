import { cp, readFile, writeFile } from "node:fs/promises";
import { resolve, join, sep } from "node:path";
import { pathToFileURL } from "node:url";
import { buildHelp } from "./build.mjs";
import { repositoryRoot } from "./load.mjs";
import {
  publicationBase,
  sitemap,
  absoluteTopicLinks,
  documentationScope,
} from "./discovery.mjs";

export async function buildPublicSite({
  root = repositoryRoot,
  out,
  url = "https://octopusgarage.github.io/mesh-talk/",
} = {}) {
  if (!out) throw Error("Output directory required");
  const base = publicationBase(url, "Site");
  const destination = resolve(out),
    source = resolve(root, "site");
  if (
    destination === resolve(root) ||
    destination === source ||
    destination.startsWith(source + sep)
  )
    throw Error("Output must not overwrite site sources");
  const data = await buildHelp({
    root,
    out: join(destination, "help"),
    url: new URL("help/", base).href,
  });
  const overview = data.topics.find((topic) => topic.id === "about-mesh-talk");
  if (!overview) throw Error("Missing about-mesh-talk manual topic");
  await cp(source, destination, {
    recursive: true,
    filter: (path) => path !== join(source, "market"),
  });
  let html = await readFile(join(destination, "index.html"), "utf8");
  // Rebase this project's absolute metadata while preserving external destinations.
  html = html.replaceAll(
    "https://octopusgarage.github.io/mesh-talk/",
    base.href,
  );
  const schema = {
    "@context": "https://schema.org",
    "@type": "SoftwareApplication",
    "@id": new URL("#application", base).href,
    name: "Mesh-Talk",
    url: base.href,
    description: overview.summary,
    applicationCategory: "CommunicationApplication",
    operatingSystem: "macOS, Windows, Linux",
    license: "https://opensource.org/license/mit/",
    sameAs: "https://github.com/OctopusGarage/mesh-talk",
    softwareHelp: new URL("help/index.html", base).href,
  };
  const marker = "<!-- PUBLIC_SITE_METADATA -->";
  if (!html.includes(marker))
    throw Error("Missing public site metadata marker");
  html = html.replace(
    marker,
    `<script type="application/ld+json">${JSON.stringify(schema).replaceAll("<", "\\u003c")}</script>`,
  );
  await writeFile(join(destination, "index.html"), html);
  const helpBase = new URL("help/", base);
  await writeFile(
    join(destination, "index.md"),
    absoluteTopicLinks(overview.body, helpBase).trim() +
      `\n\n${documentationScope(data.version)}\n\nCanonical overview: ${new URL("help/about-mesh-talk.html", base)}\n`,
  );
  const helpIndex = await readFile(join(destination, "help/llms.txt"), "utf8");
  await writeFile(
    join(destination, "llms.txt"),
    `# Mesh-Talk\n\n> ${overview.summary}\n\n${documentationScope(data.version)}\n\n## Project\n\n- [Overview](${new URL("index.md", base)})\n- [Website](${base})\n- [Source repository](https://github.com/OctopusGarage/mesh-talk)\n- [Published releases](https://github.com/OctopusGarage/mesh-talk/releases)\n- [Read-only documentation MCP setup](https://github.com/OctopusGarage/mesh-talk/blob/main/tools/help/README.md)\n\n## Public manual\n\n${helpIndex.slice(helpIndex.indexOf("- [Markdown index]"))}`,
  );
  await cp(
    join(destination, "help/llms-full.txt"),
    join(destination, "llms-full.txt"),
  );
  await writeFile(
    join(destination, "sitemap.xml"),
    sitemap([
      base,
      new URL("help/index.html", base),
      ...data.topics.map((topic) => new URL(`help/${topic.id}.html`, base)),
    ]),
  );
  return data;
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  const args = process.argv.slice(2);
  const outIndex = args.indexOf("--out"),
    urlIndex = args.indexOf("--url");
  if (
    outIndex < 0 ||
    !args[outIndex + 1] ||
    args[outIndex + 1].startsWith("--") ||
    (urlIndex >= 0 &&
      (!args[urlIndex + 1] || args[urlIndex + 1].startsWith("--")))
  )
    throw Error(
      "Usage: node tools/help/build-site.mjs --out <directory> [--url <https-directory-url>]",
    );
  await buildPublicSite({
    out: args[outIndex + 1],
    url: urlIndex >= 0 ? args[urlIndex + 1] : undefined,
  });
}
