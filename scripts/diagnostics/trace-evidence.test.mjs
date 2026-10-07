import assert from "node:assert/strict";
import { test } from "node:test";
import { deflateRawSync } from "node:zlib";
import { crc32 } from "../evals/png-evidence.mjs";

function fixture({ content = '{"type":"before"}\n{"type":"after"}\n', method = 0 } = {}) {
  const name = Buffer.from("0.trace"), data = Buffer.from(content), packed = method === 8 ? deflateRawSync(data) : data;
  const local = Buffer.alloc(30); local.writeUInt32LE(0x04034b50); local.writeUInt16LE(20, 4); local.writeUInt16LE(method, 8); local.writeUInt32LE(crc32(data), 14); local.writeUInt32LE(packed.length, 18); local.writeUInt32LE(data.length, 22); local.writeUInt16LE(name.length, 26);
  const body = Buffer.concat([local, name, packed]);
  const central = Buffer.alloc(46); central.writeUInt32LE(0x02014b50); central.writeUInt16LE(20, 4); central.writeUInt16LE(20, 6); central.writeUInt16LE(method, 10); central.writeUInt32LE(crc32(data), 16); central.writeUInt32LE(packed.length, 20); central.writeUInt32LE(data.length, 24); central.writeUInt16LE(name.length, 28);
  const directory = Buffer.concat([central, name]);
  const end = Buffer.alloc(22); end.writeUInt32LE(0x06054b50); end.writeUInt16LE(1, 8); end.writeUInt16LE(1, 10); end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(body.length, 16);
  return Buffer.concat([body, directory, end]);
}
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
