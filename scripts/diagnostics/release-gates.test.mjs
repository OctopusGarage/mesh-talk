import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");

test("release verification waits for every platform and checks exact-source signed artifacts", () => {
  assert.match(workflow, /^  verify-release:\n/m);
  const verify = workflow.split("  verify-release:\n")[1]?.split(/^  publish-release:/m)[0];
  assert.match(verify, /needs: \[preflight, build-release\]/);
  assert.match(verify, /actions\/download-artifact@/);
  assert.match(verify, /gh release download/);
  assert.match(verify, /scripts\/release\/verify-release\.sh/);
  assert.match(verify, /\$\{\{ github\.sha \}\}/);
});

test("publication is opt-in, tag-only and requires verified release evidence", () => {
  const publish = workflow.split("  publish-release:\n")[1];
  assert.ok(publish);
  assert.match(publish, /needs: \[preflight, verify-release\]/);
  assert.match(publish, /inputs\.publish_release/);
  assert.match(publish, /startsWith\(github\.ref, 'refs\/tags\/'\)/);
  assert.match(publish, /scripts\/release\/check-source-checks\.mjs/);
  assert.match(publish, /scripts\/release\/release-state\.mjs publish/);
  assert.match(publish, /name: verified-release-evidence/);
});

test("release preflight prevents overwriting published versions before building", () => {
  assert.match(workflow, /^  preflight:\n/m);
  assert.match(workflow, /scripts\/release\/preflight\.mjs/);
  assert.match(workflow.split("  build-release:\n")[1], /^    needs: preflight/m);
  assert.match(workflow, /concurrency:/);
});
