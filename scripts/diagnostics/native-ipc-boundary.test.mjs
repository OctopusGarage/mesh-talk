import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

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

test("local macOS contact acceptance stops before foreground work", { skip: process.platform !== "darwin" }, () => {
  const env = { ...process.env };
  delete env.GITHUB_ACTIONS;
  delete env.NATIVE_CONTACT_ALLOW_FOREGROUND;
  const runner = fileURLToPath(new URL("./hidden-contacts.mjs", import.meta.url));
  const result = spawnSync(process.execPath, [runner], { env, encoding: "utf8", timeout: 5000 });
  assert.ifError(result.error);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /Native contact acceptance activates windows/);
});
