import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
assert.equal(
  existsSync(resolve(root, "frontend/dist/builtin-packs")),
  false,
  "desktop build contains optional bundled packs",
);
assert.equal(
  existsSync(resolve(root, "frontend/dist/stickers/noto")),
  false,
  "desktop build contains animated Noto stickers",
);
console.log("Frontend assets verified: no optional bundled packs");
