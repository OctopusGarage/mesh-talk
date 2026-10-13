import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { checkBuildSyntax } from "./check-build-syntax.mjs";

test("checks all built JavaScript chunks and rejects class static blocks", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "mesh-talk-syntax-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  mkdirSync(join(directory, "assets"));
  writeFileSync(join(directory, "index.js"), "export const value = 1;");
  writeFileSync(
    join(directory, "assets", "chat.js"),
    "class Store { static { this.ready = true } }",
  );

  assert.throws(
    () => checkBuildSyntax(directory),
    /chat\.js: Unexpected token/,
  );

  writeFileSync(
    join(directory, "assets", "chat.js"),
    "class Store {}\nStore.ready = true;",
  );
  assert.equal(checkBuildSyntax(directory), 2);
});

test("fails if the build produced no JavaScript", (t) => {
  const directory = mkdtempSync(join(tmpdir(), "mesh-talk-syntax-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  assert.throws(() => checkBuildSyntax(directory), /No JavaScript files found/);
});
