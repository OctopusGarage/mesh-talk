import assert from "node:assert/strict";
import test from "node:test";
import { validateSelectors } from "../validation/selectors.mjs";

const plan = { coreFilters: ["postoffice::"], hostFilters: ["chat_commands::platform::screenshot_tests::"], coreTests: [] };
const artifacts = ["mesh_talk_core", "mesh_talk"].map(name => JSON.stringify({ reason: "compiler-artifact", target: { name }, executable: `/tmp/${name}` })).join("\n");

test("selectors are checked against the actual compiled test inventory without running tests", async () => {
  const commands = [];
  const run = async (program, args) => {
    commands.push([program, args]);
    return { code: 0, output: program === "cargo" ? artifacts : program.endsWith("mesh_talk_core") ? "postoffice::tests::event: test\n" : "chat_commands::platform::screenshot_tests::png: test\n" };
  };
  const result = await validateSelectors(plan, { run });
  assert.equal(result.length, 2);
  assert.ok(commands[0][1].includes("--no-run"));
  assert.deepEqual(commands.slice(1).map(([, args]) => args), [["--list"], ["--list"]]);
});

test("a nonexistent host module fails before any test/browser execution", async () => {
  const run = async program => ({ code: 0, output: program === "cargo" ? artifacts : "chat_commands::platform::screenshot_tests::png: test\n" });
  await assert.rejects(validateSelectors({ coreFilters: [], hostFilters: ["chat_commands::platform::tests::"], coreTests: [] }, { run }), /zero.*host.*chat_commands::platform::tests::/i);
});

test("failed inventory builds and missing binaries cannot validate a selection", async () => {
  await assert.rejects(validateSelectors(plan, { run: async () => ({ code: 1, output: "compile error" }) }), /compile error/);
  await assert.rejects(validateSelectors(plan, { run: async () => ({ code: 0, output: "" }) }), /test executable/i);
});
