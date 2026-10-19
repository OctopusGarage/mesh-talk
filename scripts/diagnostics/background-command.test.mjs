import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = fileURLToPath(new URL("../../src-tauri/src/", import.meta.url));
function rustFiles(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const filename = path.join(directory, entry.name);
    return entry.isDirectory() ? rustFiles(filename) : filename.endsWith(".rs") ? [filename] : [];
  });
}

test("desktop async subprocesses use the Windows-safe background command factory", () => {
  for (const filename of rustFiles(root)) {
    if (filename === path.join(root, "background_command.rs")) continue;
    const source = readFileSync(filename, "utf8");
    assert.doesNotMatch(source, /(?:tokio::process::Command::new|use\s+tokio::process::Command\b)/,
      `${path.relative(root, filename)} must use crate::background_command::command`);
  }
  const factory = readFileSync(path.join(root, "background_command.rs"), "utf8");
  assert.match(factory, /#\[cfg\(target_os = "windows"\)\]\s*command\.creation_flags\(0x0800_0000\)/);
});
