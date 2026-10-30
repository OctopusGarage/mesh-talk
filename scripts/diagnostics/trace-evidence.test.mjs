import assert from "node:assert/strict";
import { test } from "node:test";
import { traceFixture as fixture } from "./test-fixtures/trace.mjs";


test("trace evidence decodes stored and deflated ZIPs with actual action records", async () => {
  const { inspectTrace } = await import("../evals/trace-evidence.mjs");
  for (const method of [0, 8]) assert.deepEqual(inspectTrace(fixture({ method })), { entries: 1, traceEntries: 1, actionRecords: 2 });
});
test("trace evidence rejects fake headers, corrupt archives and traces without actions", async () => {
  const { inspectTrace } = await import("../evals/trace-evidence.mjs");
  const damaged = fixture(); damaged[38] ^= 1;
  const fake = Buffer.alloc(200); fake.writeUInt32LE(0x04034b50);
  for (const bytes of [fake, damaged, fixture().subarray(0, 90), fixture({ content: '{"type":"context-options"}\n' })]) assert.throws(() => inspectTrace(bytes));
});
