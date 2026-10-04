import assert from "node:assert/strict";
import { test } from "node:test";
import { Writable } from "node:stream";
import { writeChildCommand } from "./native-core-scenarios.mjs";

test("child command awaits delivery and preserves exact newlines", async () => {
  let received;
  const child = { stdin: new Writable({ write(bytes, _encoding, done) { received = bytes.toString(); done(); } }) };
  await writeChildCommand(child, "/peers\n");
  assert.equal(received, "/peers\n");
});

test("broken stdin rejects through the awaited task without an uncaught error", async () => {
  const failure = Object.assign(new Error("owned child pipe closed"), { code: "EPIPE" });
  const child = { stdin: new Writable({ write(_bytes, _encoding, done) { done(failure); } }) };
  await assert.rejects(writeChildCommand(child, "/peers\n"), { code: "EPIPE" });
  await new Promise(resolve => setImmediate(resolve));
});

test("destroyed stdin fails immediately", async () => {
  const child = { stdin: new Writable({ write(_bytes, _encoding, done) { done(); } }) };
  child.stdin.destroy();
  await assert.rejects(writeChildCommand(child, "/quit\n"), /not writable/);
});
