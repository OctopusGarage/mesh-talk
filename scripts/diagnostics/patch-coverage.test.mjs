import assert from "node:assert/strict";
import { test } from "node:test";
import {
  addedSourceLines,
  parseLcov,
  summarizePatchCoverage,
} from "../patch-coverage.mjs";

test("patch coverage counts added executable lines and ignores deleted hunks", () => {
  const diff = [
    "+++ b/frontend/src/lib/theme.ts",
    "@@ -8,0 +9,2 @@",
    "+run();",
    "+fallback();",
    "@@ -20,2 +22,0 @@",
    "-old();",
  ].join("\n");
  const added = addedSourceLines(diff);
  const unit = parseLcov(
    "SF:src/lib/theme.ts\nDA:9,1\nDA:10,0\nend_of_record\n",
  );

  assert.deepEqual(summarizePatchCoverage(added, [unit]), {
    covered: 1,
    total: 2,
    missing: [{ file: "src/lib/theme.ts", line: 10 }],
  });
});

test("browser coverage can cover lines missed by unit tests", () => {
  const added = addedSourceLines(
    "+++ b/frontend/src/components/AvatarGallery.tsx\n@@ -0,0 +12,1 @@\n+render();\n",
  );
  const unit = parseLcov(
    "SF:src/components/AvatarGallery.tsx\nDA:12,0\nend_of_record\n",
  );
  const browser = parseLcov(
    "SF:src/components/AvatarGallery.tsx\nDA:12,1\nend_of_record\n",
  );

  assert.deepEqual(summarizePatchCoverage(added, [unit, browser]), {
    covered: 1,
    total: 1,
    missing: [],
  });
});

test("patch coverage de-duplicates overlapping hunks", () => {
  const added = addedSourceLines(
    "+++ b/frontend/src/lib/theme.ts\n@@ -0,0 +4,1 @@\n+x\n@@ -0,0 +4,1 @@\n+x\n",
  );
  const unit = parseLcov("SF:src/lib/theme.ts\nDA:4,0\nend_of_record\n");

  assert.equal(summarizePatchCoverage(added, [unit]).total, 1);
});
