import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve } from "node:path";
export const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));
const platforms = ["macos", "windows", "linux"];
const categories = [
  "getting-started",
  "messaging",
  "connection",
  "privacy",
  "personalization",
];
const settings = [
  "general",
  "appearance",
  "notifications",
  "connection",
  "privacy",
  "files",
  "history",
];
export async function loadHelp({ root = repositoryRoot } = {}) {
  const catalog = JSON.parse(
    await readFile(resolve(root, "docs/help/catalog.json"), "utf8"),
  );
  if (
    !catalog ||
    typeof catalog !== "object" ||
    catalog.schemaVersion !== 1 ||
    catalog.language !== "en" ||
    !Array.isArray(catalog.topics) ||
    !catalog.topics.length
  )
    throw Error("Invalid help catalog");
  const cargo = await readFile(resolve(root, "Cargo.toml"), "utf8");
  const version = cargo.match(
    /\[workspace\.package\][\s\S]*?\bversion\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+(?:[-+][a-zA-Z0-9.-]+)?)"/,
  )?.[1];
  if (!version) throw Error("Missing workspace version");
  const ids = new Set();
  const topics = [];
  for (const t of catalog.topics) {
    if (!t || typeof t !== "object" || Array.isArray(t))
      throw Error("Invalid topic metadata");
    if (
      typeof t.id !== "string" ||
      !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(t.id) ||
      t.id === "index" ||
      ids.has(t.id)
    )
      throw Error("Invalid or duplicate topic id");
    ids.add(t.id);
    if (t.document !== `topics/${t.id}.md`)
      throw Error("Invalid topic document");
    if (
      typeof t.title !== "string" ||
      !t.title.trim() ||
      typeof t.summary !== "string" ||
      !t.summary.trim() ||
      !categories.includes(t.category) ||
      !Array.isArray(t.keywords) ||
      t.keywords.some((k) => typeof k !== "string") ||
      !Array.isArray(t.platforms) ||
      !t.platforms.length ||
      t.platforms.some((p) => !platforms.includes(p))
    )
      throw Error("Invalid topic metadata: " + t.id);
    if (
      t.target &&
      !(
        t.target.kind === "diagnostics" ||
        (t.target.kind === "settings" &&
          settings.includes(t.target.category) &&
          (t.target.controlId === undefined ||
            /^[a-zA-Z][a-zA-Z0-9-]*$/.test(t.target.controlId)))
      )
    )
      throw Error("Invalid topic target: " + t.id);
    const body = (
      await readFile(resolve(root, "docs/help", t.document), "utf8")
    ).replace(/\r\n/g, "\n");
    if (!body.startsWith("# " + t.title + "\n"))
      throw Error("Topic title mismatch: " + t.id);
    topics.push({ ...t, body });
  }
  for (const t of topics)
    for (const m of t.body.matchAll(/\]\(([a-z0-9-]+)\.md(?:#[^)]*)?\)/g))
      if (!ids.has(m[1])) throw Error("Unknown topic link: " + m[1]);
  return { schemaVersion: 1, language: "en", version, topics };
}
