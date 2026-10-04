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
