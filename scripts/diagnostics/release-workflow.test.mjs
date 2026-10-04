import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(new URL("../../.github/workflows/release.yml", import.meta.url), "utf8");

test("platform uploads remain drafts until all release artifacts are verified", () => {
  const uploads = workflow.split(/^      - /m).filter((step) => /uses: softprops\/action-gh-release@/.test(step));
  assert.ok(uploads.length > 0, "release upload action must be present");
  for (const upload of uploads) {
    const inputs = upload.match(/^        with:\n((?:^          .*\n)+)/m)?.[1];
    assert.ok(inputs, "release upload must declare its inputs");
    assert.match(inputs, /^          draft: true\s*$/m, "platform upload must not publish an incomplete release");
  }
});
