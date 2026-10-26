import { escapeHtml } from "./render.mjs";

export function publicationBase(url, label) {
  let base;
  try {
    base = new URL(url);
  } catch {
    throw Error(`${label} URL must be an absolute HTTPS directory URL`);
  }
  if (
    base.protocol !== "https:" ||
    !base.pathname.endsWith("/") ||
    base.search ||
    base.hash ||
    base.username ||
    base.password
  )
    throw Error(
      `${label} URL must be an absolute HTTPS directory URL without credentials, query or fragment`,
    );
  return base;
}

export function sitemap(urls) {
  return (
    '<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n' +
    [...new Set(urls)]
      .map((url) => `  <url><loc>${escapeHtml(String(url))}</loc></url>`)
      .join("\n") +
    "\n</urlset>\n"
  );
}

export function documentationScope(version) {
  return `Documentation source version: ${version}. This describes the checked-out source, not proof of a published Stable release. Check installed-version compatibility and GitHub Releases before applying instructions. Version-qualified documentation may change while that source version is under development; a clean deployment retains only the current snapshot.`;
}

// The manual renderer accepts this same restricted relative-topic link syntax.
// Preserve anchors while moving articles into indexes and aggregated exports.
export function absoluteTopicLinks(body, base) {
  return body.replace(
    /\]\(([a-z0-9-]+\.md(?:#[^)]*)?)\)/g,
    (_, href) => `](${new URL(href, base)})`,
  );
}

export function markdownIndex(data, base) {
  return (
    `# Mesh-Talk help\n\n${documentationScope(data.version)}\n\n` +
    data.topics
      .map(
        (topic) =>
          `- [${topic.title}](${new URL(topic.id + ".md", base)}): ${topic.summary}`,
      )
      .join("\n") +
    "\n"
  );
}

export function fullManual(data, base) {
  return (
    `# Mesh-Talk public manual\n\n${documentationScope(data.version)}\n\n` +
    data.topics
      .map(
        (topic) =>
          `---\n\nSource: ${new URL(topic.id + ".html", base)}\n\n${absoluteTopicLinks(topic.body, base).trim()}\n`,
      )
      .join("\n")
  );
}
