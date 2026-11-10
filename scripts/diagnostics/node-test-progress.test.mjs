import assert from "node:assert/strict";
import { mkdtemp, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { execute } from "../validation/stage.mjs";

test("actual Node progress identifies a begun parked file before owned interruption", { timeout: 30000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "mesh-node-progress-"));
  const fixture = join(directory, "parked.test.mjs"), output = join(directory, "progress.jsonl"), ready = join(directory, "begun");
  await writeFile(fixture, `import {test} from 'node:test';import {writeFileSync} from 'node:fs';test('parked fixture',async()=>{writeFileSync(${JSON.stringify(ready)},'begun');setInterval(()=>{},1000);await new Promise(()=>{});});`);
  const controller = new AbortController();
  let observed = false, checking = false;
  const timer = setInterval(async () => {
    if (checking) return; checking = true;
    try {
      const text = await readFile(output, "utf8");
      if (text.includes('test:dequeue') && await readFile(ready, "utf8") === "begun") { observed = true; controller.abort(); }
    } catch {} finally { checking = false; }
  }, 25);
  const watchdog = setTimeout(() => controller.abort(), 20000);
  let result; let diagnostic = "";
  try {
    const env = { ...process.env }; delete env.MESH_VALIDATION_PROCESS_GROUP_OWNER; delete env.NODE_TEST_CONTEXT;
    result = await execute([process.execPath, "--test", `--test-reporter=${new URL("./node-test-progress.mjs", import.meta.url).href}`, `--test-reporter-destination=${output}`, fixture], {
      cwd: directory, env, signal: controller.signal, onOutput(chunk) { diagnostic += chunk; },
    });
    assert.equal(observed, true, `progress must be durable while the case is still parked: ${diagnostic}`);
    assert.notEqual(result.code, 0);
    assert.equal(result.cleanupUnverified, false);
    const rows = (await readFile(output, "utf8")).trim().split("\n").map(JSON.parse);
    assert(rows.some(row => row.event === "test:dequeue" && (row.file === fixture || row.name.includes("parked"))));
  } finally {
    clearInterval(timer); clearTimeout(watchdog); controller.abort();
    if (result?.cleanupUnverified === false) await rm(directory, { recursive: true, force: true });
    else console.error(`Retaining unverified progress fixture ${directory}`);
  }
});
