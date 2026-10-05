import assert from "node:assert/strict";
import { test } from "node:test";
import { Writable } from "node:stream";
import { writeChildCommand } from "./native-core-scenarios.mjs";
test("receipt evidence rejects sparse missing or changed message IDs", async () => {
  const { orderedReceiptStatuses } = await import("./native-core-scenarios.mjs");
  const ids = ["a".repeat(64), "b".repeat(64), "c".repeat(64)];
  const records = ids.map(id => ({ id, status: "delivered" }));
  assert.deepEqual(orderedReceiptStatuses(ids, records.reverse()), ["delivered", "delivered", "delivered"]);
  assert.throws(() => orderedReceiptStatuses(ids, records.slice(1)));
  assert.throws(() => orderedReceiptStatuses(ids, [...records, records[0]]));
});

test("exact native content fixtures respect the composer's intentional outer trimming", async () => {
  const { nativeMessageContents } = await import("./native-core-scenarios.mjs");
  const contents = nativeMessageContents("fixture-nonce");
  assert.equal(contents.length, 3);
  for (const content of contents) assert.ok(content === content.trim(), "exact rendering/transport fixture must not include intentionally discarded outer whitespace");
  assert.equal(contents[1].split("\n").length, 3);
  assert.ok(contents[2].length > 2000);
  assert.ok(contents[2].endsWith("end-fixture-nonce"), "long-message tail must also be preserved exactly");
});

test("signed discovery waits for the restarted UI roster and a fresh CLI response", async () => {
  const { signedPeerObservation } = await import("./native-core-scenarios.mjs");
  const userId = "a".repeat(32), rawPeer = { name: "eval-peer", user_id: "b".repeat(32) };
  let polls = 0;
  const peer = { output: `peer ${userId}\n`, stdin: new Writable({ write(_bytes, _encoding, done) { peer.output += `peer ${userId}\n`; done(); } }) };
  const found = await signedPeerObservation({ peer, userId, peerName: rawPeer.name, observe: async () => ++polls === 1 ? [] : [rawPeer], until: async (_label, poll) => { for (let i = 0; i < 2; i++) { const value = await poll(); if (value) return value; } throw new Error("missing fresh observation"); } });
  assert.equal(found, rawPeer);
  assert.equal(polls, 2);
});

test("signed discovery cannot use a stale pre-command CLI roster line", async () => {
  const { signedPeerObservation } = await import("./native-core-scenarios.mjs");
  const userId = "a".repeat(32), rawPeer = { name: "eval-peer", user_id: "b".repeat(32) };
  const peer = { output: `peer ${userId}\n`, stdin: new Writable({ write(_bytes, _encoding, done) { done(); } }) };
  await assert.rejects(signedPeerObservation({ peer, userId, peerName: rawPeer.name, observe: async () => [rawPeer], until: async (_label, poll) => { const value = await poll(); if (value) return value; throw new Error("missing fresh observation"); } }), /missing fresh observation/);
});

test("signed discovery queries again when the first CLI roster is still empty", async () => {
  const { signedPeerObservation } = await import("./native-core-scenarios.mjs");
  const userId = "a".repeat(32), rawPeer = { name: "eval-peer", user_id: "b".repeat(32) };
  let queries = 0;
  const peer = { output: "", stdin: new Writable({ write(_bytes, _encoding, done) { peer.output += ++queries === 1 ? "(no peers yet)\n" : `peer ${userId}\n`; done(); } }) };
  await signedPeerObservation({ peer, userId, peerName: rawPeer.name, observe: async () => [rawPeer], until: async (_label, poll) => { for (let i = 0; i < 3; i++) { const value = await poll(); if (value) return value; } throw new Error("missing fresh observation"); } });
  assert.equal(queries, 2);
});

test("native attachment rendering uses the visible jump-to-latest control when history is scrolled", async () => {
  const { revealLatestNativeMessage } = await import("./native-core-scenarios.mjs");
  let observations = 0;
  const commands = [];
  await revealLatestNativeMessage({ execute: async () => ++observations === 1 ? { visible: false, jump: true } : { visible: true, jump: false }, command: async (method, path, body) => { commands.push({ method, path, body }); return { "element-6066-11e4-a52e-4f735466cecf": "jump" }; }, until: async (_label, poll) => { for (let i = 0; i < 2; i++) if (await poll()) return true; throw new Error("not visibly rendered"); } }, "attachment.txt");
  assert.equal(commands[0].body.value, 'button[aria-label="Jump to latest messages"]');
  assert.equal(commands[1].path, "/element/jump/click");
  assert.equal(observations, 2);
});

test("native attachment rendering still fails if no visible message or jump control exists", async () => {
  const { revealLatestNativeMessage } = await import("./native-core-scenarios.mjs");
  await assert.rejects(revealLatestNativeMessage({ execute: async () => ({ visible: false, jump: false }), command: async () => { throw new Error("unexpected action"); }, until: async (_label, poll) => { if (await poll()) return true; throw new Error("not visibly rendered"); } }, "attachment.txt"), /not visibly rendered/);
});

test("native settings use the observed default and minimum client viewports, not nominal outer dimensions", async () => {
  const { nativeSettingsTargets } = await import("./native-core-scenarios.mjs");
  const targets = nativeSettingsTargets({ width: 1024, height: 702 }, { width: 1024, height: 674 }, { width: 760, height: 520 });
  assert.deepEqual(targets, [
    { name: "defaultLayout", request: { width: 1024, height: 702 }, viewport: { width: 1024, height: 674 } },
    { name: "minimumLayout", request: { width: 760, height: 548 }, viewport: { width: 760, height: 520 } },
  ]);
});

test("native minimum client dimensions include the measured Windows non-client border", async () => {
  const { nativeMinimumWindowRequest } = await import("./native-core-scenarios.mjs");
  assert.deepEqual(nativeMinimumWindowRequest({ width: 1056, height: 728 }, { width: 1040, height: 720 }), { width: 776, height: 528 });
  assert.deepEqual(nativeMinimumWindowRequest({ width: 1040, height: 720 }, { width: 1040, height: 720 }), { width: 760, height: 520 });
  assert.throws(() => nativeMinimumWindowRequest({ width: 1000, height: 700 }, { width: 1040, height: 720 }), /native window chrome/);
});

test("native historical rendering uses the focusable message log and real PageUp until visible", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  const commands = [], keys = [];
  let polls = 0;
  await revealHistoricalNativeMessage({ execute: async (_script, args) => args ? ++polls === 3 : true, command: async (method, path, body) => { commands.push({ method, path, body }); return { "element-6066-11e4-a52e-4f735466cecf": "log" }; }, key: async value => keys.push(value), until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("history not visible"); } }, "old message");
  assert.equal(commands[0].body.value, '[role="log"]');
  assert.equal(commands[1].path, "/element/log/click");
  assert.deepEqual(keys, ["\uE00E", "\uE00E"]);
});

test("native historical rendering does not turn missing history into success", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  await assert.rejects(revealHistoricalNativeMessage({ execute: async (_script, args) => !args, command: async () => ({ "element-6066-11e4-a52e-4f735466cecf": "log" }), key: async () => {}, until: async (_label, poll) => { if (await poll()) return true; throw new Error("history not visible"); } }, "missing message"), /history not visible/);
});

test("native historical rendering waits for asynchronous history hydration before locating the scroller", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  let hydrated = false, polls = 0;
  await revealHistoricalNativeMessage({ execute: async (_script, args) => args ? true : (hydrated = ++polls > 1), command: async () => { assert.equal(hydrated, true); return { "element-6066-11e4-a52e-4f735466cecf": "log" }; }, key: async () => { throw new Error("visible history needs no key"); }, until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("not hydrated"); } }, "restored message");
  assert.equal(polls, 2);
});

test("native layout coverage rejects invalid or indistinguishable viewport tiers", async () => {
  const { nativeSettingsTargets } = await import("./native-core-scenarios.mjs");
  for (const viewport of [{ width: NaN, height: 674 }, { width: 760, height: 400 }, { width: 760, height: 520 }]) {
    assert.throws(() => nativeSettingsTargets({ width: 1024, height: 702 }, viewport, { width: 760, height: 520 }), /native layout/);
  }
});

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
