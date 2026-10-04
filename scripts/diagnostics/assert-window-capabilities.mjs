/** Run immediately after cargo build, before a native probe can consume stale ACL data. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = JSON.parse(readFileSync("src-tauri/capabilities/default.json", "utf8"));
const generated = JSON.parse(readFileSync("src-tauri/gen/schemas/capabilities.json", "utf8"));
const main = generated[source.identifier];
assert.ok(main, "Build must generate the main capability");
assert.deepEqual(main.windows, source.windows, "Generated window scope differs from source");
assert.deepEqual(main.permissions, source.permissions, "Generated ACL is stale; rebuild the application package");
console.log("PASS: generated main-window ACL matches the source capability");
