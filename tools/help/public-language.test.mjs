import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { fixture } from "./fixture.mjs";
import { buildHelp } from "./build.mjs";

const languages = [
  "en",
  "es",
  "pt-BR",
  "ja",
  "ko",
  "zh-Hans",
  "zh-Hant",
  "yue",
];
const resource = (code) =>
  new URL(`../../site/locales/${code}.json`, import.meta.url);
const placeholders = (value) =>
  [...value.matchAll(/\{\w+\}/g)].map((match) => match[0]).sort();
test("public translations cover the complete English contract and preserve placeholders", async () => {
  const english = JSON.parse(await readFile(resource("en"), "utf8"));
  for (const language of languages) {
    const messages = JSON.parse(await readFile(resource(language), "utf8"));
    assert.deepEqual(Object.keys(messages), Object.keys(english), language);
    for (const [key, value] of Object.entries(messages)) {
      assert.ok(
        typeof value === "string" && value.trim(),
        `${language}: ${key}`,
      );
      assert.deepEqual(
        placeholders(value),
        placeholders(key),
        `${language}: ${key}`,
      );
    }
    assert.ok(
      messages["MIT licensed · © 2026 OctopusGarage"].includes(
        "© 2026 OctopusGarage",
      ),
    );
    if (language !== "en")
      assert.notEqual(
        messages["Your conversations."],
        english["Your conversations."],
      );
  }
});

test("current and versioned readers ship their own language resources and declare English article fallback", async (t) => {
  const root = await fixture(t),
    out = join(root, "public");
  await buildHelp({ root, out });
  for (const prefix of ["", "v/0.4.0/"]) {
    for (const language of languages)
      assert.equal(
        await readFile(
          join(out, prefix, "locales", `${language}.json`),
          "utf8",
        ),
        await readFile(resource(language), "utf8"),
      );
    const html = await readFile(join(out, prefix, "connect.html"), "utf8");
    assert.match(html, /src="i18n.js"/);
    assert.match(html, /id="site-language"/);
    const selector = html.match(
      /<select id="site-language"[^>]*>([\s\S]*?)<\/select>/,
    )[1];
    assert.deepEqual(
      [...selector.matchAll(/<option value="([^"]+)"/g)].map(
        (match) => match[1],
      ),
      languages,
    );

    assert.match(html, /class="help-language-note" hidden/);
    assert.match(html, /<article lang="en"/);
    assert.match(html, /This guide is written in English/);
  }
});
