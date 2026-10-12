import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

const diagnostics = [
  "hidden-contacts.mjs",
  "native-core-scenarios.mjs",
  "network-console.mjs",
  "renderer-probe.mjs",
  "tray-geometry.mjs",
  "window-controls.mjs",
];

test("native diagnostics work with the global Tauri API disabled", async () => {
  const config = JSON.parse(
    await readFile(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  );
  assert.equal(config.app.withGlobalTauri, false);
  for (const name of diagnostics) {
    const source = await readFile(new URL(name, import.meta.url), "utf8");
    assert.doesNotMatch(
      source,
      /window\.__TAURI__\.(?:core|event)|!!window\.__TAURI__(?!_INTERNALS__)/,
      `${name} must use scoped Tauri IPC`,
    );
  }
});
