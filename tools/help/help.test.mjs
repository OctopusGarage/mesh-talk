import test from "node:test";
import assert from "node:assert/strict";
import { writeFile, readFile } from "node:fs/promises";
import { join } from "node:path";
import { fixture, topic } from "./fixture.mjs";
test("search weights titles, aliases and platform filters", async (t) => {
  const { searchHelp } = await import("./search.mjs");
  const a = { ...topic, body: "Open settings" };
  assert.equal(searchHelp([a], "连接")[0].id, "connect");
  assert.equal(searchHelp([a], "network")[0].id, "connect");
  assert.equal(searchHelp([a], "missing").length, 0);
  assert.equal(
    searchHelp([{ ...a, platforms: ["macos"] }], "", { platform: "linux" })
      .length,
    0,
  );
});
test("renderer escapes raw HTML and rejects executable links", async (t) => {
  const { renderMarkdown } = await import("./render.mjs");
  const html = renderMarkdown(
    "# Title\n\n<script>alert(1)</script>\n\n[x](javascript:alert) [other](connect.md)\n\n1. **Go** `now`",
  );
  assert.ok(!html.includes("<script>"));
  assert.ok(!html.includes('href="javascript:'));
  assert.ok(html.includes('href="connect.md"'));
  assert.ok(html.includes("<ol>"));
  assert.ok(html.includes("<strong>Go</strong>"));
});
test("loader and build preserve versioned documents and reject traversal", async (t) => {
  const { loadHelp } = await import("./load.mjs");
  const { buildHelp } = await import("./build.mjs");
  const root = await fixture(t);
  const data = await loadHelp({ root });
  assert.equal(data.version, "0.4.0");
  const out = join(root, "out");
  await buildHelp({ root, out });
  for (const p of [
    "index.html",
    "connect.html",
    "connect.md",
    "index.json",
    "llms.txt",
    "v/0.4.0/connect.html",
    "v/0.4.0/index.json",
  ])
    assert.ok((await readFile(join(out, p), "utf8")).length);
  assert.match(
    await readFile(join(out, "connect.html"), "utf8"),
    /href="connect.html"/,
  );
  await writeFile(
    join(root, "docs/help/catalog.json"),
    JSON.stringify({
      schemaVersion: 1,
      language: "en",
      topics: [{ ...topic, document: "../Cargo.toml" }],
    }),
  );
  await assert.rejects(loadHelp({ root }), /document/);
});
test("catalog rejects duplicate identifiers, invalid platform and invalid settings target", async (t) => {
  const { loadHelp } = await import("./load.mjs");
  const root = await fixture(t);
  for (const topics of [
    [topic, topic],
    [{ ...topic, platforms: ["android"] }],
    [{ ...topic, target: { kind: "settings", category: "secret" } }],
  ]) {
    await writeFile(
      join(root, "docs/help/catalog.json"),
      JSON.stringify({ schemaVersion: 1, language: "en", topics }),
    );
    await assert.rejects(loadHelp({ root }));
  }
});
test("all generated local page links resolve in root and version snapshot", async (t) => {
  const { buildHelp } = await import("./build.mjs");
  const root = await fixture(t),
    out = join(root, "site");
  await buildHelp({ root, out, url: "https://example.test/project/help/" });
  for (const prefix of ["", "v/0.4.0/"])
    for (const name of ["index.html", "connect.html"]) {
      const body = await readFile(join(out, prefix, name), "utf8");
      for (const [, href] of body.matchAll(/(?:href|src)="([^"#]+)"/g))
        if (!href.startsWith("../")) await readFile(join(out, prefix, href));
    }
  assert.match(
    await readFile(join(out, "v/0.4.0/llms.txt"), "utf8"),
    /https:\/\/example.test\/project\/help\/v\/0\.4\.0\/connect.md/,
  );
});
test("loader rejects reserved route identifiers before building", async (t) => {
  const { loadHelp } = await import("./load.mjs");
  const root = await fixture(t);
  await writeFile(
    join(root, "docs/help/catalog.json"),
    JSON.stringify({
      schemaVersion: 1,
      language: "en",
      topics: [{ ...topic, id: "index", document: "topics/index.md" }],
    }),
  );
  await assert.rejects(loadHelp({ root }), /topic id/);
});
test("loader accepts Windows line endings in Markdown", async (t) => {
  const { loadHelp } = await import("./load.mjs");
  const root = await fixture(t);
  await writeFile(
    join(root, "docs/help/topics/connect.md"),
    "# Connect devices\r\n\r\n## Start\r\n",
  );
  assert.equal(
    (await loadHelp({ root })).topics[0].body,
    "# Connect devices\n\n## Start\n",
  );
});
test("reader exposes compact navigation, current topic and machine-readable discovery", async (t) => {
  const { buildHelp } = await import("./build.mjs");
  const root = await fixture(t),
    out = join(root, "out");
  await buildHelp({ root, out });
  for (const prefix of ["", "v/0.4.0/"]) {
    const html = await readFile(join(out, prefix, "connect.html"), "utf8");
    assert.match(html, /<details/);
    assert.match(html, /Skip to content/);
    assert.match(html, /aria-current="page"/);
    assert.match(html, /type="text\/markdown"/);
    assert.match(html, /href="llms.txt"/);
    assert.match(
      html,
      new RegExp('href="' + (prefix ? "../../../" : "../") + 'index.html"'),
    );
  }
  assert.match(
    await readFile(join(out, "index.html"), "utf8"),
    /<h1>Mesh-Talk help<\/h1>/,
  );
});

test("malformed metadata produces controlled validation errors", async (t) => {
  const { loadHelp } = await import("./load.mjs");
  const root = await fixture(t);
  for (const broken of [
    { ...topic, title: 4 },
    { ...topic, summary: {} },
    null,
    { ...topic, target: "settings" },
  ]) {
    await writeFile(
      join(root, "docs/help/catalog.json"),
      JSON.stringify({ schemaVersion: 1, language: "en", topics: [broken] }),
    );
    await assert.rejects(
      loadHelp({ root }),
      (error) =>
        error instanceof Error &&
        !(error instanceof TypeError) &&
        /Invalid/.test(error.message),
    );
  }
});
