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


test("integration and MCP selectors use their real compiled inventories and reject empty matches", async () => {
  const selected = { coreFilters: [], hostFilters: [], coreTests: [], integrationFilters: ["grants::"], mcpFilters: ["tools::"] };
  const output = ["mesh_talk_integrations", "mesh_talk_mcp"].map(name => JSON.stringify({ reason: "compiler-artifact", target: { name }, executable: `/tmp/${name}` })).join("\n");
  const run = async program => ({ code: 0, output: program === "cargo" ? output : program.endsWith("integrations") ? "grants::allows: test\n" : "tools::send: test\n" });
  assert.deepEqual(await validateSelectors(selected, { run }), [{ kind: "integration", filter: "grants::", count: 1 }, { kind: "mcp", filter: "tools::", count: 1 }]);
  await assert.rejects(validateSelectors({ ...selected, mcpFilters: ["missing::"] }, { run }), /zero.*mcp.*missing::/i);
  await assert.rejects(validateSelectors(selected, { run: async () => ({ code: 0, output: "" }) }), /integration test executable/i);
});

test("direct selector CLI owns its command group without an inherited stage", { skip: process.platform === "win32", timeout: 10000 }, async t => {
  const { mkdtemp, writeFile, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { fileURLToPath } = await import("node:url");
  const { runCommand } = await import("../pr-platform/guest.mjs");
  const directory = await mkdtemp(join(tmpdir(), "mesh-selector-cli-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const cargo = join(directory, "fixture-cargo");
  await writeFile(cargo, `#!${process.execPath}\nif(process.argv.includes('--list'))console.log('fixture::actual_name: test');else console.log(JSON.stringify({reason:'compiler-artifact',target:{name:'mesh_talk_core'},executable:__filename}));\n`, { mode: 0o755 });
  const result = await runCommand(process.execPath, [fileURLToPath(new URL("../validation/selectors.mjs", import.meta.url)), "--core-filter", "fixture::"], { cwd: directory, env: { ...process.env, MESH_PR_CARGO: cargo, MESH_VALIDATION_PROCESS_GROUP_OWNER: "" } });
  assert.equal(result.code, 0, result.error || result.output);
  assert.match(result.output, /fixture::.*1 tests available/);
});
