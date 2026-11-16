import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import postcss from "postcss";
import config from "../postcss.config.js";

async function compile(css) {
  const plugins = Array.isArray(config.plugins)
    ? config.plugins
    : await Promise.all(
        Object.entries(config.plugins).map(async ([name, options]) =>
          (await import(name)).default(options),
        ),
      );
  return postcss(plugins).process(css, {
    from: fileURLToPath(new URL("../src/index.css", import.meta.url)),
  });
}

test("theme opacity utilities retain alpha without color-mix support", async () => {
  const result = await compile(`@import "tailwindcss";
    @config "../tailwind.config.js";
    @source inline("bg-signal/10 text-primary-foreground/80 border-border/50 dark:bg-signal/20 hover:bg-signal/15");`);
  const declarations = [];
  result.root.walkDecls((decl) => declarations.push(decl.value));
  for (const value of [
    "hsl(var(--signal) / 10%)",
    "hsl(var(--primary-foreground) / 80%)",
    "hsl(var(--border) / 50%)",
    "hsl(var(--signal) / 20%)",
    "hsl(var(--signal) / 15%)",
  ])
    assert.ok(
      declarations.includes(value),
      `Missing compatible alpha: ${value}`,
    );
  assert.ok(
    !declarations.some((value) =>
      /color-mix\(in \w+, hsl\(var\(--/.test(value),
    ),
  );
});

test("alpha lowering preserves unrelated mixes and supports boundary percentages", async () => {
  const input = `.control {
    color: color-mix(in oklab, red 10%, transparent);
    background: color-mix(in oklab, hsl(var(--signal)) 0%, transparent);
    border-color: color-mix(in oklab, hsl(var(--signal)) 100%, transparent);
    outline-color: color-mix(in srgb, hsl(var(--signal)) 12.5%, transparent);
    fill: color-mix(in oklab, hsl(var(--signal)) 10%, white);
  }`;
  const result = await compile(input);
  assert.ok(result.css.includes("color-mix(in oklab, red 10%, transparent)"));
  assert.ok(result.css.includes("hsl(var(--signal) / 0%)"));
  assert.ok(result.css.includes("hsl(var(--signal) / 100%)"));
  assert.ok(result.css.includes("hsl(var(--signal) / 12.5%)"));
  assert.ok(
    result.css.includes("color-mix(in oklab, hsl(var(--signal)) 10%, white)"),
  );
});

test("production CSS retains translucent theme backgrounds outside feature guards", async () => {
  const { build } = await import("vite");
  const result = await build({
    root: fileURLToPath(new URL("../", import.meta.url)),
    logLevel: "silent",
    build: { write: false },
  });
  const css = result.output.find((asset) =>
    asset.fileName.endsWith(".css"),
  ).source;
  const root = postcss.parse(css);
  let found = false;
  root.walkRules((rule) => {
    if (rule.selector !== ".bg-signal\\/10") return;
    const backgrounds = rule.nodes.filter(
      (node) => node.type === "decl" && node.prop === "background-color",
    );
    for (const decl of backgrounds.slice(-1)) {
      assert.equal(decl.value, "hsl(var(--signal) / 10%)");
      for (let parent = rule.parent; parent; parent = parent.parent) {
        assert.ok(
          parent.type !== "atrule" || parent.name !== "supports",
          "Compatible background must not require color-mix support",
        );
      }
      found = true;
    }
  });
  assert.ok(found, "Missing production personal-icon background");
});
