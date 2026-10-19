import test from "node:test";
import assert from "node:assert/strict";
import { readFile, readdir, mkdtemp, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { loadHelp, repositoryRoot } from "./load.mjs";
import { buildHelp } from "./build.mjs";
test("canonical corpus loads and every settings target exists in frontend source", async (t) => {
  const data = await loadHelp();
  assert.ok(data.topics.length >= 15);
  const settingsDir = join(
    repositoryRoot,
    "frontend/src/features/chat/settings",
  );
  const sources = (
    await Promise.all(
      (await readdir(settingsDir))
        .filter((f) => f.endsWith(".tsx"))
        .map((f) => readFile(join(settingsDir, f), "utf8")),
    )
  ).join("\n");
  for (const topic of data.topics) {
    assert.ok(topic.body.includes("## "), topic.id + " needs useful sections");
    if (topic.target?.controlId)
      assert.ok(
        sources.includes('id="' + topic.target.controlId + '"'),
        topic.id + " target " + topic.target.controlId + " missing",
      );
  }
});
test("canonical static corpus produces readable root and versioned routes with no broken local links", async (t) => {
  const out = await mkdtemp(join(tmpdir(), "mesh-help-corpus-"));
  t.after(() => rm(out, { recursive: true, force: true }));
  const data = await buildHelp({ out });
  for (const prefix of ["", `v/${data.version}/`])
    for (const file of [
      "index.html",
      ...data.topics.map((t) => t.id + ".html"),
    ]) {
      const html = await readFile(join(out, prefix, file), "utf8");
      assert.match(html, /<html\b[^>]*\blang="en"/);
      for (const [, href] of html.matchAll(/(?:href|src)="([^"#]+)"/g)) {
        if (href.startsWith("https://") || href.startsWith("../")) continue;
        await readFile(join(out, prefix, href.split("#")[0]));
      }
    }
});
