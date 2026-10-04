import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const workflow = readFileSync(new URL("../../.github/workflows/mutants.yml", import.meta.url), "utf8");
const commands = [...workflow.matchAll(/^\s+run: (cargo mutants .+)$/gm)].map((match) => match[1]);

test("both mutation modes forward test threads to the test binary, not Cargo", () => {
  assert.equal(commands.length, 2);
  for (const command of commands) {
    const args = command.split(/\s+/).slice(2);
    const cargoArgs = args.slice(args.indexOf("--") + 1);
    assert.deepEqual(cargoArgs, ["--", "--test-threads=2"]);
  }
});

test("mutation report upload includes the workspace-root output directory", () => {
  const upload = workflow.split(/^      - /m).find((step) => /name: Upload mutants report/.test(step));
  assert.ok(upload, "mutation report upload must be present");
  assert.match(upload, /^\s+(?:path: )?mutants\.out\/\s*$/m);
  assert.match(upload, /if: always\(\) && hashFiles\('mutants\.out\/\*\*'\) != ''/);
});
