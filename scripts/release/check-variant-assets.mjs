import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const variant = process.argv[2];
assert.ok(variant === "default" || variant === "lite", "unknown edition");

const root = fileURLToPath(new URL("../../", import.meta.url));
const packDirectory = resolve(root, "frontend/dist/builtin-packs");
if (variant === "lite") {
  assert.equal(existsSync(packDirectory), false, "lite contains bundled packs");
} else {
  const catalog = JSON.parse(
    readFileSync(resolve(root, "site/market/catalog.json"), "utf8"),
  );
  const lock = JSON.parse(
    readFileSync(resolve(root, "frontend/bundled-packs.lock.json"), "utf8"),
  );
  const bundled = lock.map((pin) => {
    const entry = catalog.find(({ id }) => id === pin.id);
    assert.ok(entry, `missing catalog entry: ${pin.id}`);
    assert.equal(entry.version, pin.version);
    assert.equal(entry.sha256, pin.sha256);
    return entry;
  });
  const expected = bundled.map(({ id }) => `${id}.zip`).sort();
  assert.deepEqual(readdirSync(packDirectory).sort(), expected);
  for (const { id, sha256 } of bundled) {
    const actual = createHash("sha256")
      .update(readFileSync(resolve(packDirectory, `${id}.zip`)))
      .digest("hex");
    assert.equal(actual, sha256, `bundled pack checksum mismatch: ${id}`);
  }
}
console.log(`${variant} frontend assets verified`);
