import assert from "node:assert/strict";
import { test } from "node:test";
import { Writable } from "node:stream";
import { writeChildCommand } from "./native-core-scenarios.mjs";
test("Windows native helper only emits bounded owned-PID control keys with ABI-sized SendInput", async () => {
  const { nativeWindowsKeyboardScript } = await import("./native-core-scenarios.mjs");
  for (const [key, vk] of [["\uE004", 9], ["\uE007", 13], ["\uE00C", 27], ["\uE00E", 33], ["\uE00F", 34]]) {
    const script = nativeWindowsKeyboardScript(12345, key);
    assert.ok(script.includes(`[NativeKeyboard]::SendOwned(12345, ${vk})`));
    for (const text of ["GetForegroundWindow", "GetWindowThreadProcessId", "actualPid != ownedPid", "Marshal.SizeOf(typeof(INPUT))", "SendInput(2", "inserted != 2", "GetLastWin32Error", "LayoutKind.Explicit", "MOUSEINPUT", "KEYEVENTF_KEYUP", "EnumWindows", "IsWindowVisible", "SetForegroundWindow", "FocusOwned", "Mark 'start'", "Mark 'activate'", "Mark 'compiled'", "keyboard:owned", "Mark 'sent'", "Out.Flush"]) assert.ok(script.includes(text), text);
    assert.ok(!script.includes("SendWait") && !script.includes("dispatchEvent") && !script.includes("WScript.Shell"));
    assert.match(script, /static string DescribeWindow\(IntPtr window\)/);
    assert.match(script, /GetWindowRect\(window, out rect\)/);
    assert.match(script, /GetProcessById\(\(int\)process\).SessionId/);
    assert.match(script, /SetLastError\(0\);\s*bool activated = SetForegroundWindow\(target\);\s*int\? activationError = activated \? \(int\?\)null : Marshal.GetLastWin32Error\(\);/);
    assert.match(script, /Owned application did not become foreground[\s\S]*DescribeWindow\(GetForegroundWindow\(\)\)[\s\S]*DescribeWindow\(target\)[\s\S]*activationError.HasValue/);
    assert.ok(!script.includes("GetWindowText"), "foreground diagnostics never collect window titles");
  }
  for (const pid of [0, -1, NaN, 1.5, "123;evil"]) assert.throws(() => nativeWindowsKeyboardScript(pid, "\uE00C"));
  for (const key of ["a", "{ESC}", "\uE008"]) assert.throws(() => nativeWindowsKeyboardScript(12345, key));
  const { readFile } = await import("node:fs/promises");
  const runner = await readFile(new URL("./hidden-contacts.mjs", import.meta.url), "utf8");
  assert.ok(runner.includes('until("native keyboard helper completion", () => !owned.has(child), process.platform === "win32" ? 60000 : 10000)'));
  assert.ok(runner.includes("redact(child.output.slice(-2000))"));
  assert.ok(runner.includes("nativeWindowsKeyboardScript(pid, value)"));
});
test("cold receipt checkpoint requires exited peer and durable original IDs before navigation", async () => {
  const { receiptColdRestartCheckpoint } = await import("./native-core-scenarios.mjs");
  const ids = ["a".repeat(64), "b".repeat(64), "c".repeat(64)];
  const state = { owner: "owner", ids };
  const c = { owner: "owner", account: "target", peerExited: () => true, observe: async command => command === "owner_account_history" ? ids.map(id => ({ id, from_me: true })) : ids.map(id => ({ id, status: "delivered" })) };
  const checkpoint = await receiptColdRestartCheckpoint(c, state);
  assert.deepEqual(checkpoint.ids, ids);
  assert.equal(checkpoint.offlineBackendDurabilityVerified, true);
  await assert.rejects(receiptColdRestartCheckpoint({ ...c, peerExited: () => false }, state));
  await assert.rejects(receiptColdRestartCheckpoint({ ...c, observe: async command => command === "owner_account_history" ? ids.map(id => ({ id, from_me: true })) : ids.map(id => ({ id, status: "awaiting" })) }, state));
});
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

test("private restart retains UI discovery without requiring public CLI presence", async () => {
  const { signedPeerObservation } = await import("./native-core-scenarios.mjs");
  const rawPeer = { name: "eval-peer", user_id: "b".repeat(32) };
  const peer = { output: "", stdin: new Writable({ write(_bytes, _encoding, done) { done(new Error("private peer must not query public roster")); } }) };
  const found = await signedPeerObservation({ peer, userId: "a".repeat(32), peerName: rawPeer.name, observe: async () => [rawPeer], publiclyVisible: false, until: async (_label, poll) => { const value = await poll(); if (value) return value; throw new Error("missing private observation"); } });
  assert.equal(found, rawPeer);
});

test("native attachment rendering uses the visible jump-to-latest control when history is scrolled", async () => {
  const { revealLatestNativeMessage } = await import("./native-core-scenarios.mjs");
  let observations = 0;
  const commands = [];
  await revealLatestNativeMessage({ execute: async () => ++observations < 3 ? { visible: false, jump: true } : { visible: true, jump: false }, command: async (method, path, body) => { commands.push({ method, path, body }); return { "element-6066-11e4-a52e-4f735466cecf": "jump" }; }, until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("not visibly rendered"); } }, "attachment.txt");
  assert.equal(commands[0].body.value, '[data-testid="jump-to-latest"]');
  assert.equal(commands[1].path, "/element/jump/click");
  assert.equal(commands.length, 2, "do not restart the smooth scroll on every poll");
  assert.equal(observations, 3);
});

test("native attachment rendering retries the same button when WebDriver click does not move the list", async () => {
  const { revealLatestNativeMessage } = await import("./native-core-scenarios.mjs");
  let clock = 0, domClicks = 0;
  const commands = [];
  await revealLatestNativeMessage({
    now: () => clock,
    execute: async script => {
      if (script.includes("b.click()")) { domClicks++; return true; }
      return { visible: domClicks > 0, jump: domClicks === 0 };
    },
    command: async (method, path, body) => { commands.push({ method, path, body }); return { "element-6066-11e4-a52e-4f735466cecf": "jump" }; },
    until: async (_label, poll) => { for (let i = 0; i < 5; i++) { clock += 1000; if (await poll()) return true; } throw new Error("not visibly rendered"); },
  }, "attachment.txt");
  assert.equal(commands.length, 2, "WebDriver still tries the real button first");
  assert.equal(domClicks, 1, "the fallback activates the same button only once");
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
  await revealHistoricalNativeMessage({ execute: async (_script, args) => args ? { visible: ++polls === 3 } : true, command: async (method, path, body) => { commands.push({ method, path, body }); return { "element-6066-11e4-a52e-4f735466cecf": "log" }; }, key: async value => keys.push(value), until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("history not visible"); } }, "old message");
  assert.equal(commands[0].body.value, '[role="log"]');
  assert.equal(commands[1].path, "/element/log/click");
  assert.deepEqual(keys, ["\uE00E", "\uE00E"]);
});

test("native historical rendering scrolls until the receipt badge clears both log edges", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  for (const [initialTop, expectedKey, hasBadge] of [[93, "\uE00E", true], [610, "\uE00F", true], [93, null, false]]) {
    let top = initialTop;
    const keys = [];
    const rect = (top, bottom) => ({ top, bottom, left: 300, right: 1010, width: 710, height: bottom - top });
    const badge = { getBoundingClientRect: () => rect(top, top + 24) };
    const bubble = { textContent: "old receipt", getBoundingClientRect: () => rect(top - 55, top + 26), querySelectorAll: () => hasBadge ? [badge] : [] };
    const log = { getBoundingClientRect: () => rect(102, 624) };
    const document = { activeElement: log, querySelector: () => log, querySelectorAll: () => [bubble] };
    await revealHistoricalNativeMessage({
      execute: async (script, args = []) => Function("document", "innerWidth", "innerHeight", `return function(){${script}}`)(document, 1040, 720)(...args),
      command: async () => ({ "element-6066-11e4-a52e-4f735466cecf": "log" }),
      key: async value => { keys.push(value); top = 300; },
      until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("receipt remains clipped"); },
    }, "old receipt");
    assert.deepEqual(keys, expectedKey ? [expectedKey] : [], "an intersecting body is sufficient only when there is no receipt footer");
  }
});

test("native historical rendering does not turn missing history into success", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  await assert.rejects(revealHistoricalNativeMessage({ execute: async (_script, args) => args ? { visible: false } : true, command: async () => ({ "element-6066-11e4-a52e-4f735466cecf": "log" }), key: async () => {}, until: async (_label, poll) => { if (await poll()) return true; throw new Error("history not visible"); } }, "missing message"), /history not visible/);
});

test("native historical rendering waits for asynchronous history hydration before locating the scroller", async () => {
  const { revealHistoricalNativeMessage } = await import("./native-core-scenarios.mjs");
  let hydrated = false, polls = 0;
  await revealHistoricalNativeMessage({ execute: async (_script, args) => args ? { visible: true } : (hydrated = ++polls > 1), command: async () => { assert.equal(hydrated, true); return { "element-6066-11e4-a52e-4f735466cecf": "log" }; }, key: async () => { throw new Error("visible history needs no key"); }, until: async (_label, poll) => { for (let i = 0; i < 3; i++) if (await poll()) return true; throw new Error("not hydrated"); } }, "restored message");
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

test("each native receipt card is captured while its own expected state is visible", async () => {
  const { receiptPhase } = await import("./native-core-scenarios.mjs");
  const ids = ["a", "b", "c"], labels = ["text", "sticker", "file"];
  let visible, recorded;
  const c = {
    owner: "owner", account: "account",
    observe: async command => command === "owner_account_history" ? ids.map(id => ({ id, from_me: true })) : ids.map(id => ({ id, status: "awaiting" })),
    until: async (_description, poll) => { const result = await poll(); assert.ok(result); return result; },
    execute: async (_script, args) => { visible = args[0]; return args.length === 1 ? { visible: true } : true; },
    command: async () => { throw new Error("already visible"); },
    captureReceiptCard: async (name, expected) => { assert.equal(expected.label, visible); assert.equal(expected.status, "awaiting"); return { filename: `${name}.png`, label: visible, status: expected.status, visible: true }; },
    passed: async (_name, observations, captures) => { recorded = { observations, captures }; },
  };
  await receiptPhase(c, { owner: "owner", ids, labels }, "receipt-offline-awaiting", "awaiting");
  assert.deepEqual(recorded.captures.map(capture => capture.label), [...labels].reverse());
  assert.deepEqual(recorded.observations.captures, recorded.captures);
});

test("localized wrong-password error is captured before successful sign-in recovery", async () => {
  const { wrongPasswordCheckpoint } = await import("./native-core-scenarios.mjs");
  const steps = [], errorCapture = { filename: "auth-logout-wrong-password-error.png", text: "用户名或密码错误", visible: true };
  let observed;
  await wrongPasswordCheckpoint({
    userName: "user", invalidCredentialTexts: ["用户名或密码错误"],
    captureAuthError: async (name, expected) => { steps.push("error capture"); assert.equal(name, "auth-logout-wrong-password-error"); assert.deepEqual(expected.texts, ["用户名或密码错误"]); return errorCapture; },
    login: async name => { steps.push("recovery"); assert.equal(name, "user"); return {owner:"a".repeat(36),backendReady:true,shellReady:true}; },
    passed: async (name, observations, captures) => { steps.push("passed"); assert.equal(name, "auth-logout-wrong-password"); observed = observations; assert.deepEqual(captures, [errorCapture]); },
  });
  assert.deepEqual(steps, ["error capture", "recovery", "passed"]);
  assert.equal(observed.invalidCredentialsKey, "login.invalidCredentials");
  assert.equal(observed.errorCapture.text, "用户名或密码错误");
});

test("attachment completion waits for backend readiness and completed rendered action", async () => {
  const { settledIncomingAttachment } = await import("./native-core-scenarios.mjs");
  assert.equal(typeof settledIncomingAttachment,"function");
  let ready=false,rendered=false,polls=0;
  const c={observe:async()=>[{file_conv:"conv",ready}],execute:async()=>rendered,until:async(_label,poll)=>{for(let i=0;i<3;i++){polls++; if(i===1)ready=true;if(i===2)rendered=true;const value=await poll();if(value)return value;}throw Error("not ready");}};
  const observed=await settledIncomingAttachment(c,"conv","file.txt");
  assert.equal(observed.backendReady,true);assert.equal(observed.renderedComplete,true);assert.equal(polls,3);
});

test("session readiness waits for matching backend owner and rendered ready shell", async () => {
  const { settledSignedInSession } = await import("./native-core-scenarios.mjs");
  let polls=0;
  const c={observe:async()=>({owner:polls<2?'stale':'owner'}),execute:async()=>polls>2,until:async(_label,poll)=>{for(let i=0;i<3;i++){polls++;const result=await poll();if(result)return result;}throw Error('not ready');}};
  assert.deepEqual(await settledSignedInSession(c,'owner'),{owner:'owner',backendReady:true,shellReady:true});
  assert.equal(polls,3);
  await assert.rejects(settledSignedInSession({...c,observe:async()=>({owner:'stale'})},'owner'),/not ready/);
});
test("privacy evidence waits for confirmation removal before settled settings", async()=>{
  const { settledPrivacyMode } = await import("./native-core-scenarios.mjs");
  let present=true;
  const c={execute:async()=>!present,until:async(_label,poll)=>{assert.equal(await poll(),false);present=false;assert.equal(await poll(),true);},settledDialog:async id=>{assert.equal(present,false);assert.equal(id,'settings-dialog');}};
  assert.deepEqual(await settledPrivacyMode(c),{confirmationDismissed:true,dialogSettled:true});
});
test("attachment completion retains the verified backend checkpoint after automatic-save pruning",async()=>{
  const { settledIncomingAttachment } = await import("./native-core-scenarios.mjs");
  const c={observe:async()=>[{file_conv:'conv',ready:false}],execute:async()=>true,until:async(_label,poll)=>{const result=await poll();if(!result)throw Error('not ready');return result;}};
  await assert.rejects(settledIncomingAttachment(c,'conv','file'),/not ready/);
  assert.deepEqual(await settledIncomingAttachment(c,'conv','file',true),{backendReady:true,renderedComplete:true});
});

test("native attachment fixture creates an owned download directory before configuring real settings", async () => {
  const { mkdtemp, stat, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { prepareNativeDownloadFixture } = await import("./native-core-scenarios.mjs");
  const fixture = await mkdtemp(join(tmpdir(), "native-download-fixture-"));
  const owner = "fixture-user";
  const account = "0123456789abcdef0123456789abcdef";
  let settings = { download_dir: "", stay_signed_in: false, retention_days: 30 };
  const calls = [];
  try {
    await assert.rejects(stat(join(fixture, "downloads")), { code: "ENOENT" });
    const invoke = async (command, args) => {
      calls.push(command);
      if (command === "account_id") { assert.deepEqual(args, {}); return account; }
      assert.equal(args.owner, owner, "fixture settings remain scoped to the signed-in owner");
      if (command === "get_app_settings") {
        const snapshot = { ...settings };
        if (calls.filter((call) => call === "get_app_settings").length === 1)
          settings = { ...settings, retention_days: 7 };
        return snapshot;
      }
      assert.equal(command, "set_app_settings");
      assert.equal(args.expectedAccount, account, "settings target the current cryptographic account");
      assert.deepEqual(Object.keys(args).sort(), ["expectedAccount", "owner", "patch", "settings"]);
      assert.equal(args.settings.retention_days, 30, "required IPC settings carrier may be stale");
      assert.equal((await stat(args.patch.download_dir)).isDirectory(), true, "save destination exists before it is configured");
      assert.deepEqual(args.patch, { download_dir: join(fixture, "downloads") });
      settings = { ...settings, ...args.patch };
    };
    const directory = await prepareNativeDownloadFixture({ fixture, owner, invoke });
    assert.equal(directory, join(fixture, "downloads"));
    assert.deepEqual(settings, { download_dir: directory, stay_signed_in: false, retention_days: 7 });
    assert.deepEqual(calls, ["account_id", "get_app_settings", "set_app_settings", "get_app_settings"]);
  } finally { await rm(fixture, { recursive: true, force: true }); }
});


test("native attachment verifies durable export after transient chunks are pruned", async () => {
  const { mkdtemp, writeFile, rm } = await import("node:fs/promises");
  const { join } = await import("node:path");
  const { tmpdir } = await import("node:os");
  const { verifyNativeAttachmentDownload } = await import("./native-core-scenarios.mjs");
  const root = await mkdtemp(join(tmpdir(), "native-download-"));
  const path = join(root, "unique.txt"), expected = Buffer.from("exact decrypted 你好 bytes");
  try {
    let polls = 0;
    const c = { until: async (_label, poll) => {
      assert.equal(await poll(), false, "missing file cannot count as received");
      polls++;
      await writeFile(path, expected);
      return poll();
    } };
    assert.equal(await verifyNativeAttachmentDownload(c, path, expected), true);
    assert.equal(polls, 1);
    await writeFile(path, Buffer.from("corrupt"));
    await assert.rejects(verifyNativeAttachmentDownload({ until: (_label, poll) => poll() }, path, expected), /complete original bytes/);
  } finally { await rm(root, { recursive: true, force: true }); }
});


test("public restoration rechecks an early empty CLI roster despite stale pre-private output", async () => {
  const { signedPeerObservation } = await import("./native-core-scenarios.mjs");
  const userId = "a".repeat(32), rawPeer = { name: "eval-peer", user_id: "b".repeat(32) };
  let queries = 0;
  const peer = { output: `peer ${userId} before private mode\n`, stdin: new Writable({
    write(_bytes, _encoding, done) {
      peer.output += ++queries === 1 ? "(no peers yet)\n" : `peer ${userId} after public restoration\n`;
      done();
    },
  }) };
  const found = await signedPeerObservation({ peer, userId, peerName: rawPeer.name,
    observe: async () => [rawPeer], until: async (_label, poll) => {
      for (let i = 0; i < 3; i++) { const result = await poll(); if (result) return result; }
      throw new Error("fresh public discovery missing");
    },
  });
  assert.equal(found, rawPeer);
  assert.equal(queries, 2, "old stdout and one early empty query cannot prove current public discovery");
});

test("native account entry submits once and defers the optional backup reminder", async () => {
  const { enterNativeAccount } = await import("./native-core-scenarios.mjs");
  for (const register of [false, true]) {
    const calls = [];
    const ui = {
      click: async id => calls.push(["click", id]),
      fill: async (id, value) => calls.push(["fill", id, value]),
      element: async id => calls.push(["element", id]),
    };
    await enterNativeAccount(ui, "alice", "password123", register);
    assert.deepEqual(calls, [
      ...(register ? [["click", "login-tab-register"]] : []),
      ["fill", "login-username", "alice"],
      ["fill", "login-password", "password123"],
      ["click", "login-submit"],
      ["element", "chat-shell"],
      ...(register ? [["click", "account-protection-later"]] : []),
    ]);
  }
});


test("native attachment fixture never writes settings without a current account", async () => {
  const { mkdtemp, rm } = await import("node:fs/promises");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const { prepareNativeDownloadFixture } = await import("./native-core-scenarios.mjs");
  const fixture = await mkdtemp(join(tmpdir(), "native-download-account-"));
  const calls = [];
  try {
    await assert.rejects(prepareNativeDownloadFixture({ fixture, owner: "fixture-user", invoke: async (command) => { calls.push(command); return undefined; } }), /requires the current account/);
    assert.deepEqual(calls, ["account_id"]);
  } finally { await rm(fixture, { recursive: true, force: true }); }
});
