import test from "node:test";
import assert from "node:assert/strict";
import { readFile, writeFile, mkdtemp, rm, access } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fixture, topic } from "./fixture.mjs";
import { buildHelp } from "./build.mjs";

const base = "https://example.test/products/chat/help/";
test("help discovery preserves canonical version scope and escapes metadata", async (t) => {
  const root = await fixture(t),
    out = join(root, "out");
  await writeFile(
    join(root, "docs/help/catalog.json"),
    JSON.stringify({
      schemaVersion: 1,
      language: "en",
      topics: [{ ...topic, summary: 'Find "peers" & <devices>' }],
    }),
  );
  await buildHelp({ root, out, url: base });
  for (const prefix of ["", "v/0.4.0/"]) {
    const html = await readFile(join(out, prefix, "connect.html"), "utf8");
    assert.ok(
      html.includes(
        `<link rel="canonical" href="${base}${prefix}connect.html">`,
      ),
    );
    assert.ok(
      html.includes(
        'name="description" content="Find &quot;peers&quot; &amp; &lt;devices&gt;"',
      ),
    );
    const index = await readFile(join(out, prefix, "index.html"), "utf8");
    assert.ok(index.includes('type="text/markdown" href="index.md"'));
    const markdown = await readFile(join(out, prefix, "index.md"), "utf8");
    assert.ok(markdown.includes(`${base}${prefix}connect.md`));
    assert.ok(markdown.includes("0.4.0"));
    const full = await readFile(join(out, prefix, "llms-full.txt"), "utf8");
    assert.ok(full.includes(`${base}${prefix}connect.html`));
    assert.ok(full.includes("not proof of a published Stable release"));
    assert.ok(full.includes("# Connect devices"));
    assert.ok(full.includes(`[Again](${base}${prefix}connect.md)`));
  }
});

test("help sitemap lists current canonical HTML once, excluding representations and snapshots", async (t) => {
  const root = await fixture(t),
    out = join(root, "out");
  await buildHelp({ root, out, url: base });
  const sitemap = await readFile(join(out, "sitemap.xml"), "utf8");
  assert.deepEqual(
    [...sitemap.matchAll(/<loc>(.*?)<\/loc>/g)].map((m) => m[1]),
    [base + "index.html", base + "connect.html"],
  );
  assert.doesNotMatch(sitemap, /lastmod|\/v\/|\.md<|\.json</);
});

test("invalid publication URLs fail before writing output", async (t) => {
  const root = await fixture(t),
    out = join(root, "out");
  for (const url of [
    "http://example.test/help/",
    "https://example.test/help",
    "https://example.test/help/?lang=en",
    "https://example.test/help/#intro",
    "https://user:pass@example.test/help/",
  ]) {
    await assert.rejects(buildHelp({ root, out, url }), /Help URL/);
    await assert.rejects(access(out));
  }
});

test("public site builds share the manual and honor a custom project subpath", async (t) => {
  const { buildPublicSite } = await import("./build-site.mjs");
  const out = await mkdtemp(join(tmpdir(), "mesh-discovery-"));
  t.after(() => rm(out, { recursive: true, force: true }));
  const url = "https://example.test/products/chat/";
  const data = await buildPublicSite({ out, url });
  const html = await readFile(join(out, "index.html"), "utf8");
  assert.match(html, /rel="alternate" type="text\/markdown" href="index.md"/);
  assert.match(html, /rel="describedby" type="text\/plain" href="llms.txt"/);
  assert.ok(html.includes(`<link rel="canonical" href="${url}"`));
  assert.doesNotMatch(html, /https:\/\/octopusgarage.github.io\/mesh-talk\//);
  const schema = JSON.parse(
    html.match(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/)[1],
  );
  assert.equal(schema["@type"], "SoftwareApplication");
  assert.equal(schema.url, url);
  assert.equal(schema.softwareVersion, undefined);
  assert.equal(schema.aggregateRating, undefined);
  const overview = await readFile(join(out, "index.md"), "utf8");
  assert.ok(
    overview.includes(
      data.topics.find((t) => t.id === "about-mesh-talk").body.split("\n")[0],
    ),
  );
  assert.ok(overview.includes(`${url}help/routed-networks.md`));
  assert.ok(overview.includes(`Documentation source version: ${data.version}`));
  const llms = await readFile(join(out, "llms.txt"), "utf8");
  assert.ok(llms.includes(`${url}index.md`));
  for (const topic of data.topics)
    assert.ok(llms.includes(`${url}help/${topic.id}.md`));
  const full = await readFile(join(out, "llms-full.txt"), "utf8");
  assert.ok(full.includes("# About Mesh-Talk"));
  const sitemap = await readFile(join(out, "sitemap.xml"), "utf8");
  const locations = [...sitemap.matchAll(/<loc>(.*?)<\/loc>/g)].map(
    (m) => m[1],
  );
  assert.equal(new Set(locations).size, locations.length);
  assert.ok(locations.includes(url));
  for (const topic of data.topics)
    assert.ok(locations.includes(`${url}help/${topic.id}.html`));
  for (const location of locations) {
    const relative = location.slice(url.length) || "index.html";
    await access(join(out, relative));
  }
  await assert.rejects(access(join(out, "market")));
});
